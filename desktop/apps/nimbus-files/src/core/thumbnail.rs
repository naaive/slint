// SPDX-License-Identifier: MIT

//! Thumbnails stored per the freedesktop.org Thumbnail Managing Standard.
//!
//! Each thumbnail is a PNG named after the MD5 of the file's URI, carrying the URI and the
//! file's modification time in `Thumb::URI` and `Thumb::MTime` text chunks.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest as _, Md5};

use super::uri;

/// The sizes of the standard; the large one keeps thumbnails sharp on high-density screens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThumbnailSize {
    #[default]
    Normal,
    Large,
}

impl ThumbnailSize {
    /// The longest edge in pixels.
    pub fn edge(self) -> u32 {
        match self {
            ThumbnailSize::Normal => 128,
            ThumbnailSize::Large => 256,
        }
    }

    fn dir(self) -> &'static str {
        match self {
            ThumbnailSize::Normal => "normal",
            ThumbnailSize::Large => "large",
        }
    }

    /// The size to use for icons of `logical` pixels on a screen with `scale_factor`.
    pub fn for_display(logical: f32, scale_factor: f32) -> Self {
        if logical * scale_factor > 128.0 { ThumbnailSize::Large } else { ThumbnailSize::Normal }
    }
}
/// Larger files aren't thumbnailed, since decoding them would take too long or too much memory.
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;
/// The subfolder of `fail/` that records files this app couldn't thumbnail.
const FAIL_DIR: &str = "nimbus-files-1.0";

/// An RGBA thumbnail in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("the file is too large to thumbnail")]
    TooLarge,
    #[error("unsupported or corrupt image: {0}")]
    Decode(String),
    #[error("a previous attempt failed")]
    PreviouslyFailed,
}

/// The thumbnail cache, normally `$XDG_CACHE_HOME/thumbnails`.
#[derive(Clone, Debug)]
pub struct ThumbnailCache {
    root: PathBuf,
}

impl ThumbnailCache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The cached thumbnail's path for a file URI.
    pub fn path_for(&self, uri: &str, size: ThumbnailSize) -> PathBuf {
        self.root.join(size.dir()).join(hash_name(uri))
    }

    fn fail_path_for(&self, uri: &str) -> PathBuf {
        self.root.join("fail").join(FAIL_DIR).join(hash_name(uri))
    }

    /// Loads a valid cached thumbnail, or generates, stores, and returns a new one.
    pub fn load_or_generate(
        &self,
        path: &Path,
        mime: &str,
        size: ThumbnailSize,
    ) -> Result<Thumbnail, ThumbnailError> {
        let meta = fs::metadata(path)?;
        let mtime = meta.modified().ok().and_then(unix_seconds).unwrap_or(0);
        let uri = uri::path_to_uri(path);
        let cached = self.path_for(&uri, size);
        if let Some(thumbnail) = read_valid(&cached, mtime) {
            return Ok(thumbnail);
        }
        if read_valid_meta(&self.fail_path_for(&uri), mtime) {
            return Err(ThumbnailError::PreviouslyFailed);
        }
        if meta.len() > MAX_SOURCE_BYTES {
            return Err(ThumbnailError::TooLarge);
        }
        // Files inside the cache are never thumbnailed, as the standard requires.
        if path.starts_with(&self.root) {
            return Err(ThumbnailError::Decode("file is in the thumbnail cache".into()));
        }
        match render(path, mime, size.edge()) {
            Ok(image) => {
                let thumbnail = Thumbnail {
                    width: image.width(),
                    height: image.height(),
                    rgba: image.into_raw(),
                };
                if let Err(error) = write_png(&cached, &thumbnail, &uri, mtime) {
                    tracing::debug!("couldn't store a thumbnail for {}: {error}", path.display());
                }
                Ok(thumbnail)
            }
            Err(error) => {
                let marker = Thumbnail { width: 1, height: 1, rgba: vec![0; 4] };
                let _ = write_png(&self.fail_path_for(&uri), &marker, &uri, mtime);
                Err(error)
            }
        }
    }
}

