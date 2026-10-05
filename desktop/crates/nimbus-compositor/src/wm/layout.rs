// SPDX-License-Identifier: MIT

use smithay::utils::{Logical, Rectangle, Size};

pub type Rect = Rectangle<i32, Logical>;

/// Arranges the tiled windows of one output's workspace.
///
/// Implementations return exactly `count` rectangles inside `area`, in window order.
pub trait Layout {
    fn arrange(&self, area: Rect, count: usize, gaps: i32) -> Vec<Rect>;
}

/// Shrinks `area` by the given edge insets, never below zero size.
pub fn inset(area: Rect, top: i32, right: i32, bottom: i32, left: i32) -> Rect {
    let width = (area.size.w - left.max(0) - right.max(0)).max(0);
    let height = (area.size.h - top.max(0) - bottom.max(0)).max(0);
    Rect::new(
        (area.loc.x + left.max(0), area.loc.y + top.max(0)).into(),
        Size::from((width, height)),
    )
}

/// Returns the intersection, or an empty rectangle at `a`'s origin when they don't overlap.
pub fn intersect(a: Rect, b: Rect) -> Rect {
    a.intersection(b).unwrap_or_else(|| Rect::new(a.loc, Size::from((0, 0))))
}

pub fn center(rect: Rect) -> (f64, f64) {
    (
        f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0,
        f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inset_clamps_to_zero() {
        let area = Rect::new((10, 20).into(), (100, 50).into());
        assert_eq!(inset(area, 5, 10, 5, 20), Rect::new((30, 25).into(), (70, 40).into()));
        assert_eq!(inset(area, 40, 0, 40, 0).size.h, 0);
    }

    #[test]
    fn intersect_without_overlap_is_empty() {
        let a = Rect::new((0, 0).into(), (10, 10).into());
        let b = Rect::new((20, 20).into(), (10, 10).into());
        assert_eq!(intersect(a, b).size, Size::from((0, 0)));
    }
}
