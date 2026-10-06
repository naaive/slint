// SPDX-License-Identifier: MIT

use smithay::utils::{Logical, Rectangle, Size};

pub type Rect = Rectangle<i32, Logical>;

/// Arranges the tiled windows of one output's workspace.
///
/// Implementations return exactly `count` rectangles inside `area`, in window order.
pub trait Layout {
    fn arrange(&self, area: Rect, count: usize, gaps: i32) -> Vec<Rect>;
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
    fn intersect_without_overlap_is_empty() {
        let a = Rect::new((0, 0).into(), (10, 10).into());
        let b = Rect::new((20, 20).into(), (10, 10).into());
        assert_eq!(intersect(a, b).size, Size::from((0, 0)));
    }
}
