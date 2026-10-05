// SPDX-License-Identifier: MIT

//! Multi-selection over the rows of a view, with the click and keyboard semantics of desktop file managers.

/// Selected rows, the anchor for Shift ranges, and the keyboard cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    selected: Vec<bool>,
    anchor: Option<usize>,
    cursor: Option<usize>,
}

/// How a click or key press combines with the current selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Ctrl: toggle a row, or move the cursor without selecting.
    pub toggle: bool,
    /// Shift: select a range from the anchor.
    pub extend: bool,
}

impl Selection {
    pub fn new(len: usize) -> Self {
        Self { selected: vec![false; len], anchor: None, cursor: None }
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.get(index).copied().unwrap_or(false)
    }

    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    pub fn count(&self) -> usize {
        self.selected.iter().filter(|s| **s).count()
    }

    pub fn indices(&self) -> Vec<usize> {
        self.selected.iter().enumerate().filter_map(|(i, s)| s.then_some(i)).collect()
    }

    /// Replaces the selection, such as after a view refresh; out-of-range indices are ignored.
    pub fn set(
        &mut self,
        len: usize,
        indices: impl IntoIterator<Item = usize>,
        cursor: Option<usize>,
    ) {
        self.selected = vec![false; len];
        for i in indices {
            if let Some(s) = self.selected.get_mut(i) {
                *s = true;
            }
        }
        self.cursor = cursor.filter(|&c| c < len);
        self.anchor = self.cursor;
    }

    pub fn clear(&mut self) {
        self.selected.iter_mut().for_each(|s| *s = false);
    }

    pub fn select_all(&mut self) {
        self.selected.iter_mut().for_each(|s| *s = true);
    }

    /// A pointer click on a row.
    pub fn click(&mut self, index: usize, modifiers: Modifiers) {
        if index >= self.len() {
            return;
        }
        if modifiers.extend {
            let anchor = self.anchor.unwrap_or(index);
            if !modifiers.toggle {
                self.clear();
            }
            self.select_range(anchor, index);
            self.anchor = Some(anchor);
        } else if modifiers.toggle {
            self.selected[index] = !self.selected[index];
            self.anchor = Some(index);
        } else {
            self.clear();
            self.selected[index] = true;
            self.anchor = Some(index);
        }
        self.cursor = Some(index);
    }

    /// A right click: keeps a selection that includes the row, otherwise selects just the row.
    pub fn context_click(&mut self, index: usize) {
        if !self.is_selected(index) {
            self.click(index, Modifiers::default());
        } else {
            self.cursor = Some(index);
        }
    }

    /// Moves the cursor by `step` rows, clamped to the view, and updates the selection like arrow keys do.
    ///
    /// Without a cursor, the first key press lands on the first or last row.
    pub fn move_cursor(&mut self, step: isize, modifiers: Modifiers) -> Option<usize> {
        let len = self.len();
        if len == 0 {
            return None;
        }
        let target = match self.cursor {
            None if step < 0 => len - 1,
            None => 0,
            Some(c) => c.saturating_add_signed(step).min(len - 1),
        };
        if modifiers.extend {
            let anchor = self.anchor.or(self.cursor).unwrap_or(target);
            if !modifiers.toggle {
                self.clear();
            }
            self.select_range(anchor, target);
            self.anchor = Some(anchor);
        } else if !modifiers.toggle {
            self.clear();
            self.selected[target] = true;
            self.anchor = Some(target);
        }
        self.cursor = Some(target);
        Some(target)
    }

    /// Ctrl+Space: toggles the row under the cursor.
    pub fn toggle_cursor(&mut self) {
        if let Some(c) = self.cursor
            && let Some(s) = self.selected.get_mut(c)
        {
            *s = !*s;
            self.anchor = Some(c);
        }
    }

    /// Selects exactly these rows, as a rubber band does; with `add`, keeps `base` selected too.
    pub fn select_band(&mut self, rows: impl IntoIterator<Item = usize>, base: Option<&[usize]>) {
        self.clear();
        for &i in base.unwrap_or_default() {
            if let Some(s) = self.selected.get_mut(i) {
                *s = true;
            }
        }
        let mut last = None;
        for i in rows {
            if let Some(s) = self.selected.get_mut(i) {
                *s = true;
                last = Some(i);
            }
        }
        if last.is_some() {
            self.cursor = last;
            self.anchor = last;
        }
    }

    fn select_range(&mut self, a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let hi = hi.min(self.len().saturating_sub(1));
        for s in &mut self.selected[lo..=hi] {
            *s = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Modifiers = Modifiers { toggle: false, extend: false };
    const CTRL: Modifiers = Modifiers { toggle: true, extend: false };
    const SHIFT: Modifiers = Modifiers { toggle: false, extend: true };
    const CTRL_SHIFT: Modifiers = Modifiers { toggle: true, extend: true };

    #[test]
    fn clicks() {
        let mut s = Selection::new(6);
        s.click(1, PLAIN);
        assert_eq!(s.indices(), [1]);
        s.click(3, CTRL);
        assert_eq!(s.indices(), [1, 3]);
        s.click(1, CTRL);
        assert_eq!(s.indices(), [3]);
        s.click(5, SHIFT);
        assert_eq!(s.indices(), [1, 2, 3, 4, 5]);
        s.click(2, SHIFT);
        assert_eq!(s.indices(), [1, 2]);
        s.click(0, CTRL);
        s.click(4, CTRL_SHIFT);
        assert_eq!(s.indices(), [0, 1, 2, 3, 4]);
        assert_eq!(s.cursor(), Some(4));
        s.click(99, PLAIN);
        assert_eq!(s.count(), 5);
        s.context_click(2);
        assert_eq!(s.count(), 5);
        s.context_click(5);
        assert_eq!(s.indices(), [5]);
    }

    #[test]
    fn keyboard() {
        let mut s = Selection::new(5);
        assert_eq!(s.move_cursor(1, PLAIN), Some(0));
        assert_eq!(s.move_cursor(1, PLAIN), Some(1));
        assert_eq!(s.indices(), [1]);
        s.move_cursor(2, SHIFT);
        assert_eq!(s.indices(), [1, 2, 3]);
        s.move_cursor(-3, SHIFT);
        assert_eq!(s.indices(), [0, 1]);
        s.move_cursor(isize::MAX, CTRL);
        assert_eq!(s.cursor(), Some(4));
        assert_eq!(s.indices(), [0, 1]);
        s.toggle_cursor();
        assert_eq!(s.indices(), [0, 1, 4]);
        s.move_cursor(isize::MIN, PLAIN);
        assert_eq!(s.indices(), [0]);

        let mut fresh = Selection::new(3);
        assert_eq!(fresh.move_cursor(-1, PLAIN), Some(2));
        assert_eq!(Selection::new(0).move_cursor(1, PLAIN), None);
    }

    #[test]
    fn select_all_set_and_band() {
        let mut s = Selection::new(4);
        s.select_all();
        assert_eq!(s.count(), 4);
        s.clear();
        assert_eq!(s.count(), 0);
        s.set(3, [0, 2, 7], Some(2));
        assert_eq!(s.indices(), [0, 2]);
        assert_eq!(s.cursor(), Some(2));
        s.set(3, [], Some(9));
        assert_eq!(s.cursor(), None);
        s.select_band([1, 2], Some(&[0]));
        assert_eq!(s.indices(), [0, 1, 2]);
        s.select_band([], None);
        assert!(s.indices().is_empty());
        assert!(!s.is_selected(10));
        assert!(Selection::new(0).is_empty());
    }
}
