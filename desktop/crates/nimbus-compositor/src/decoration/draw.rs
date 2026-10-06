// SPDX-License-Identifier: MIT

//! Rasterizing titlebars with tiny-skia and ab_glyph.

use super::frame::{self, Button, Style};
use super::theme::{Palette, Rgba, Theme};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use smithay::utils::{Buffer, Logical, Rectangle, Size};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

/// Everything a titlebar's pixels depend on, besides the theme and the scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub style: Style,
    pub size: Size<i32, Logical>,
    pub title: String,
    pub focused: bool,
    pub maximized: bool,
    pub hovered: Option<Button>,
    /// Round the top corners, as floating windows have them.
    pub rounded: bool,
}

/// Draws a titlebar at `scale` into premultiplied RGBA pixels.
///
/// Returns `None` for an empty titlebar.
pub fn titlebar(look: &Look, scale: f64, theme: &Theme, font: Option<&FontVec>) -> Option<Pixmap> {
    let (width, height) = pixel_size(look, scale);
    let scale = scale as f32;
    let mut pixmap = Pixmap::new(width, height)?;
    let palette = theme.palette();
    let colors = Colors::new(look, &palette);

    let background = if look.rounded {
        rounded_top(width as f32, height as f32, corner_radius(look, scale, theme))
    } else {
        Rect::from_xywh(0.0, 0.0, width as f32, height as f32).map(PathBuilder::from_rect)
    };
    if let Some(path) = background {
        pixmap.fill_path(
            &path,
            &paint(colors.background),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    if look.style == Style::Full
        && let Some(line) =
            Rect::from_xywh(0.0, height as f32 - scale.max(1.0), width as f32, scale.max(1.0))
    {
        pixmap.fill_rect(line, &paint(palette.separator), Transform::identity(), None);
    }

    for (button, rect) in frame::buttons(look.style, look.size) {
        let hovered = look.hovered == Some(button);
        let side = rect.size.w as f32 * scale;
        let cx = rect.loc.x as f32 * scale + side / 2.0;
        let cy = rect.loc.y as f32 * scale + side / 2.0;
        let (circle, icon) = match (button, hovered) {
            (Button::Close, true) => (palette.close_hover, palette.on_close_hover),
            (_, true) => (palette.control_hover, colors.text),
            (_, false) => (palette.control, colors.text),
        };
        if let Some(path) = PathBuilder::from_circle(cx, cy, side / 2.0) {
            pixmap.fill_path(&path, &paint(circle), FillRule::Winding, Transform::identity(), None);
        }
        draw_icon(&mut pixmap, button, look.maximized, (cx, cy), side * 0.18, scale, icon);
    }

    if let Some(font) = font {
        let area = frame::title_area(look.style, look.size);
        let size = match look.style {
            Style::Full => theme.font_size,
            Style::Slim => theme.font_size * 0.9,
        } * scale;
        let left = area.loc.x as f32 * scale;
        let right = left + area.size.w as f32 * scale;
        draw_text(
            &mut pixmap,
            font,
            &look.title,
            size,
            (left, right),
            height as f32 / 2.0,
            colors.text,
        );
    }
    Some(pixmap)
}

/// The colors that depend on the titlebar's style and focus.
struct Colors {
    background: Rgba,
    text: Rgba,
}

impl Colors {
    fn new(look: &Look, palette: &Palette) -> Self {
        match (look.style, look.focused) {
            (Style::Slim, true) => Self { background: palette.accent, text: palette.on_accent },
            (Style::Full, true) => Self { background: palette.background, text: palette.text },
            (_, false) => {
                Self { background: palette.background_inactive, text: palette.text_inactive }
            }
        }
    }
}

fn paint(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.0, color.1, color.2, color.3);
    paint.anti_alias = true;
    paint
}

/// The opaque part of the pixels `titlebar` draws: all but its rounded corners.
pub fn opaque_region(look: &Look, scale: f64, theme: &Theme) -> Vec<Rectangle<i32, Buffer>> {
    let (width, height) = pixel_size(look, scale);
    let (width, height) = (width as i32, height as i32);
    if !look.rounded {
        return vec![Rectangle::from_size((width, height).into())];
    }
    let r = corner_radius(look, scale as f32, theme).ceil() as i32;
    vec![
        Rectangle::new((r, 0).into(), (width - 2 * r, r).into()),
        Rectangle::new((0, r).into(), (width, height - r).into()),
    ]
}

fn pixel_size(look: &Look, scale: f64) -> (u32, u32) {
    let scale = scale as f32;
    ((look.size.w as f32 * scale).ceil() as u32, (look.size.h as f32 * scale).ceil() as u32)
}

fn corner_radius(look: &Look, scale: f32, theme: &Theme) -> f32 {
    let (width, height) = pixel_size(look, scale.into());
    (theme.corner_radius * scale).min(width as f32 / 2.0).min(height as f32).max(0.0)
}

/// A rectangle whose top corners are rounded by `r`.
fn rounded_top(width: f32, height: f32, r: f32) -> Option<tiny_skia::Path> {
    // The control point distance that makes a cubic Bézier curve approximate a quarter circle.
    let k = r * 0.552_284_8;
    let mut path = PathBuilder::new();
    path.move_to(0.0, height);
    path.line_to(0.0, r);
    path.cubic_to(0.0, r - k, r - k, 0.0, r, 0.0);
    path.line_to(width - r, 0.0);
    path.cubic_to(width - r + k, 0.0, width, r - k, width, r);
    path.line_to(width, height);
    path.close();
    path.finish()
}

/// Strokes a button's symbol inside a square of half-width `r` around `center`.
fn draw_icon(
    pixmap: &mut Pixmap,
    button: Button,
    maximized: bool,
    (cx, cy): (f32, f32),
    r: f32,
    scale: f32,
    color: Rgba,
) {
    let mut path = PathBuilder::new();
    match button {
        Button::Minimize => {
            path.move_to(cx - r, cy);
            path.line_to(cx + r, cy);
        }
        Button::Maximize if maximized => {
            // A restore symbol: a window in front of another.
            let o = r * 0.5;
            if let Some(front) = Rect::from_xywh(cx - r, cy - r + o, 2.0 * r - o, 2.0 * r - o) {
                path.push_rect(front);
            }
            path.move_to(cx - r + o, cy - r + o);
            path.line_to(cx - r + o, cy - r);
            path.line_to(cx + r, cy - r);
            path.line_to(cx + r, cy + r - o);
            path.line_to(cx + r - o, cy + r - o);
        }
        Button::Maximize => {
            if let Some(rect) = Rect::from_xywh(cx - r, cy - r, 2.0 * r, 2.0 * r) {
                path.push_rect(rect);
            }
        }
        Button::Close => {
            path.move_to(cx - r, cy - r);
            path.line_to(cx + r, cy + r);
            path.move_to(cx + r, cy - r);
            path.line_to(cx - r, cy + r);
        }
    }
    let Some(path) = path.finish() else {
        return;
    };
    let stroke = Stroke { width: 1.25 * scale, ..Stroke::default() };
    pixmap.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
}

/// Draws `text` vertically centered on `center_y`, centered between `left` and `right`,
/// or shortened with an ellipsis when it doesn't fit.
fn draw_text(
    pixmap: &mut Pixmap,
    font: &FontVec,
    text: &str,
    size: f32,
    (left, right): (f32, f32),
    center_y: f32,
    color: Rgba,
) {
    let scaled = font.as_scaled(PxScale::from(size));
    let available = right - left;
    if available <= 0.0 || text.is_empty() {
        return;
    }
    let line = fit(&scaled, text, available);
    let width = advance(&scaled, &line);
    let x0 = if width < available { left + (available - width) / 2.0 } else { left };
    let baseline = center_y + (scaled.ascent() + scaled.descent()) / 2.0;

    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();
    let mut x = x0;
    let mut previous = None;
    for c in line.chars() {
        let id = scaled.glyph_id(c);
        if let Some(previous) = previous {
            x += scaled.kern(previous, id);
        }
        previous = Some(id);
        let glyph = id.with_scale_and_position(size, point(x, baseline));
        x += scaled.h_advance(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = bounds.min.x as i32 + gx as i32;
            let py = bounds.min.y as i32 + gy as i32;
            if px < 0 || py < 0 || px >= pw || py >= ph || (px as f32) >= right {
                return;
            }
            let offset = ((py * pw + px) * 4) as usize;
            blend(&mut data[offset..offset + 4], color, coverage);
        });
    }
}

