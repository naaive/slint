// SPDX-License-Identifier: MIT

//! Software rendering of the visible grid into a pixel buffer.
//!
//! Rendering has two steps so the terminal stays locked only briefly:
//! [`Renderer::snapshot`] copies the rows that may have changed, using `alacritty_terminal`'s damage tracking,
//! and [`Renderer::draw`] rasterizes those whose content differs from the last frame.

use std::hash::{Hash, Hasher};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::{Term, TermDamage};
use alacritty_terminal::vte::ansi::{CursorShape, Rgb};
use slint::{Rgb8Pixel, SharedPixelBuffer};

use crate::glyphs::{CellMetrics, GlyphCache, GlyphPixels, GlyphStyle, RasterGlyph};
use crate::palette::{ColorTable, Scheme, blend};

/// The background of search matches, blended over the cell.
const MATCH_COLOR: Rgb = Rgb { r: 0xf5, g: 0xc0, b: 0x4a };
const MATCH_ALPHA: f32 = 0.4;
/// The focused search match, drawn opaque with dark text.
const FOCUSED_MATCH_COLOR: Rgb = Rgb { r: 0xff, g: 0x9f, b: 0x1c };
const FOCUSED_MATCH_TEXT: Rgb = Rgb { r: 0x1c, g: 0x1c, b: 0x21 };

/// How to present the terminal, beyond its own state.
pub struct ViewOptions<'a> {
    pub scheme: &'a Scheme,
    pub bold_is_bright: bool,
    pub focused: bool,
    /// The visible phase of a blinking cursor.
    pub blink_on: bool,
    pub matches: &'a [Match],
    pub focused_match: Option<&'a Match>,
}

mod attr {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 3;
    pub const UNDERCURL: u16 = 1 << 4;
    pub const DOTTED_UNDERLINE: u16 = 1 << 5;
    pub const DASHED_UNDERLINE: u16 = 1 << 6;
    pub const STRIKEOUT: u16 = 1 << 7;
    pub const WIDE: u16 = 1 << 8;
    pub const SPACER: u16 = 1 << 9;
}

const FLAG_ATTRS: [(Flags, u16); 10] = [
    (Flags::BOLD, attr::BOLD),
    (Flags::ITALIC, attr::ITALIC),
    (Flags::UNDERLINE, attr::UNDERLINE),
    (Flags::DOUBLE_UNDERLINE, attr::DOUBLE_UNDERLINE),
    (Flags::UNDERCURL, attr::UNDERCURL),
    (Flags::DOTTED_UNDERLINE, attr::DOTTED_UNDERLINE),
    (Flags::DASHED_UNDERLINE, attr::DASHED_UNDERLINE),
    (Flags::STRIKEOUT, attr::STRIKEOUT),
    (Flags::WIDE_CHAR, attr::WIDE),
    (Flags::WIDE_CHAR_SPACER, attr::SPACER),
];

