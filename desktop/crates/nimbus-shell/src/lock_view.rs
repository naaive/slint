// SPDX-License-Identifier: MIT

//! [`LockView`]: the lock screen for one output, a window of its own.

use std::rc::Rc;

use slint::{ComponentHandle, SharedString};

use crate::model::Model;
use crate::{LockWindow, ShellModel};

/// The lock screen for one output: clock, status, and password field.
///
/// It's a window apart from the [`crate::ShellView`] of the same output,
/// so a host can put it on a surface of its own, such as an `ext-session-lock` surface.
/// Passwords go to the handler registered with [`ShellModel::on_unlock_attempt`].
pub struct LockView {
    ui: LockWindow,
    _model: Rc<Model>,
}

impl LockView {
    /// Creates a lock screen; the current Slint platform supplies its window.
    pub fn new(model: &ShellModel) -> Result<Self, slint::PlatformError> {
        let ui = LockWindow::new()?;
        let weak_model = Rc::downgrade(&model.0);
        let weak_ui = ui.as_weak();
        ui.on_unlock_requested(move |password| {
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_password(SharedString::new());
            }
            if let Some(model) = weak_model.upgrade() {
                model.unlock_requested(password.into());
            }
        });
        model.0.add_lock(&ui);
        ui.invoke_focus_password();
        Ok(Self { ui, _model: model.0.clone() })
    }

    pub fn window(&self) -> &slint::Window {
        self.ui.window()
    }

    /// The Slint component, for tests and for hosts that need direct access to its properties.
    pub fn component(&self) -> &LockWindow {
        &self.ui
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.ui.show()
    }
}
