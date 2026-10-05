// SPDX-License-Identifier: MIT

//! Box drawing, block elements, and Powerline separators drawn to fill the cell exactly,
//! so lines join across cells whatever the font's metrics.

/// Whether `c` is drawn here instead of taken from a font.
pub fn is_procedural(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259F}' | '\u{E0B0}'..='\u{E0B3}')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum W {
    None,
    Light,
    Heavy,
    Double,
}

impl W {
    fn single(self) -> bool {
        matches!(self, W::Light | W::Heavy)
    }
}

/// The arms of U+2500 to U+257F that are plain lines, as `up, right, down, left`;
/// `0` none, `1` light, `2` heavy, `3` double. Dashed, arc, and diagonal characters are `None`.
fn arms(c: char) -> Option<[W; 4]> {
    const TABLE: [&str; 128] = [
        "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", "0110", "0210", "0120",
        "0220", "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002",
        "2001", "2002", "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", "1011",
        "1012", "2011", "1021", "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212",
        "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", "2101", "2102", "2201",
        "2202", "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", "2211", "1122",
        "1221", "2212", "1222", "2122", "2221", "2222", "", "", "", "", "0303", "3030", "0310",
        "0130", "0330", "0013", "0031", "0033", "1300", "3100", "3300", "1003", "3001", "3003",
        "1310", "3130", "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", "3101",
        "3303", "1313", "3131", "3333", "", "", "", "", "", "", "", "0001", "1000", "0100", "0010",
        "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010",
    ];
    let index = (c as u32).checked_sub(0x2500)? as usize;
    let code = TABLE.get(index)?.as_bytes();
    if code.len() != 4 {
        return None;
    }
    let weight = |b: u8| match b {
        b'1' => W::Light,
        b'2' => W::Heavy,
        b'3' => W::Double,
        _ => W::None,
    };
    Some([weight(code[0]), weight(code[1]), weight(code[2]), weight(code[3])])
}

struct Canvas {
    width: i32,
    height: i32,
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width: width as i32,
            height: height as i32,
            data: vec![0; (width * height) as usize],
        }
    }

    fn put(&mut self, x: i32, y: i32, alpha: u8) {
        if (0..self.width).contains(&x) && (0..self.height).contains(&y) {
            let pixel = &mut self.data[(y * self.width + x) as usize];
            *pixel = (*pixel).max(alpha);
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        for y in y0.max(0)..y1.min(self.height) {
            for x in x0.max(0)..x1.min(self.width) {
                self.put(x, y, alpha);
            }
        }
    }

    /// A rectangle with the main axis horizontal, or vertical when `vertical`.
    fn oriented(&mut self, vertical: bool, main: (i32, i32), cross: (i32, i32)) {
        if vertical {
            self.rect(cross.0, main.0, cross.1, main.1, 255);
        } else {
            self.rect(main.0, cross.0, main.1, cross.1, 255);
        }
    }

    /// Antialiased coverage from a signed distance: positive inside, in pixels.
    fn shade(&mut self, coverage: impl Fn(f32, f32) -> f32) {
        for y in 0..self.height {
            for x in 0..self.width {
                let value = coverage(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0);
                if value > 0.0 {
                    self.put(x, y, (value * 255.0).round() as u8);
                }
            }
        }
    }
}

/// Stroke thicknesses for a cell.
#[derive(Clone, Copy)]
struct Strokes {
    light: i32,
    heavy: i32,
    /// Distance from the center line to each line of a double stroke.
    gap: i32,
}

impl Strokes {
    fn of(self, w: W) -> i32 {
        match w {
            W::Heavy => self.heavy,
            _ => self.light,
        }
    }
}

fn span(center: i32, thickness: i32) -> (i32, i32) {
    let start = center - thickness / 2;
    (start, start + thickness)
}

