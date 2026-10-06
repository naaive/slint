// SPDX-License-Identifier: MIT

//! `ext-image-capture-source-v1` for outputs and ext-foreign-toplevel-list toplevels,
//! and `ext-image-copy-capture-v1` sessions on them.
//!
//! Cursor sessions stop as soon as a client asks for their capture session;
//! frames paint the cursor with the `paint_cursors` option instead.

use super::{
    DmabufConstraints, Failure, PendingFrame, Rendered, SHM_FORMATS, SharedDamage, Source, Spec,
    lock, timestamp,
};
use crate::state::State;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::{
    ext_foreign_toplevel_image_capture_source_manager_v1::{
        self, ExtForeignToplevelImageCaptureSourceManagerV1,
    },
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
};
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::{
    ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Buffer, Size, Transform};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub fn create_globals(dh: &DisplayHandle) {
    dh.create_global::<State, ExtOutputImageCaptureSourceManagerV1, _>(1, ());
    dh.create_global::<State, ExtForeignToplevelImageCaptureSourceManagerV1, _>(1, ());
    dh.create_global::<State, ExtImageCopyCaptureManagerV1, _>(1, ());
}

/// The live sessions, whose constraints follow their source.
#[derive(Default)]
pub struct SessionList(Vec<ExtImageCopyCaptureSessionV1>);

pub struct SessionData {
    spec: Spec,
    damage: SharedDamage,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    /// The buffer size in the constraints sent last.
    size: Option<Size<i32, Buffer>>,
    stopped: bool,
    frame: Option<ExtImageCopyCaptureFrameV1>,
}

pub struct FrameData {
    spec: Spec,
    damage: SharedDamage,
    state: Mutex<FrameState>,
}

#[derive(Default)]
struct FrameState {
    buffer: Option<WlBuffer>,
    captured: bool,
    /// The session stopped before the frame was captured.
    stopped: bool,
}

macro_rules! manager_globals {
    ($($interface:ty),*) => {$(
        impl GlobalDispatch<$interface, ()> for State {
            fn bind(
                _: &mut Self,
                _: &DisplayHandle,
                _: &Client,
                resource: New<$interface>,
                _: &(),
                data_init: &mut DataInit<'_, Self>,
            ) {
                data_init.init(resource, ());
            }
        }
    )*};
}

