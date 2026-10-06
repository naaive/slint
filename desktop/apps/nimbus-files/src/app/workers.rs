// SPDX-License-Identifier: MIT

//! Background threads and the messages they send to the UI thread.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use futures_channel::mpsc::UnboundedSender;
use nimbus_xdg::{DesktopEntry, MimeLookup};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::core::apps;
use crate::core::entry::{self, FileEntry, ListOptions};
use crate::core::ops::{self, Conflict, Decision, Operation, Outcome, Progress};
use crate::core::places::{self, Mount, Place};
use crate::core::prefs::Preferences;
use crate::core::properties;
use crate::core::search::{self, SearchEnd};
use crate::core::sort::NameFilter;
use crate::core::thumbnail::{ThumbnailCache, ThumbnailSize};
use crate::core::trash::{Trash, TrashItem};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);
/// Pending thumbnail requests beyond this many drop the oldest, which have usually scrolled away.
const MAX_QUEUED_THUMBNAILS: usize = 256;

/// Everything a worker reports to the UI thread.
pub enum Msg {
    Listed {
        generation: u64,
        dir: PathBuf,
        result: Result<Vec<FileEntry>, String>,
        free_space: Option<(u64, u64)>,
        writable: bool,
    },
    TrashListed {
        generation: u64,
        items: Vec<TrashItem>,
    },
    DirChanged {
        generation: u64,
    },
    SearchBatch {
        generation: u64,
        entries: Vec<FileEntry>,
    },
    SearchDone {
        generation: u64,
        end: SearchEnd,
    },
    Thumbnail {
        path: PathBuf,
        pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    },
    JobProgress {
        id: i32,
        progress: Progress,
    },
    JobConflict {
        id: i32,
        conflict: Conflict,
        details: ConflictDetails,
        reply: mpsc::Sender<Option<Decision>>,
    },
    JobDone {
        id: i32,
        outcome: Outcome,
    },
    Properties {
        id: u64,
        rows: Vec<(String, String)>,
        done: bool,
    },
    Apps {
        id: u64,
        apps: Vec<DesktopEntry>,
        default: Option<String>,
    },
    PlacesChanged,
    Places {
        places: Vec<Place>,
        mounts: Vec<Mount>,
    },
    Disks(nimbus_services::udisks::Event),
}

pub type Sender = UnboundedSender<Msg>;

pub fn send(tx: &Sender, msg: Msg) {
    // The receiver is gone only while the app shuts down.
    let _ = tx.unbounded_send(msg);
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(error) = std::thread::Builder::new().name(name.into()).spawn(work) {
        tracing::error!("couldn't start the {name} thread: {error}");
    }
}

/// Saves preferences in order on one thread, skipping versions that a newer one replaced.
pub struct PrefsWriter {
    requests: Option<mpsc::Sender<Preferences>>,
}

impl PrefsWriter {
    pub fn start(path: Option<PathBuf>) -> Self {
        let Some(path) = path else {
            return Self { requests: None };
        };
        let (tx, rx) = mpsc::channel::<Preferences>();
        spawn("nimbus-files-prefs", move || {
            while let Ok(mut prefs) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    prefs = newer;
                }
                if let Err(error) = prefs.save_to(&path) {
                    tracing::warn!("couldn't save preferences: {error}");
                }
            }
        });
        Self { requests: Some(tx) }
    }

    pub fn save(&self, prefs: &Preferences) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(prefs.clone());
        }
    }
}

/// Lists a folder in the background.
pub fn list_dir(tx: Sender, generation: u64, dir: PathBuf) {
    spawn("nimbus-files-list", move || {
        let result = entry::list_dir(&dir, ListOptions { sniff: true, count_children: true })
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => "The folder doesn't exist.".to_string(),
                std::io::ErrorKind::PermissionDenied => {
                    "You don't have permission to view this folder.".to_string()
                }
                std::io::ErrorKind::NotADirectory => "This is a file, not a folder.".to_string(),
                _ => format!("{}.", ops::error_message(&e)),
            });
        let free_space = properties::disk_space(&dir);
        let writable = rustix::fs::access(&dir, rustix::fs::Access::WRITE_OK).is_ok();
        send(&tx, Msg::Listed { generation, dir, result, free_space, writable });
    });
}

pub fn list_trash(tx: Sender, generation: u64, trash: Trash) {
    spawn("nimbus-files-trash", move || {
        let _ = std::fs::create_dir_all(trash.home().join("info"));
        let items = trash.list();
        send(&tx, Msg::TrashListed { generation, items });
    });
}

/// Searches below `root` until done or `cancel` is set.
pub fn search(
    tx: Sender,
    generation: u64,
    root: PathBuf,
    query: String,
    show_hidden: bool,
    cancel: Arc<AtomicBool>,
) {
    spawn("nimbus-files-search", move || {
        let filter = NameFilter::new(&query);
        let end = search::search(&root, &filter, show_hidden, &cancel, |entries| {
            send(&tx, Msg::SearchBatch { generation, entries });
        });
        send(&tx, Msg::SearchDone { generation, end });
    });
}

