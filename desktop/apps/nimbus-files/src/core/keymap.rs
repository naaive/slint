// SPDX-License-Identifier: MIT

//! Keyboard shortcuts.

use slint::platform::Key;

use super::menu::Command;

/// A cursor movement in the file view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Command(Command),
    Move {
        step: Step,
        extend: bool,
        toggle: bool,
    },
    /// Opens the selection.
    Activate,
    GoUp,
    Back,
    Forward,
    ToggleCursor,
    EditLocation,
    StartSearch,
    /// Starts a search with typed text, as type-ahead does.
    TypeAhead(String),
    Escape,
    ViewGrid,
    ViewList,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewWindow,
    CloseWindow,
    /// Opens the context menu from the keyboard.
    ContextMenu,
}

/// Where a key press arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The file view has focus.
    View,
    /// Anywhere else in the window, after the focused control ignored the key.
    Window,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

fn is(text: &str, key: Key) -> bool {
    let mut chars = text.chars();
    chars.next() == Some(char::from(key)) && chars.next().is_none()
}

/// Maps a key press to an action, or `None` to let it propagate.
pub fn map(text: &str, m: Modifiers, scope: Scope) -> Option<KeyAction> {
    use KeyAction as A;
    let lower = text.to_lowercase();
    let letter = lower.as_str();
    let global = match (m.ctrl, m.shift, m.alt) {
        (true, false, false) => match letter {
            "l" => Some(A::EditLocation),
            "h" => Some(A::Command(Command::ToggleHidden)),
            "f" => Some(A::StartSearch),
            "r" => Some(A::Command(Command::Reload)),
            "d" => Some(A::Command(Command::AddBookmark)),
            "n" => Some(A::NewWindow),
            "w" | "q" => Some(A::CloseWindow),
            "1" => Some(A::ViewGrid),
            "2" => Some(A::ViewList),
            "+" | "=" => Some(A::ZoomIn),
            "-" => Some(A::ZoomOut),
            "0" => Some(A::ZoomReset),
            _ => None,
        },
        (true, true, false) => match letter {
            "n" => Some(A::Command(Command::NewFolder)),
            "+" => Some(A::ZoomIn),
            _ => None,
        },
        (false, _, true) if is(text, Key::LeftArrow) => Some(A::Back),
        (false, _, true) if is(text, Key::RightArrow) => Some(A::Forward),
        (false, _, true) if is(text, Key::UpArrow) => Some(A::GoUp),
        (false, false, true) if is(text, Key::Return) => Some(A::Command(Command::Properties)),
        (false, false, false) if is(text, Key::F5) => Some(A::Command(Command::Reload)),
        (false, false, false) if is(text, Key::Escape) => Some(A::Escape),
        _ => None,
    };
    if global.is_some() || scope == Scope::Window {
        return global;
    }
    let step = [
        (Key::LeftArrow, Step::Left),
        (Key::RightArrow, Step::Right),
        (Key::UpArrow, Step::Up),
        (Key::DownArrow, Step::Down),
        (Key::PageUp, Step::PageUp),
        (Key::PageDown, Step::PageDown),
        (Key::Home, Step::Home),
        (Key::End, Step::End),
    ]
    .into_iter()
    .find(|(key, _)| is(text, *key));
    if let Some((_, step)) = step
        && !m.alt
    {
        return Some(A::Move { step, extend: m.shift, toggle: m.ctrl });
    }
    match (m.ctrl, m.shift, m.alt) {
        (true, false, false) => match letter {
            "a" => Some(A::Command(Command::SelectAll)),
            "c" => Some(A::Command(Command::Copy)),
            "x" => Some(A::Command(Command::Cut)),
            "v" => Some(A::Command(Command::Paste)),
            " " => Some(A::ToggleCursor),
            _ => None,
        },
        (true, true, false) if letter == "c" => Some(A::Command(Command::CopyLocation)),
        (false, false, false) if is(text, Key::Return) => Some(A::Activate),
        (false, false, false) if is(text, Key::Backspace) => Some(A::GoUp),
        (false, false, false) if is(text, Key::Delete) => Some(A::Command(Command::Trash)),
        (false, true, false) if is(text, Key::Delete) => Some(A::Command(Command::Delete)),
        (false, false, false) if is(text, Key::F2) => Some(A::Command(Command::Rename)),
        (false, false, false) if is(text, Key::Menu) => Some(A::ContextMenu),
        (false, true, false) if is(text, Key::F10) => Some(A::ContextMenu),
        (false, _, false) if is_printable(text) => Some(A::TypeAhead(text.to_string())),
        _ => None,
    }
}

