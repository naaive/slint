// SPDX-License-Identifier: MIT

//! Rasterized glyphs and cell metrics for one font size.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Angle, Format, Transform};

use crate::boxdraw;
use crate::fonts::{Face, FontSet};

/// Bounds the cache; terminals rarely show more distinct glyphs than this at once.
const MAX_CACHED_GLYPHS: usize = 8192;

/// The size of a cell and where text decorations go, in physical pixels from the cell's top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    pub baseline: u32,
    pub underline_top: u32,
    pub strikeout_top: u32,
    /// The thickness of underlines, strikeouts, and box drawing lines.
    pub stroke: u32,
}

#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub struct GlyphStyle {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GlyphPixels {
    /// Coverage, to tint with the text color.
    Mask(Vec<u8>),
    /// Straight-alpha RGBA, for emoji.
    Color(Vec<[u8; 4]>),
}

/// A glyph image placed relative to the top-left corner of its cell.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterGlyph {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub pixels: GlyphPixels,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct GlyphKey {
    c: char,
    style: GlyphStyle,
    wide: bool,
}

/// Rasterizes and caches glyphs for a font set at one pixel size.
pub struct GlyphCache {
    fonts: Arc<FontSet>,
    size: f32,
    metrics: CellMetrics,
    context: ScaleContext,
    glyphs: HashMap<GlyphKey, Option<Rc<RasterGlyph>>>,
}

fn compute_metrics(face: &Face, size: f32) -> CellMetrics {
    let font = face.font();
    let metrics = font.metrics(&[]).scale(size);
    let glyph_metrics = font.glyph_metrics(&[]).scale(size);
    let charmap = font.charmap();
    let advance = ['M', '0', ' ']
        .into_iter()
        .map(|c| glyph_metrics.advance_width(charmap.map(c)))
        .find(|a| *a > 0.0)
        .unwrap_or(size * 0.6);
    let width = advance.round().max(1.0) as u32;
    let content = metrics.ascent + metrics.descent;
    let height = (content + metrics.leading.max(0.0)).ceil().max(1.0) as u32;
    let baseline =
        (metrics.ascent + (height as f32 - content) / 2.0).round().clamp(1.0, height as f32) as u32;
    let stroke = metrics.stroke_size.round().max((size / 14.0).round()).max(1.0) as u32;
    let underline =
        (baseline as f32 - metrics.underline_offset).round().max(baseline as f32 + 1.0) as u32;
    let underline_top = underline.min(height.saturating_sub(stroke));
    let strikeout = if metrics.strikeout_offset > 0.0 {
        metrics.strikeout_offset
    } else {
        metrics.x_height / 2.0
    };
    let strikeout_top =
        (baseline as f32 - strikeout).round().clamp(0.0, (height - stroke) as f32) as u32;
    CellMetrics { width, height, baseline, underline_top, strikeout_top, stroke }
}

