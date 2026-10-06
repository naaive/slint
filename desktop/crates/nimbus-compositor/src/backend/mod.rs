// SPDX-License-Identifier: MIT

//! Output and input backends: a nested window, real hardware, or memory only.

pub mod headless;
pub mod udev;
pub mod winit;

use crate::capture;
use crate::outputs::{OutputBackend, OutputError, OutputState};
use crate::state::Nimbus;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use std::time::Instant;

/// The nominal frame interval for outputs without a known refresh rate.
pub const DEFAULT_REFRESH_MHZ: i32 = 60_000;

pub enum Backend {
    Winit(Box<winit::WinitBackend>),
    Udev(Box<udev::UdevBackend>),
    Headless(headless::HeadlessBackend),
}

impl Backend {
    /// Renders the outputs in [`Nimbus::pending_redraws`] that are ready for a new frame.
    pub fn render(&mut self, nimbus: &mut Nimbus) {
        match self {
            Self::Winit(b) => b.render(nimbus),
            Self::Udev(b) => b.render(nimbus),
            Self::Headless(b) => b.render(nimbus),
        }
    }

    /// When the event loop must wake up next for this backend's frame clock.
    pub fn next_deadline(&self, nimbus: &Nimbus) -> Option<Instant> {
        match self {
            Self::Winit(b) => b.next_deadline(nimbus),
            Self::Udev(_) => None,
            Self::Headless(b) => b.next_deadline(nimbus),
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self {
            Self::Winit(b) => b.import_dmabuf(dmabuf),
            Self::Udev(b) => b.import_dmabuf(dmabuf),
            Self::Headless(_) => false,
        }
    }

    /// Uploads a committed buffer before rendering, where that's worth it.
    pub fn early_import(&mut self, surface: &WlSurface) {
        if let Self::Udev(b) = self {
            b.early_import(surface);
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        match self {
            Self::Udev(b) => b.change_vt(vt),
            _ => tracing::debug!("VT switching needs the udev backend"),
        }
    }

    /// Renders a capture; see [`capture::render`].
    pub fn capture(
        &mut self,
        nimbus: &Nimbus,
        job: capture::Job<'_>,
    ) -> anyhow::Result<Option<capture::Rendered>> {
        match self {
            Self::Winit(b) => b.capture(nimbus, job),
            Self::Udev(b) => b.capture(nimbus, job),
            Self::Headless(b) => b.capture(nimbus, job),
        }
    }

    /// The dmabufs captures can render into, if any.
    pub fn capture_dmabuf(&mut self) -> Option<capture::DmabufConstraints> {
        match self {
            Self::Winit(b) => b.capture_dmabuf(),
            Self::Udev(b) => b.capture_dmabuf(),
            Self::Headless(_) => None,
        }
    }

    pub fn apply_input_config(&mut self, input: &nimbus_config::Input) {
        if let Self::Udev(b) = self {
            b.apply_input_config(input);
        }
    }
}

impl OutputBackend for Backend {
    fn apply_outputs(
        &mut self,
        layout: &[(Output, OutputState)],
        test: bool,
    ) -> Result<(), OutputError> {
        match self {
            Self::Winit(b) => b.apply_outputs(layout, test),
            Self::Udev(b) => b.apply_outputs(layout, test),
            Self::Headless(b) => b.apply_outputs(layout, test),
        }
    }
}
