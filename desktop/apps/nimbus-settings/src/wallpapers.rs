// SPDX-License-Identifier: MIT

//! Finding wallpapers on disk and making their thumbnails.
//!
//! Thumbnails are shared with other apps through the freedesktop.org thumbnail cache,
//! `$XDG_CACHE_HOME/thumbnails/x-large`, keyed by the MD5 of the file URI and validated by modification time.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, BufWriter};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use image::RgbaImage;
use md5::{Digest as _, Md5};

/// The edge length of `x-large` thumbnails in the thumbnail specification.
pub const THUMBNAIL_SIZE: u32 = 512;
/// Raster images smaller than this in either dimension are icons or tiles, not wallpapers.
const MIN_DIMENSION: u32 = 400;
const MAX_DEPTH: usize = 6;
const EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "svg", "jxl"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wallpaper {
    pub path: PathBuf,
    pub name: String,
}

/// `~/Pictures` (or the XDG pictures directory), `/usr/share/backgrounds`, and `/usr/share/wallpapers`.
pub fn default_dirs() -> Vec<PathBuf> {
    let pictures = dirs::picture_dir().or_else(|| dirs::home_dir().map(|h| h.join("Pictures")));
    pictures
        .into_iter()
        .chain(["/usr/share/backgrounds".into(), "/usr/share/wallpapers".into()])
        .collect()
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|known| e.eq_ignore_ascii_case(known)))
}

/// For a KDE wallpaper package, `<package>/contents/images/<W>x<H>.<ext>`, the package directory.
fn kde_package(path: &Path) -> Option<&Path> {
    let images = path.parent()?;
    let contents = images.parent()?;
    let is_images = images.file_name()?.to_str()?.starts_with("images");
    (is_images && contents.file_name()? == "contents").then(|| contents.parent()).flatten()
}

/// Whether a KDE package image is in a variant directory such as `images_dark`.
fn is_dark_variant(path: &Path) -> bool {
    path.parent().and_then(Path::file_name).is_some_and(|name| name != "images")
}

/// The pixel area encoded in a KDE package image name such as `1920x1080.jpg`.
fn named_area(path: &Path) -> u64 {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    stem.split_once('x')
        .and_then(|(w, h)| Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?))
        .unwrap_or(0)
}

