// SPDX-License-Identifier: MIT

//! Pointer and keyboard input for the shell's surfaces,
//! with keys translated through the compositor's keymap and the locale's compose table.

use crate::state::State;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{LogicalPosition, SharedString};
use smithay_client_toolkit::reexports::calloop::RegistrationToken;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use std::ffi::OsString;
use std::time::Duration;
use wayland_client::protocol::wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use xkbcommon::xkb::{self, Keysym, compose, keysyms};

/// Linux input event codes of the pointer buttons.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// The difference between Linux input event codes and XKB keycodes.
const XKB_KEYCODE_OFFSET: u32 = 8;

#[derive(Default)]
pub struct Input {
    /// The seat whose input the shell follows; idle notifications watch it too.
    pub seat: Option<WlSeat>,
    pub pointer: Option<WlPointer>,
    pub cursor_shape: Option<WpCursorShapeDeviceV1>,
    pub keyboard: Option<WlKeyboard>,
    keyboard_focus: Option<WlSurface>,
    xkb: Option<xkb::State>,
    compose: Option<compose::State>,
    repeat: Option<RepeatInfo>,
    repeating: Option<(u32, RegistrationToken)>,
    /// The serial of the last button or key press, which proves user intent when asking for activation tokens.
    pub last_serial: Option<u32>,
}

#[derive(Clone, Copy)]
struct RepeatInfo {
    delay: Duration,
    interval: Duration,
}

impl State {
    fn stop_repeat(&mut self) {
        if let Some((_, token)) = self.input.repeating.take() {
            self.loop_handle.remove(token);
        }
    }

    fn start_repeat(&mut self, keycode: u32, text: SharedString) {
        let Some(RepeatInfo { delay, interval }) = self.input.repeat else {
            return;
        };
        let timer =
            self.loop_handle.insert_source(Timer::from_duration(delay), move |_, _, state| {
                if let Some(surface) =
                    state.input.keyboard_focus.clone().and_then(|s| state.surface(&s))
                {
                    surface.dispatch(WindowEvent::KeyPressRepeated { text: text.clone() });
                }
                TimeoutAction::ToDuration(interval)
            });
        match timer {
            Ok(token) => self.input.repeating = Some((keycode, token)),
            Err(err) => tracing::debug!("cannot start key repeat: {err}"),
        }
    }

    fn key(&mut self, key: u32, pressed: bool) {
        let keycode = key + XKB_KEYCODE_OFFSET;
        let Some(xkb) = &self.input.xkb else {
            return;
        };
        let sym = xkb.key_get_one_sym(keycode.into());
        let composition = match &mut self.input.compose {
            Some(compose) if pressed => compose_key(compose, sym),
            _ => Composition::None,
        };
        let repeats =
            composition == Composition::None && xkb.get_keymap().key_repeats(keycode.into());
        let text = match composition {
            Composition::None => slint_key_text(sym).unwrap_or_default(),
            Composition::Pending => SharedString::new(),
            Composition::Composed(text) => text,
        };
        if self.input.repeating.as_ref().is_some_and(|(code, _)| *code == keycode) || pressed {
            self.stop_repeat();
        }
        if text.is_empty() {
            return;
        }
        let Some(surface) = self.input.keyboard_focus.clone().and_then(|s| self.surface(&s)) else {
            return;
        };
        if pressed {
            surface.dispatch(WindowEvent::KeyPressed { text: text.clone() });
            if repeats {
                self.start_repeat(keycode, text);
            }
        } else {
            surface.dispatch(WindowEvent::KeyReleased { text });
        }
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let position = LogicalPosition::new(event.position.0 as f32, event.position.1 as f32);
            let window_event = match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = &self.input.cursor_shape {
                        device.set_shape(serial, Shape::Default);
                    }
                    WindowEvent::PointerMoved { position }
                }
                PointerEventKind::Leave { .. } => WindowEvent::PointerExited,
                PointerEventKind::Motion { .. } => WindowEvent::PointerMoved { position },
                PointerEventKind::Press { button, serial, .. } => {
                    self.input.last_serial = Some(serial);
                    WindowEvent::PointerPressed { position, button: slint_button(button) }
                }
                PointerEventKind::Release { button, .. } => {
                    WindowEvent::PointerReleased { position, button: slint_button(button) }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    WindowEvent::PointerScrolled {
                        position,
                        delta_x: -horizontal.absolute as f32,
                        delta_y: -vertical.absolute as f32,
                    }
                }
            };
            if let Some(surface) = self.surface(&event.surface) {
                surface.dispatch(window_event);
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(KeymapFormat::XkbV1), fd, size } => {
                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                // SAFETY: the compositor shares a keymap of `size` bytes through `fd`, as wl_keyboard specifies.
                let keymap = unsafe {
                    xkb::Keymap::new_from_fd(
                        &context,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                };
                match keymap {
                    Ok(Some(keymap)) => {
                        state.input.xkb = Some(xkb::State::new(&keymap));
                        state.input.compose = compose_state(&context);
                    }
                    Ok(None) => tracing::warn!("the compositor's keymap doesn't compile"),
                    Err(err) => tracing::warn!("cannot read the compositor's keymap: {err}"),
                }
            }
            wl_keyboard::Event::Keymap { .. } => tracing::warn!("unsupported keymap format"),
            wl_keyboard::Event::Enter { surface, .. } => {
                if let Some(target) = state.surface(&surface) {
                    target.dispatch(WindowEvent::WindowActiveChanged(true));
                }
                state.input.keyboard_focus = Some(surface);
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                state.stop_repeat();
                if let Some(compose) = &mut state.input.compose {
                    compose.reset();
                }
                if let Some(target) = state.surface(&surface) {
                    target.dispatch(WindowEvent::WindowActiveChanged(false));
                }
                state.input.keyboard_focus = None;
            }
            wl_keyboard::Event::Key { serial, key, state: WEnum::Value(key_state), .. } => {
                let pressed = key_state != KeyState::Released;
                if pressed {
                    state.input.last_serial = Some(serial);
                }
                state.key(key, pressed);
            }
            wl_keyboard::Event::Key { .. } => {}
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb) = &mut state.input.xkb {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.input.repeat =
                    u32::try_from(rate).ok().filter(|&r| r > 0).map(|rate| RepeatInfo {
                        delay: Duration::from_millis(u64::try_from(delay).unwrap_or(0)),
                        interval: Duration::from_secs(1) / rate,
                    });
            }
            _ => {}
        }
    }
}