/// Reports to the UI thread while an operation runs, and waits for answers to conflicts.
struct JobObserver {
    id: i32,
    tx: Sender,
    cancel: Arc<AtomicBool>,
    last: Instant,
}

impl ops::Observer for JobObserver {
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn progress(&mut self, progress: &Progress) {
        if self.last.elapsed() >= PROGRESS_INTERVAL {
            self.last = Instant::now();
            send(&self.tx, Msg::JobProgress { id: self.id, progress: progress.clone() });
        }
    }

    fn resolve(&mut self, conflict: &Conflict) -> Option<Decision> {
        let (reply, answer) = mpsc::channel();
        let details = ConflictDetails {
            existing: summarize(&conflict.target),
            incoming: summarize(&conflict.source),
        };
        send(
            &self.tx,
            Msg::JobConflict { id: self.id, conflict: conflict.clone(), details, reply },
        );
        loop {
            match answer.recv_timeout(Duration::from_millis(100)) {
                Ok(decision) => return decision,
                Err(mpsc::RecvTimeoutError::Timeout) if !self.is_cancelled() => {}
                Err(_) => return None,
            }
        }
    }
}

/// Size and date of both sides of a conflict, for the dialog.
#[derive(Clone, Debug, Default)]
pub struct ConflictDetails {
    pub existing: String,
    pub incoming: String,
}

fn summarize(path: &Path) -> String {
    use crate::core::format;
    let Ok(entry) = entry::read_entry(path, ListOptions { sniff: false, count_children: true })
    else {
        return String::new();
    };
    let size = crate::app::convert::size_label(&entry);
    let date = entry
        .modified
        .map(|t| format!("modified {}", format::short_date(format::local_time(t), format::now())));
    [Some(size).filter(|s| !s.is_empty()), date]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn run_operation(
    tx: Sender,
    id: i32,
    operation: Operation,
    trash: Trash,
    cancel: Arc<AtomicBool>,
) {
    spawn("nimbus-files-job", move || {
        let mut observer = JobObserver { id, tx: tx.clone(), cancel, last: Instant::now() };
        let outcome = ops::run(&operation, &trash, &mut observer);
        send(&tx, Msg::JobDone { id, outcome });
    });
}

/// Builds the properties rows for `paths`, sending updates while folder sizes are summed.
pub fn properties(
    tx: Sender,
    id: u64,
    paths: Vec<PathBuf>,
    home: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    spawn("nimbus-files-properties", move || {
        let base = properties::describe(&paths, &home);
        let needs_walk = paths.len() > 1
            || paths.iter().any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()));
        if !needs_walk {
            send(&tx, Msg::Properties { id, rows: base, done: true });
            return;
        }
        send(&tx, Msg::Properties { id, rows: base.clone(), done: false });
        let result = properties::deep_size(&paths, &cancel, |partial| {
            send(
                &tx,
                Msg::Properties { id, rows: properties::with_size(&base, partial), done: false },
            );
        });
        if let Some(total) = result {
            send(
                &tx,
                Msg::Properties { id, rows: properties::with_size(&base, total), done: true },
            );
        }
    });
}

/// Files the sidebar is built from.
#[derive(Clone, Debug)]
pub struct PlaceSources {
    pub home: PathBuf,
    pub user_dirs: PathBuf,
    pub bookmarks: PathBuf,
    pub mountinfo: PathBuf,
}

/// Reads the sidebar's places and the mount table.
pub fn read_places(sources: &PlaceSources) -> (Vec<Place>, Vec<Mount>) {
    let user_dirs = std::fs::read_to_string(&sources.user_dirs).ok();
    let mut list = places::standard_places(&sources.home, user_dirs.as_deref());
    let mounts = std::fs::read_to_string(&sources.mountinfo)
        .map(|t| places::parse_mountinfo(&t))
        .unwrap_or_default();
    list.extend(places::device_places(&mounts));
    if let Ok(text) = std::fs::read_to_string(&sources.bookmarks) {
        list.extend(places::bookmark_places(&text));
    }
    (list, mounts)
}

pub fn load_places(tx: Sender, sources: PlaceSources) {
    spawn("nimbus-files-places", move || {
        let (places, mounts) = read_places(&sources);
        send(&tx, Msg::Places { places, mounts });
    });
}

/// Adds or removes a bookmark in the background, then reloads the sidebar.
pub fn set_bookmark(tx: Sender, file: PathBuf, dir: PathBuf, bookmarked: bool) {
    spawn("nimbus-files-bookmark", move || {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let updated = if bookmarked {
            places::add_bookmark(&text, &dir)
        } else {
            Some(places::remove_bookmark(&text, &dir))
        };
        if let Some(updated) = updated {
            let result = file
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&file, updated));
            if let Err(error) = result {
                tracing::warn!("couldn't save the bookmark: {error}");
            }
        }
        send(&tx, Msg::PlacesChanged);
    });
}

