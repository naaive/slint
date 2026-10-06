// SPDX-License-Identifier: MIT

//! Display rectangles in the global layout, in logical pixels, and keeping displays edge to edge,
//! as the compositor and the Displays page in Settings both do.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Whether the two share part of an edge or overlap; touching at a corner isn't enough.
    pub fn touches(&self, other: &Rect) -> bool {
        let x = span_overlap(self.x, self.right(), other.x, other.right());
        let y = span_overlap(self.y, self.bottom(), other.y, other.bottom());
        x >= 0 && y >= 0 && (x > 0 || y > 0)
    }

    /// Whether the two cover some area in common.
    pub fn overlaps(&self, other: &Rect) -> bool {
        span_overlap(self.x, self.right(), other.x, other.right()) > 0
            && span_overlap(self.y, self.bottom(), other.y, other.bottom()) > 0
    }

    fn moved(self, (dx, dy): (i32, i32)) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..self }
    }
}

fn span_overlap(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
    a1.min(b1) - a0.max(b0)
}

fn distance((x, y): (i32, i32)) -> i64 {
    i64::from(x) * i64::from(x) + i64::from(y) * i64::from(y)
}

/// The smallest rectangle that holds all of `rects`.
pub fn bounds<'a>(rects: impl IntoIterator<Item = &'a Rect>) -> Option<Rect> {
    rects.into_iter().fold(None, |bounds: Option<Rect>, r| {
        let Some(b) = bounds else { return Some(*r) };
        let (x, y) = (b.x.min(r.x), b.y.min(r.y));
        let (right, bottom) = (b.right().max(r.right()), b.bottom().max(r.bottom()));
        Some(Rect { x, y, w: right - x, h: bottom - y })
    })
}

/// The position nearest `target` where `moving` shares an edge with one of `others`.
pub fn attach(moving: Rect, target: (i32, i32), others: &[Rect]) -> Option<(i32, i32)> {
    others
        .iter()
        .flat_map(|other| edge_positions(moving, target, other))
        .min_by_key(|&(x, y)| distance((x - target.0, y - target.1)))
}

/// The positions near `target` where `moving` shares an edge with `other`: left of it, right, above, and below.
fn edge_positions(moving: Rect, target: (i32, i32), other: &Rect) -> [(i32, i32); 4] {
    let x = slide(target.0, moving.w, other.x, other.w);
    let y = slide(target.1, moving.h, other.y, other.h);
    [(other.x - moving.w, y), (other.right(), y), (x, other.y - moving.h), (x, other.bottom())]
}

/// Where a span of `length` at `position` goes along another span's edge:
/// lined up with its start or end when within 5% of `length`, and overlapping it by at least a pixel.
fn slide(position: i32, length: i32, other_start: i32, other_length: i32) -> i32 {
    let threshold = length / 20;
    [other_start, other_start + other_length - length]
        .into_iter()
        .find(|aligned| (position - aligned).abs() <= threshold)
        .unwrap_or(position)
        .clamp(other_start - length + 1, other_start + other_length - 1)
}

/// The indices of `rects` in groups that touch each other, in order of their first index.
pub fn groups(rects: &[Rect]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut rest: Vec<usize> = (0..rects.len()).collect();
    while !rest.is_empty() {
        let mut group = vec![rest.remove(0)];
        while let Some(index) =
            rest.iter().position(|&r| group.iter().any(|&g| rects[g].touches(&rects[r])))
        {
            group.push(rest.remove(index));
        }
        groups.push(group);
    }
    groups
}

/// Whether `rects` form one group, each touching another.
pub fn connected(rects: &[Rect]) -> bool {
    groups(rects).len() <= 1
}

/// Moves each group of `rects` apart from the largest one, as little as it takes to touch it without covering it.
pub fn join(rects: &mut [Rect]) {
    loop {
        let groups = groups(rects);
        let Some(main) = groups.iter().rev().max_by_key(|g| g.len()) else {
            return;
        };
        let Some(apart) = groups.iter().find(|g| *g != main) else {
            return;
        };
        let main: Vec<Rect> = main.iter().map(|&i| rects[i]).collect();
        let offset = join_offset(apart.iter().map(|&i| rects[i]).collect(), &main);
        for &i in apart {
            rects[i] = rects[i].moved(offset);
        }
    }
}

/// The smallest move of `group` that makes it share an edge with `main` without covering any of it.
fn join_offset(group: Vec<Rect>, main: &[Rect]) -> (i32, i32) {
    let fits = |offset: (i32, i32)| {
        group.iter().all(|g| main.iter().all(|m| !g.moved(offset).overlaps(m)))
    };
    let nearest = group
        .iter()
        .flat_map(|g| {
            main.iter().flat_map(move |m| {
                edge_positions(*g, (g.x, g.y), m).map(|(x, y)| (x - g.x, y - g.y))
            })
        })
        .filter(|&offset| fits(offset))
        .min_by_key(|&offset| distance(offset));
    nearest.unwrap_or_else(|| {
        // Right of everything, level with the rightmost rectangle of `main`.
        let (Some(b), Some(g)) = (bounds(main), bounds(&group)) else {
            return (0, 0);
        };
        let rightmost = main.iter().max_by_key(|m| m.right()).map_or(b.y, |m| m.y);
        let leftmost = group.iter().min_by_key(|r| r.x).map_or(g.y, |r| r.y);
        (b.right() - g.x, rightmost - leftmost)
    })
}