/// The compose state for the locale from `LC_ALL`, `LC_CTYPE`, or `LANG`.
fn compose_state(context: &xkb::Context) -> Option<compose::State> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|locale| !locale.is_empty())
        .unwrap_or_else(|| OsString::from("C"));
    match compose::Table::new_from_locale(context, &locale, compose::COMPILE_NO_FLAGS) {
        Ok(table) => Some(compose::State::new(&table, compose::STATE_NO_FLAGS)),
        Err(()) => {
            tracing::info!("no compose table for locale {locale:?}");
            None
        }
    }
}

#[derive(Debug, PartialEq)]
enum Composition {
    /// The key isn't part of a compose sequence.
    None,
    /// The key starts, continues, or cancels a sequence.
    Pending,
    Composed(SharedString),
}

fn compose_key(state: &mut compose::State, sym: Keysym) -> Composition {
    if state.feed(sym) == compose::FeedResult::Ignored {
        return Composition::None;
    }
    match state.status() {
        compose::Status::Nothing => Composition::None,
        compose::Status::Composing => Composition::Pending,
        compose::Status::Composed => {
            let text = state
                .utf8()
                .filter(|text| !text.is_empty())
                .map(|text| SharedString::from(text.as_str()))
                .or_else(|| state.keysym().and_then(slint_key_text));
            state.reset();
            text.map_or(Composition::Pending, Composition::Composed)
        }
        compose::Status::Cancelled => {
            state.reset();
            Composition::Pending
        }
    }
}

fn slint_button(button: u32) -> PointerEventButton {
    match button {
        BTN_LEFT => PointerEventButton::Left,
        BTN_RIGHT => PointerEventButton::Right,
        BTN_MIDDLE => PointerEventButton::Middle,
        BTN_SIDE => PointerEventButton::Back,
        BTN_EXTRA => PointerEventButton::Forward,
        _ => PointerEventButton::Other,
    }
}

/// The text Slint expects for a key: a [`Key`] code for special keys, otherwise the produced character.
fn slint_key_text(sym: Keysym) -> Option<SharedString> {
    let key = match sym.raw() {
        keysyms::KEY_BackSpace => Key::Backspace,
        keysyms::KEY_Tab => Key::Tab,
        keysyms::KEY_ISO_Left_Tab => Key::Backtab,
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => Key::Return,
        keysyms::KEY_Escape => Key::Escape,
        keysyms::KEY_Delete | keysyms::KEY_KP_Delete => Key::Delete,
        keysyms::KEY_Shift_L => Key::Shift,
        keysyms::KEY_Shift_R => Key::ShiftR,
        keysyms::KEY_Control_L => Key::Control,
        keysyms::KEY_Control_R => Key::ControlR,
        keysyms::KEY_Alt_L | keysyms::KEY_Alt_R => Key::Alt,
        keysyms::KEY_ISO_Level3_Shift | keysyms::KEY_Mode_switch => Key::AltGr,
        keysyms::KEY_Super_L | keysyms::KEY_Meta_L => Key::Meta,
        keysyms::KEY_Super_R | keysyms::KEY_Meta_R => Key::MetaR,
        keysyms::KEY_Caps_Lock => Key::CapsLock,
        keysyms::KEY_space | keysyms::KEY_KP_Space => Key::Space,
        keysyms::KEY_Up | keysyms::KEY_KP_Up => Key::UpArrow,
        keysyms::KEY_Down | keysyms::KEY_KP_Down => Key::DownArrow,
        keysyms::KEY_Left | keysyms::KEY_KP_Left => Key::LeftArrow,
        keysyms::KEY_Right | keysyms::KEY_KP_Right => Key::RightArrow,
        keysyms::KEY_Home | keysyms::KEY_KP_Home => Key::Home,
        keysyms::KEY_End | keysyms::KEY_KP_End => Key::End,
        keysyms::KEY_Page_Up | keysyms::KEY_KP_Page_Up => Key::PageUp,
        keysyms::KEY_Page_Down | keysyms::KEY_KP_Page_Down => Key::PageDown,
        keysyms::KEY_Insert | keysyms::KEY_KP_Insert => Key::Insert,
        keysyms::KEY_Menu => Key::Menu,
        keysyms::KEY_Pause => Key::Pause,
        keysyms::KEY_Scroll_Lock => Key::ScrollLock,
        raw @ keysyms::KEY_F1..=keysyms::KEY_F24 => {
            FUNCTION_KEYS[usize::try_from(raw - keysyms::KEY_F1).unwrap_or(0)]
        }
        _ => {
            let c = sym.key_char().filter(|c| !c.is_control())?;
            return Some(SharedString::from(c.to_string().as_str()));
        }
    };
    Some(key.into())
}

