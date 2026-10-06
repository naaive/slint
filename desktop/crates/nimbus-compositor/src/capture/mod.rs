// SPDX-License-Identifier: MIT

//! Screen capture: `wlr-screencopy`, `ext-image-copy-capture`, and the control socket's screenshots,
//! all rendered by [`render`] from the scene in [`crate::render`].

pub mod image_copy;
mod render;
pub mod screencopy;

pub use render::render;

use crate::render::Capture;
use crate::state::{Nimbus, State};
use anyhow::Context;
use image_copy::SessionList;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Format, Fourcc};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Id;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Size, Transform};
use smithay::wayland::shm::with_buffer_contents;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Formats of captured buffers, as DRM fourcc codes; the shm formats are the same.
const FORMATS: [Fourcc; 4] =
    [Fourcc::Xrgb8888, Fourcc::Argb8888, Fourcc::Xbgr8888, Fourcc::Abgr8888];

pub const SHM_FORMATS: [wl_shm::Format; 4] = [
    wl_shm::Format::Xrgb8888,
    wl_shm::Format::Argb8888,
    wl_shm::Format::Xbgr8888,
    wl_shm::Format::Abgr8888,
];

/// What a capture shows.
#[derive(Clone, Debug)]
pub enum Source {
    Output(Output),
    /// A window, by its ext-foreign-toplevel-list identifier.
    Toplevel(String),
    /// An output or window that's gone.
    Gone,
}

/// A capture request that outlives the event it came with.
#[derive(Clone, Debug)]
pub struct Spec {
    pub source: Source,
    /// Part of an output, in its logical coordinates.
    pub region: Option<Rectangle<i32, Logical>>,
    pub cursor: bool,
}

impl Spec {
    pub fn output(output: Output, cursor: bool) -> Self {
        Self { source: Source::Output(output), region: None, cursor }
    }

    /// Finds what to capture now, or `None` when it's gone, disabled, or empty.
    pub fn resolve(&self, nimbus: &Nimbus) -> Option<Resolved> {
        match &self.source {
            Source::Output(output) => {
                if !nimbus.outputs().any(|o| o == output) {
                    return None;
                }
                let mode = output.current_mode()?;
                let scale = output.current_scale().fractional_scale();
                let transform = output.current_transform();
                let full = Rectangle::from_size(transform.transform_size(mode.size));
                let area = match self.region {
                    Some(region) => region.to_physical_precise_round(scale).intersection(full)?,
                    None => full,
                };
                let subject = Subject::Output(output.clone());
                Resolved::new(subject, area, scale, transform)
            }
            Source::Toplevel(identifier) => {
                let window = toplevel_window(nimbus, identifier)?;
                let scale = nimbus
                    .wm
                    .space
                    .outputs_for_element(&window)
                    .first()
                    .map_or(1.0, |o| o.current_scale().fractional_scale());
                let area =
                    Rectangle::from_size(window.geometry().size.to_physical_precise_round(scale));
                Resolved::new(Subject::Window(window), area, scale, Transform::Normal)
            }
            Source::Gone => None,
        }
    }
}

fn toplevel_window(nimbus: &Nimbus, identifier: &str) -> Option<Window> {
    nimbus.wm.infos().into_iter().find_map(|info| {
        let managed = nimbus.wm.get(info.id)?;
        let handle = managed.foreign.as_ref()?;
        (handle.identifier() == identifier).then(|| managed.window.clone())
    })
}

pub enum Subject {
    Output(Output),
    Window(Window),
}

/// A capture's subject and the part of it to capture.
pub struct Resolved {
    pub subject: Subject,
    /// In the subject's physical pixels, upright.
    pub area: Rectangle<i32, Physical>,
    pub scale: f64,
    /// From upright to the buffer, like `wl_output.transform`.
    pub transform: Transform,
}

impl Resolved {
    fn new(
        subject: Subject,
        area: Rectangle<i32, Physical>,
        scale: f64,
        transform: Transform,
    ) -> Option<Self> {
        (area.size.w > 0 && area.size.h > 0).then_some(Self { subject, area, scale, transform })
    }

    pub fn buffer_size(&self) -> Size<i32, Buffer> {
        self.area.size.to_logical(1).to_buffer(1, self.transform)
    }
}

/// Where a capture goes.
pub enum Target<'a> {
    /// A [`Capture`] in [`Rendered::image`].
    Memory,
    Shm(&'a WlBuffer),
    Dmabuf(Dmabuf),
}

