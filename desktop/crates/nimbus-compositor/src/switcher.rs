// SPDX-License-Identifier: MIT

//! The window switcher: a session from the first press of its shortcut until its modifiers are released.
//!
//! The shell shows the session through `ShellCommand`s.
//! Without a control socket subscriber to show it, each selection takes focus at once.

use crate::keybindings::Mods;
use crate::state::State;
use nimbus_ipc::{Request, Response, ShellCommand, WindowId};

/// An open window switcher, over the windows in their order when it opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switcher {
    /// Most recently focused first.
    windows: Vec<WindowId>,
    selected: usize,
    /// The modifiers of the chord that opened it, without Shift; releasing one commits.
    held: Mods,
}

/// A switcher session from its first press until it commits or cancels.
pub struct Session {
    switcher: Switcher,
    /// Nothing shows the switcher, so each selection takes focus at once.
    direct: bool,
    /// The window focused when the session opened.
    focused: Option<WindowId>,
}

impl Switcher {
    /// Opens over `windows`, most recently focused first, and selects the one after `focused`, or the last when `backward`.
    /// Without a focused window, the most recent one comes after it.
    pub fn open(
        windows: Vec<WindowId>,
        focused: Option<WindowId>,
        held: Mods,
        backward: bool,
    ) -> Option<Self> {
        let first = *windows.first()?;
        let last = windows.len() - 1;
        let mut switcher = Self { windows, selected: 0, held: Mods { shift: false, ..held } };
        if focused == Some(first) {
            switcher.step(backward);
        } else if backward {
            switcher.selected = last;
        }
        Some(switcher)
    }

    pub fn step(&mut self, backward: bool) {
        let count = self.windows.len();
        let next = if backward { self.selected + count - 1 } else { self.selected + 1 };
        self.selected = next % count;
    }

    pub fn windows(&self) -> &[WindowId] {
        &self.windows
    }

    pub fn selected(&self) -> WindowId {
        self.windows[self.selected]
    }

    /// Keeps only the windows for which `keep` holds, moving the selection on from a dropped window.
    /// Returns whether any window is left.
    pub fn retain(&mut self, keep: impl Fn(WindowId) -> bool) -> bool {
        let before = self.windows[..self.selected].iter().filter(|&&id| !keep(id)).count();
        self.windows.retain(|&id| keep(id));
        self.selected -= before;
        if self.selected >= self.windows.len() {
            self.selected = 0;
        }
        !self.windows.is_empty()
    }

    /// Whether a modifier opened it, so it stays open until that modifier is released.
    pub fn has_modifiers(&self) -> bool {
        self.held != Mods::default()
    }

    /// Whether `mods` still holds every modifier that opened it.
    pub fn held_by(&self, mods: Mods) -> bool {
        (!self.held.ctrl || mods.ctrl)
            && (!self.held.alt || mods.alt)
            && (!self.held.logo || mods.logo)
    }
}

impl State {
    /// Opens the window switcher, or selects the next window in the open one.
    pub fn switch_windows(&mut self, backward: bool) {
        let wm = &self.nimbus.wm;
        if let Some(session) = &mut self.nimbus.switcher {
            if !session.switcher.retain(|id| wm.get(id).is_some()) {
                self.cancel_switcher();
                return;
            }
            session.switcher.step(backward);
            let selected = session.switcher.selected();
            self.show_selection(ShellCommand::SwitcherStep { selected }, selected);
            return;
        }
        let held = self.nimbus.keyboard.as_ref().map(|k| Mods::from(&k.modifier_state()));
        let Some(switcher) =
            Switcher::open(wm.recent_windows(), wm.focused(), held.unwrap_or_default(), backward)
        else {
            return;
        };
        let selected = switcher.selected();
        let open = ShellCommand::SwitcherOpen { windows: switcher.windows().to_vec(), selected };
        let has_modifiers = switcher.has_modifiers();
        self.nimbus.switcher = Some(Session {
            switcher,
            direct: !self.nimbus.ipc.has_subscribers(),
            focused: wm.focused(),
        });
        if has_modifiers {
            self.show_selection(open, selected);
        } else {
            self.commit_switcher();
        }
    }

    /// Shows a new selection: on the shell's switcher, or by focusing it at once when nothing shows the switcher.
    fn show_selection(&mut self, command: ShellCommand, selected: WindowId) {
        if self.nimbus.switcher.as_ref().is_some_and(|session| session.direct) {
            self.activate_from_switcher(selected);
        } else {
            self.nimbus.shell_command(command);
        }
    }

