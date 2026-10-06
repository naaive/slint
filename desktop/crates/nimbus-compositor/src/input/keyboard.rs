// SPDX-License-Identifier: MIT

//! Keyboard shortcuts, VT switching, and the emergency exit.

use crate::keybindings::Mods;
use crate::state::State;
use smithay::backend::input::{Event, InputBackend, KeyState, KeyboardKeyEvent};
use smithay::input::keyboard::{
    FilterResult, Keycode, Keysym, KeysymHandle, ModifiersState, keysyms,
};
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;

/// What a key press does instead of reaching the focused client.
enum KeyAction {
    None,
    Action(nimbus_config::Action),
    CancelSwitcher,
    SwitchVt(i32),
    EmergencyQuit,
}

impl State {
    pub(super) fn on_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        self.keyboard_key(event.key_code(), event.state(), Event::time_msec(&event));
    }

    pub(super) fn keyboard_key(&mut self, keycode: Keycode, key_state: KeyState, time: u32) {
        let Some(keyboard) = self.nimbus.keyboard.clone() else {
            return;
        };
        // A grab taken since the last dispatch mustn't see a key on the lock screen.
        self.refresh_keyboard_grab(&keyboard);
        let serial = SERIAL_COUNTER.next_serial();
        let action = keyboard.input::<KeyAction, _>(
            self,
            keycode,
            key_state,
            serial,
            time,
            |state, modifiers, handle| state.filter_key(keycode, key_state, modifiers, &handle),
        );
        if key_state == KeyState::Released {
            self.switcher_key_released(Mods::from(&keyboard.modifier_state()));
        }
        match action {
            Some(KeyAction::Action(action)) => self.run_action(action),
            Some(KeyAction::CancelSwitcher) => self.cancel_switcher(),
            Some(KeyAction::SwitchVt(vt)) => self.backend.change_vt(vt),
            Some(KeyAction::EmergencyQuit) => {
                tracing::warn!("emergency exit requested with Ctrl+Alt+Backspace");
                self.nimbus.stop();
            }
            Some(KeyAction::None) | None => {}
        }
    }

    fn filter_key(
        &mut self,
        code: Keycode,
        key_state: KeyState,
        modifiers: &ModifiersState,
        handle: &KeysymHandle<'_>,
    ) -> FilterResult<KeyAction> {
        if key_state == KeyState::Released {
            if self.nimbus.suppressed_keys.remove(&code) {
                return FilterResult::Intercept(KeyAction::None);
            }
            return FilterResult::Forward;
        }

        let raw_syms = handle.raw_syms();
        let modified = handle.modified_sym();
        if let Some(vt) = vt_for_keysym(modified) {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::SwitchVt(vt));
        }
        if self.nimbus.switcher.is_some() && raw_syms.iter().any(|s| s.raw() == keysyms::KEY_Escape)
        {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::CancelSwitcher);
        }
        let locked = self.nimbus.is_locked();
        if is_emergency_quit(modified, &raw_syms, modifiers, locked) {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::EmergencyQuit);
        }
        if !self.nimbus.seat.keyboard_shortcuts_inhibited()
            && let Some(action) =
                self.nimbus.bindings.lookup(Mods::from(modifiers), &raw_syms).cloned()
            && (!locked || allowed_while_locked(&action))
        {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::Action(action));
        }
        FilterResult::Forward
    }
}

/// Ctrl+Alt+Backspace or `XF86Terminate_Server`, as in Xorg, except on a lock screen.
fn is_emergency_quit(
    modified: Keysym,
    raw_syms: &[Keysym],
    modifiers: &ModifiersState,
    locked: bool,
) -> bool {
    let is_backspace = raw_syms.iter().any(|s| s.raw() == keysyms::KEY_BackSpace);
    !locked
        && (modified.raw() == keysyms::KEY_Terminate_Server
            || (modifiers.ctrl && modifiers.alt && is_backspace))
}

fn vt_for_keysym(sym: Keysym) -> Option<i32> {
    let raw = sym.raw();
    (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12)
        .contains(&raw)
        .then(|| i32::try_from(raw - keysyms::KEY_XF86Switch_VT_1 + 1).unwrap_or(1))
}

/// Hardware keys keep working on the lock screen.
fn allowed_while_locked(action: &nimbus_config::Action) -> bool {
    use nimbus_config::Action::*;
    matches!(action, VolumeUp | VolumeDown | ToggleMute | BrightnessUp | BrightnessDown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt_keysyms_map_to_numbers() {
        assert_eq!(vt_for_keysym(Keysym::new(keysyms::KEY_XF86Switch_VT_1)), Some(1));
        assert_eq!(vt_for_keysym(Keysym::new(keysyms::KEY_XF86Switch_VT_12)), Some(12));
        assert_eq!(vt_for_keysym(Keysym::new(keysyms::KEY_F1)), None);
    }

    #[test]
    fn emergency_quit_is_ignored_on_the_lock_screen() {
        let ctrl_alt = ModifiersState { ctrl: true, alt: true, ..ModifiersState::default() };
        let backspace = [Keysym::new(keysyms::KEY_BackSpace)];
        let terminate = Keysym::new(keysyms::KEY_Terminate_Server);
        assert!(is_emergency_quit(backspace[0], &backspace, &ctrl_alt, false));
        assert!(is_emergency_quit(terminate, &[terminate], &ModifiersState::default(), false));
        assert!(!is_emergency_quit(backspace[0], &backspace, &ctrl_alt, true));
        assert!(!is_emergency_quit(terminate, &[terminate], &ModifiersState::default(), true));
    }

    #[test]
    fn only_hardware_keys_work_while_locked() {
        assert!(allowed_while_locked(&nimbus_config::Action::VolumeUp));
        assert!(!allowed_while_locked(&nimbus_config::Action::ToggleLauncher));
        assert!(!allowed_while_locked(&nimbus_config::Action::Spawn("sh".into())));
    }
}
