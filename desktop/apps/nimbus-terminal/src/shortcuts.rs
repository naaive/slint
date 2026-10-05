// SPDX-License-Identifier: MIT

//! The app's keyboard shortcuts, checked before a key reaches the terminal.

use crate::keys::{Key, Modifiers};

/// An app command bound to a key chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    SelectAll,
    NewTab,
    CloseTab,
    NewWindow,
    CloseWindow,
    NextTab,
    PreviousTab,
    MoveTabLeft,
    MoveTabRight,
    /// Selects the tab at a zero-based index; the last tab for `usize::MAX`.
    GoToTab(usize),
    Find,
    FindNext,
    FindPrevious,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollLineUp,
    ScrollLineDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    Preferences,
    ToggleFullscreen,
}

/// A row of the shortcuts reference: the chord as shown to users, and what it does.
pub const REFERENCE: &[(&str, &str)] = &[
    ("Ctrl+Shift+C", "Copy"),
    ("Ctrl+Shift+V", "Paste"),
    ("Ctrl+Shift+T", "New tab"),
    ("Ctrl+Shift+W", "Close tab"),
    ("Ctrl+Shift+N", "New window"),
    ("Ctrl+Page Up/Down", "Switch tabs"),
    ("Ctrl+Shift+F", "Find"),
    ("Ctrl+Plus/Minus/0", "Zoom"),
    ("Shift+Page Up/Down", "Scroll"),
    ("F11", "Full screen"),
];

fn letter(key: &Key) -> Option<char> {
    match key {
        Key::Text(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c.to_ascii_lowercase()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The action bound to `key` with `mods`, if any.
pub fn lookup(key: &Key, mods: Modifiers) -> Option<Action> {
    use Action::*;
    let ctrl_shift = mods.ctrl && mods.shift && !mods.alt && !mods.meta;
    let ctrl_only = mods == Modifiers::CTRL;
    let shift_only = mods == Modifiers::SHIFT;
    let alt_only = mods == Modifiers::ALT;

    if let Some(c) = letter(key) {
        if ctrl_shift {
            return Some(match c {
                'c' => Copy,
                'v' => Paste,
                'a' => SelectAll,
                't' => NewTab,
                'w' => CloseTab,
                'n' => NewWindow,
                'q' => CloseWindow,
                'f' => Find,
                'g' => FindNext,
                'h' => FindPrevious,
                // Shifted symbols on common layouts, such as `+` for `=`.
                '+' => ZoomIn,
                '_' => ZoomOut,
                ')' => ZoomReset,
                _ => return None,
            });
        }
        if ctrl_only {
            return match c {
                '+' | '=' => Some(ZoomIn),
                '-' => Some(ZoomOut),
                '0' => Some(ZoomReset),
                ',' => Some(Preferences),
                _ => None,
            };
        }
        if alt_only && c.is_ascii_digit() {
            let digit = c as usize - '0' as usize;
            return Some(match digit {
                0 => GoToTab(9),
                9 => GoToTab(usize::MAX),
                n => GoToTab(n - 1),
            });
        }
        return None;
    }

    match key {
        Key::PageUp if ctrl_only => Some(PreviousTab),
        Key::PageDown if ctrl_only => Some(NextTab),
        Key::PageUp if ctrl_shift => Some(MoveTabLeft),
        Key::PageDown if ctrl_shift => Some(MoveTabRight),
        Key::Tab if ctrl_only => Some(NextTab),
        Key::Tab | Key::Backtab if ctrl_shift => Some(PreviousTab),
        Key::PageUp if shift_only => Some(ScrollPageUp),
        Key::PageDown if shift_only => Some(ScrollPageDown),
        Key::Home if shift_only => Some(ScrollTop),
        Key::End if shift_only => Some(ScrollBottom),
        Key::Up if ctrl_shift => Some(ScrollLineUp),
        Key::Down if ctrl_shift => Some(ScrollLineDown),
        Key::Insert if shift_only => Some(Paste),
        Key::Insert if ctrl_only => Some(Copy),
        Key::F(11) if mods.is_empty() => Some(ToggleFullscreen),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Key {
        Key::Text(s.into())
    }

    #[test]
    fn clipboard_and_tabs() {
        assert_eq!(lookup(&text("C"), Modifiers::CTRL_SHIFT), Some(Action::Copy));
        assert_eq!(lookup(&text("c"), Modifiers::CTRL_SHIFT), Some(Action::Copy));
        assert_eq!(lookup(&text("V"), Modifiers::CTRL_SHIFT), Some(Action::Paste));
        assert_eq!(lookup(&Key::Insert, Modifiers::SHIFT), Some(Action::Paste));
        assert_eq!(lookup(&text("T"), Modifiers::CTRL_SHIFT), Some(Action::NewTab));
        assert_eq!(lookup(&text("W"), Modifiers::CTRL_SHIFT), Some(Action::CloseTab));
        assert_eq!(lookup(&text("F"), Modifiers::CTRL_SHIFT), Some(Action::Find));
        assert_eq!(lookup(&Key::PageDown, Modifiers::CTRL), Some(Action::NextTab));
        assert_eq!(lookup(&Key::PageUp, Modifiers::CTRL_SHIFT), Some(Action::MoveTabLeft));
        assert_eq!(lookup(&text("1"), Modifiers::ALT), Some(Action::GoToTab(0)));
        assert_eq!(lookup(&text("0"), Modifiers::ALT), Some(Action::GoToTab(9)));
        assert_eq!(lookup(&text("9"), Modifiers::ALT), Some(Action::GoToTab(usize::MAX)));
    }

    #[test]
    fn zoom_and_scroll() {
        assert_eq!(lookup(&text("+"), Modifiers::CTRL), Some(Action::ZoomIn));
        assert_eq!(lookup(&text("="), Modifiers::CTRL), Some(Action::ZoomIn));
        assert_eq!(lookup(&text("+"), Modifiers::CTRL_SHIFT), Some(Action::ZoomIn));
        assert_eq!(lookup(&text("-"), Modifiers::CTRL), Some(Action::ZoomOut));
        assert_eq!(lookup(&text("0"), Modifiers::CTRL), Some(Action::ZoomReset));
        assert_eq!(lookup(&Key::PageUp, Modifiers::SHIFT), Some(Action::ScrollPageUp));
        assert_eq!(lookup(&Key::End, Modifiers::SHIFT), Some(Action::ScrollBottom));
        assert_eq!(lookup(&Key::F(11), Modifiers::NONE), Some(Action::ToggleFullscreen));
    }

    #[test]
    fn terminal_keys_pass_through() {
        assert_eq!(lookup(&text("c"), Modifiers::CTRL), None);
        assert_eq!(lookup(&text("w"), Modifiers::CTRL), None);
        assert_eq!(lookup(&text("a"), Modifiers::NONE), None);
        assert_eq!(lookup(&text("x"), Modifiers::ALT), None);
        assert_eq!(lookup(&Key::PageUp, Modifiers::NONE), None);
        assert_eq!(lookup(&Key::Up, Modifiers::SHIFT), None);
        assert_eq!(lookup(&Key::Tab, Modifiers::NONE), None);
        assert_eq!(
            lookup(&text("c"), Modifiers { ctrl: true, shift: true, alt: true, meta: false }),
            None
        );
    }
}