/// Pushes apart the rectangles that cover each other in `rects` but not in `before`, where `None` is a new one.
///
/// In order of position, each moves right or down, whichever is less, until it covers none of those before it.
pub fn separate(rects: &mut [Rect], before: &[Option<Rect>]) {
    let covered_before = |i: usize, j: usize| matches!((before.get(i), before.get(j)), (Some(Some(a)), Some(Some(b))) if a.overlaps(b));
    let mut order: Vec<usize> = (0..rects.len()).collect();
    order.sort_by_key(|&i| (rects[i].x, rects[i].y));
    for (k, &i) in order.iter().enumerate() {
        while let Some(&j) =
            order[..k].iter().find(|&&j| rects[i].overlaps(&rects[j]) && !covered_before(i, j))
        {
            let dx = rects[j].right() - rects[i].x;
            let dy = rects[j].bottom() - rects[i].y;
            if dx <= dy {
                rects[i].x += dx;
            } else {
                rects[i].y += dy;
            }
        }
    }
}

/// Moves what lies past `before`'s right or bottom edge by the change of its width or height to `size`,
/// so that a rectangle that grows or shrinks keeps its neighbors at its edges.
pub fn follow_resize(rects: &mut [Rect], before: Rect, (w, h): (i32, i32)) {
    let (right, bottom) = (before.right(), before.bottom());
    for rect in rects {
        if rect.x >= right {
            rect.x += w - before.w;
        }
        if rect.y >= bottom {
            rect.y += h - before.h;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn touching() {
        let a = rect(0, 0, 100, 100);
        assert!(a.touches(&rect(100, 50, 100, 100)), "sharing an edge");
        assert!(a.touches(&rect(50, 50, 100, 100)), "overlapping");
        assert!(!a.touches(&rect(100, 100, 100, 100)), "a corner");
        assert!(!a.touches(&rect(101, 0, 100, 100)), "a gap");
        assert!(!a.overlaps(&rect(100, 0, 100, 100)));
        assert!(a.overlaps(&rect(99, 99, 100, 100)));
    }

    #[test]
    fn attaching_lines_up_near_edges() {
        let a = rect(0, 0, 1920, 1080);
        let b = rect(1920, 0, 1280, 1024);
        assert_eq!(attach(b, (20, 1100), &[a]), Some((0, 1080)), "lined up below");
        assert_eq!(attach(b, (-1100, 0), &[a]), Some((-1280, 0)), "left");
        assert_eq!(attach(b, (0, 0), &[]), None);
    }

    #[test]
    fn groups_and_joining() {
        let mut rects = [rect(0, 0, 100, 100), rect(300, 0, 100, 100), rect(100, 0, 100, 100)];
        assert_eq!(groups(&rects), [vec![0, 2], vec![1]]);
        join(&mut rects);
        assert_eq!(rects[1], rect(200, 0, 100, 100), "only the stranded one moves");
        assert!(connected(&rects));

        // A group moves as a whole, to the nearest edge.
        let mut rects = [
            rect(0, 0, 100, 100),
            rect(150, 0, 100, 100),
            rect(150, 100, 100, 100),
            rect(0, 100, 50, 50),
        ];
        join(&mut rects);
        assert_eq!((rects[1].x, rects[2].x), (100, 100));
        assert_eq!((rects[1].y, rects[2].y), (0, 100));
        assert_eq!(rects[3], rect(0, 100, 50, 50));
        assert!(connected(&rects));
    }

    #[test]
    fn separating_what_grew_into_each_other() {
        let before = [rect(0, 0, 960, 540), rect(960, 0, 960, 540), rect(0, 540, 960, 540)];
        let mut rects = before.map(|r| Rect { w: 1920, h: 1080, ..r });
        separate(&mut rects, &before.map(Some));
        let positions = rects.map(|r| (r.x, r.y));
        assert_eq!(positions, [(0, 0), (1920, 0), (0, 1080)]);

        // Rectangles that covered each other before stay as they are.
        let mirrored = [rect(0, 0, 100, 100), rect(0, 0, 100, 100)];
        let mut rects = mirrored;
        separate(&mut rects, &mirrored.map(Some));
        assert_eq!(rects, mirrored);
    }

    #[test]
    fn resizing_keeps_neighbors_at_the_edges() {
        let a = rect(0, 0, 1920, 1080);
        let mut rects = [a, rect(1920, 0, 1920, 1080), rect(0, 1080, 1920, 1080)];
        follow_resize(&mut rects, a, (960, 540));
        assert_eq!((rects[0].x, rects[0].y), (0, 0));
        assert_eq!((rects[1].x, rects[1].y), (960, 0));
        assert_eq!((rects[2].x, rects[2].y), (0, 540));
    }
}
