// SPDX-License-Identifier: MIT

//! How a Slint window becomes the contents of a Wayland surface.

mod software;

use slint::platform::WindowAdapter;
use std::rc::Rc;
use wayland_client::protocol::wl_surface::WlSurface;

pub use software::SoftwareRenderer;

/// Draws one Slint window onto one Wayland surface.
///
/// The platform creates a renderer for every Slint window; see [`crate::platform::Windows`].
pub trait Renderer {
    /// The window adapter Slint draws through.
    fn window_adapter(&self) -> Rc<dyn WindowAdapter>;

    /// Draws what changed since the last frame, then attaches and damages the new buffer on `surface`.
    /// Returns whether there is a frame to commit; without changes, or while no buffer is free, there's none.
    fn render(&mut self, surface: &WlSurface) -> bool;
}
