// SPDX-License-Identifier: MIT

//! XCursor theme loading for named cursors (default and `wp_cursor_shape_v1` shapes).

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::input::pointer::CursorIcon;
use smithay::utils::{Physical, Point, Transform};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

const DEFAULT_SIZE: u32 = 24;

#[derive(Clone)]
pub struct CursorFrame {
    pub buffer: MemoryRenderBuffer,
    /// In physical pixels of the buffer.
    pub hotspot: Point<i32, Physical>,
    /// The integer scale the buffer was loaded for.
    pub scale: i32,
    delay_ms: u32,
}

struct CursorFrames {
    frames: Vec<CursorFrame>,
    total_delay_ms: u32,
}

impl CursorFrames {
    fn frame_at(&self, time: Duration) -> Option<&CursorFrame> {
        if self.total_delay_ms == 0 {
            return self.frames.first();
        }
        let mut t = u32::try_from(time.as_millis() % u128::from(self.total_delay_ms)).unwrap_or(0);
        for frame in &self.frames {
            if t < frame.delay_ms {
                return Some(frame);
            }
            t -= frame.delay_ms;
        }
        self.frames.last()
    }
}

/// Loads and caches cursor images from `$XCURSOR_THEME` at `$XCURSOR_SIZE`.
pub struct CursorThemeManager {
    theme: xcursor::CursorTheme,
    size: u32,
    cache: RefCell<HashMap<(CursorIcon, i32), Option<Rc<CursorFrames>>>>,
}

impl CursorThemeManager {
    pub fn from_env() -> Self {
        let name = std::env::var("XCURSOR_THEME")
            .ok()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&s| (8..=256).contains(&s))
            .unwrap_or(DEFAULT_SIZE);
        Self { theme: xcursor::CursorTheme::load(&name), size, cache: RefCell::default() }
    }

    /// The cursor image for `icon` at `scale` and `time`, falling back to the default arrow and then a built-in one.
    pub fn frame(&self, icon: CursorIcon, scale: f64, time: Duration) -> CursorFrame {
        let scale = scale.ceil().clamp(1.0, 8.0) as i32;
        let lookup = |icon: CursorIcon| {
            self.cache
                .borrow_mut()
                .entry((icon, scale))
                .or_insert_with(|| self.load(icon, scale).map(Rc::new))
                .clone()
        };
        lookup(icon)
            .or_else(|| lookup(CursorIcon::Default))
            .and_then(|frames| frames.frame_at(time).cloned())
            .unwrap_or_else(|| fallback_frame(self.size, scale))
    }

    fn load(&self, icon: CursorIcon, scale: i32) -> Option<CursorFrames> {
        let names = std::iter::once(icon.name()).chain(icon.alt_names().iter().copied());
        let path = names
            .chain(if icon == CursorIcon::Default { Some("left_ptr") } else { None })
            .find_map(|name| self.theme.load_icon(name))?;
        let bytes = std::fs::read(&path).ok()?;
        let images = xcursor::parser::parse_xcursor(&bytes)?;
        let target = self.size * u32::try_from(scale).unwrap_or(1);
        let nominal = images.iter().map(|i| i.size).min_by_key(|&s| s.abs_diff(target))?;
        let frames: Vec<CursorFrame> = images
            .into_iter()
            .filter(|i| i.size == nominal)
            .filter_map(|image| {
                let (w, h) = (i32::try_from(image.width).ok()?, i32::try_from(image.height).ok()?);
                if image.pixels_rgba.len() < (image.width * image.height * 4) as usize {
                    return None;
                }
                // XCursor pixels are little-endian premultiplied ARGB words.
                let buffer = MemoryRenderBuffer::from_slice(
                    &image.pixels_rgba,
                    Fourcc::Argb8888,
                    (w, h),
                    scale,
                    Transform::Normal,
                    None,
                );
                Some(CursorFrame {
                    buffer,
                    hotspot: Point::from((
                        i32::try_from(image.xhot).unwrap_or(0),
                        i32::try_from(image.yhot).unwrap_or(0),
                    )),
                    scale,
                    delay_ms: image.delay,
                })
            })
            .collect();
        if frames.is_empty() {
            return None;
        }
        let total_delay_ms = frames.iter().map(|f| f.delay_ms).sum();
        Some(CursorFrames { frames, total_delay_ms })
    }
}

/// A black-outlined white arrow, for systems without any cursor theme.
fn fallback_frame(size: u32, scale: i32) -> CursorFrame {
    let side = i32::try_from(size).unwrap_or(24) * scale;
    let pixels = arrow_pixels(side);
    CursorFrame {
        buffer: MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (side, side),
            scale,
            Transform::Normal,
            None,
        ),
        hotspot: Point::from((scale, scale)),
        scale,
        delay_ms: 0,
    }
}

/// Premultiplied BGRA bytes of an arrow filling the top-left of a `side`×`side` square.
fn arrow_pixels(side: i32) -> Vec<u8> {
    let mut pixels = vec![0u8; usize::try_from(side * side * 4).unwrap_or(0)];
    let height = side * 3 / 4;
    let border = (side / 16).max(1);
    for y in 0..height {
        let width = y * 5 / 8 + 1;
        for x in 0..width.min(side) {
            let edge = x < border || x >= width - border || y >= height - border;
            let value = if edge { 0 } else { 255 };
            let offset = usize::try_from((y * side + x) * 4).unwrap_or(0);
            pixels[offset..offset + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_arrow_has_opaque_and_transparent_pixels() {
        let pixels = arrow_pixels(24);
        assert_eq!(pixels.len(), 24 * 24 * 4);
        assert_eq!(pixels[3], 255, "the tip is opaque");
        assert_eq!(pixels[(23 * 4) + 3], 0, "the top-right corner is transparent");
    }

    #[test]
    fn animation_frames_follow_their_delays() {
        let frame = |delay_ms| CursorFrame {
            buffer: MemoryRenderBuffer::default(),
            hotspot: Point::from((delay_ms as i32, 0)),
            scale: 1,
            delay_ms,
        };
        let frames = CursorFrames { frames: vec![frame(10), frame(30)], total_delay_ms: 40 };
        assert_eq!(frames.frame_at(Duration::from_millis(5)).map(|f| f.hotspot.x), Some(10));
        assert_eq!(frames.frame_at(Duration::from_millis(15)).map(|f| f.hotspot.x), Some(30));
        assert_eq!(frames.frame_at(Duration::from_millis(45)).map(|f| f.hotspot.x), Some(10));
    }

    #[test]
    fn missing_theme_falls_back_to_builtin_arrow() {
        let manager = CursorThemeManager {
            theme: xcursor::CursorTheme::load("nimbus-test-no-such-theme"),
            size: 16,
            cache: RefCell::default(),
        };
        let frame = manager.frame(CursorIcon::Wait, 2.0, Duration::ZERO);
        assert_eq!(frame.scale, 2);
    }
}