const FUNCTION_KEYS: [Key; 24] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::F13,
    Key::F14,
    Key::F15,
    Key::F16,
    Key::F17,
    Key::F18,
    Key::F19,
    Key::F20,
    Key::F21,
    Key::F22,
    Key::F23,
    Key::F24,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn text(raw: u32) -> Option<SharedString> {
        slint_key_text(Keysym::new(raw))
    }

    #[test]
    fn special_keys_map_to_slint_codes() {
        assert_eq!(text(keysyms::KEY_BackSpace), Some(Key::Backspace.into()));
        assert_eq!(text(keysyms::KEY_KP_Enter), Some(Key::Return.into()));
        assert_eq!(text(keysyms::KEY_F12), Some(Key::F12.into()));
        assert_eq!(text(keysyms::KEY_Super_L), Some(Key::Meta.into()));
        assert_eq!(text(keysyms::KEY_ISO_Left_Tab), Some(Key::Backtab.into()));
    }

    #[test]
    fn printable_keys_produce_their_character() {
        assert_eq!(text(keysyms::KEY_a).as_deref(), Some("a"));
        assert_eq!(text(keysyms::KEY_A).as_deref(), Some("A"));
        assert_eq!(text(keysyms::KEY_adiaeresis).as_deref(), Some("ä"));
        assert_eq!(text(keysyms::KEY_XF86AudioMute), None);
    }

    #[test]
    fn keys_follow_the_keymap_and_modifiers() {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the US keymap");
        let mut state = xkb::State::new(&keymap);
        // Evdev code 30 is the A key.
        let a = xkb::Keycode::new(30 + XKB_KEYCODE_OFFSET);
        assert_eq!(slint_key_text(state.key_get_one_sym(a)).as_deref(), Some("a"));
        let shift = keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
        state.update_mask(1 << shift, 0, 0, 0, 0, 0);
        assert_eq!(slint_key_text(state.key_get_one_sym(a)).as_deref(), Some("A"));
    }

    fn compose_state() -> compose::State {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let table = compose::Table::new_from_buffer(
            &context,
            "<dead_acute> <e> : \"é\" eacute\n<Multi_key> <a> <e> : \"æ\" ae\n",
            "C",
            compose::FORMAT_TEXT_V1,
            compose::COMPILE_NO_FLAGS,
        )
        .expect("the compose table");
        compose::State::new(&table, compose::STATE_NO_FLAGS)
    }

    fn feed(state: &mut compose::State, raw: u32) -> Composition {
        compose_key(state, Keysym::new(raw))
    }

    #[test]
    fn a_dead_key_composes_with_the_next_key() {
        let mut state = compose_state();
        assert_eq!(feed(&mut state, keysyms::KEY_dead_acute), Composition::Pending);
        assert_eq!(feed(&mut state, keysyms::KEY_Shift_L), Composition::None);
        assert_eq!(feed(&mut state, keysyms::KEY_e), Composition::Composed("é".into()));
        assert_eq!(feed(&mut state, keysyms::KEY_e), Composition::None);
    }

    #[test]
    fn a_cancelled_sequence_swallows_its_keys_and_resets() {
        let mut state = compose_state();
        assert_eq!(feed(&mut state, keysyms::KEY_Multi_key), Composition::Pending);
        assert_eq!(feed(&mut state, keysyms::KEY_a), Composition::Pending);
        assert_eq!(feed(&mut state, keysyms::KEY_x), Composition::Pending);
        assert_eq!(feed(&mut state, keysyms::KEY_x), Composition::None);
        assert_eq!(feed(&mut state, keysyms::KEY_Multi_key), Composition::Pending);
        assert_eq!(feed(&mut state, keysyms::KEY_a), Composition::Pending);
        assert_eq!(feed(&mut state, keysyms::KEY_e), Composition::Composed("æ".into()));
    }

    #[test]
    fn keys_outside_a_sequence_pass_through() {
        let mut state = compose_state();
        assert_eq!(feed(&mut state, keysyms::KEY_a), Composition::None);
        assert_eq!(feed(&mut state, keysyms::KEY_Return), Composition::None);
    }
}
