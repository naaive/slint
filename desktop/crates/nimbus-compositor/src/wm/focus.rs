// SPDX-License-Identifier: MIT

use super::layout::{Rect, center};
use nimbus_ipc::{Direction, WindowId};

/// Windows ordered by how recently they had focus.
#[derive(Clone, Debug, Default)]
pub struct FocusStack {
    /// Least recent first.
    order: Vec<WindowId>,
}

impl FocusStack {
    pub fn touch(&mut self, id: WindowId) {
        self.remove(id);
        self.order.push(id);
    }

    pub fn remove(&mut self, id: WindowId) {
        self.order.retain(|&w| w != id);
    }

    /// Most recent first.
    pub fn recent(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.order.iter().rev().copied()
    }
}

/// Picks the window to focus when moving from `from` in `direction`.
///
/// Candidates must lie beyond `from`'s center in that direction.
/// Distance along the direction counts once, distance across it twice,
/// so a window straight ahead beats a closer one off to the side.
pub fn pick_in_direction(
    from: Rect,
    candidates: impl IntoIterator<Item = (WindowId, Rect)>,
    direction: Direction,
) -> Option<WindowId> {
    let (fx, fy) = center(from);
    candidates
        .into_iter()
        .filter_map(|(id, rect)| {
            let (cx, cy) = center(rect);
            let (along, across) = match direction {
                Direction::Left => (fx - cx, (cy - fy).abs()),
                Direction::Right => (cx - fx, (cy - fy).abs()),
                Direction::Up => (fy - cy, (cx - fx).abs()),
                Direction::Down => (cy - fy, (cx - fx).abs()),
            };
            (along > 0.0).then_some((id, along + 2.0 * across))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn stack_orders_by_recency() {
        let mut stack = FocusStack::default();
        stack.touch(1);
        stack.touch(2);
        stack.touch(3);
        stack.touch(1);
        assert_eq!(stack.recent().collect::<Vec<_>>(), vec![1, 3, 2]);
        stack.remove(3);
        assert_eq!(stack.recent().collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn picks_nearest_in_direction() {
        let from = r(500, 300, 200, 200);
        let windows = [
            (1, r(0, 300, 200, 200)),
            (2, r(900, 300, 200, 200)),
            (3, r(500, 0, 200, 200)),
            (4, r(520, 700, 100, 100)),
        ];
        assert_eq!(pick_in_direction(from, windows, Direction::Left), Some(1));
        assert_eq!(pick_in_direction(from, windows, Direction::Right), Some(2));
        assert_eq!(pick_in_direction(from, windows, Direction::Up), Some(3));
        assert_eq!(pick_in_direction(from, windows, Direction::Down), Some(4));
    }

    #[test]
    fn prefers_aligned_windows_over_diagonal_ones() {
        let from = r(0, 0, 100, 100);
        let aligned = (1, r(400, 0, 100, 100));
        let diagonal = (2, r(200, 300, 100, 100));
        assert_eq!(pick_in_direction(from, [aligned, diagonal], Direction::Right), Some(1));
    }

    #[test]
    fn nothing_in_direction_returns_none() {
        let from = r(0, 0, 100, 100);
        assert_eq!(pick_in_direction(from, [(1, r(300, 0, 100, 100))], Direction::Left), None);
        assert_eq!(pick_in_direction(from, std::iter::empty(), Direction::Up), None);
    }
}