impl<'a> Target<'a> {
    /// Takes `buffer` if it's an shm or dmabuf buffer of `size` in one of the capture formats.
    pub fn from_buffer(buffer: &'a WlBuffer, size: Size<i32, Buffer>) -> Option<Self> {
        if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer) {
            return (dmabuf.size() == size && FORMATS.contains(&dmabuf.format().code))
                .then(|| Self::Dmabuf(dmabuf.clone()));
        }
        let fits = with_buffer_contents(buffer, |_, _, data| {
            SHM_FORMATS.contains(&data.format)
                && (data.width, data.height) == (size.w, size.h)
                && data.stride >= data.width * 4
        });
        fits.unwrap_or(false).then_some(Self::Shm(buffer))
    }
}

/// What [`render`] works on.
pub struct Job<'a> {
    pub resolved: &'a Resolved,
    pub cursor: bool,
    /// Shows the lock screen while locked, instead of black.
    pub reveal_lock: bool,
    pub target: Target<'a>,
    /// Waits for damage since the last frame, and reports it.
    pub damage: Option<&'a mut Damage>,
}

pub struct Rendered {
    pub damage: Vec<Rectangle<i32, Buffer>>,
    /// The pixels, for [`Target::Memory`].
    pub image: Option<Capture>,
}

/// The state of a capture stream that reports what changed between frames.
#[derive(Default)]
pub struct Damage {
    tracker: Option<(Size<i32, Physical>, f64, Transform, OutputDamageTracker)>,
}

impl Damage {
    /// The tracker for `resolved`, and whether it's new, so its first damage is the whole buffer.
    fn tracker(&mut self, resolved: &Resolved) -> (&mut OutputDamageTracker, bool) {
        let key = (resolved.area.size, resolved.scale, resolved.transform);
        let fresh = self
            .tracker
            .as_ref()
            .is_none_or(|(size, scale, transform, _)| (*size, *scale, *transform) != key);
        if fresh {
            let tracker = OutputDamageTracker::new(
                resolved.transform.transform_size(resolved.area.size),
                resolved.scale,
                resolved.transform,
            );
            self.tracker = Some((key.0, key.1, key.2, tracker));
        }
        let (.., tracker) = self.tracker.as_mut().expect("set above");
        (tracker, fresh)
    }
}

pub type SharedDamage = Arc<Mutex<Damage>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Dmabuf constraints of a renderer: the device and the formats it renders to.
pub struct DmabufConstraints {
    pub device: libc::dev_t,
    pub formats: Vec<(Fourcc, Vec<u64>)>,
}