/// Draws one arm. The main axis runs along the arm; `perp` are the arms on the negative and positive cross sides.
#[allow(clippy::too_many_arguments)]
fn arm(
    canvas: &mut Canvas,
    vertical: bool,
    positive: bool,
    weight: W,
    perp: [W; 2],
    opposite: W,
    s: Strokes,
) {
    let (length, mc, cc) = if vertical {
        (canvas.height, canvas.height / 2, canvas.width / 2)
    } else {
        (canvas.width, canvas.width / 2, canvas.height / 2)
    };
    let tl = s.light;
    let g = s.gap;
    let range = |start: i32, end: i32| if positive { (start, length) } else { (0, end) };
    match weight {
        W::None => {}
        W::Light | W::Heavy => {
            let t = s.of(weight);
            let main = if perp.contains(&W::Double) {
                if opposite != W::None {
                    range(span(mc, t).0, span(mc, t).1)
                } else {
                    range(mc + g - tl / 2, mc - g - tl / 2 + tl)
                }
            } else if perp.iter().any(|p| p.single()) {
                let pt = perp.iter().filter(|p| p.single()).map(|p| s.of(*p)).max().unwrap_or(t);
                range(span(mc, pt).0, span(mc, pt).1)
            } else {
                range(span(mc, t).0, span(mc, t).1)
            };
            canvas.oriented(vertical, main, span(cc, t));
        }
        W::Double => {
            for (side, center) in [(0usize, cc - g), (1, cc + g)] {
                let same = perp[side];
                let other = perp[1 - side];
                let (start, end) = if same == W::Double {
                    (mc + g - tl / 2, mc - g - tl / 2 + tl)
                } else if same.single() {
                    span(mc, tl)
                } else if other == W::Double {
                    (mc - g - tl / 2, mc + g - tl / 2 + tl)
                } else {
                    span(mc, tl)
                };
                canvas.oriented(vertical, range(start, end), span(center, tl));
            }
        }
    }
}

fn lines(canvas: &mut Canvas, [up, right, down, left]: [W; 4], s: Strokes) {
    arm(canvas, false, true, right, [up, down], left, s);
    arm(canvas, false, false, left, [up, down], right, s);
    arm(canvas, true, true, down, [left, right], up, s);
    arm(canvas, true, false, up, [left, right], down, s);
}

fn dashes(canvas: &mut Canvas, vertical: bool, count: i32, thickness: i32) {
    let (length, cc) = if vertical {
        (canvas.height, canvas.width / 2)
    } else {
        (canvas.width, canvas.height / 2)
    };
    let gap = (length / count / 3).max(1);
    for i in 0..count {
        let start = i * length / count + gap / 2;
        let end = (i + 1) * length / count - (gap - gap / 2);
        canvas.oriented(vertical, (start, end.max(start + 1)), span(cc, thickness));
    }
}