/// A readable name: the KDE package name, or the file stem with separators as spaces.
pub fn display_name(path: &Path) -> String {
    let source = kde_package(path).unwrap_or(path);
    let stem = if source == path { path.file_stem() } else { source.file_name() };
    let stem = stem.and_then(|s| s.to_str()).unwrap_or_default();
    stem.replace(['-', '_'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Finds wallpapers under `dirs`, recursively, sorted by name.
///
/// Hidden files and directories are skipped, small raster images are dropped,
/// and each KDE wallpaper package contributes only its largest image (light variants first).
/// At most `limit` wallpapers are returned. Blocks on the file system.
pub fn scan(dirs: &[PathBuf], limit: usize) -> Vec<Wallpaper> {
    // Each image once, under its real path rather than a symbolic link to it.
    let mut by_target: HashMap<PathBuf, (PathBuf, bool)> = HashMap::new();
    for dir in dirs {
        let walker =
            walkdir::WalkDir::new(dir).follow_links(true).max_depth(MAX_DEPTH).sort_by_file_name();
        let entries = walker
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(Result::ok);
        for entry in entries {
            let path = entry.path();
            if !entry.file_type().is_file() || !is_image(path) {
                continue;
            }
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let link = entry.path_is_symlink();
            match by_target.get(&canonical) {
                Some((_, false)) => {}
                Some((_, true)) if link => {}
                _ => {
                    by_target.insert(canonical, (path.to_path_buf(), link));
                }
            }
        }
    }

    let mut plain = Vec::new();
    let mut packages: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for (path, _) in by_target.into_values() {
        if let Some(package) = kde_package(&path) {
            let dark = is_dark_variant(&path);
            let better = packages.get(package).is_none_or(|current| {
                let current_dark = is_dark_variant(current);
                (current_dark && !dark)
                    || (current_dark == dark && named_area(&path) > named_area(current))
            });
            if better {
                packages.insert(package.to_path_buf(), path.clone());
            }
            continue;
        }
        let usable = match crate::imaging::raster_dimensions(&path) {
            Some((width, height)) => width >= MIN_DIMENSION && height >= MIN_DIMENSION,
            None => path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")),
        };
        if usable {
            plain.push(path);
        }
    }
    let mut wallpapers: Vec<Wallpaper> = plain
        .into_iter()
        .chain(packages.into_values())
        .map(|path| Wallpaper { name: display_name(&path), path })
        .collect();
    wallpapers.sort_by_cached_key(|w| (w.name.to_lowercase(), w.path.clone()));
    wallpapers.truncate(limit);
    wallpapers
}

/// The `file://` URI of an absolute path, percent-encoding everything but unreserved characters and `/`.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The cache file for `path`'s `x-large` thumbnail under `cache_dir`, usually `$XDG_CACHE_HOME`.
pub fn thumbnail_path(cache_dir: &Path, path: &Path) -> PathBuf {
    let digest = Md5::digest(file_uri(path).as_bytes());
    let name: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    cache_dir.join("thumbnails").join("x-large").join(format!("{name}.png"))
}

fn modified_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Returns a thumbnail of `path` that fits in [`THUMBNAIL_SIZE`], from the cache when it's current.
///
/// New thumbnails are written to the cache under `cache_dir` when it's given; failing to write is not an error.
pub fn thumbnail(path: &Path, cache_dir: Option<&Path>) -> Option<RgbaImage> {
    let mtime = modified_secs(path)?;
    let cached = cache_dir.map(|dir| thumbnail_path(dir, path));
    if let Some(cached) = &cached
        && let Some(image) = read_cached(cached, &file_uri(path), mtime)
    {
        return Some(image);
    }
    let image = crate::imaging::load_fitting(path, THUMBNAIL_SIZE, THUMBNAIL_SIZE)?;
    if let Some(cached) = &cached
        && let Err(error) = write_cached(cached, &image, &file_uri(path), mtime)
    {
        tracing::debug!("cannot cache the thumbnail of {}: {error}", path.display());
    }
    Some(image)
}

fn read_cached(cached: &Path, uri: &str, mtime: u64) -> Option<RgbaImage> {
    let file = std::fs::File::open(cached).ok()?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let text = &reader.info().uncompressed_latin1_text;
    let field = |key: &str| text.iter().find(|c| c.keyword == key).map(|c| c.text.as_str());
    if field("Thumb::URI") != Some(uri) || field("Thumb::MTime") != Some(mtime.to_string().as_str())
    {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let bytes = buffer.get(..frame.buffer_size())?;
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => {
            bytes.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
        }
        _ => return None,
    };
    RgbaImage::from_raw(frame.width, frame.height, rgba)
}

fn write_cached(cached: &Path, image: &RgbaImage, uri: &str, mtime: u64) -> std::io::Result<()> {
    let dir = cached.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = cached.with_extension(format!("{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let result = (|| {
        encoder.add_text_chunk("Thumb::URI".into(), uri.into())?;
        encoder.add_text_chunk("Thumb::MTime".into(), mtime.to_string())?;
        encoder.add_text_chunk("Software".into(), "Nimbus Settings".into())?;
        let mut writer = encoder.write_header()?;
        writer.write_image_data(image.as_raw())?;
        writer.finish()
    })();
    match result {
        Ok(()) => std::fs::rename(&tmp, cached),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            Err(std::io::Error::other(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(path: &Path, width: u32, height: u32) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255])).save(path).unwrap();
    }

    #[test]
    fn spec_uri_and_hash() {
        assert_eq!(
            file_uri(Path::new("/home/jens/photos/me.png")),
            "file:///home/jens/photos/me.png"
        );
        assert_eq!(file_uri(Path::new("/a b/ü.png")), "file:///a%20b/%C3%BC.png");
        // The example from the thumbnail specification.
        let path = thumbnail_path(Path::new("/c"), Path::new("/home/jens/photos/me.png"));
        assert_eq!(path, Path::new("/c/thumbnails/x-large/c6ee772d9e49320e97ec29a7eb5b1697.png"));
    }

    #[test]
    fn names() {
        assert_eq!(display_name(Path::new("/x/blue-sky_morning.jpg")), "blue sky morning");
        assert_eq!(
            display_name(Path::new("/usr/share/wallpapers/Next/contents/images/1920x1080.png")),
            "Next"
        );
    }

    #[test]
    fn scanning_filters_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        png(&root.join("a/zebra.png"), 800, 600);
        png(&root.join("a/icon.png"), 64, 64);
        png(&root.join(".hidden/secret.png"), 800, 600);
        png(&root.join("a/.dot.png"), 800, 600);
        std::fs::write(root.join("a/notes.txt"), "x").unwrap();
        std::fs::write(root.join("a/broken.jpg"), "not an image").unwrap();
        std::fs::write(
            root.join("a/drawing.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"/>"#,
        )
        .unwrap();
        png(&root.join("kde/Aurora/contents/images/1280x800.png"), 640, 400);
        png(&root.join("kde/Aurora/contents/images/1920x1080.png"), 640, 400);
        png(&root.join("kde/Aurora/contents/images_dark/3840x2160.png"), 640, 400);
        std::os::unix::fs::symlink(root.join("a/zebra.png"), root.join("a/link.png")).unwrap();

        let found = scan(&[root.to_path_buf(), root.join("a"), root.join("missing")], 100);
        let names: Vec<&str> = found.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Aurora", "drawing", "zebra"]);
        assert!(found[0].path.ends_with("images/1920x1080.png"));
        assert_eq!(scan(&[root.to_path_buf()], 1).len(), 1);
    }

    #[test]
    fn thumbnails_are_cached_per_spec() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("pics/wide.png");
        png(&image, 1600, 900);
        let cache = dir.path().join("cache");
        let thumb = thumbnail(&image, Some(&cache)).unwrap();
        assert_eq!(thumb.dimensions(), (512, 288));
        let cached = thumbnail_path(&cache, &image);
        assert!(cached.is_file());
        assert!(read_cached(&cached, &file_uri(&image), modified_secs(&image).unwrap()).is_some());
        assert!(
            read_cached(&cached, &file_uri(&image), 1).is_none(),
            "a stale thumbnail is ignored"
        );
        assert_eq!(thumbnail(&image, Some(&cache)).unwrap(), thumb);
        assert!(thumbnail(&image, None).is_some());
        assert!(thumbnail(&dir.path().join("missing.png"), Some(&cache)).is_none());
    }
}