manager_globals!(
    ExtOutputImageCaptureSourceManagerV1,
    ExtForeignToplevelImageCaptureSourceManagerV1,
    ExtImageCopyCaptureManagerV1
);

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source,
            output,
        } = request
        {
            let target = Output::from_resource(&output).map_or(Source::Gone, Source::Output);
            data_init.init(source, target);
        }
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource {
            source,
            toplevel_handle,
        } = request
        {
            let target = ForeignToplevelHandle::from_resource(&toplevel_handle)
                .filter(|handle| !handle.is_closed())
                .map_or(Source::Gone, |handle| Source::Toplevel(handle.identifier()));
            data_init.init(source, target);
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, Source> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtImageCaptureSourceV1,
        _: ext_image_capture_source_v1::Request,
        _: &Source,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession {
                session,
                source,
                options,
            } => {
                let WEnum::Value(options) = options else {
                    manager.post_error(
                        ext_image_copy_capture_manager_v1::Error::InvalidOption,
                        "unknown options",
                    );
                    return;
                };
                let spec = Spec {
                    source: source.data::<Source>().cloned().unwrap_or(Source::Gone),
                    region: None,
                    cursor: options
                        .contains(ext_image_copy_capture_manager_v1::Options::PaintCursors),
                };
                let data =
                    SessionData { spec, damage: SharedDamage::default(), state: Mutex::default() };
                let session = data_init.init(session, data);
                state.nimbus.capture.sessions.0.push(session);
                state.refresh_capture_sessions();
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                ..
            } => {
                data_init.init(session, AtomicBool::new(false));
            }
            ext_image_copy_capture_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, AtomicBool> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        cursor_session: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        has_session: &AtomicBool,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } =
            request
        {
            if has_session.swap(true, Ordering::Relaxed) {
                cursor_session.post_error(
                    ext_image_copy_capture_cursor_session_v1::Error::DuplicateSession,
                    "the cursor session already has a capture session",
                );
                return;
            }
            let data = SessionData {
                spec: Spec { source: Source::Gone, region: None, cursor: false },
                damage: SharedDamage::default(),
                state: Mutex::new(SessionState { stopped: true, ..SessionState::default() }),
            };
            data_init.init(session, data).stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        data: &SessionData,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            let mut session_state = lock(&data.state);
            if session_state.frame.as_ref().is_some_and(Resource::is_alive) {
                session.post_error(
                    ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                    "the previous frame still exists",
                );
                return;
            }
            let frame_data = FrameData {
                spec: data.spec.clone(),
                damage: data.damage.clone(),
                state: Mutex::new(FrameState {
                    stopped: session_state.stopped,
                    ..FrameState::default()
                }),
            };
            session_state.frame = Some(data_init.init(frame, frame_data));
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Error;
        let mut frame_state = lock(&data.state);
        let already_captured = |frame: &ExtImageCopyCaptureFrameV1| {
            frame.post_error(Error::AlreadyCaptured, "the frame was already captured");
        };
        match request {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                if frame_state.captured {
                    return already_captured(frame);
                }
                frame_state.buffer = Some(buffer);
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer { x, y, width, height } => {
                if frame_state.captured {
                    return already_captured(frame);
                }
                // Every capture writes the whole buffer, so the damage only needs checking.
                if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(Error::InvalidBufferDamage, "invalid buffer damage");
                }
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                if frame_state.captured {
                    return already_captured(frame);
                }
                let Some(buffer) = frame_state.buffer.clone() else {
                    frame.post_error(Error::NoBuffer, "no buffer is attached");
                    return;
                };
                frame_state.captured = true;
                if frame_state.stopped {
                    frame.failed(ext_image_copy_capture_frame_v1::FailureReason::Stopped);
                    return;
                }
                state.nimbus.capture.queue(PendingFrame {
                    spec: data.spec.clone(),
                    buffer,
                    damage: Some(data.damage.clone()),
                    sink: super::Sink::ImageCopy(Sink(frame.clone())),
                });
            }
            ext_image_copy_capture_frame_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl State {
    /// Stops sessions whose source is gone, and sends new constraints when a source changes size.
    pub(super) fn refresh_capture_sessions(&mut self) {
        let mut sessions = std::mem::take(&mut self.nimbus.capture.sessions.0);
        sessions.retain(Resource::is_alive);
        let mut dmabuf: Option<Option<DmabufConstraints>> = None;
        for session in &sessions {
            let Some(data) = session.data::<SessionData>() else {
                continue;
            };
            let mut session_state = lock(&data.state);
            if session_state.stopped {
                continue;
            }
            let Some(size) = data.spec.resolve(&self.nimbus).map(|r| r.buffer_size()) else {
                session_state.stopped = true;
                session.stopped();
                continue;
            };
            if session_state.size == Some(size) {
                continue;
            }
            session_state.size = Some(size);
            session.buffer_size(size.w.unsigned_abs(), size.h.unsigned_abs());
            for format in SHM_FORMATS {
                session.shm_format(format);
            }
            let dmabuf = dmabuf.get_or_insert_with(|| self.backend.capture_dmabuf());
            if let Some(constraints) = dmabuf {
                session.dmabuf_device(constraints.device.to_ne_bytes().to_vec());
                for (code, modifiers) in &constraints.formats {
                    let modifiers = modifiers.iter().flat_map(|m| m.to_ne_bytes()).collect();
                    session.dmabuf_format(*code as u32, modifiers);
                }
            }
            session.done();
        }
        sessions.retain(|s| s.data::<SessionData>().is_some_and(|d| !lock(&d.state).stopped));
        self.nimbus.capture.sessions.0 = sessions;
    }
}

pub struct Sink(ExtImageCopyCaptureFrameV1);

impl Sink {
    pub fn alive(&self) -> bool {
        self.0.is_alive()
    }

    pub fn ready(&self, rendered: &Rendered, transform: Transform, time: Duration) {
        self.0.transform(transform.into());
        for rect in &rendered.damage {
            self.0.damage(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
        }
        let (hi, lo, nsec) = timestamp(time);
        self.0.presentation_time(hi, lo, nsec);
        self.0.ready();
    }

    pub fn failed(&self, failure: Failure) {
        use ext_image_copy_capture_frame_v1::FailureReason;
        self.0.failed(match failure {
            Failure::Unknown => FailureReason::Unknown,
            Failure::BufferConstraints => FailureReason::BufferConstraints,
            Failure::Stopped => FailureReason::Stopped,
        });
    }
}