impl DmabufConstraints {
    /// The capture formats among `formats`, with their modifiers.
    pub fn new(device: libc::dev_t, formats: impl IntoIterator<Item = Format>) -> Option<Self> {
        let mut by_code: Vec<(Fourcc, Vec<u64>)> = Vec::new();
        for format in formats.into_iter().filter(|f| FORMATS.contains(&f.code)) {
            let modifier = u64::from(format.modifier);
            match by_code.iter_mut().find(|(code, _)| *code == format.code) {
                Some((_, modifiers)) => modifiers.push(modifier),
                None => by_code.push((format.code, vec![modifier])),
            }
        }
        by_code.sort_by_key(|(code, _)| FORMATS.iter().position(|f| f == code));
        (!by_code.is_empty()).then_some(Self { device, formats: by_code })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Failure {
    Unknown,
    BufferConstraints,
    Stopped,
}

/// The protocol object that hears about a frame.
pub enum Sink {
    Screencopy(screencopy::Sink),
    ImageCopy(image_copy::Sink),
}

impl Sink {
    fn alive(&self) -> bool {
        match self {
            Self::Screencopy(sink) => sink.alive(),
            Self::ImageCopy(sink) => sink.alive(),
        }
    }

    fn ready(&self, rendered: &Rendered, transform: Transform, time: Duration) {
        match self {
            Self::Screencopy(sink) => sink.ready(rendered, time),
            Self::ImageCopy(sink) => sink.ready(rendered, transform, time),
        }
    }

    fn failed(&self, failure: Failure) {
        match self {
            Self::Screencopy(sink) => sink.failed(),
            Self::ImageCopy(sink) => sink.failed(failure),
        }
    }
}

/// A frame waiting for the next render, or for damage.
pub struct PendingFrame {
    pub spec: Spec,
    pub buffer: WlBuffer,
    pub damage: Option<SharedDamage>,
    pub sink: Sink,
}

pub struct CaptureState {
    pending: Vec<PendingFrame>,
    sessions: SessionList,
    /// The black that covers captures while locked; a stable id keeps it from damaging every frame.
    locked: Id,
}

impl CaptureState {
    /// Creates the capture globals.
    pub fn new(dh: &DisplayHandle) -> Self {
        screencopy::create_global(dh);
        image_copy::create_globals(dh);
        Self { pending: Vec::new(), sessions: SessionList::default(), locked: Id::new() }
    }

    pub fn queue(&mut self, frame: PendingFrame) {
        self.pending.push(frame);
    }
}

impl State {
    /// Captures the frames that are due, and updates the constraints of capture sessions.
    pub fn process_captures(&mut self) {
        self.refresh_capture_sessions();
        if self.nimbus.capture.pending.is_empty() {
            return;
        }
        let mut waiting = Vec::new();
        for frame in std::mem::take(&mut self.nimbus.capture.pending) {
            if !frame.sink.alive() {
                continue;
            }
            let Some(resolved) = frame.spec.resolve(&self.nimbus) else {
                frame.sink.failed(Failure::Stopped);
                continue;
            };
            let Some(target) = Target::from_buffer(&frame.buffer, resolved.buffer_size()) else {
                frame.sink.failed(Failure::BufferConstraints);
                continue;
            };
            let mut damage = frame.damage.as_deref().map(lock);
            let job = Job {
                resolved: &resolved,
                cursor: frame.spec.cursor,
                reveal_lock: false,
                target,
                damage: damage.as_deref_mut(),
            };
            match self.backend.capture(&self.nimbus, job) {
                Ok(Some(rendered)) => {
                    frame.sink.ready(&rendered, resolved.transform, self.nimbus.clock.now().into());
                }
                Ok(None) => {
                    drop(damage);
                    waiting.push(frame);
                }
                Err(err) => {
                    tracing::debug!("capture failed: {err:#}");
                    // The next frame reports everything, since this one never reached the client.
                    if let Some(damage) = damage.as_deref_mut() {
                        *damage = Damage::default();
                    }
                    frame.sink.failed(Failure::Unknown);
                }
            }
        }
        self.nimbus.capture.pending.extend(waiting);
    }

    /// Renders `output` for a screenshot; see [`screenshot`].
    pub fn screenshot_image(&mut self, output: &Output) -> anyhow::Result<Capture> {
        screenshot(&self.nimbus, output, |job| self.backend.capture(&self.nimbus, job))
    }
}

/// Renders `output` into memory through `render`, upright and with the lock screen while locked.
pub fn screenshot(
    nimbus: &Nimbus,
    output: &Output,
    render: impl FnOnce(Job<'_>) -> anyhow::Result<Option<Rendered>>,
) -> anyhow::Result<Capture> {
    let mut resolved =
        Spec::output(output.clone(), false).resolve(nimbus).context("the output isn't enabled")?;
    resolved.transform = Transform::Normal;
    let job = Job {
        resolved: &resolved,
        cursor: false,
        reveal_lock: true,
        target: Target::Memory,
        damage: None,
    };
    render(job)?.and_then(|rendered| rendered.image).context("nothing was rendered")
}

/// Splits a monotonic time into the `tv_sec_hi`, `tv_sec_lo`, and `tv_nsec` of capture protocols.
fn timestamp(time: Duration) -> (u32, u32, u32) {
    let secs = time.as_secs();
    ((secs >> 32) as u32, secs as u32, time.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::allocator::Modifier;

    #[test]
    fn timestamps_split_seconds() {
        let time = Duration::new((7 << 32) + 5, 123);
        assert_eq!(timestamp(time), (7, 5, 123));
    }

    #[test]
    fn dmabuf_constraints_keep_capture_formats_in_order() {
        let formats = [
            Format { code: Fourcc::Nv12, modifier: Modifier::Linear },
            Format { code: Fourcc::Argb8888, modifier: Modifier::Linear },
            Format { code: Fourcc::Xrgb8888, modifier: Modifier::Linear },
            Format { code: Fourcc::Xrgb8888, modifier: Modifier::Invalid },
        ];
        let constraints = DmabufConstraints::new(1, formats).unwrap();
        assert_eq!(
            constraints.formats,
            vec![
                (Fourcc::Xrgb8888, vec![0, u64::from(Modifier::Invalid)]),
                (Fourcc::Argb8888, vec![0]),
            ]
        );
        assert!(DmabufConstraints::new(1, [formats[0]]).is_none());
    }
}
