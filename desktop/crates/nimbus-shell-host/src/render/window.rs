// SPDX-License-Identifier: MIT

//! The window adapter that both renderers share.

use crate::text_input::TextField;
use i_slint_core::InternalToken;
use i_slint_core::window::WindowAdapterInternal;
use slint::PhysicalSize;
use slint::platform::{WindowAdapter, WindowEvent};
use std::cell::Cell;
use std::rc::{Rc, Weak};

/// A shell window: its size, whether it needs drawing, and its focused text field, around Slint's renderer `R`.
pub struct ShellWindow<R> {
    window: slint::Window,
    pub renderer: R,
    size: Cell<PhysicalSize>,
    needs_redraw: Cell<bool>,
    text_field: TextField,
}

impl<R: slint::platform::Renderer + 'static> ShellWindow<R> {
    pub fn new(renderer: R) -> Rc<Self> {
        Rc::new_cyclic(|adapter: &Weak<Self>| Self {
            window: slint::Window::new(adapter.clone()),
            renderer,
            size: Cell::default(),
            needs_redraw: Cell::new(true),
            text_field: TextField::default(),
        })
    }

    /// Whether Slint asked for a redraw since the last call.
    pub fn take_redraw(&self) -> bool {
        self.needs_redraw.replace(false)
    }
}

impl<R: slint::platform::Renderer + 'static> WindowAdapter for ShellWindow<R> {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn set_size(&self, size: slint::WindowSize) {
        let scale_factor = self.window.scale_factor();
        self.size.set(size.to_physical(scale_factor));
        let size = size.to_logical(scale_factor);
        if let Err(err) = self.window.dispatch_event_with_result(WindowEvent::Resized { size }) {
            tracing::warn!("cannot resize a shell window: {err}");
        }
    }

    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }

    fn internal(&self, _: InternalToken) -> Option<&dyn WindowAdapterInternal> {
        Some(&self.text_field)
    }
}
