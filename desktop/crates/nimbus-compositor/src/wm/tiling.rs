// SPDX-License-Identifier: MIT

use super::layout::{Layout, Rect};
use smithay::utils::Size;

/// One master window on the left, the rest stacked on the right.
#[derive(Clone, Copy, Debug)]
pub struct MasterStack {
    /// The master column's share of the width available after gaps, in `0.1..=0.9`.
    pub master_ratio: f64,
}

impl Default for MasterStack {
    fn default() -> Self {
        Self { master_ratio: 0.5 }
    }
}

impl Layout for MasterStack {
    fn arrange(&self, area: Rect, count: usize, gaps: i32) -> Vec<Rect> {
        let gaps = gaps.max(0);
        let rect = |x: i32, y: i32, w: i32, h: i32| {
            Rect::new((x, y).into(), Size::from((w.max(1), h.max(1))))
        };
        match count {
            0 => Vec::new(),
            1 => vec![rect(
                area.loc.x + gaps,
                area.loc.y + gaps,
                area.size.w - 2 * gaps,
                area.size.h - 2 * gaps,
            )],
            _ => {
                let inner_width = area.size.w - 3 * gaps;
                let ratio = self.master_ratio.clamp(0.1, 0.9);
                let master_width = (f64::from(inner_width) * ratio).round() as i32;
                let stack_width = inner_width - master_width;
                let height = area.size.h - 2 * gaps;
                let mut rects = Vec::with_capacity(count);
                rects.push(rect(area.loc.x + gaps, area.loc.y + gaps, master_width, height));

                let stacked = i32::try_from(count - 1).unwrap_or(i32::MAX);
                let stack_x = area.loc.x + 2 * gaps + master_width;
                let available = height - (stacked - 1) * gaps;
                let mut y = area.loc.y + gaps;
                for index in 0..stacked {
                    // The last window takes the rounding remainder so the column ends exactly at the gap.
                    let h = if index == stacked - 1 {
                        area.loc.y + area.size.h - gaps - y
                    } else {
                        available / stacked
                    };
                    rects.push(rect(stack_x, y, stack_width, h));
                    y += h + gaps;
                }
                rects
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new((0, 32).into(), (1280, 688).into())
    }

    #[test]
    fn single_window_fills_area_minus_gaps() {
        let rects = MasterStack::default().arrange(area(), 1, 10);
        assert_eq!(rects, vec![Rect::new((10, 42).into(), (1260, 668).into())]);
    }

    #[test]
    fn two_windows_split_horizontally() {
        let rects = MasterStack::default().arrange(area(), 2, 10);
        assert_eq!(rects[0], Rect::new((10, 42).into(), (625, 668).into()));
        assert_eq!(rects[1], Rect::new((645, 42).into(), (625, 668).into()));
    }

    #[test]
    fn stack_column_tiles_vertically_without_overlap() {
        let rects = MasterStack::default().arrange(area(), 4, 8);
        assert_eq!(rects.len(), 4);
        let stack = &rects[1..];
        for pair in stack.windows(2) {
            assert_eq!(pair[0].loc.y + pair[0].size.h + 8, pair[1].loc.y);
            assert_eq!(pair[0].loc.x, pair[1].loc.x);
        }
        let last = stack[2];
        assert_eq!(last.loc.y + last.size.h, 32 + 688 - 8);
        for r in &rects {
            assert!(area().contains_rect(*r), "{r:?} escapes the area");
        }
    }

    #[test]
    fn zero_gaps_and_tiny_areas_stay_positive() {
        let tiny = Rect::new((0, 0).into(), (4, 4).into());
        for rect in MasterStack::default().arrange(tiny, 5, 3) {
            assert!(rect.size.w >= 1 && rect.size.h >= 1);
        }
        let rects = MasterStack { master_ratio: 0.6 }.arrange(area(), 2, 0);
        assert_eq!(rects[0].size.w + rects[1].size.w, 1280);
        assert_eq!(rects[0].size.w, 768);
    }
}