#[derive(Clone, Debug, PartialEq)]
struct CellSnapshot {
    c: char,
    zerowidth: Option<Box<[char]>>,
    fg: Rgb,
    bg: Rgb,
    decoration: Rgb,
    attrs: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorOverlay {
    Beam,
    Underline,
    Hollow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CursorSnapshot {
    column: usize,
    overlay: CursorOverlay,
    color: Rgb,
    wide: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct RowSnapshot {
    cells: Vec<CellSnapshot>,
    cursor: Option<CursorSnapshot>,
}

/// Rows copied from the terminal, ready to draw.
pub struct Frame {
    rows: Vec<(usize, RowSnapshot)>,
    background: Rgb,
}

impl Frame {
    /// The terminal's default background, for the area around the grid.
    pub fn background(&self) -> Rgb {
        self.background
    }

    /// Whether the frame has any rows to draw.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Everything besides the terminal's damage that changes how cells look.
#[derive(Clone, Copy, PartialEq, Eq)]
struct OverlayKey {
    selection: Option<SelectionRange>,
    matches: u64,
    focused: bool,
    colors: u64,
}

/// A pixel buffer and the rows drawn into it.
struct Surface {
    pixels: SharedPixelBuffer<Rgb8Pixel>,
    rows: Vec<Option<RowSnapshot>>,
}

impl Surface {
    fn new(width: u32, height: u32, lines: usize) -> Self {
        Self { pixels: SharedPixelBuffer::new(width, height), rows: vec![None; lines] }
    }
}

/// Renders a terminal's visible grid with a glyph cache.
///
/// It draws into two surfaces in turn,
/// because the window keeps showing the last image, and writing to a shared buffer copies it whole.
pub struct Renderer {
    glyphs: GlyphCache,
    columns: usize,
    lines: usize,
    front: Surface,
    back: Surface,
    /// The lines drawn into `front` that `back` doesn't have yet.
    stale: Vec<usize>,
    overlay: Option<OverlayKey>,
    cursor_line: Option<usize>,
    blink_on: bool,
}

fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn match_key(m: &Match) -> (i32, usize, i32, usize) {
    (m.start().line.0, m.start().column.0, m.end().line.0, m.end().column.0)
}

impl Renderer {
    pub fn new(glyphs: GlyphCache) -> Self {
        Self {
            glyphs,
            columns: 0,
            lines: 0,
            front: Surface::new(1, 1, 0),
            back: Surface::new(1, 1, 0),
            stale: Vec::new(),
            overlay: None,
            cursor_line: None,
            blink_on: true,
        }
    }

    pub fn metrics(&self) -> CellMetrics {
        self.glyphs.metrics()
    }

    pub fn glyphs(&self) -> &GlyphCache {
        &self.glyphs
    }

    /// Switches to another font or size; everything is redrawn.
    pub fn set_glyphs(&mut self, glyphs: GlyphCache) {
        self.glyphs = glyphs;
        self.columns = 0;
        self.lines = 0;
        self.invalidate();
    }

    /// Forgets the last frame, so the next one draws every row, such as after switching tabs.
    pub fn invalidate(&mut self) {
        for surface in [&mut self.front, &mut self.back] {
            surface.rows.iter_mut().for_each(|row| *row = None);
        }
        self.stale.clear();
        self.overlay = None;
    }

    /// The rendered image.
    pub fn image(&self) -> SharedPixelBuffer<Rgb8Pixel> {
        self.front.pixels.clone()
    }

    /// Copies the rows of `term` that may have changed, and clears its damage.
    pub fn snapshot<T: EventListener>(&mut self, term: &mut Term<T>, view: &ViewOptions) -> Frame {
        let (columns, lines) = (term.columns(), term.screen_lines());
        let metrics = self.glyphs.metrics();
        if columns != self.columns || lines != self.lines {
            self.columns = columns;
            self.lines = lines;
            let width = (columns as u32 * metrics.width).max(1);
            let height = (lines as u32 * metrics.height).max(1);
            self.front = Surface::new(width, height, lines);
            self.back = Surface::new(width, height, lines);
            self.stale.clear();
            self.overlay = None;
        }

        let mut table = ColorTable::new(view.scheme, term.colors());
        table.bold_is_bright = view.bold_is_bright;
        let overlay = OverlayKey {
            selection: term.selection.as_ref().and_then(|s| s.to_range(term)),
            matches: hash_of(
                view.matches.iter().chain(view.focused_match).map(match_key).collect::<Vec<_>>(),
            ),
            focused: view.focused,
            colors: hash_of(
                (0..alacritty_terminal::term::color::COUNT)
                    .map(|i| {
                        let c = table.get(i);
                        (c.r, c.g, c.b)
                    })
                    .collect::<Vec<_>>(),
            ),
        };

        let mut dirty = vec![self.overlay != Some(overlay); lines];
        self.overlay = Some(overlay);
        match term.damage() {
            TermDamage::Full => dirty.iter_mut().for_each(|d| *d = true),
            TermDamage::Partial(damaged) => {
                for line in damaged {
                    if let Some(d) = dirty.get_mut(line.line) {
                        *d = true;
                    }
                }
            }
        }
        term.reset_damage();

        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let selection = content.selection;
        let cursor = content.cursor;
        let cursor_line = usize::try_from(cursor.point.line.0 + display_offset as i32)
            .ok()
            .filter(|l| *l < lines);
        if self.blink_on != view.blink_on || self.cursor_line != cursor_line {
            for line in [self.cursor_line, cursor_line].into_iter().flatten() {
                if let Some(d) = dirty.get_mut(line) {
                    *d = true;
                }
            }
        }
        self.cursor_line = cursor_line;
        self.blink_on = view.blink_on;
        for (d, row) in dirty.iter_mut().zip(&self.front.rows) {
            *d |= row.is_none();
        }

        let shape = match cursor.shape {
            CursorShape::Hidden => None,
            _ if !view.focused => Some(CursorShape::HollowBlock),
            _ if term.cursor_style().blinking && !view.blink_on => None,
            shape => Some(shape),
        };

        // Search matches per viewport line, as inclusive column ranges.
        let mut match_ranges: Vec<Vec<(usize, usize, bool)>> = vec![Vec::new(); lines];
        let focused_match = view.focused_match;
        let all = view.matches.iter().map(|m| (m, false)).chain(focused_match.map(|m| (m, true)));
        for (m, focused) in all {
            for grid_line in m.start().line.0..=m.end().line.0 {
                let viewport = usize::try_from(grid_line + display_offset as i32).ok();
                let Some(ranges) = viewport.and_then(|l| match_ranges.get_mut(l)) else {
                    continue;
                };
                let start = if grid_line == m.start().line.0 { m.start().column.0 } else { 0 };
                let end = if grid_line == m.end().line.0 {
                    m.end().column.0
                } else {
                    columns.saturating_sub(1)
                };
                ranges.push((start, end, focused));
            }
        }

        let scheme = view.scheme;
        let mut rows = Vec::new();
        for viewport_line in (0..lines).filter(|l| dirty[*l]) {
            let line = Line(viewport_line as i32 - display_offset as i32);
            let grid_row = &term.grid()[line];
            let mut cells = Vec::with_capacity(columns);
            for column in 0..columns {
                let cell = &grid_row[Column(column)];
                let flags = cell.flags;
                let (mut fg, mut bg) = table.cell_colors(cell.fg, cell.bg, flags);
                if selection.is_some_and(|s| s.contains(Point::new(line, Column(column)))) {
                    bg = blend(bg, scheme.selection, scheme.selection_alpha);
                }
                let hit = match_ranges[viewport_line]
                    .iter()
                    .filter(|(start, end, _)| (*start..=*end).contains(&column))
                    .map(|range| range.2)
                    .max();
                match hit {
                    Some(true) => {
                        bg = FOCUSED_MATCH_COLOR;
                        fg = FOCUSED_MATCH_TEXT;
                    }
                    Some(false) => bg = blend(bg, MATCH_COLOR, MATCH_ALPHA),
                    None => {}
                }
                let on_cursor = cursor.point.line == line
                    && (cursor.point.column.0 == column
                        || (cursor.point.column.0 + 1 == column
                            && flags.contains(Flags::WIDE_CHAR_SPACER)));
                if on_cursor && shape == Some(CursorShape::Block) {
                    fg = bg;
                    bg = table.cursor();
                    if fg == bg {
                        fg = table.background();
                    }
                }
                let decoration = table.underline_color(cell.underline_color(), fg, flags);
                let attrs = FLAG_ATTRS
                    .iter()
                    .filter(|(flag, _)| flags.contains(*flag))
                    .fold(0, |attrs, (_, bit)| attrs | bit);
                let hidden = flags.contains(Flags::HIDDEN);
                cells.push(CellSnapshot {
                    c: if hidden { ' ' } else { cell.c },
                    zerowidth: if hidden { None } else { cell.zerowidth().map(Box::from) },
                    fg,
                    bg,
                    decoration,
                    attrs,
                });
            }
            let overlay = match shape {
                Some(CursorShape::Beam) => Some(CursorOverlay::Beam),
                Some(CursorShape::Underline) => Some(CursorOverlay::Underline),
                Some(CursorShape::HollowBlock) => Some(CursorOverlay::Hollow),
                _ => None,
            };
            let cursor = overlay.filter(|_| cursor.point.line == line).map(|overlay| {
                let column = cursor.point.column.0;
                let wide = cells.get(column).is_some_and(|c| c.attrs & attr::WIDE != 0);
                CursorSnapshot { column, overlay, color: table.cursor(), wide }
            });
            rows.push((viewport_line, RowSnapshot { cells, cursor }));
        }
        Frame { rows, background: table.background() }
    }

    /// Rasterizes the rows of `frame` that differ from the last frame. Returns whether any pixels changed.
    pub fn draw(&mut self, frame: Frame) -> bool {
        let metrics = self.glyphs.metrics();
        let changed: Vec<_> = frame
            .rows
            .into_iter()
            .filter(|(line, row)| {
                !self.front.rows.get(*line).is_some_and(|old| old.as_ref() == Some(row))
            })
            .collect();
        if changed.is_empty() {
            return false;
        }

        let width = self.back.pixels.width() as usize;
        let height = self.back.pixels.height() as usize;
        let row_len = width * metrics.height as usize;
        let pixels = self.back.pixels.make_mut_slice();
        let front = self.front.pixels.as_slice();
        for line in self.stale.drain(..) {
            let start = (line * row_len).min(pixels.len());
            let end = (start + row_len).min(pixels.len());
            pixels[start..end].copy_from_slice(&front[start..end]);
            if let Some(slot) = self.back.rows.get_mut(line) {
                *slot = self.front.rows.get(line).cloned().flatten();
            }
        }

        let mut target = Target { pixels, width, height, clip_top: 0, clip_bottom: 0 };
        for (line, row) in changed {
            draw_row(&mut target, &mut self.glyphs, metrics, line, &row);
            if let Some(slot) = self.back.rows.get_mut(line) {
                *slot = Some(row);
            }
            self.stale.push(line);
        }
        std::mem::swap(&mut self.front, &mut self.back);
        true
    }
}

struct Target<'a> {
    pixels: &'a mut [Rgb8Pixel],
    width: usize,
    height: usize,
    clip_top: usize,
    clip_bottom: usize,
}

fn pixel(c: Rgb) -> Rgb8Pixel {
    Rgb8Pixel { r: c.r, g: c.g, b: c.b }
}

fn mix(dst: u8, src: u8, alpha: u32) -> u8 {
    ((u32::from(src) * alpha + u32::from(dst) * (255 - alpha) + 127) / 255) as u8
}

impl Target<'_> {
    fn fill(&mut self, x: i64, y: i64, w: i64, h: i64, color: Rgb) {
        let x0 = x.clamp(0, self.width as i64) as usize;
        let x1 = (x + w).clamp(0, self.width as i64) as usize;
        let y0 = y.clamp(self.clip_top as i64, self.clip_bottom as i64) as usize;
        let y1 = (y + h).clamp(self.clip_top as i64, self.clip_bottom as i64) as usize;
        if x0 >= x1 {
            return;
        }
        let p = pixel(color);
        for row in y0..y1 {
            self.pixels[row * self.width + x0..row * self.width + x1].fill(p);
        }
    }

    fn blend_at(&mut self, x: i64, y: i64, color: Rgb, alpha: u8) {
        let inside = (0..self.width as i64).contains(&x)
            && (self.clip_top as i64..self.clip_bottom as i64).contains(&y);
        if alpha == 0 || !inside {
            return;
        }
        let p = &mut self.pixels[y as usize * self.width + x as usize];
        if alpha == 255 {
            *p = pixel(color);
        } else {
            let a = u32::from(alpha);
            *p = Rgb8Pixel {
                r: mix(p.r, color.r, a),
                g: mix(p.g, color.g, a),
                b: mix(p.b, color.b, a),
            };
        }
    }

    fn glyph(&mut self, x: i64, y: i64, glyph: &RasterGlyph, color: Rgb) {
        let (gw, gh) = (i64::from(glyph.width), i64::from(glyph.height));
        for gy in 0..gh {
            let py = y + i64::from(glyph.top) + gy;
            if py < self.clip_top as i64 || py >= self.clip_bottom as i64 {
                continue;
            }
            for gx in 0..gw {
                let px = x + i64::from(glyph.left) + gx;
                let index = (gy * gw + gx) as usize;
                match &glyph.pixels {
                    GlyphPixels::Mask(mask) => {
                        if let Some(&a) = mask.get(index) {
                            self.blend_at(px, py, color, a);
                        }
                    }
                    GlyphPixels::Color(rgba) => {
                        if let Some(&[r, g, b, a]) = rgba.get(index) {
                            self.blend_at(px, py, Rgb { r, g, b }, a);
                        }
                    }
                }
            }
        }
    }
}

fn draw_row(
    target: &mut Target,
    glyphs: &mut GlyphCache,
    m: CellMetrics,
    line: usize,
    row: &RowSnapshot,
) {
    let (cw, ch) = (i64::from(m.width), i64::from(m.height));
    let y0 = line as i64 * ch;
    target.clip_top = (y0.max(0) as usize).min(target.height);
    target.clip_bottom = ((y0 + ch).max(0) as usize).min(target.height);

    for (column, cell) in row.cells.iter().enumerate() {
        target.fill(column as i64 * cw, y0, cw, ch, cell.bg);
    }

    for (column, cell) in row.cells.iter().enumerate() {
        if cell.attrs & attr::SPACER != 0 {
            continue;
        }
        let x0 = column as i64 * cw;
        let wide = cell.attrs & attr::WIDE != 0;
        let style = GlyphStyle {
            bold: cell.attrs & attr::BOLD != 0,
            italic: cell.attrs & attr::ITALIC != 0,
        };
        let marks = cell.zerowidth.iter().flat_map(|z| z.iter().copied());
        for c in std::iter::once(cell.c).filter(|c| *c != ' ').chain(marks) {
            if let Some(glyph) = glyphs.glyph(c, style, wide) {
                target.glyph(x0, y0, &glyph, cell.fg);
            }
        }
        draw_decorations(target, m, x0, y0, if wide { cw * 2 } else { cw }, cell);
    }

    if let Some(cursor) = row.cursor {
        let x0 = cursor.column as i64 * cw;
        let w = if cursor.wide { cw * 2 } else { cw };
        let stroke = i64::from(m.stroke.max(1));
        match cursor.overlay {
            CursorOverlay::Beam => {
                target.fill(x0, y0, ((cw as f32 * 0.14).round() as i64).max(1), ch, cursor.color);
            }
            CursorOverlay::Underline => {
                let h = ((ch as f32 * 0.1).round() as i64).max(1);
                target.fill(x0, y0 + ch - h, w, h, cursor.color);
            }
            CursorOverlay::Hollow => {
                target.fill(x0, y0, w, stroke, cursor.color);
                target.fill(x0, y0 + ch - stroke, w, stroke, cursor.color);
                target.fill(x0, y0, stroke, ch, cursor.color);
                target.fill(x0 + w - stroke, y0, stroke, ch, cursor.color);
            }
        }
    }
}

fn draw_decorations(
    target: &mut Target,
    m: CellMetrics,
    x0: i64,
    y0: i64,
    w: i64,
    cell: &CellSnapshot,
) {
    let stroke = i64::from(m.stroke.max(1));
    let bottom = y0 + i64::from(m.height);
    let underline = y0 + i64::from(m.underline_top);
    let color = cell.decoration;
    let a = cell.attrs;
    if a & (attr::UNDERLINE | attr::DOUBLE_UNDERLINE) != 0 {
        target.fill(x0, underline, w, stroke, color);
    }
    if a & attr::DOUBLE_UNDERLINE != 0 {
        let below = underline + 2 * stroke;
        let second = if below + stroke <= bottom { below } else { underline - 2 * stroke };
        target.fill(x0, second, w, stroke, color);
    }
    if a & attr::UNDERCURL != 0 {
        let amplitude = stroke as f32;
        let period = i64::from(m.width).max(4) as f32;
        let base = (underline + stroke).min(bottom - stroke - stroke);
        for dx in 0..w {
            let phase = (x0 + dx) as f32 / period * std::f32::consts::TAU;
            target.fill(x0 + dx, base + (phase.sin() * amplitude).round() as i64, 1, stroke, color);
        }
    }
    if a & attr::DOTTED_UNDERLINE != 0 {
        for dx in (0..w).filter(|dx| ((x0 + dx) / stroke) % 2 == 0) {
            target.fill(x0 + dx, underline, 1, stroke, color);
        }
    }
    if a & attr::DASHED_UNDERLINE != 0 {
        let dash = (i64::from(m.width) / 2).max(2);
        for dx in (0..w).filter(|dx| ((x0 + dx) / dash) % 2 == 0) {
            target.fill(x0 + dx, underline, 1, stroke, color);
        }
    }
    if a & attr::STRIKEOUT != 0 {
        target.fill(x0, y0 + i64::from(m.strikeout_top), w, stroke, color);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;
    use crate::engine::{GridSize, feed, test_term};
    use crate::fonts::FontSet;
    use crate::palette::resolve_scheme;

    fn renderer() -> Option<Renderer> {
        let fonts = FontSet::discover("").ok()?;
        Some(Renderer::new(GlyphCache::new(Arc::new(fonts), 16.0)))
    }

    fn view(scheme: &Scheme) -> ViewOptions<'_> {
        ViewOptions {
            scheme,
            bold_is_bright: true,
            focused: true,
            blink_on: true,
            matches: &[],
            focused_match: None,
        }
    }

    fn at(r: &Renderer, x: u32, y: u32) -> Rgb {
        let p = r.front.pixels.as_slice()[(y * r.front.pixels.width() + x) as usize];
        Rgb { r: p.r, g: p.g, b: p.b }
    }

    fn tuple(c: Rgb) -> (u8, u8, u8) {
        (c.r, c.g, c.b)
    }

    /// The colors used in the cell at `(column, line)`.
    fn cell_colors(r: &Renderer, column: u32, line: u32) -> HashSet<(u8, u8, u8)> {
        let m = r.metrics();
        let mut colors = HashSet::new();
        for y in line * m.height..(line + 1) * m.height {
            for x in column * m.width..(column + 1) * m.width {
                colors.insert(tuple(at(r, x, y)));
            }
        }
        colors
    }

    fn lines_of(frame: &Frame) -> Vec<usize> {
        frame.rows.iter().map(|(l, _)| *l).collect()
    }

    #[test]
    fn renders_text_colors_and_only_damaged_rows() {
        let Some(mut r) = renderer() else { return };
        let scheme = resolve_scheme("nimbus-dark", true);
        let mut term = test_term(20, 4);
        feed(&mut term, b"\x1b[?25lab\x1b[41m  \x1b[0m\r\n\x1b[38;2;10;200;30mX\x1b[0m");
        let frame = r.snapshot(&mut term, &view(scheme));
        assert_eq!(frame.rows.len(), 4, "the first frame draws every row");
        assert_eq!(frame.background(), scheme.background);
        assert!(r.draw(frame));
        let m = r.metrics();
        assert_eq!((r.image().width(), r.image().height()), (20 * m.width, 4 * m.height));
        assert_eq!(at(&r, 2 * m.width + 1, 1), scheme.ansi[1]);
        assert_eq!(at(&r, 10 * m.width, 3 * m.height), scheme.background);
        let a = cell_colors(&r, 0, 0);
        assert!(a.len() > 2 && a.contains(&tuple(scheme.foreground)));
        assert!(cell_colors(&r, 0, 1).contains(&(10, 200, 30)));

        // Only the cursor line is checked again, and it hasn't changed.
        let frame = r.snapshot(&mut term, &view(scheme));
        assert!(frame.rows.len() <= 1);
        assert!(!r.draw(frame), "an unchanged terminal draws nothing");

        feed(&mut term, b"\x1b[4;1Hz");
        let frame = r.snapshot(&mut term, &view(scheme));
        let lines = lines_of(&frame);
        assert!(lines.contains(&3) && !lines.contains(&0), "{lines:?}");
        assert!(r.draw(frame));
    }

    #[test]
    fn cursor_selection_and_matches() {
        let Some(mut r) = renderer() else { return };
        let scheme = resolve_scheme("nimbus-dark", true);
        let m = r.metrics();
        let mut term = test_term(10, 2);
        feed(&mut term, b"   ");
        let frame = r.snapshot(&mut term, &view(scheme));
        r.draw(frame);
        assert_eq!(at(&r, 3 * m.width + 1, 1), scheme.cursor, "a block cursor after the spaces");

        let mut unfocused = view(scheme);
        unfocused.focused = false;
        let frame = r.snapshot(&mut term, &unfocused);
        r.draw(frame);
        assert_eq!(at(&r, 3 * m.width, m.height / 2), scheme.cursor, "a hollow outline");
        assert_eq!(at(&r, 3 * m.width + m.width / 2, m.height / 2), scheme.background);

        crate::selection::select_all(&mut term);
        let frame = r.snapshot(&mut term, &unfocused);
        r.draw(frame);
        let selected = blend(scheme.background, scheme.selection, scheme.selection_alpha);
        assert_eq!(at(&r, 1, m.height + 1), selected);

        term.selection = None;
        let found: Match = Point::new(Line(0), Column(0))..=Point::new(Line(0), Column(1));
        let other: Match = Point::new(Line(1), Column(5))..=Point::new(Line(1), Column(6));
        let matches = [found.clone(), other];
        let mut searching = view(scheme);
        searching.matches = &matches;
        searching.focused_match = Some(&found);
        let frame = r.snapshot(&mut term, &searching);
        r.draw(frame);
        assert_eq!(at(&r, 1, 1), FOCUSED_MATCH_COLOR);
        assert_eq!(
            at(&r, 5 * m.width + 1, m.height + 1),
            blend(scheme.background, MATCH_COLOR, MATCH_ALPHA)
        );
    }

    #[test]
    fn blinking_redraws_the_cursor_line() {
        let Some(mut r) = renderer() else { return };
        let scheme = resolve_scheme("nimbus-light", false);
        let m = r.metrics();
        let mut term = test_term(10, 3);
        feed(&mut term, b"\x1b[1 q\r\n");
        let frame = r.snapshot(&mut term, &view(scheme));
        r.draw(frame);
        assert_eq!(at(&r, 1, m.height + 1), scheme.cursor);
        let mut off = view(scheme);
        off.blink_on = false;
        let frame = r.snapshot(&mut term, &off);
        assert_eq!(lines_of(&frame), [1]);
        r.draw(frame);
        assert_eq!(at(&r, 1, m.height + 1), scheme.background);
    }

    #[test]
    fn draws_into_two_buffers_in_turn() {
        let Some(mut r) = renderer() else { return };
        let scheme = resolve_scheme("nimbus-dark", true);
        let mut term = test_term(10, 3);
        feed(&mut term, b"\x1b[?25l");
        let frame = r.snapshot(&mut term, &view(scheme));
        assert!(r.draw(frame));
        let mut shown = r.image();
        let mut addresses = vec![shown.as_slice().as_ptr()];
        for text in [&b"\x1b[1;1Ha"[..], b"\x1b[2;1Hb", b"\x1b[3;1Hc", b"\x1b[1;2Hd"] {
            feed(&mut term, text);
            let frame = r.snapshot(&mut term, &view(scheme));
            assert!(r.draw(frame));
            shown = r.image();
            addresses.push(shown.as_slice().as_ptr());
        }
        assert_ne!(addresses[0], addresses[1]);
        for pair in addresses.windows(3) {
            assert_eq!(pair[0], pair[2], "{addresses:?}");
        }

        // Each buffer catches up on the rows drawn into the other.
        for (column, line) in [(0, 0), (0, 1), (0, 2), (1, 0)] {
            assert!(cell_colors(&r, column, line).contains(&tuple(scheme.foreground)));
        }
        let previous = std::mem::replace(&mut r.front, Surface::new(1, 1, 0));
        r.front = std::mem::replace(&mut r.back, previous);
        for (column, line) in [(0, 0), (0, 1), (0, 2)] {
            assert!(cell_colors(&r, column, line).contains(&tuple(scheme.foreground)));
        }
        assert_eq!(cell_colors(&r, 1, 0), HashSet::from([tuple(scheme.background)]));
    }

    #[test]
    fn decorations_and_resizing() {
        let Some(mut r) = renderer() else { return };
        let scheme = resolve_scheme("dracula", true);
        let m = r.metrics();
        let mut term = test_term(6, 2);
        feed(&mut term, b"\x1b[?25l\x1b[4m  \x1b[24;9m  \x1b[0m\x1b[7m \x1b[0m");
        let frame = r.snapshot(&mut term, &view(scheme));
        r.draw(frame);
        assert_eq!(at(&r, 1, m.underline_top), scheme.foreground);
        assert_eq!(at(&r, 2 * m.width + 1, m.strikeout_top), scheme.foreground);
        assert_eq!(at(&r, 4 * m.width + 1, 1), scheme.foreground, "inverse video");

        term.resize(GridSize { columns: 8, lines: 3 });
        let frame = r.snapshot(&mut term, &view(scheme));
        assert_eq!(frame.rows.len(), 3);
        r.draw(frame);
        assert_eq!((r.image().width(), r.image().height()), (8 * m.width, 3 * m.height));
    }
}
