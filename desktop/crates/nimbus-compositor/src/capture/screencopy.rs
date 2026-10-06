// SPDX-License-Identifier: MIT

//! `zwlr_screencopy_manager_v1` version 3: outputs and their regions, into shm or dmabuf buffers.

use super::{PendingFrame, Rendered, SharedDamage, Spec, lock, timestamp};
use crate::state::State;
use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Buffer, Rectangle, Size};
use smithay::wayland::shm::with_buffer_contents;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const VERSION: u32 = 3;
const SHM_FORMAT: wl_shm::Format = wl_shm::Format::Xrgb8888;

pub fn create_global(dh: &DisplayHandle) {
    dh.create_global::<State, ZwlrScreencopyManagerV1, _>(VERSION, ());
}

/// Per manager, the damage of `copy_with_damage` frames by output name.
#[derive(Default)]
pub struct ManagerData {
    damage: Mutex<HashMap<String, SharedDamage>>,
}

pub struct FrameData {
    /// `None` for a frame that failed when it was created.
    spec: Option<Spec>,
    size: Size<i32, Buffer>,
    dmabuf_format: Option<Fourcc>,
    damage: SharedDamage,
    used: AtomicBool,
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ManagerData::default());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ManagerData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        data: &ManagerData,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let (frame, overlay_cursor, output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, overlay_cursor, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (
                frame,
                overlay_cursor,
                output,
                Some(Rectangle::new((x, y).into(), (width, height).into())),
            ),
            zwlr_screencopy_manager_v1::Request::Destroy => return,
            _ => unreachable!(),
        };
        state.new_screencopy_frame(data, frame, &output, region, overlay_cursor != 0, data_init);
    }
}

impl State {
    fn new_screencopy_frame(
        &mut self,
        manager: &ManagerData,
        frame: New<ZwlrScreencopyFrameV1>,
        output: &WlOutput,
        region: Option<Rectangle<i32, smithay::utils::Logical>>,
        cursor: bool,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let spec = Output::from_resource(output)
            .map(|output| Spec { region, ..Spec::output(output, cursor) });
        let resolved = spec.as_ref().and_then(|spec| spec.resolve(&self.nimbus));
        let damage = match &spec {
            Some(Spec { source: super::Source::Output(output), .. }) => {
                lock(&manager.damage).entry(output.name()).or_default().clone()
            }
            _ => SharedDamage::default(),
        };
        let Some(resolved) = resolved else {
            let data = FrameData {
                spec: None,
                size: Size::default(),
                dmabuf_format: None,
                damage,
                used: AtomicBool::new(false),
            };
            data_init.init(frame, data).failed();
            return;
        };
        let size = resolved.buffer_size();
        let dmabuf_format = self
            .backend
            .capture_dmabuf()
            .and_then(|constraints| constraints.formats.first().map(|(code, _)| *code));
        let data = FrameData { spec, size, dmabuf_format, damage, used: AtomicBool::new(false) };
        let frame = data_init.init(frame, data);
        let (width, height) = (size.w.unsigned_abs(), size.h.unsigned_abs());
        frame.buffer(SHM_FORMAT, width, height, width * 4);
        if frame.version() >= 3 {
            if let Some(format) = dmabuf_format {
                frame.linux_dmabuf(format as u32, width, height);
            }
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            zwlr_screencopy_frame_v1::Request::Destroy => return,
            _ => unreachable!(),
        };
        if data.used.swap(true, Ordering::Relaxed) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "the frame was already copied",
            );
            return;
        }
        let Some(spec) = data.spec.clone() else {
            return;
        };
        if !matches_frame(&buffer, data) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                "the buffer doesn't match the advertised format or size",
            );
            return;
        }
        state.nimbus.capture.queue(PendingFrame {
            spec,
            buffer,
            damage: with_damage.then(|| data.damage.clone()),
            sink: super::Sink::Screencopy(Sink { frame: frame.clone(), with_damage }),
        });
    }
}

/// Whether `buffer` is one of the buffers the frame advertised.
fn matches_frame(buffer: &WlBuffer, data: &FrameData) -> bool {
    if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer) {
        return dmabuf.size() == data.size && Some(dmabuf.format().code) == data.dmabuf_format;
    }
    with_buffer_contents(buffer, |_, _, shm| {
        shm.format == SHM_FORMAT
            && (shm.width, shm.height) == (data.size.w, data.size.h)
            && shm.stride == data.size.w * 4
    })
    .unwrap_or(false)
}

pub struct Sink {
    frame: ZwlrScreencopyFrameV1,
    with_damage: bool,
}

impl Sink {
    pub fn alive(&self) -> bool {
        self.frame.is_alive()
    }

    pub fn ready(&self, rendered: &Rendered, time: Duration) {
        self.frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
        if self.with_damage {
            for rect in &rendered.damage {
                self.frame.damage(
                    rect.loc.x.unsigned_abs(),
                    rect.loc.y.unsigned_abs(),
                    rect.size.w.unsigned_abs(),
                    rect.size.h.unsigned_abs(),
                );
            }
        }
        let (hi, lo, nsec) = timestamp(time);
        self.frame.ready(hi, lo, nsec);
    }

    pub fn failed(&self) {
        self.frame.failed();
    }
}