    /// Commits the open switcher once a key release lets go of a modifier that opened it.
    pub fn switcher_key_released(&mut self, mods: Mods) {
        if self.nimbus.switcher.as_ref().is_some_and(|s| !s.switcher.held_by(mods)) {
            self.commit_switcher();
        }
    }

    /// Closes the switcher and focuses its selected window.
    pub fn commit_switcher(&mut self) {
        let Some(Session { mut switcher, direct, .. }) = self.nimbus.switcher.take() else {
            return;
        };
        if switcher.has_modifiers() && !direct {
            self.nimbus.shell_command(ShellCommand::SwitcherCommit);
        }
        let wm = &self.nimbus.wm;
        if switcher.retain(|id| wm.get(id).is_some()) {
            self.activate_from_switcher(switcher.selected());
        }
    }

    /// Closes the switcher, and gives focus back to the window that had it if the switcher moved it.
    pub fn cancel_switcher(&mut self) {
        let Some(session) = self.nimbus.switcher.take() else {
            return;
        };
        if !session.direct {
            self.nimbus.shell_command(ShellCommand::SwitcherCancel);
        } else if let Some(id) = session.focused.filter(|&id| self.nimbus.wm.get(id).is_some()) {
            self.activate_from_switcher(id);
        }
    }

    fn activate_from_switcher(&mut self, id: WindowId) {
        if let Response::Error { message } = self.handle_request(Request::Activate { id }) {
            tracing::warn!("window switcher failed: {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALT: Mods = Mods { ctrl: false, alt: true, shift: false, logo: false };

    #[test]
    fn opens_on_the_previous_window_and_wraps() {
        let mut switcher = Switcher::open(vec![3, 1, 2], Some(3), ALT, false).unwrap();
        assert_eq!(switcher.selected(), 1);
        switcher.step(false);
        assert_eq!(switcher.selected(), 2);
        switcher.step(false);
        assert_eq!(switcher.selected(), 3);
        switcher.step(true);
        assert_eq!(switcher.selected(), 2);
        assert_eq!(switcher.windows(), [3, 1, 2]);
    }

    #[test]
    fn backward_opens_on_the_least_recent_window() {
        let switcher = Switcher::open(vec![3, 1, 2], Some(3), ALT, true).unwrap();
        assert_eq!(switcher.selected(), 2);
        assert_eq!(Switcher::open(vec![7], Some(7), ALT, false).unwrap().selected(), 7);
        assert_eq!(Switcher::open(Vec::new(), None, ALT, false), None);
    }

    #[test]
    fn without_focus_opens_on_the_most_recent_window() {
        assert_eq!(Switcher::open(vec![3, 1, 2], None, ALT, false).unwrap().selected(), 3);
        assert_eq!(Switcher::open(vec![3, 1, 2], None, ALT, true).unwrap().selected(), 2);
        assert_eq!(Switcher::open(vec![7], None, ALT, false).unwrap().selected(), 7);
    }

    #[test]
    fn releasing_a_modifier_that_opened_it_commits() {
        let shift_alt = Mods { shift: true, ..ALT };
        let switcher = Switcher::open(vec![1, 2], Some(1), shift_alt, true).unwrap();
        assert!(switcher.has_modifiers());
        assert!(switcher.held_by(ALT), "releasing Shift only changes direction");
        assert!(switcher.held_by(Mods { ctrl: true, ..ALT }));
        assert!(!switcher.held_by(Mods { shift: true, ..Mods::default() }));

        let ctrl_alt = Mods { ctrl: true, ..ALT };
        let switcher = Switcher::open(vec![1, 2], Some(1), ctrl_alt, false).unwrap();
        assert!(!switcher.held_by(ALT));

        let shift = Mods { shift: true, ..Mods::default() };
        assert!(!Switcher::open(vec![1, 2], Some(1), shift, false).unwrap().has_modifiers());
    }

    #[test]
    fn closed_windows_leave_the_selection_on_a_neighbor() {
        let mut switcher = Switcher::open(vec![1, 2, 3, 4], Some(1), ALT, false).unwrap();
        switcher.step(false);
        assert_eq!(switcher.selected(), 3);
        assert!(switcher.retain(|id| id != 1));
        assert_eq!(switcher.selected(), 3);
        assert!(switcher.retain(|id| id != 3));
        assert_eq!(switcher.selected(), 4);
        assert!(switcher.retain(|id| id != 4));
        assert_eq!(switcher.selected(), 2);
        assert!(!switcher.retain(|_| false));
    }
}
