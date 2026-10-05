// SPDX-License-Identifier: MIT

use super::layout::Rect;
use smithay::utils::{Logical, Point, Size};

/// Offset between cascaded windows that would otherwise open at the same spot.
pub const CASCADE_STEP: i32 = 32;

/// Picks the location of a new floating window: centered in `area`, cascaded away from `occupied` locations,
/// and kept inside `area`.
pub fn place(
    size: Size<i32, Logical>,
    area: Rect,
    occupied: &[Point<i32, Logical>],
) -> Point<i32, Logical> {
    let size = clamp_size(size, area);
    let centered = Point::from((
        area.loc.x + (area.size.w - size.w) / 2,
        area.loc.y + (area.size.h - size.h) / 2,
    ));
    let mut location = centered;
    let close = |a: Point<i32, Logical>, b: Point<i32, Logical>| {
        (a.x - b.x).abs() < 4 && (a.y - b.y).abs() < 4
    };
    // Bounded so a crowded workspace can't loop forever; wrapping restarts at the area's corner.
    for _ in 0..64 {
        if !occupied.iter().any(|&o| close(o, location)) {
            break;
        }
        location += Point::from((CASCADE_STEP, CASCADE_STEP));
        if location.x + size.w > area.loc.x + area.size.w
            || location.y + size.h > area.loc.y + area.size.h
        {
            location = area.loc;
        }
    }
    clamp_location(Rect::new(location, size), area)
}

/// Limits a window size to the area it must fit in.
pub fn clamp_size(size: Size<i32, Logical>, area: Rect) -> Size<i32, Logical> {
    Size::from((size.w.clamp(1, area.size.w.max(1)), size.h.clamp(1, area.size.h.max(1))))
}

/// Moves `rect` the least distance needed to lie inside `area`, aligning to the top-left when it's larger.
pub fn clamp_location(rect: Rect, area: Rect) -> Point<i32, Logical> {
    let axis = |pos: i32, len: i32, start: i32, avail: i32| {
        if len >= avail { start } else { pos.clamp(start, start + avail - len) }
    };
    Point::from((
        axis(rect.loc.x, rect.size.w, area.loc.x, area.size.w),
        axis(rect.loc.y, rect.size.h, area.loc.y, area.size.h),
    ))
}

/// Keeps at least `margin` pixels of a dragged window's title area reachable inside `area`.
pub fn keep_reachable(rect: Rect, area: Rect, margin: i32) -> Point<i32, Logical> {
    let min_x = area.loc.x - rect.size.w + margin;
    let max_x = area.loc.x + area.size.w - margin;
    let min_y = area.loc.y;
    let max_y = area.loc.y + area.size.h - margin;
    Point::from((
        rect.loc.x.clamp(min_x, max_x.max(min_x)),
        rect.loc.y.clamp(min_y, max_y.max(min_y)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new((0, 32).into(), (1280, 688).into())
    }

    #[test]
    fn first_window_is_centered() {
        let p = place((400, 300).into(), area(), &[]);
        assert_eq!(p, Point::from((440, 32 + 194)));
    }

    #[test]
    fn same_size_windows_cascade() {
        let first = place((400, 300).into(), area(), &[]);
        let second = place((400, 300).into(), area(), &[first]);
        assert_eq!(second, first + Point::from((CASCADE_STEP, CASCADE_STEP)));
        let third = place((400, 300).into(), area(), &[first, second]);
        assert_eq!(third, second + Point::from((CASCADE_STEP, CASCADE_STEP)));
    }

    #[test]
    fn oversized_windows_align_to_area_origin() {
        let p = place((2000, 1000).into(), area(), &[]);
        assert_eq!(p, area().loc);
        assert_eq!(clamp_size((2000, 1000).into(), area()), Size::from((1280, 688)));
    }

    #[test]
    fn cascade_wraps_inside_the_area() {
        let size: Size<i32, Logical> = (1200, 600).into();
        let mut occupied = Vec::new();
        for _ in 0..10 {
            let p = place(size, area(), &occupied);
            assert!(area().contains_rect(Rect::new(p, size)), "{p:?} escapes");
            occupied.push(p);
        }
    }

    #[test]
    fn clamp_location_moves_minimally() {
        let r = Rect::new((1200, 600).into(), (200, 200).into());
        assert_eq!(clamp_location(r, area()), Point::from((1080, 520)));
        let inside = Rect::new((100, 100).into(), (200, 200).into());
        assert_eq!(clamp_location(inside, area()), inside.loc);
    }

    #[test]
    fn dragged_windows_stay_reachable() {
        let r = Rect::new((-5000, -100).into(), (400, 300).into());
        assert_eq!(keep_reachable(r, area(), 50), Point::from((-350, 32)));
        let r = Rect::new((5000, 5000).into(), (400, 300).into());
        assert_eq!(keep_reachable(r, area(), 50), Point::from((1230, 670)));
    }
}
