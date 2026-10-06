// SPDX-License-Identifier: MIT

//! The lock state in the shared model, and password checks for every lock screen.

use std::rc::Rc;

use slint::SharedString;

use super::Model;

const UNLOCK_FAILED: &str = "Incorrect password, please try again";
const UNLOCK_UNAVAILABLE: &str = "Unlocking is unavailable";

impl Model {
    pub fn set_locked(&self, locked: bool) {
        if locked {
            for view in self.views() {
                view.close_everything();
            }
        }
        {
            let mut state = self.state.borrow_mut();
            state.desktop.locked = locked;
            state.desktop.lock_busy = false;
            state.desktop.lock_error = SharedString::new();
        }
        self.refresh_clock();
        self.reset_passwords(locked);
    }

    pub fn is_locked(&self) -> bool {
        self.state.borrow().desktop.locked
    }

    pub fn set_unlock_handler(&self, handler: Rc<dyn Fn(String)>) {
        *self.on_unlock.borrow_mut() = Some(handler);
    }

    pub fn unlock_requested(&self, password: String) {
        let handler = self.on_unlock.borrow().clone();
        {
            let mut state = self.state.borrow_mut();
            let desktop = &mut state.desktop;
            if !desktop.locked || desktop.lock_busy {
                return;
            }
            if handler.is_some() {
                desktop.lock_error = SharedString::new();
                desktop.lock_busy = true;
            } else {
                tracing::warn!("No unlock handler is registered; the session stays locked");
                desktop.lock_error = UNLOCK_UNAVAILABLE.into();
            }
        }
        self.publish();
        if let Some(handler) = handler {
            handler(password);
        }
    }

    pub fn unlock_failed(&self) {
        let locked = {
            let mut state = self.state.borrow_mut();
            let desktop = &mut state.desktop;
            desktop.lock_busy = false;
            if desktop.locked {
                desktop.lock_error = UNLOCK_FAILED.into();
            }
            desktop.locked
        };
        self.publish();
        self.reset_passwords(locked);
    }

    /// Clears the password field of every lock screen, and focuses it while locked.
    fn reset_passwords(&self, locked: bool) {
        for lock in self.lock_windows() {
            lock.set_password(SharedString::new());
            if locked {
                lock.invoke_focus_password();
            }
        }
    }
}
