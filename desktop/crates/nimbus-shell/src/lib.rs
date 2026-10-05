// SPDX-License-Identifier: MIT

//! The Nimbus shell: a full-screen, transparent Slint overlay drawn above all windows on one output.
//!
//! The shell doesn't know how it's displayed.
//! The compositor renders it with Slint's software renderer and forwards input inside [`Shell::input_region`];
//! `nimbus-shell-preview` runs it in an ordinary window.
//! Data flows in through the `set_*` methods; user intents flow out as [`ShellAction`]s.
//!
//! The lock screen asks the host to check passwords: register a handler with [`Shell::on_unlock_attempt`],
//! then call [`Shell::set_locked`] with `false` on success or [`Shell::unlock_failed`] on failure.

use std::path::PathBuf;
use std::rc::Rc;

mod backdrop;
mod client_images;
mod clock;
mod controller;
mod icons;
mod notifications;
mod persist;
mod status;
mod system_scheme;
mod windows;

use controller::Controller;

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

    /// Whether the rectangle covers no area.
    pub fn is_empty(&self) -> bool {
        !(self.width > 0.0 && self.height > 0.0)
    }
}

impl From<RectData> for Rect {
    fn from(rect: RectData) -> Self {
        Self { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
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
    controller: Rc<Controller>,
}

impl Shell {
    /// Creates the shell UI; the current Slint platform supplies its window.
    pub fn new(
        config: &nimbus_config::Config,
        on_action: impl Fn(ShellAction) + 'static,
    ) -> Result<Self, slint::PlatformError> {
        let window = ShellWindow::new()?;
        let controller = Controller::new(&window, config, Box::new(on_action));
        Ok(Self { window, controller })
    }

    pub fn window(&self) -> &slint::Window {
        slint::ComponentHandle::window(&self.window)
    }

    /// The Slint component, for tests and for hosts that need direct access to its properties.
    pub fn component(&self) -> &ShellWindow {
        &self.window
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        slint::ComponentHandle::show(&self.window)
    }

    pub fn set_config(&self, config: &nimbus_config::Config) {
        self.controller.set_config(&self.window, config);
    }

    /// Sets the configuration file that shell-initiated changes are saved to,
    /// such as the dark style toggle and dock pins.
    /// Defaults to `nimbus_config::default_path()`; set it when the compositor runs with `--config`.
    pub fn set_config_path(&self, path: impl Into<PathBuf>) {
        self.controller.set_config_path(Some(path.into()));
    }

    pub fn set_output_name(&self, name: &str) {
        self.controller.set_output_name(&self.window, name);
    }

    pub fn set_compositor_state(&self, state: &nimbus_ipc::CompositorState) {
        self.controller.set_compositor_state(&self.window, state);
    }

    /// Applies one compositor event incrementally.
    pub fn handle_compositor_event(&self, event: &nimbus_ipc::Event) {
        self.controller.handle_compositor_event(&self.window, event);
    }

    /// Provides the launcher's application list and the icons for window and dock entries.
    pub fn set_apps(&self, apps: &nimbus_xdg::AppIndex, icons: &nimbus_xdg::IconResolver) {
        self.controller.set_apps(&self.window, apps, icons);
    }

    pub fn handle_service_event(&self, event: &nimbus_services::ServiceEvent) {
        self.controller.handle_service_event(&self.window, event);
    }

    pub fn show_osd(&self, osd: Osd) {
        Controller::show_osd(&self.controller, &self.window, osd);
    }

    pub fn toggle_launcher(&self) {
        if !self.is_locked() {
            let open = !self.window.get_launcher_open();
            self.controller.set_launcher(&self.window, open, "");
        }
    }

    pub fn toggle_overview(&self) {
        if !self.is_locked() {
            let open = !self.window.get_overview_open();
            self.controller.set_overview(&self.window, open);
        }
    }

    /// Shows or hides the lock screen.
    /// Locking takes effect in the next frame, without a transition, so nothing behind the lock screen shows.
    pub fn set_locked(&self, locked: bool) {
        self.controller.set_locked(&self.window, locked);
    }

    pub fn is_locked(&self) -> bool {
        self.window.get_locked()
    }

    /// Registers the handler that checks a password typed on the lock screen, typically through PAM.
    ///
    /// The lock screen shows a spinner until the host answers:
    /// call [`Shell::set_locked`] with `false` when the password is right, or [`Shell::unlock_failed`] when it's wrong.
    /// The handler may answer immediately, from inside the call, or later.
    /// Without a handler, the lock screen reports that unlocking is unavailable.
    pub fn on_unlock_attempt(&self, handler: impl Fn(String) + 'static) {
        self.controller.set_unlock_handler(Rc::new(handler));
    }

    /// Registers the handler for a toast the user clicked away, so the host can hide it on every output.
    ///
    /// The host calls [`Shell::close_toast`] on each shell; the notification stays in the history.
    /// Without a handler, the toast closes only on this shell.
    pub fn on_toast_closed(&self, handler: impl Fn(u32) + 'static) {
        self.controller.set_toast_closed_handler(Rc::new(handler));
    }

    /// Fades out the toast for the notification `id`, keeping the notification in the history.
    pub fn close_toast(&self, id: u32) {
        self.controller.close_toast(id);
    }

    /// Tells the lock screen that the last password was wrong; it clears the field and shows an error.
    pub fn unlock_failed(&self) {
        self.controller.unlock_failed(&self.window);
    }

    /// Returns where the shell takes pointer input; everywhere else belongs to the windows below.
    /// While a popup, the launcher, the overview, or the lock screen is open, this covers the whole output.
    ///
    /// Toasts count from the frame that first draws them, so query this after rendering.
    pub fn input_region(&self) -> Vec<Rect> {
        let ui = &self.window;
        if self.covers_output() {
            let window = self.window();
            let size = window.size().to_logical(window.scale_factor());
            return vec![Rect { x: 0.0, y: 0.0, width: size.width, height: size.height }];
        }
        [ui.get_panel_rect(), ui.get_dock_rect(), ui.get_toast_rect()]
            .into_iter()
            .map(Rect::from)
            .filter(|rect| !rect.is_empty())
            .collect()
    }

    /// Returns whether the shell currently wants keyboard focus.
    pub fn wants_keyboard(&self) -> bool {
        self.covers_output()
    }

    pub fn exclusive_zone(&self) -> Exclusive {
        let panel = self.window.get_panel();
        let dock = if panel.show_dock && !panel.dock_autohide {
            self.window.get_dock_exclusive()
        } else {
            0.0
        };
        if panel.top {
            Exclusive { top: panel.height, bottom: dock, ..Exclusive::default() }
        } else {
            Exclusive { bottom: panel.height + dock, ..Exclusive::default() }
        }
    }

    fn covers_output(&self) -> bool {
        let ui = &self.window;
        ui.get_locked()
            || ui.get_launcher_open()
            || ui.get_overview_open()
            || ui.get_popup() != Popup::None
            || ui.get_power_action() != PowerAction::None
    }
}