fn arc(canvas: &mut Canvas, c: char, t: i32) {
    let (w, h) = (canvas.width, canvas.height);
    let fcx = span(w / 2, t).0 as f32 + t as f32 / 2.0;
    let fcy = span(h / 2, t).0 as f32 + t as f32 / 2.0;
    let r = (w.min(h) as f32 / 2.0 - 0.5).max(1.0);
    // The direction from the corner toward the center of the circle.
    let (dx, dy) = match c {
        '\u{256D}' => (1.0, 1.0),
        '\u{256E}' => (-1.0, 1.0),
        '\u{256F}' => (-1.0, -1.0),
        _ => (1.0, -1.0),
    };
    let (ccx, ccy) = (fcx + dx * r, fcy + dy * r);
    let half = t as f32 / 2.0;
    canvas.shade(|x, y| {
        if (x - ccx) * dx > 0.0 || (y - ccy) * dy > 0.0 {
            return 0.0;
        }
        let distance = ((x - ccx).powi(2) + (y - ccy).powi(2)).sqrt();
        half + 0.5 - (distance - r).abs()
    });
    let hx = span(h / 2, t);
    let vx = span(w / 2, t);
    // The straight parts overlap the arc's ends by a pixel, so they join without a seam.
    if dx > 0.0 {
        canvas.rect(ccx.floor() as i32 - 1, hx.0, w, hx.1, 255);
    } else {
        canvas.rect(0, hx.0, ccx.ceil() as i32 + 1, hx.1, 255);
    }
    if dy > 0.0 {
        canvas.rect(vx.0, ccy.floor() as i32 - 1, vx.1, h, 255);
    } else {
        canvas.rect(vx.0, 0, vx.1, ccy.ceil() as i32 + 1, 255);
    }
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let length = abx * abx + aby * aby;
    let t = if length > 0.0 {
        (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (a.0 + abx * t, a.1 + aby * t);
    ((p.0 - qx).powi(2) + (p.1 - qy).powi(2)).sqrt()
}

fn strokes_along(canvas: &mut Canvas, segments: &[((f32, f32), (f32, f32))], t: i32) {
    let half = t as f32 / 2.0;
    canvas.shade(|x, y| {
        segments
            .iter()
            .map(|(a, b)| half + 0.5 - segment_distance((x, y), *a, *b))
            .fold(0.0, f32::max)
    });
}

fn blocks(canvas: &mut Canvas, c: char) {
    let (w, h) = (canvas.width, canvas.height);
    let eighth_h = |n: i32| (h * n + 4) / 8;
    let eighth_w = |n: i32| (w * n + 4) / 8;
    let (hw, hh) = (w / 2, h / 2);
    match c {
        '\u{2580}' => canvas.rect(0, 0, w, hh, 255),
        '\u{2581}'..='\u{2588}' => {
            let n = c as i32 - 0x2580;
            canvas.rect(0, h - eighth_h(n), w, h, 255);
        }
        '\u{2589}'..='\u{258F}' => {
            let n = 8 - (c as i32 - 0x2588);
            canvas.rect(0, 0, eighth_w(n), h, 255);
        }
        '\u{2590}' => canvas.rect(hw, 0, w, h, 255),
        '\u{2591}' => canvas.rect(0, 0, w, h, 64),
        '\u{2592}' => canvas.rect(0, 0, w, h, 128),
        '\u{2593}' => canvas.rect(0, 0, w, h, 192),
        '\u{2594}' => canvas.rect(0, 0, w, eighth_h(1), 255),
        '\u{2595}' => canvas.rect(w - eighth_w(1), 0, w, h, 255),
        _ => {
            // Quadrants, as upper-left, upper-right, lower-left, lower-right.
            let quadrants: [bool; 4] = match c {
                '\u{2596}' => [false, false, true, false],
                '\u{2597}' => [false, false, false, true],
                '\u{2598}' => [true, false, false, false],
                '\u{2599}' => [true, false, true, true],
                '\u{259A}' => [true, false, false, true],
                '\u{259B}' => [true, true, true, false],
                '\u{259C}' => [true, true, false, true],
                '\u{259D}' => [false, true, false, false],
                '\u{259E}' => [false, true, true, false],
                _ => [false, true, true, true],
            };
            let rects = [(0, 0, hw, hh), (hw, 0, w, hh), (0, hh, hw, h), (hw, hh, w, h)];
            for (on, (x0, y0, x1, y1)) in quadrants.iter().zip(rects) {
                if *on {
                    canvas.rect(x0, y0, x1, y1, 255);
                }
            }
        }
    }
}

fn powerline(canvas: &mut Canvas, c: char, t: i32) {
    let (w, h) = (canvas.width as f32, canvas.height as f32);
    match c {
        '\u{E0B0}' | '\u{E0B2}' => {
            let right = c == '\u{E0B0}';
            canvas.shade(|x, y| {
                let reach = w * (1.0 - ((y - h / 2.0).abs() / (h / 2.0)));
                let edge = if right { reach - x } else { x - (w - reach) };
                edge + 0.5
            });
        }
        _ => {
            let (tip, base) = if c == '\u{E0B1}' { (w - 1.0, 0.0) } else { (1.0, w) };
            strokes_along(canvas, &[((base, 0.0), (tip, h / 2.0)), ((tip, h / 2.0), (base, h))], t);
        }
    }
}

/// Draws `c` into an alpha mask of `width` by `height` pixels, with lines `stroke` pixels thick.
/// Returns `None` for characters that aren't drawn here.
pub fn render(c: char, width: u32, height: u32, stroke: u32) -> Option<Vec<u8>> {
    if !is_procedural(c) || width == 0 || height == 0 {
        return None;
    }
    let light = stroke.max(1) as i32;
    let strokes = Strokes { light, heavy: (light * 2).max(light + 1), gap: light };
    let mut canvas = Canvas::new(width, height);
    if let Some(arms) = arms(c) {
        lines(&mut canvas, arms, strokes);
        return Some(canvas.data);
    }
    let heavy = strokes.heavy;
    match c {
        '\u{2504}' => dashes(&mut canvas, false, 3, light),
        '\u{2505}' => dashes(&mut canvas, false, 3, heavy),
        '\u{2506}' => dashes(&mut canvas, true, 3, light),
        '\u{2507}' => dashes(&mut canvas, true, 3, heavy),
        '\u{2508}' => dashes(&mut canvas, false, 4, light),
        '\u{2509}' => dashes(&mut canvas, false, 4, heavy),
        '\u{250A}' => dashes(&mut canvas, true, 4, light),
        '\u{250B}' => dashes(&mut canvas, true, 4, heavy),
        '\u{254C}' => dashes(&mut canvas, false, 2, light),
        '\u{254D}' => dashes(&mut canvas, false, 2, heavy),
        '\u{254E}' => dashes(&mut canvas, true, 2, light),
        '\u{254F}' => dashes(&mut canvas, true, 2, heavy),
        '\u{256D}'..='\u{2570}' => arc(&mut canvas, c, light),
        '\u{2571}'..='\u{2573}' => {
            let (w, h) = (width as f32, height as f32);
            let rising = ((0.0, h), (w, 0.0));
            let falling = ((0.0, 0.0), (w, h));
            let segments: &[_] = match c {
                '\u{2571}' => &[rising],
                '\u{2572}' => &[falling],
                _ => &[rising, falling],
            };
            strokes_along(&mut canvas, segments, light);
        }
        '\u{2580}'..='\u{259F}' => blocks(&mut canvas, c),
        _ => powerline(&mut canvas, c, light),
    }
    Some(canvas.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 10;
    const H: u32 = 20;

    fn mask(c: char) -> Vec<u8> {
        render(c, W, H, 1).expect("drawn procedurally")
    }

    fn at(mask: &[u8], x: u32, y: u32) -> u8 {
        mask[(y * W + x) as usize]
    }

    #[test]
    fn coverage_of_the_block() {
        for c in '\u{2500}'..='\u{259F}' {
            let m = mask(c);
            assert_eq!(m.len(), (W * H) as usize);
            assert!(m.iter().any(|&a| a > 0), "{c:?} ({:X}) draws something", c as u32);
        }
        for c in '\u{E0B0}'..='\u{E0B3}' {
            assert!(mask(c).iter().any(|&a| a > 0));
        }
        assert_eq!(render('a', W, H, 1), None);
        assert_eq!(render('\u{2500}', 0, H, 1), None);
        assert!(is_procedural('█') && !is_procedural('A'));
    }

    #[test]
    fn lines_reach_the_edges_to_join_neighbors() {
        let h = mask('─');
        assert_eq!(at(&h, 0, H / 2), 255);
        assert_eq!(at(&h, W - 1, H / 2), 255);
        assert_eq!(at(&h, W / 2, 0), 0);
        let v = mask('│');
        assert_eq!(at(&v, W / 2, 0), 255);
        assert_eq!(at(&v, W / 2, H - 1), 255);
        let corner = mask('┌');
        assert_eq!(at(&corner, W - 1, H / 2), 255);
        assert_eq!(at(&corner, W / 2, H - 1), 255);
        assert_eq!(at(&corner, 0, H / 2), 0);
        assert_eq!(at(&corner, W / 2, 0), 0);
        let cross = mask('┼');
        for (x, y) in [(0, H / 2), (W - 1, H / 2), (W / 2, 0), (W / 2, H - 1)] {
            assert_eq!(at(&cross, x, y), 255);
        }
    }

    #[test]
    fn heavy_and_double_lines() {
        let light = mask('─').iter().filter(|&&a| a == 255).count();
        let heavy = mask('━').iter().filter(|&&a| a == 255).count();
        assert!(heavy >= light * 2);
        let double = mask('═');
        // Two lines with a gap between them.
        assert_eq!(at(&double, 0, H / 2 - 1), 255);
        assert_eq!(at(&double, 0, H / 2), 0);
        assert_eq!(at(&double, 0, H / 2 + 1), 255);
        let corner = mask('╔');
        assert_eq!(at(&corner, W / 2 - 1, H - 1), 255);
        assert_eq!(at(&corner, W / 2 + 1, H - 1), 255);
        assert_eq!(at(&corner, W - 1, H / 2 - 1), 255);
        assert_eq!(at(&corner, 0, H / 2 - 1), 0);
    }

    #[test]
    fn blocks_fill_their_part() {
        let full = mask('█');
        assert!(full.iter().all(|&a| a == 255));
        let lower = mask('▄');
        assert_eq!(at(&lower, 0, 0), 0);
        assert_eq!(at(&lower, 0, H - 1), 255);
        let left = mask('▌');
        assert_eq!(at(&left, 0, 0), 255);
        assert_eq!(at(&left, W - 1, 0), 0);
        let shade = mask('▒');
        assert!(shade.iter().all(|&a| a == 128));
        let quadrant = mask('▗');
        assert_eq!(at(&quadrant, W - 1, H - 1), 255);
        assert_eq!(at(&quadrant, 0, 0), 0);
    }

    #[test]
    fn arcs_and_triangles() {
        let arc = mask('╭');
        assert_eq!(at(&arc, W - 1, H / 2), 255);
        assert_eq!(at(&arc, W / 2, H - 1), 255);
        assert_eq!(at(&arc, 0, 0), 0);
        let triangle = mask('\u{E0B0}');
        assert_eq!(at(&triangle, 0, H / 2), 255);
        assert_eq!(at(&triangle, W - 1, 0), 0);
    }
}
