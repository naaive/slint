// SPDX-License-Identifier: MIT

//! The lock screen's backdrop: the wallpaper, shrunk and blurred, decoded off the UI thread.
//!
//! Drawing a tiny image stretched across the output with smooth scaling looks like a strong blur,
//! and costs the software renderer far less than a real one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// The width the wallpaper is shrunk to.
const WIDTH: u32 = 96;
/// Wallpapers larger than this aren't decoded.
const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;

type Slot = Arc<Mutex<Option<(PathBuf, Option<SharedPixelBuffer<Rgba8Pixel>>)>>>;

#[derive(Default)]
pub struct Backdrop {
    requested: Option<PathBuf>,
    applied: Option<PathBuf>,
    pending: bool,
    ready: Slot,
    /// Counts requests, so that a slow decode of an older wallpaper doesn't overwrite a newer one.
    generation: Arc<AtomicU64>,
}

impl Backdrop {
    /// Starts decoding `wallpaper` in the background if it changed.
    pub fn request(&mut self, wallpaper: Option<&Path>) {
        if self.requested.as_deref() == wallpaper {
            return;
        }
        self.requested = wallpaper.map(Path::to_path_buf);
        let current = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.pending = false;
        let Some(path) = self.requested.clone() else {
            return;
        };
        let slot = self.ready.clone();
        let generation = self.generation.clone();
        let spawned =
            std::thread::Builder::new().name("nimbus-shell-backdrop".into()).spawn(move || {
                let pixels = decode(&path);
                let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
                if generation.load(Ordering::SeqCst) == current {
                    *slot = Some((path, pixels));
                }
            });
        match spawned {
            Ok(_) => self.pending = true,
            Err(err) => tracing::warn!("Can't decode the wallpaper in the background: {err}"),
        }
    }

    /// Whether a decode is running whose result [`Backdrop::take_update`] hasn't returned yet.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    #[cfg(test)]
    pub fn is_decoded(&self) -> bool {
        self.ready.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Returns the new backdrop once it's decoded: `Some(None)` means no wallpaper, so the default gradient.
    pub fn take_update(&mut self) -> Option<Option<slint::Image>> {
        if self.requested.is_none() {
            return self.applied.take().map(|_| None);
        }
        let mut slot = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        let (path, pixels) = slot.take()?;
        if Some(&path) != self.requested.as_ref() {
            return None;
        }
        self.applied = Some(path);
        self.pending = false;
        Some(pixels.map(slint::Image::from_rgba8))
    }
}

fn decode(path: &Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_FILE_SIZE {
        tracing::warn!(path = %path.display(), "The wallpaper is too large to show on the lock screen");
        return None;
    }
    let image = match image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(reader) => match reader.decode() {
            Ok(image) => image,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "Can't decode the wallpaper");
                return None;
            }
        },
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "Can't open the wallpaper");
            return None;
        }
    };
    let height = (u64::from(WIDTH) * u64::from(image.height()) / u64::from(image.width().max(1)))
        .clamp(1, 512) as u32;
    let small = image.resize_exact(WIDTH, height, image::imageops::FilterType::Triangle);
    let blurred = image::imageops::blur(&small.to_rgba8(), 2.5);
    Some(SharedPixelBuffer::clone_from_slice(blurred.as_raw(), blurred.width(), blurred.height()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn decodes_and_shrinks_a_wallpaper() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("wall.png");
        let image =
            image::RgbImage::from_fn(400, 200, |x, _| image::Rgb([(x % 256) as u8, 40, 90]));
        image.save(&path).expect("fixture saves");
        let pixels = decode(&path).expect("decodes");
        assert_eq!((pixels.width(), pixels.height()), (WIDTH, WIDTH / 2));

        let mut backdrop = Backdrop::default();
        backdrop.request(Some(&path));
        let deadline = Instant::now() + Duration::from_secs(5);
        let update = loop {
            if let Some(update) = backdrop.take_update() {
                break update;
            }
            assert!(Instant::now() < deadline, "the backdrop never arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(update.is_some_and(|image| image.size().width == WIDTH));
        assert!(backdrop.take_update().is_none(), "an update arrives once");
        assert!(!backdrop.is_pending());
        backdrop.request(None);
        assert!(matches!(backdrop.take_update(), Some(None)));
    }

    #[test]
    fn bad_files_fall_back() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("wall.jpg");
        std::fs::write(&path, b"not an image").expect("fixture writes");
        assert!(decode(&path).is_none());
        assert!(decode(&dir.path().join("missing.png")).is_none());
    }
}