fn hash_name(uri: &str) -> String {
    let digest = Md5::digest(uri.as_bytes());
    let mut name = String::with_capacity(36);
    for byte in digest {
        name.push_str(&format!("{byte:02x}"));
    }
    name.push_str(".png");
    name
}

fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Scales an image to fit `edge` without enlarging it.
fn render(path: &Path, mime: &str, edge: u32) -> Result<image::RgbaImage, ThumbnailError> {
    if mime == "image/svg+xml" {
        return render_svg(path, edge);
    }
    let mut reader = image::ImageReader::open(path)?
        .with_guessed_format()
        .map_err(|e| ThumbnailError::Decode(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| ThumbnailError::Decode(e.to_string()))?;
    let image = if image.width() > edge || image.height() > edge {
        image.thumbnail(edge, edge)
    } else {
        image
    };
    Ok(image.into_rgba8())
}

fn render_svg(path: &Path, edge: u32) -> Result<image::RgbaImage, ThumbnailError> {
    use resvg::{tiny_skia, usvg};
    let data = fs::read(path)?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default())
        .map_err(|e| ThumbnailError::Decode(e.to_string()))?;
    let size = tree.size();
    let scale = (edge as f32 / size.width().max(size.height()).max(1.0)).min(1.0);
    let width = ((size.width() * scale).round() as u32).clamp(1, edge);
    let height = ((size.height() * scale).round() as u32).clamp(1, edge);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| ThumbnailError::Decode("empty SVG".into()))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia stores premultiplied alpha; PNG and Slint expect straight alpha.
    let mut rgba = pixmap.take();
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| ThumbnailError::Decode("bad SVG size".into()))
}

/// Reads the text chunks of a cached PNG and checks that `Thumb::MTime` matches.
fn read_valid_meta(path: &Path, mtime: u64) -> bool {
    let Ok(file) = File::open(path) else { return false };
    let decoder = png::Decoder::new(BufReader::new(file));
    let Ok(reader) = decoder.read_info() else { return false };
    mtime_matches(reader.info(), mtime)
}

fn mtime_matches(info: &png::Info<'_>, mtime: u64) -> bool {
    let latin1 = info.uncompressed_latin1_text.iter().map(|c| (c.keyword.as_str(), c.text.clone()));
    let utf8 = info.utf8_text.iter().filter_map(|c| Some((c.keyword.as_str(), c.get_text().ok()?)));
    latin1
        .chain(utf8)
        .any(|(key, text)| key == "Thumb::MTime" && text.trim().parse::<u64>().ok() == Some(mtime))
}

