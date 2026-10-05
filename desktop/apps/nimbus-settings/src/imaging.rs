// SPDX-License-Identifier: MIT

//! Decoding raster and SVG images into bounded RGBA buffers, off the UI thread.

use std::path::Path;

use image::RgbaImage;
use resvg::{tiny_skia, usvg};

/// Raster images larger than this in either dimension are refused rather than decoded.
const MAX_SOURCE_DIMENSION: u32 = 16384;

fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

/// Loads `path` scaled down to fit in `max_width` x `max_height`, keeping its aspect ratio.
///
/// SVGs are rendered at the size that fits. Returns `None` for unreadable or undecodable files.
pub fn load_fitting(path: &Path, max_width: u32, max_height: u32) -> Option<RgbaImage> {
    if is_svg(path) {
        return render_svg(&std::fs::read(path).ok()?, max_width, max_height);
    }
    let reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width > MAX_SOURCE_DIMENSION || height > MAX_SOURCE_DIMENSION {
        return None;
    }
    let decoded = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode();
    match decoded {
        Ok(image) if image.width() > max_width || image.height() > max_height => {
            Some(image.thumbnail(max_width, max_height).into_rgba8())
        }
        Ok(image) => Some(image.into_rgba8()),
        Err(error) => {
            tracing::debug!("cannot decode {}: {error}", path.display());
            None
        }
    }
}

/// Renders SVG data to fit in `max_width` x `max_height`.
pub fn render_svg(data: &[u8], max_width: u32, max_height: u32) -> Option<RgbaImage> {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = (max_width as f32 / size.width()).min(max_height as f32 / size.height());
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    // The scale makes both dimensions at most the maximum, so the casts stay in range.
    let width = ((size.width() * scale).round() as u32).max(1);
    let height = ((size.height() * scale).round() as u32).max(1);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let pixels: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    RgbaImage::from_raw(width, height, pixels)
}

/// The dimensions of a raster image from its header, without decoding it; `None` for SVGs and unreadable files.
pub fn raster_dimensions(path: &Path) -> Option<(u32, u32)> {
    if is_svg(path) {
        return None;
    }
    image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#ff0000"/></svg>"##;

    #[test]
    fn svg_fits_the_box() {
        let image = render_svg(SVG.as_bytes(), 64, 64).unwrap();
        assert_eq!(image.dimensions(), (64, 32));
        assert_eq!(image.get_pixel(10, 10).0, [255, 0, 0, 255]);
        assert!(render_svg(b"not svg", 64, 64).is_none());
    }

    #[test]
    fn rasters_shrink_and_bad_files_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        RgbaImage::from_pixel(800, 400, image::Rgba([0, 128, 255, 255])).save(&path).unwrap();
        assert_eq!(raster_dimensions(&path), Some((800, 400)));
        let image = load_fitting(&path, 100, 100).unwrap();
        assert_eq!(image.dimensions(), (100, 50));
        let small = load_fitting(&path, 1000, 1000).unwrap();
        assert_eq!(small.dimensions(), (800, 400));

        let bogus = dir.path().join("bogus.jpg");
        std::fs::write(&bogus, b"definitely not a jpeg").unwrap();
        assert!(load_fitting(&bogus, 100, 100).is_none());
        assert!(load_fitting(&dir.path().join("missing.png"), 100, 100).is_none());

        let svg = dir.path().join("a.svg");
        std::fs::write(&svg, SVG).unwrap();
        assert_eq!(load_fitting(&svg, 50, 50).unwrap().dimensions(), (50, 25));
        assert_eq!(raster_dimensions(&svg), None);
    }
}
