// SPDX-License-Identifier: MIT

//! Images that notifying clients name by absolute path, read and decoded on a worker thread.
//!
//! Any D-Bus client can send such a path.
//! It may sit on a stalled network mount or hold a huge image,
//! so the UI thread never touches the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Files larger than this aren't read.
const MAX_FILE_SIZE: u64 = 8 * 1024 * 1024;
/// Raster images wider or taller than this aren't decoded.
const MAX_SIDE: u32 = 4096;
/// The most memory one decode may allocate.
const MAX_ALLOC: u64 = 64 * 1024 * 1024;
/// Finished images kept; the cache starts over beyond this.
const MAX_CACHED: usize = 256;

enum Loaded {
    Pixels(SharedPixelBuffer<Rgba8Pixel>),
    Svg(Vec<u8>),
}

struct Request {
    path: PathBuf,
    size: u32,
    generation: u64,
}

type Results = Arc<Mutex<Vec<(u64, PathBuf, Option<Loaded>)>>>;

enum Entry {
    Pending,
    Ready(Option<slint::Image>),
}

pub struct ClientImages {
    /// The edge length images are shrunk to, in physical pixels.
    size: u32,
    /// Counts size changes, so that results decoded for an older size are dropped.
    generation: u64,
    entries: HashMap<PathBuf, Entry>,
    requests: Option<mpsc::Sender<Request>>,
    results: Results,
    /// The directories images must be in, or `None` for the defaults from [`allowed_roots`].
    roots: Option<Vec<PathBuf>>,
}

impl ClientImages {
    pub fn new(size: u32) -> Self {
        Self {
            size: size.max(1),
            generation: 0,
            entries: HashMap::new(),
            requests: None,
            results: Results::default(),
            roots: None,
        }
    }

    #[cfg(test)]
    fn with_roots(size: u32, roots: Vec<PathBuf>) -> Self {
        Self { roots: Some(roots), ..Self::new(size) }
    }

    /// Sets the edge length images are shrunk to; images are reloaded at the new size.
    pub fn set_size(&mut self, size: u32) {
        let size = size.max(1);
        if size != self.size {
            self.size = size;
            self.generation += 1;
            self.entries.clear();
        }
    }

    /// Returns the image at `path` once it's loaded, and starts loading it otherwise.
    /// `None` means it's still loading, or can't be shown.
    pub fn image(&mut self, path: &Path) -> Option<slint::Image> {
        match self.entries.get(path) {
            Some(Entry::Ready(image)) => return image.clone(),
            Some(Entry::Pending) => return None,
            None => {}
        }
        if self.entries.len() >= MAX_CACHED {
            self.entries.retain(|_, entry| matches!(entry, Entry::Pending));
        }
        let request =
            Request { path: path.to_owned(), size: self.size, generation: self.generation };
        let sent = self.worker().is_some_and(|worker| worker.send(request).is_ok());
        let entry = if sent { Entry::Pending } else { Entry::Ready(None) };
        self.entries.insert(path.to_owned(), entry);
        None
    }

    /// Whether an image is still loading.
    pub fn is_pending(&self) -> bool {
        self.entries.values().any(|entry| matches!(entry, Entry::Pending))
    }

    /// Takes the images the worker finished; returns whether any arrived.
    pub fn poll(&mut self) -> bool {
        let finished = std::mem::take(&mut *lock(&self.results));
        let mut arrived = false;
        for (generation, path, loaded) in finished {
            if generation != self.generation {
                continue;
            }
            let image = loaded.and_then(|loaded| match loaded {
                Loaded::Pixels(pixels) => Some(slint::Image::from_rgba8(pixels)),
                Loaded::Svg(data) => slint::Image::load_from_svg_data(&data).ok(),
            });
            self.entries.insert(path, Entry::Ready(image));
            arrived = true;
        }
        arrived
    }