impl GlyphCache {
    /// Creates a cache for `fonts` at `size` pixels per em.
    pub fn new(fonts: Arc<FontSet>, size: f32) -> Self {
        let size = if size.is_finite() { size.clamp(4.0, 400.0) } else { 16.0 };
        let metrics = compute_metrics(&fonts.regular, size);
        Self { fonts, size, metrics, context: ScaleContext::new(), glyphs: HashMap::new() }
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn fonts(&self) -> &Arc<FontSet> {
        &self.fonts
    }

    /// The face that draws `c` in `style`, and whether bold and italic must be synthesized.
    fn pick_face(&self, c: char, style: GlyphStyle) -> Option<(Face, bool, bool)> {
        let styled = self.fonts.styled(style.bold, style.italic).filter(|f| f.has_char(c));
        let face = styled
            .or_else(|| Some(&self.fonts.regular).filter(|f| f.has_char(c)))
            .or_else(|| self.fonts.fallbacks.iter().find(|f| f.has_char(c)))?;
        Some((face.clone(), style.bold && !face.bold, style.italic && !face.italic))
    }

    /// The glyph for `c`, spanning two cells when `wide`; `None` for blank or unknown characters.
    pub fn glyph(&mut self, c: char, style: GlyphStyle, wide: bool) -> Option<Rc<RasterGlyph>> {
        let key = GlyphKey { c, style, wide };
        if let Some(glyph) = self.glyphs.get(&key) {
            return glyph.clone();
        }
        if self.glyphs.len() >= MAX_CACHED_GLYPHS {
            self.glyphs.clear();
        }
        let glyph = self.rasterize(c, style, wide).map(Rc::new);
        self.glyphs.insert(key, glyph.clone());
        glyph
    }

    fn rasterize(&mut self, c: char, style: GlyphStyle, wide: bool) -> Option<RasterGlyph> {
        let span = self.metrics.width * if wide { 2 } else { 1 };
        if c == ' ' || c.is_control() {
            return None;
        }
        if let Some(mask) = boxdraw::render(c, span, self.metrics.height, self.metrics.stroke) {
            return Some(RasterGlyph {
                left: 0,
                top: 0,
                width: span,
                height: self.metrics.height,
                pixels: GlyphPixels::Mask(mask),
            });
        }
        let (face, synthetic_bold, synthetic_italic) = self.pick_face(c, style)?;
        let glyph = self.render_with(&face, c, self.size, synthetic_bold, synthetic_italic)?;
        let (image, advance) = glyph;
        let mut glyph = self.place(image, advance, span);
        if let GlyphPixels::Color(_) = glyph.pixels {
            let fit = (span as f32 / glyph.width.max(1) as f32)
                .min(self.metrics.height as f32 / glyph.height.max(1) as f32);
            if fit < 1.0 {
                let (image, advance) = self.render_with(&face, c, self.size * fit, false, false)?;
                glyph = self.place(image, advance, span);
            }
            glyph.left = (span as i32 - glyph.width as i32) / 2;
            glyph.top = (self.metrics.height as i32 - glyph.height as i32) / 2;
        }
        Some(glyph)
    }

    fn render_with(
        &mut self,
        face: &Face,
        c: char,
        size: f32,
        bold: bool,
        italic: bool,
    ) -> Option<(swash::scale::image::Image, f32)> {
        let font = face.font();
        let glyph_id = font.charmap().map(c);
        let advance = font.glyph_metrics(&[]).scale(size).advance_width(glyph_id);
        let mut scaler = self.context.builder(font).size(size).hint(true).build();
        let sources =
            [Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline];
        let mut render = Render::new(&sources);
        render.format(Format::Alpha);
        if bold {
            render.embolden((size / 24.0).max(0.5));
        }
        if italic {
            render.transform(Some(Transform::skew(Angle::from_degrees(12.0), Angle::ZERO)));
        }
        let image = render.render(&mut scaler, glyph_id)?;
        Some((image, advance))
    }

    /// Positions a rendered glyph in its cells: on the baseline, centered when its advance differs from the span.
    fn place(&self, image: swash::scale::image::Image, advance: f32, span: u32) -> RasterGlyph {
        let placement = image.placement;
        let centering =
            if advance > 0.0 { ((span as f32 - advance) / 2.0).round() as i32 } else { 0 };
        let pixels = match image.content {
            Content::Color => GlyphPixels::Color(
                image.data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect(),
            ),
            Content::SubpixelMask => GlyphPixels::Mask(
                image.data.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).collect(),
            ),
            Content::Mask => GlyphPixels::Mask(image.data),
        };
        RasterGlyph {
            left: placement.left + centering,
            top: self.metrics.baseline as i32 - placement.top,
            width: placement.width,
            height: placement.height,
            pixels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(size: f32) -> Option<GlyphCache> {
        FontSet::discover("").ok().map(|fonts| GlyphCache::new(Arc::new(fonts), size))
    }

    #[test]
    fn metrics_are_sane_and_scale() {
        let (Some(small), Some(large)) = (cache(14.0), cache(28.0)) else { return };
        let m = small.metrics();
        assert!(m.width >= 6 && m.width <= 12, "{m:?}");
        assert!(m.height >= 14 && m.height <= 24, "{m:?}");
        assert!(m.baseline < m.height && m.underline_top >= m.baseline);
        assert!(m.underline_top + m.stroke <= m.height);
        assert!(m.strikeout_top < m.baseline);
        let l = large.metrics();
        assert!(l.width >= m.width * 2 - 1 && l.height >= m.height * 2 - 2, "{m:?} {l:?}");
        assert_eq!(GlyphCache::new(small.fonts().clone(), f32::NAN).size(), 16.0);
    }

    #[test]
    fn letters_render_inside_their_cell() {
        let Some(mut cache) = cache(16.0) else { return };
        let m = cache.metrics();
        let glyph = cache.glyph('A', GlyphStyle::default(), false).expect("A renders");
        assert!(matches!(glyph.pixels, GlyphPixels::Mask(_)));
        assert!(glyph.width > 0 && glyph.height > 0);
        assert!(glyph.left >= -1 && glyph.left + glyph.width as i32 <= m.width as i32 + 1);
        assert!(glyph.top >= 0 && glyph.top + glyph.height as i32 <= m.baseline as i32 + 1);
        assert!(cache.glyph(' ', GlyphStyle::default(), false).is_none());
        let bold =
            cache.glyph('A', GlyphStyle { bold: true, italic: false }, false).expect("bold A");
        let ink = |g: &RasterGlyph| match &g.pixels {
            GlyphPixels::Mask(m) => m.iter().map(|&a| u32::from(a)).sum::<u32>(),
            GlyphPixels::Color(_) => 0,
        };
        assert!(ink(&bold) > ink(&glyph));
        // Cached glyphs are shared.
        let again = cache.glyph('A', GlyphStyle::default(), false).expect("cached");
        assert!(Rc::ptr_eq(&glyph, &again));
    }

    #[test]
    fn box_drawing_fills_the_span() {
        let Some(mut cache) = cache(16.0) else { return };
        let m = cache.metrics();
        let line = cache.glyph('─', GlyphStyle::default(), false).expect("drawn");
        assert_eq!((line.left, line.top, line.width, line.height), (0, 0, m.width, m.height));
        let wide = cache.glyph('█', GlyphStyle::default(), true).expect("drawn");
        assert_eq!(wide.width, m.width * 2);
    }

    #[test]
    fn wide_glyphs_are_centered() {
        let Some(mut cache) = cache(16.0) else { return };
        let m = cache.metrics();
        if let Some(glyph) = cache.glyph('漢', GlyphStyle::default(), true) {
            let center = glyph.left + glyph.width as i32 / 2;
            assert!((center - m.width as i32).abs() <= 2, "{glyph:?}");
        }
        assert!(cache.glyph('\u{10FFFD}', GlyphStyle::default(), false).is_none());
    }
}
