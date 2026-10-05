// SPDX-License-Identifier: MIT

//! The Nimbus shell: a full-screen, transparent Slint overlay drawn above all windows on one output.
//!
//! The shell doesn't know how it's displayed.
//! The compositor renders it with Slint's software renderer and forwards input inside [`Shell::input_region`];
//! `nimbus-shell-preview` runs it in an ordinary window.
//! Data flows in through the `set_*` methods; user intents flow out as [`ShellAction`]s.

use std::rc::Rc;

slint::include_modules!();

/// Something the user asked the shell to do, handled by the host.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellAction {
    Compositor(nimbus_ipc::Request),
    Service(nimbus_services::ServiceCommand),
    /// Launch the desktop entry with this id.
    Launch(String),
    /// Open the Settings app, optionally on a page such as `appearance` or `network`.
    OpenSettings(Option<String>),
}

/// An on-screen display shown briefly after a hardware key changes a level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Osd {
    Volume { level: f32, muted: bool },
    Brightness { level: f32 },
}

/// A rectangle in logical pixels of the shell's output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Space the shell permanently occupies at the output edges; the compositor keeps maximized and tiled windows out of it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Exclusive {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
}

/// One shell instance, bound to the output it's displayed on.
pub struct Shell {
    window: ShellWindow,
}

impl Shell {
    /// Creates the shell UI; the current Slint platform supplies its window.
    pub fn new(
        config: &nimbus_config::Config,
        on_action: impl Fn(ShellAction) + 'static,
    ) -> Result<Self, slint::PlatformError> {
        let _ = (config, Rc::new(on_action));
        todo!()
    }

    pub fn window(&self) -> &slint::Window {
        slint::ComponentHandle::window(&self.window)
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        slint::ComponentHandle::show(&self.window)
    }

    pub fn set_config(&self, config: &nimbus_config::Config) {
        let _ = config;
        todo!()
    }

    pub fn set_output_name(&self, name: &str) {
        let _ = name;
        todo!()
    }

    pub fn set_compositor_state(&self, state: &nimbus_ipc::CompositorState) {
        let _ = state;
        todo!()
    }

    /// Applies one compositor event incrementally.
    pub fn handle_compositor_event(&self, event: &nimbus_ipc::Event) {
        let _ = event;
        todo!()
    }

    /// Provides the launcher's application list and the icons for window and dock entries.
    pub fn set_apps(&self, apps: &nimbus_xdg::AppIndex, icons: &nimbus_xdg::IconResolver) {
        let _ = (apps, icons);
        todo!()
    }

    pub fn handle_service_event(&self, event: &nimbus_services::ServiceEvent) {
        let _ = event;
        todo!()
    }

    pub fn show_osd(&self, osd: Osd) {
        let _ = osd;
        todo!()
    }

    pub fn toggle_launcher(&self) {
        todo!()
    }

    pub fn toggle_overview(&self) {
        todo!()
    }

    pub fn set_locked(&self, locked: bool) {
        let _ = locked;
        todo!()
    }

    pub fn is_locked(&self) -> bool {
        todo!()
    }

    /// Returns where the shell takes pointer input; everywhere else belongs to the windows below.
    /// While a popup, the launcher, the overview, or the lock screen is open, this covers the whole output.
    pub fn input_region(&self) -> Vec<Rect> {
        todo!()
    }

    /// Returns whether the shell currently wants keyboard focus.
    pub fn wants_keyboard(&self) -> bool {
        todo!()
    }

    pub fn exclusive_zone(&self) -> Exclusive {
        todo!()
    }
}