/// Text that types something visible, excluding the private-use code points of special keys and controls.
fn is_printable(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| !c.is_control() && !('\u{E000}'..='\u{F8FF}').contains(&c))
        && text.chars().any(|c| !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers { ctrl: false, shift: false, alt: false };
    const CTRL: Modifiers = Modifiers { ctrl: true, shift: false, alt: false };
    const SHIFT: Modifiers = Modifiers { ctrl: false, shift: true, alt: false };
    const ALT: Modifiers = Modifiers { ctrl: false, shift: false, alt: true };
    const CTRL_SHIFT: Modifiers = Modifiers { ctrl: true, shift: true, alt: false };

    fn key(k: Key) -> String {
        char::from(k).to_string()
    }

    #[test]
    fn view_shortcuts() {
        let v = Scope::View;
        assert_eq!(
            map(&key(Key::DownArrow), SHIFT, v),
            Some(KeyAction::Move { step: Step::Down, extend: true, toggle: false })
        );
        assert_eq!(
            map(&key(Key::Home), CTRL, v),
            Some(KeyAction::Move { step: Step::Home, extend: false, toggle: true })
        );
        assert_eq!(map(&key(Key::Return), NONE, v), Some(KeyAction::Activate));
        assert_eq!(map(&key(Key::Backspace), NONE, v), Some(KeyAction::GoUp));
        assert_eq!(map(&key(Key::UpArrow), ALT, v), Some(KeyAction::GoUp));
        assert_eq!(map(&key(Key::Delete), NONE, v), Some(KeyAction::Command(Command::Trash)));
        assert_eq!(map(&key(Key::Delete), SHIFT, v), Some(KeyAction::Command(Command::Delete)));
        assert_eq!(map(&key(Key::F2), NONE, v), Some(KeyAction::Command(Command::Rename)));
        assert_eq!(map("a", CTRL, v), Some(KeyAction::Command(Command::SelectAll)));
        assert_eq!(map("V", CTRL, v), Some(KeyAction::Command(Command::Paste)));
        assert_eq!(map("N", CTRL_SHIFT, v), Some(KeyAction::Command(Command::NewFolder)));
        assert_eq!(map(" ", CTRL, v), Some(KeyAction::ToggleCursor));
        assert_eq!(map("r", NONE, v), Some(KeyAction::TypeAhead("r".into())));
        assert_eq!(map("R", SHIFT, v), Some(KeyAction::TypeAhead("R".into())));
        assert_eq!(map(&key(Key::F10), SHIFT, v), Some(KeyAction::ContextMenu));
        assert_eq!(map(&key(Key::Menu), NONE, v), Some(KeyAction::ContextMenu));
        assert_eq!(map(" ", NONE, v), None);
        assert_eq!(map("\t", NONE, v), None);
        assert_eq!(map(&key(Key::F7), NONE, v), None);
    }

    #[test]
    fn window_shortcuts() {
        let w = Scope::Window;
        assert_eq!(map("l", CTRL, w), Some(KeyAction::EditLocation));
        assert_eq!(map("h", CTRL, w), Some(KeyAction::Command(Command::ToggleHidden)));
        assert_eq!(map(&key(Key::LeftArrow), ALT, w), Some(KeyAction::Back));
        assert_eq!(map(&key(Key::RightArrow), ALT, w), Some(KeyAction::Forward));
        assert_eq!(map(&key(Key::Return), ALT, w), Some(KeyAction::Command(Command::Properties)));
        assert_eq!(map(&key(Key::F5), NONE, w), Some(KeyAction::Command(Command::Reload)));
        assert_eq!(map("2", CTRL, w), Some(KeyAction::ViewList));
        assert_eq!(map("=", CTRL, w), Some(KeyAction::ZoomIn));
        assert_eq!(map(&key(Key::Escape), NONE, w), Some(KeyAction::Escape));
        // View-only keys don't fire from text fields.
        assert_eq!(map(&key(Key::Delete), NONE, w), None);
        assert_eq!(map("a", CTRL, w), None);
        assert_eq!(map("x", NONE, w), None);
        assert_eq!(map(&key(Key::Backspace), NONE, w), None);
    }
}