    fn worker(&mut self) -> Option<&mpsc::Sender<Request>> {
        if self.requests.is_none() {
            let (sender, receiver) = mpsc::channel::<Request>();
            let results = self.results.clone();
            let roots = self.roots.clone();
            let spawned =
                std::thread::Builder::new().name("nimbus-shell-images".into()).spawn(move || {
                    let roots = roots.unwrap_or_else(allowed_roots);
                    for request in receiver {
                        let loaded = load(&request.path, request.size, &roots);
                        lock(&results).push((request.generation, request.path, loaded));
                    }
                });
            match spawned {
                Ok(_) => self.requests = Some(sender),
                Err(err) => tracing::warn!("Can't load notification images: {err}"),
            }
        }
        self.requests.as_ref()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The directories notification images may come from, canonicalized:
/// the XDG data directories, the home and runtime directories, and `/tmp`.
fn allowed_roots() -> Vec<PathBuf> {
    let mut roots = nimbus_xdg::data_dirs();
    roots.extend(
        ["HOME", "XDG_RUNTIME_DIR"].into_iter().filter_map(std::env::var_os).map(PathBuf::from),
    );
    roots.push(PathBuf::from("/tmp"));
    roots.into_iter().filter(|p| p.is_absolute()).filter_map(|p| p.canonicalize().ok()).collect()
}

fn load(path: &Path, size: u32, roots: &[PathBuf]) -> Option<Loaded> {
    let path = path.canonicalize().ok()?;
    if !roots.iter().any(|root| path.starts_with(root)) {
        tracing::debug!(path = %path.display(), "notification image outside the allowed directories");
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_SIZE {
        tracing::debug!(path = %path.display(), "notification image isn't a small regular file");
        return None;
    }
    let svg = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz"));
    if svg {
        return std::fs::read(&path).ok().map(Loaded::Svg);
    }
    let mut reader = image::ImageReader::open(&path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "can't decode notification image");
            return None;
        }
    };
    let image = if image.width() > size || image.height() > size {
        image.thumbnail(size, size)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(Loaded::Pixels(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(images: &mut ClientImages) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while images.is_pending() {
            images.poll();
            assert!(Instant::now() < deadline, "the image never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn loads_and_shrinks_allowed_images() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let root = dir.path().canonicalize().expect("canonical path");
        let path = root.join("photo.png");
        image::RgbImage::new(300, 150).save(&path).expect("fixture saves");

        let mut images = ClientImages::with_roots(64, vec![root]);
        assert!(images.image(&path).is_none(), "loading starts in the background");
        assert!(images.is_pending());
        wait(&mut images);
        let image = images.image(&path).expect("loaded");
        assert_eq!((image.size().width, image.size().height), (64, 32));
    }

    #[test]
    fn rejects_files_outside_the_roots_and_oversized_images() {
        let allowed = tempfile::tempdir().expect("temporary directory");
        let other = tempfile::tempdir().expect("temporary directory");
        let root = allowed.path().canonicalize().expect("canonical path");
        let outside = other.path().join("icon.png");
        image::RgbImage::new(8, 8).save(&outside).expect("fixture saves");
        let huge = root.join("huge.png");
        image::GrayImage::new(MAX_SIDE + 1, 1).save(&huge).expect("fixture saves");
        let sparse = root.join("sparse.png");
        std::fs::File::create(&sparse)
            .and_then(|f| f.set_len(MAX_FILE_SIZE + 1))
            .expect("fixture writes");
        let missing = root.join("missing.png");

        let mut images = ClientImages::with_roots(64, vec![root]);
        for path in [&outside, &huge, &sparse, &missing] {
            assert!(images.image(path).is_none());
        }
        wait(&mut images);
        for path in [&outside, &huge, &sparse, &missing] {
            assert!(images.image(path).is_none(), "{} is rejected", path.display());
        }
        assert!(!images.is_pending(), "rejected images aren't retried");
    }
}