/// Finds applications for "Open With" in the background.
pub fn find_apps(tx: Sender, id: u64, mime: String) {
    spawn("nimbus-files-apps", move || {
        let (found, default) = apps::apps_for_mime(&MimeLookup::from_env(), &mime);
        send(&tx, Msg::Apps { id, apps: found, default });
    });
}

struct ThumbnailRequest {
    path: PathBuf,
    mime: String,
    size: ThumbnailSize,
}

/// A last-in, first-out queue of thumbnail requests served by one worker thread.
#[derive(Clone)]
pub struct ThumbnailQueue {
    shared: Arc<(Mutex<VecDeque<ThumbnailRequest>>, Condvar)>,
}

impl ThumbnailQueue {
    pub fn start(tx: Sender, cache: ThumbnailCache) -> Self {
        let queue = Self { shared: Arc::new((Mutex::new(VecDeque::new()), Condvar::new())) };
        let worker = queue.clone();
        spawn("nimbus-files-thumbnails", move || {
            while let Some(ThumbnailRequest { path, mime, size }) = worker.next() {
                let pixels = match cache.load_or_generate(&path, &mime, size) {
                    Ok(thumb) => {
                        let mut buffer =
                            SharedPixelBuffer::<Rgba8Pixel>::new(thumb.width, thumb.height);
                        let bytes = buffer.make_mut_bytes();
                        if bytes.len() == thumb.rgba.len() {
                            bytes.copy_from_slice(&thumb.rgba);
                            Some(buffer)
                        } else {
                            None
                        }
                    }
                    Err(error) => {
                        tracing::debug!("no thumbnail for {}: {error}", path.display());
                        None
                    }
                };
                if tx.unbounded_send(Msg::Thumbnail { path, pixels }).is_err() {
                    break;
                }
            }
        });
        queue
    }

    pub fn request(&self, path: PathBuf, mime: String, size: ThumbnailSize) {
        let (lock, ready) = &*self.shared;
        let Ok(mut queue) = lock.lock() else { return };
        queue.retain(|r| r.path != path);
        queue.push_front(ThumbnailRequest { path, mime, size });
        queue.truncate(MAX_QUEUED_THUMBNAILS);
        ready.notify_one();
    }

    /// Drops pending requests, such as after leaving a folder.
    pub fn clear(&self) {
        if let Ok(mut queue) = self.shared.0.lock() {
            queue.clear();
        }
    }

    fn next(&self) -> Option<ThumbnailRequest> {
        let (lock, ready) = &*self.shared;
        let mut queue = lock.lock().ok()?;
        loop {
            if let Some(item) = queue.pop_front() {
                return Some(item);
            }
            queue = ready.wait(queue).ok()?;
        }
    }
}

/// Watches a folder and reports changes, coalesced by the controller.
pub fn watch_dir(tx: Sender, generation: u64, dir: &Path) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, notify::EventKind::Access(_))
        {
            send(&tx, Msg::DirChanged { generation });
        }
    })
    .map_err(|e| tracing::warn!("can't watch {}: {e}", dir.display()))
    .ok()?;
    watcher
        .watch(dir, notify::RecursiveMode::NonRecursive)
        .map_err(|e| tracing::debug!("can't watch {}: {e}", dir.display()))
        .ok()?;
    Some(watcher)
}

/// Watches files that define the sidebar, such as the bookmarks and `user-dirs.dirs`.
pub fn watch_places(tx: Sender, files: &[PathBuf]) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher as _;
    let names: Vec<std::ffi::OsString> =
        files.iter().filter_map(|f| f.file_name().map(Into::into)).collect();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event
                .paths
                .iter()
                .any(|p| p.file_name().is_some_and(|n| names.iter().any(|m| m == n)))
        {
            send(&tx, Msg::PlacesChanged);
        }
    })
    .ok()?;
    for dir in files.iter().filter_map(|f| f.parent()) {
        if dir.is_dir() {
            let _ = watcher.watch(dir, notify::RecursiveMode::NonRecursive);
        }
    }
    Some(watcher)
}

/// Polls the mount table, which signals changes with `POLLPRI`, and sends the new mounts.
pub fn watch_mounts(tx: Sender, path: PathBuf, stop: Arc<AtomicBool>) {
    spawn("nimbus-files-mounts", move || {
        use rustix::event::{PollFd, PollFlags, poll};
        use std::io::{Read as _, Seek as _};
        let Ok(mut file) = std::fs::File::open(&path) else { return };
        let mut last = String::new();
        let _ = file.read_to_string(&mut last);
        let timeout = rustix::event::Timespec { tv_sec: 1, tv_nsec: 0 };
        while !stop.load(Ordering::Relaxed) {
            let mut fds = [PollFd::new(&file, PollFlags::PRI | PollFlags::ERR)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) => continue,
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
            let mut text = String::new();
            if file.rewind().is_err() || file.read_to_string(&mut text).is_err() {
                return;
            }
            if text != last {
                send(&tx, Msg::PlacesChanged);
                last = text;
            }
        }
    });
}