fn read_valid(path: &Path, mtime: u64) -> Option<Thumbnail> {
    let file = File::open(path).ok()?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    if !mtime_matches(reader.info(), mtime) {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    buffer.truncate(frame.buffer_size());
    let pixels = frame.width as usize * frame.height as usize;
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::GrayscaleAlpha => {
            buffer.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Rgb => {
            buffer.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        }
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    (rgba.len() == pixels * 4).then_some(Thumbnail {
        width: frame.width,
        height: frame.height,
        rgba,
    })
}

/// Writes a thumbnail atomically with owner-only permissions, as the standard requires.
fn write_png(path: &Path, thumbnail: &Thumbnail, uri: &str, mtime: u64) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut builder = fs::DirBuilder::new();
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.recursive(true).create(dir)?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        std::process::id(),
        path.file_name().and_then(|n| n.to_str()).unwrap_or("thumb")
    ));
    let file =
        fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
    let result = (|| {
        let mut encoder =
            png::Encoder::new(BufWriter::new(file), thumbnail.width, thumbnail.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let to_io = |e: png::EncodingError| io::Error::other(e.to_string());
        encoder.add_text_chunk("Thumb::URI".into(), uri.into()).map_err(to_io)?;
        encoder.add_text_chunk("Thumb::MTime".into(), mtime.to_string()).map_err(to_io)?;
        encoder.add_text_chunk("Software".into(), "Nimbus Files".into()).map_err(to_io)?;
        let mut writer = encoder.write_header().map_err(to_io)?;
        writer.write_image_data(&thumbnail.rgba).map_err(to_io)?;
        writer.finish().map_err(to_io)
    })();
    match result.and_then(|()| fs::rename(&temp, path)) {
        Ok(()) => {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_png(path: &Path, width: u32, height: u32) {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        image.save(path).expect("save png");
    }

    #[test]
    fn hashes_uris_like_other_desktops() {
        // The example from the Thumbnail Managing Standard.
        assert_eq!(
            hash_name("file:///home/jens/photos/me.png"),
            "c6ee772d9e49320e97ec29a7eb5b1697.png"
        );
    }

    #[test]
    fn generates_caches_and_invalidates() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = ThumbnailCache::new(dir.path().join("cache"));
        let source = dir.path().join("photo.png");
        write_test_png(&source, 400, 200);

        let thumb =
            cache.load_or_generate(&source, "image/png", ThumbnailSize::Normal).expect("thumbnail");
        assert_eq!((thumb.width, thumb.height), (128, 64));
        assert_eq!(thumb.rgba.len(), 128 * 64 * 4);

        let cached = cache.path_for(&uri::path_to_uri(&source), ThumbnailSize::Normal);
        assert!(cached.exists());
        let mode = fs::metadata(&cached).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let mtime = fs::metadata(&source)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(unix_seconds)
            .expect("mtime");
        assert!(read_valid_meta(&cached, mtime));
        assert!(!read_valid_meta(&cached, mtime + 1));
        assert_eq!(read_valid(&cached, mtime), Some(thumb.clone()));

        // A small image is stored as is.
        let small = dir.path().join("small.png");
        write_test_png(&small, 20, 10);
        let thumb =
            cache.load_or_generate(&small, "image/png", ThumbnailSize::Normal).expect("thumbnail");
        assert_eq!((thumb.width, thumb.height), (20, 10));

        let large =
            cache.load_or_generate(&source, "image/png", ThumbnailSize::Large).expect("thumbnail");
        assert_eq!((large.width, large.height), (256, 128));
        assert!(cache.path_for(&uri::path_to_uri(&source), ThumbnailSize::Large).exists());
        assert_eq!(ThumbnailSize::for_display(64.0, 1.0), ThumbnailSize::Normal);
        assert_eq!(ThumbnailSize::for_display(64.0, 2.5), ThumbnailSize::Large);
    }

    #[test]
    fn svg_thumbnails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = ThumbnailCache::new(dir.path().join("cache"));
        let source = dir.path().join("shape.svg");
        fs::write(
            &source,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="512" height="256"><rect width="512" height="256" fill="#ff000080"/></svg>"##,
        )
        .expect("write");
        let thumb = cache
            .load_or_generate(&source, "image/svg+xml", ThumbnailSize::Normal)
            .expect("thumbnail");
        assert_eq!((thumb.width, thumb.height), (128, 64));
        // Straight alpha: half-transparent pure red.
        assert_eq!(&thumb.rgba[..4], &[255, 0, 0, 128]);
    }

    #[test]
    fn records_failures() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = ThumbnailCache::new(dir.path().join("cache"));
        let source = dir.path().join("broken.png");
        fs::write(&source, "not a png").expect("write");
        assert!(matches!(
            cache.load_or_generate(&source, "image/png", ThumbnailSize::Normal),
            Err(ThumbnailError::Decode(_))
        ));
        assert!(matches!(
            cache.load_or_generate(&source, "image/png", ThumbnailSize::Normal),
            Err(ThumbnailError::PreviouslyFailed)
        ));
        assert!(matches!(
            cache.load_or_generate(
                &dir.path().join("missing.png"),
                "image/png",
                ThumbnailSize::Normal
            ),
            Err(ThumbnailError::Io(_))
        ));
    }
}