/// The longest prefix of `text` that fits `available`, with an ellipsis when it's shortened.
fn fit<F: Font>(font: &impl ScaleFont<F>, text: &str, available: f32) -> String {
    if advance(font, text) <= available {
        return text.to_owned();
    }
    let ellipsis = advance(font, "…");
    text.char_indices()
        .rev()
        .map(|(end, _)| text[..end].trim_end())
        .find(|prefix| advance(font, prefix) + ellipsis <= available)
        .map(|prefix| format!("{prefix}…"))
        .unwrap_or_default()
}

fn advance<F: Font>(font: &impl ScaleFont<F>, text: &str) -> f32 {
    let mut width = 0.0;
    let mut previous = None;
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(previous) = previous {
            width += font.kern(previous, id);
        }
        width += font.h_advance(id);
        previous = Some(id);
    }
    width
}

/// Draws `color` at `coverage` over a premultiplied RGBA pixel.
fn blend(pixel: &mut [u8], color: Rgba, coverage: f32) {
    let alpha = coverage.clamp(0.0, 1.0) * f32::from(color.3) / 255.0;
    let source = [color.0, color.1, color.2].map(|c| f32::from(c) * alpha);
    for (channel, s) in pixel[..3].iter_mut().zip(source) {
        *channel = (s + f32::from(*channel) * (1.0 - alpha)).round().min(255.0) as u8;
    }
    pixel[3] = (alpha * 255.0 + f32::from(pixel[3]) * (1.0 - alpha)).round().min(255.0) as u8;
}
