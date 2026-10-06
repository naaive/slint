// SPDX-License-Identifier: MIT

//! The Nimbus shell: panel, dock, launcher, overview, popups, toasts, OSD, and lock screen.
//!
//! The shell doesn't know how it's displayed.
//! One [`ShellModel`] holds the state; data flows in through its methods and user intents flow out as [`ShellAction`]s.
//! Each output shows the model through a [`ShellView`], a transparent overlay whose input is limited to [`ShellView::input_region`],
//! and while the session is locked, through a [`LockView`], a window of its own.
//!
//! The lock screen asks the host to check passwords: register a handler with [`ShellModel::on_unlock_attempt`],
//! then call [`ShellModel::set_locked`] with `false` on success or [`ShellModel::unlock_failed`] on failure.

use std::path::PathBuf;
use std::rc::Rc;

mod backdrop;
mod client_images;
mod clock;
mod icons;
mod lock_view;
mod model;
mod notifications;
mod persist;
mod status;
mod system_scheme;
mod view;
mod windows;

pub use lock_view::LockView;
pub use view::ShellView;

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

/// The shell's state, shared by all of its views.
///
/// Create it after setting the Slint platform, then create a [`ShellView`] per output.
#[derive(Clone)]
pub struct ShellModel(Rc<model::Model>);

impl ShellModel {
    /// Creates the model; `on_action` receives what the user asks for in any view.
    pub fn new(config: &nimbus_config::Config, on_action: impl Fn(ShellAction) + 'static) -> Self {
        Self(model::Model::new(config, Box::new(on_action)))
    }

    pub fn set_config(&self, config: &nimbus_config::Config) {
        self.0.set_config(config);
    }

    /// Sets the configuration file that shell-initiated changes are saved to,
    /// such as the dark style toggle and dock pins.
    /// Defaults to `nimbus_config::default_path()`; set it when the compositor runs with `--config`.
    pub fn set_config_path(&self, path: impl Into<PathBuf>) {
        self.0.set_config_path(Some(path.into()));
    }

    pub fn set_compositor_state(&self, state: &nimbus_ipc::CompositorState) {
        self.0.set_compositor_state(state);
    }

    /// Applies one compositor event incrementally.
    pub fn handle_compositor_event(&self, event: &nimbus_ipc::Event) {
        self.0.handle_compositor_event(event);
    }

    /// Provides the launcher's application list and the icons for window and dock entries.
    pub fn set_apps(&self, apps: &nimbus_xdg::AppIndex, icons: &nimbus_xdg::IconResolver) {
        self.0.set_apps(apps, icons);
    }

    pub fn handle_service_event(&self, event: &nimbus_services::ServiceEvent) {
        self.0.handle_service_event(event);
    }

    /// Shows the on-screen display on every view.
    pub fn show_osd(&self, osd: Osd) {
        self.0.show_osd(osd);
    }

    /// Enters or leaves the locked state.
    /// Locking closes everything open in the views; the host shows a [`LockView`] per output while locked.
    pub fn set_locked(&self, locked: bool) {
        self.0.set_locked(locked);
    }

    pub fn is_locked(&self) -> bool {
        self.0.is_locked()
    }

    /// Registers the handler that checks a password typed on a lock screen, typically through PAM.
    ///
    /// Every lock screen shows a spinner until the host answers:
    /// call [`ShellModel::set_locked`] with `false` when the password is right, or [`ShellModel::unlock_failed`] when it's wrong.
    /// The handler may answer immediately, from inside the call, or later.
    /// Without a handler, the lock screen reports that unlocking is unavailable.
    pub fn on_unlock_attempt(&self, handler: impl Fn(String) + 'static) {
        self.0.set_unlock_handler(Rc::new(handler));
    }

    /// Tells the lock screens that the last password was wrong; they clear the field and show an error.
    pub fn unlock_failed(&self) {
        self.0.unlock_failed();
    }
}
