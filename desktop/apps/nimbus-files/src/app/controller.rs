// SPDX-License-Identifier: MIT

//! The UI thread's state and its synchronization with the window.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

use futures_channel::mpsc::UnboundedReceiver;
use slint::{ComponentHandle as _, Image, Model as _, ModelRc, VecModel};

use super::convert::{self, ItemContext, SpecialFolders};
use super::workers::{self, ConflictDetails, Msg, PlaceSources, Sender, ThumbnailQueue};
use crate::core::apps::AppInfo;
use crate::core::diff::{self, Edit};
use crate::core::entry::{EntryKind, FileEntry};
use crate::core::format;
use crate::core::history::{History, Location};
use crate::core::menu::MenuEntry;
use crate::core::mime;
use crate::core::ops::{Conflict, Decision, Operation, Progress};
use crate::core::pathbar;
use crate::core::places::{self, Mount, Place, Target};
use crate::core::prefs::{Preferences, ViewMode};
use crate::core::search::SearchEnd;
use crate::core::selection::Selection;
use crate::core::sort::{self, NameFilter, SortKey};
use crate::core::thumbnail::{ThumbnailCache, ThumbnailSize};
use crate::core::trash::{Trash, TrashItem};
use crate::{AppWindow, DialogKind, FileItem, IconKind, JobItem, ViewKind};

/// Running jobs appear in the panel only after this long, so quick ones don't flash.
const JOB_PANEL_DELAY: Duration = Duration::from_millis(400);
const TOAST_DURATION: Duration = Duration::from_secs(6);
/// Change notifications within this window cause one refresh.
const REFRESH_DELAY: Duration = Duration::from_millis(250);
const SEARCH_DELAY: Duration = Duration::from_millis(250);
/// Type-ahead input older than this starts a new prefix.
pub(super) const TYPE_AHEAD_TIMEOUT: Duration = Duration::from_millis(900);

/// Paths and files the app reads, overridable for tests.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub prefs_path: Option<PathBuf>,
    pub mountinfo: PathBuf,
    /// Watches folders and the mount table; tests turn it off for determinism.
    pub live_updates: bool,
}

impl Env {
    pub fn from_system() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            config_dir: dirs::config_dir().unwrap_or_else(|| home.join(".config")),
            data_dir: dirs::data_dir().unwrap_or_else(|| home.join(".local/share")),
            cache_dir: dirs::cache_dir().unwrap_or_else(|| home.join(".cache")),
            prefs_path: Preferences::default_path(),
            mountinfo: PathBuf::from("/proc/self/mountinfo"),
            live_updates: true,
            home,
        }
    }

    pub fn place_sources(&self) -> PlaceSources {
        PlaceSources {
            home: self.home.clone(),
            user_dirs: self.config_dir.join("user-dirs.dirs"),
            bookmarks: self.bookmarks_file(),
            mountinfo: self.mountinfo.clone(),
        }
    }

    pub fn bookmarks_file(&self) -> PathBuf {
        self.config_dir.join("gtk-3.0").join("bookmarks")
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Clipboard {
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

pub(super) struct JobState {
    pub id: i32,
    pub operation: Operation,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
    pub progress: Progress,
    /// Select these paths when the job finishes, such as a renamed file.
    pub select_after: bool,
}

pub(super) struct PendingConflict {
    pub job: i32,
    pub conflict: Conflict,
    pub details: ConflictDetails,
    pub reply: mpsc::Sender<Option<Decision>>,
}

/// The open dialog and what it acts on.
pub(super) enum Dialog {
    None,
    Rename { path: PathBuf },
    NewFolder { parent: PathBuf },
    ConfirmDelete { paths: Vec<PathBuf> },
    ConfirmDeleteTrashed { items: Vec<TrashItem> },
    Conflict,
    Properties { id: u64, cancel: Arc<AtomicBool> },
    OpenWith { id: u64, paths: Vec<PathBuf>, apps: Vec<AppInfo> },
}

pub(super) enum ToastAction {
    None,
    UndoTrash(Vec<TrashItem>),
}

pub(super) struct SearchState {
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub results: Vec<FileEntry>,
    pub running: bool,
}

pub(super) struct State {
    pub history: History,
    /// The listing of the current folder.
    pub entries: Vec<FileEntry>,
    /// Trashed items by their path inside the trash, when showing the trash.
    pub trash_items: HashMap<PathBuf, TrashItem>,
    /// The rows shown, in order.
    pub view: Vec<FileEntry>,
    pub index: HashMap<PathBuf, usize>,
    pub selection: Selection,
    /// The `selected` flag of each model row, so selection changes touch only the rows that differ.
    pub row_selected: Vec<bool>,
    pub prefs: Preferences,
    pub filter: String,
    pub search_active: bool,
    pub search_recursive: bool,
    pub search: Option<SearchState>,
    pub generation: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub free_space: Option<(u64, u64)>,
    /// The current folder accepts new files.
    pub writable: bool,
    pub clipboard: Option<Clipboard>,
    pub jobs: Vec<JobState>,
    pub next_job: i32,
    pub conflicts: VecDeque<PendingConflict>,
    pub dialog: Dialog,
    pub next_request: u64,
    pub menu: Vec<MenuEntry>,
    /// A folder the open menu applies to instead of the current one, for "Paste Into Folder".
    pub menu_target: Option<PathBuf>,
    pub places: Vec<Place>,
    pub mounts: Vec<Mount>,
    pub special: SpecialFolders,
    pub pending_select: Vec<PathBuf>,
    pub thumbnails: HashMap<PathBuf, Image>,
    pub thumbnail_failures: HashSet<PathBuf>,
    pub thumbnails_requested: HashSet<PathBuf>,
    pub toast_action: ToastAction,
    pub band_base: Vec<usize>,
    pub watcher: Option<notify::RecommendedWatcher>,
    pub refresh_pending: bool,
    pub type_ahead: String,
    pub type_ahead_at: Option<Instant>,
}

pub struct Controller {
    pub(super) ui: AppWindow,
    pub(super) env: Env,
    pub(super) tx: Sender,
    rx: RefCell<Option<UnboundedReceiver<Msg>>>,
    pub(super) files: Rc<VecModel<FileItem>>,
    jobs_model: Rc<VecModel<JobItem>>,
    pub(super) thumbnails: ThumbnailQueue,
    pub(super) state: RefCell<State>,
    toast_timer: slint::Timer,
    refresh_timer: slint::Timer,
    search_timer: slint::Timer,
    jobs_timer: slint::Timer,
    _places_watcher: Option<notify::RecommendedWatcher>,
    mounts_stop: Arc<AtomicBool>,
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.mounts_stop.store(true, Ordering::Relaxed);
        let state = self.state.get_mut();
        if let Some(search) = &state.search {
            search.cancel.store(true, Ordering::Relaxed);
        }
        for job in &state.jobs {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Controller {
    /// Creates the controller for a window, showing `start` (or the home folder).
    pub fn new(ui: AppWindow, env: Env, start: Option<PathBuf>) -> Rc<Self> {
        let (tx, rx) = futures_channel::mpsc::unbounded();
        let prefs = env.prefs_path.as_deref().map(Preferences::load_from).unwrap_or_default();
        let (places, mounts) = workers::read_places(&env.place_sources());
        let thumbnails = ThumbnailQueue::start(
            tx.clone(),
            ThumbnailCache::new(env.cache_dir.join("thumbnails")),
        );
        let mounts_stop = Arc::new(AtomicBool::new(false));
        let places_watcher = if env.live_updates {
            workers::watch_mounts(tx.clone(), env.mountinfo.clone(), mounts_stop.clone());
            let sources = env.place_sources();
            workers::watch_places(tx.clone(), &[sources.user_dirs, sources.bookmarks])
        } else {
            None
        };
        let start = start.unwrap_or_else(|| env.home.clone());
        let state = State {
            history: History::new(Location::Dir(start.clone())),
            entries: Vec::new(),
            trash_items: HashMap::new(),
            view: Vec::new(),
            index: HashMap::new(),
            selection: Selection::new(0),
            row_selected: Vec::new(),
            prefs,
            filter: String::new(),
            search_active: false,
            search_recursive: false,
            search: None,
            generation: 0,
            loading: false,
            error: None,
            free_space: None,
            writable: false,
            clipboard: None,
            jobs: Vec::new(),
            next_job: 1,
            conflicts: VecDeque::new(),
            dialog: Dialog::None,
            next_request: 1,
            menu: Vec::new(),
            menu_target: None,
            special: SpecialFolders::from_places(&places),
            places,
            mounts,
            pending_select: Vec::new(),
            thumbnails: HashMap::new(),
            thumbnail_failures: HashSet::new(),
            thumbnails_requested: HashSet::new(),
            toast_action: ToastAction::None,
            band_base: Vec::new(),
            watcher: None,
            refresh_pending: false,
            type_ahead: String::new(),
            type_ahead_at: None,
        };
        let files = Rc::new(VecModel::default());
        let jobs_model = Rc::new(VecModel::default());
        ui.set_files(ModelRc::from(files.clone()));
        ui.set_jobs(ModelRc::from(jobs_model.clone()));
        let controller = Rc::new(Self {
            ui,
            env,
            tx,
            rx: RefCell::new(Some(rx)),
            files,
            jobs_model,
            thumbnails,
            state: RefCell::new(state),
            toast_timer: slint::Timer::default(),
            refresh_timer: slint::Timer::default(),
            search_timer: slint::Timer::default(),
            jobs_timer: slint::Timer::default(),
            _places_watcher: places_watcher,
            mounts_stop,
        });
        super::bindings::connect(&controller);
        controller.sync_prefs();
        controller.sync_places();
        controller.load_location(Location::Dir(start));
        controller
    }

    /// Handles worker messages as they arrive, for as long as the event loop runs.
    pub fn start_message_loop(self: &Rc<Self>) {
        let Some(mut rx) = self.rx.borrow_mut().take() else { return };
        let weak = Rc::downgrade(self);
        let result = slint::spawn_local(async move {
            use futures_util::StreamExt as _;
            while let Some(msg) = rx.next().await {
                let Some(this) = weak.upgrade() else { break };
                this.handle(msg);
            }
        });
        if let Err(error) = result {
            tracing::error!("can't receive background results: {error}");
        }
    }

    /// Handles all messages that already arrived; for tests that drive the controller without an event loop.
    pub fn pump(self: &Rc<Self>) -> usize {
        let mut handled = 0;
        loop {
            let msg = match self.rx.borrow_mut().as_mut().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => msg,
                _ => break,
            };
            self.handle(msg);
            handled += 1;
        }
        handled
    }

    pub fn ui(&self) -> &AppWindow {
        &self.ui
    }

    pub(super) fn location(&self) -> Location {
        self.state.borrow().history.current().clone()
    }

    pub(super) fn current_dir(&self) -> Option<PathBuf> {
        self.location().dir().map(Path::to_path_buf)
    }

    pub(super) fn trash(&self) -> Trash {
        let points = places::mount_points(&self.state.borrow().mounts);
        Trash::new(self.env.data_dir.join("Trash"), rustix::process::getuid().as_raw(), points)
    }

    fn handle(self: &Rc<Self>, msg: Msg) {
        match msg {
            Msg::Listed { generation, dir, result, free_space, writable } => {
                self.on_listed(generation, &dir, result, (free_space, writable));
            }
            Msg::TrashListed { generation, items } => self.on_trash_listed(generation, items),
            Msg::DirChanged { generation } => self.schedule_refresh(generation),
            Msg::SearchBatch { generation, entries } => self.on_search_batch(generation, entries),
            Msg::SearchDone { generation, end } => self.on_search_done(generation, end),
            Msg::Thumbnail { path, pixels } => self.on_thumbnail(path, pixels),
            Msg::JobProgress { id, progress } => self.on_job_progress(id, progress),
            Msg::JobConflict { id, conflict, details, reply } => {
                self.state.borrow_mut().conflicts.push_back(PendingConflict {
                    job: id,
                    conflict,
                    details,
                    reply,
                });
                self.show_next_conflict();
            }
            Msg::JobDone { id, outcome } => self.on_job_done(id, outcome),
            Msg::Properties { id, rows, done } => self.on_properties(id, rows, done),
            Msg::Apps { id, apps, default } => self.on_apps(id, apps, default.as_deref()),
            Msg::PlacesChanged => workers::load_places(self.tx.clone(), self.env.place_sources()),
            Msg::Places { places, mounts } => {
                {
                    let mut state = self.state.borrow_mut();
                    state.special = SpecialFolders::from_places(&places);
                    state.places = places;
                    state.mounts = mounts;
                }
                self.sync_places();
            }
        }
    }

    /// Shows a location, recording it in the history.
    pub(super) fn navigate(&self, location: Location) {
        if location == self.location() {
            self.reload();
            return;
        }
        self.state.borrow_mut().history.visit(location.clone());
        self.load_location(location);
    }

    /// Starts listing a location that is already current in the history.
    pub(super) fn load_location(&self, location: Location) {
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            if let Some(search) = state.search.take() {
                search.cancel.store(true, Ordering::Relaxed);
            }
            state.search_active = false;
            state.filter.clear();
            state.entries.clear();
            state.trash_items.clear();
            state.selection = Selection::new(0);
            state.loading = true;
            state.error = None;
            state.free_space = None;
            state.thumbnails.clear();
            state.thumbnail_failures.clear();
            state.thumbnails_requested.clear();
            state.type_ahead.clear();
            state.watcher = None;
            state.generation
        };
        self.thumbnails.clear();
        self.ui.set_search_active(false);
        self.ui.set_search_text("".into());
        self.ui.set_editing_location(false);
        self.ui.set_keyboard_cursor(false);
        self.ui.set_view_kind(if location == Location::Trash {
            ViewKind::Trash
        } else {
            ViewKind::Folder
        });
        self.rebuild_view();
        match &location {
            Location::Dir(dir) => {
                workers::list_dir(self.tx.clone(), generation, dir.clone());
                if self.env.live_updates {
                    let watcher = workers::watch_dir(self.tx.clone(), generation, dir);
                    self.state.borrow_mut().watcher = watcher;
                }
            }
            Location::Trash => workers::list_trash(self.tx.clone(), generation, self.trash()),
        }
        self.sync_location();
    }

    /// Lists the current location again, keeping the selection.
    pub(super) fn reload(&self) {
        let generation = self.state.borrow().generation;
        match self.location() {
            Location::Dir(dir) => workers::list_dir(self.tx.clone(), generation, dir),
            Location::Trash => workers::list_trash(self.tx.clone(), generation, self.trash()),
        }
    }

    fn schedule_refresh(self: &Rc<Self>, generation: u64) {
        {
            let mut state = self.state.borrow_mut();
            if generation != state.generation || state.refresh_pending {
                return;
            }
            state.refresh_pending = true;
        }
        let weak = Rc::downgrade(self);
        self.refresh_timer.start(slint::TimerMode::SingleShot, REFRESH_DELAY, move || {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
    }

    fn on_listed(
        &self,
        generation: u64,
        dir: &Path,
        result: Result<Vec<FileEntry>, String>,
        (free_space, writable): (Option<(u64, u64)>, bool),
    ) {
        {
            let mut state = self.state.borrow_mut();
            if generation != state.generation || state.history.current().dir() != Some(dir) {
                return;
            }
            state.loading = false;
            state.refresh_pending = false;
            state.free_space = free_space;
            state.writable = writable;
            match result {
                Ok(entries) => {
                    state.entries = entries;
                    state.error = None;
                }
                Err(error) => {
                    state.entries.clear();
                    state.error = Some(error);
                }
            }
        }
        self.rebuild_view();
        // Items to select that a fresh listing doesn't contain are gone for good.
        self.state.borrow_mut().pending_select.clear();
    }

    fn on_trash_listed(&self, generation: u64, items: Vec<TrashItem>) {
        {
            let mut state = self.state.borrow_mut();
            if generation != state.generation || *state.history.current() != Location::Trash {
                return;
            }
            state.loading = false;
            state.refresh_pending = false;
            state.entries = items.iter().map(trash_entry).collect();
            state.trash_items = items.into_iter().map(|i| (i.file.clone(), i)).collect();
        }
        // The listing created the home trash's folders, so they can be watched now.
        if self.env.live_updates && self.state.borrow().watcher.is_none() {
            let info = self.env.data_dir.join("Trash").join("info");
            let watcher = workers::watch_dir(self.tx.clone(), generation, &info);
            self.state.borrow_mut().watcher = watcher;
        }
        self.rebuild_view();
        self.state.borrow_mut().pending_select.clear();
    }

    fn on_search_batch(&self, generation: u64, entries: Vec<FileEntry>) {
        {
            let mut state = self.state.borrow_mut();
            match &mut state.search {
                Some(search) if search.generation == generation => search.results.extend(entries),
                _ => return,
            }
        }
        self.rebuild_view();
    }

    fn on_search_done(&self, generation: u64, end: SearchEnd) {
        {
            let mut state = self.state.borrow_mut();
            match &mut state.search {
                Some(search) if search.generation == generation => search.running = false,
                _ => return,
            }
        }
        self.ui.set_searching(false);
        if end == SearchEnd::Truncated {
            self.show_toast(
                format!(
                    "Showing the first {} results; refine the search to see more",
                    crate::core::search::MAX_RESULTS
                ),
                ToastAction::None,
                None,
            );
        }
        self.rebuild_view();
    }

    /// Starts or restarts the recursive search for the current filter after a short pause in typing.
    pub(super) fn schedule_search(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.search_timer.start(slint::TimerMode::SingleShot, SEARCH_DELAY, move || {
            if let Some(this) = weak.upgrade() {
                this.start_search();
            }
        });
    }

    pub(super) fn start_search(&self) {
        let (root, query, show_hidden, generation, cancel) = {
            let mut state = self.state.borrow_mut();
            if let Some(old) = state.search.take() {
                old.cancel.store(true, Ordering::Relaxed);
            }
            let query = state.filter.trim().to_string();
            let Some(root) = state.history.current().dir().map(Path::to_path_buf) else { return };
            if !state.search_active || !state.search_recursive || query.is_empty() {
                drop(state);
                self.ui.set_searching(false);
                self.rebuild_view();
                return;
            }
            // Searches count separately from listings, so a search doesn't discard a pending refresh.
            state.next_request += 1;
            let generation = state.next_request;
            let cancel = Arc::new(AtomicBool::new(false));
            state.search = Some(SearchState {
                generation,
                cancel: cancel.clone(),
                results: Vec::new(),
                running: true,
            });
            (root, query, state.prefs.show_hidden, generation, cancel)
        };
        self.ui.set_searching(true);
        self.rebuild_view();
        workers::search(self.tx.clone(), generation, root, query, show_hidden, cancel);
    }

    pub(super) fn stop_search(&self) {
        {
            let mut state = self.state.borrow_mut();
            if let Some(search) = state.search.take() {
                search.cancel.store(true, Ordering::Relaxed);
            }
            state.search_active = false;
            state.filter.clear();
        }
        self.search_timer.stop();
        self.ui.set_search_active(false);
        self.ui.set_search_text("".into());
        self.ui.set_searching(false);
        self.rebuild_view();
        self.ui.invoke_focus_view();
    }

    /// Recomputes the visible rows and updates the model with as few changes as possible.
    pub(super) fn rebuild_view(&self) {
        let now = format::now();
        let (edits, new_view, reveal) = {
            let mut state = self.state.borrow_mut();
            let selected: HashSet<PathBuf> = state
                .selection
                .indices()
                .into_iter()
                .filter_map(|i| state.view.get(i).map(|e| e.path.clone()))
                .chain(state.pending_select.iter().cloned())
                .collect();
            let cursor_path =
                state.selection.cursor().and_then(|i| state.view.get(i)).map(|e| e.path.clone());
            let recursive =
                state.search_active && state.search_recursive && !state.filter.trim().is_empty();
            let new_view = if recursive {
                let results =
                    state.search.as_ref().map(|s| s.results.as_slice()).unwrap_or_default();
                sort::visible(results, true, &NameFilter::default(), state.prefs.sort)
            } else {
                let filter = if state.search_active {
                    NameFilter::new(&state.filter)
                } else {
                    NameFilter::default()
                };
                sort::visible(&state.entries, state.prefs.show_hidden, &filter, state.prefs.sort)
            };
            invalidate_changed_thumbnails(&mut state, &new_view);
            let edits = diff::plan(&state.view, &new_view, |e| e.path.clone());
            let index: HashMap<PathBuf, usize> =
                new_view.iter().enumerate().map(|(i, e)| (e.path.clone(), i)).collect();
            let selected_indices: Vec<usize> =
                selected.iter().filter_map(|p| index.get(p).copied()).collect();
            let pending_found =
                state.pending_select.iter().filter_map(|p| index.get(p).copied()).max();
            let cursor = pending_found.or_else(|| cursor_path.and_then(|p| index.get(&p).copied()));
            if pending_found.is_some() {
                state.pending_select.clear();
            }
            state.selection.set(new_view.len(), selected_indices, cursor);
            state.index = index;
            state.view = new_view.clone();
            (edits, new_view, pending_found)
        };
        match edits {
            Some(edits) => {
                for edit in edits {
                    match edit {
                        Edit::Remove(i) => {
                            self.files.remove(i);
                        }
                        Edit::Insert(i, entry) => {
                            let item = self.item_for(&entry, i, now);
                            self.files.insert(i, item);
                        }
                        Edit::Update(i, entry) => {
                            let item = self.item_for(&entry, i, now);
                            self.files.set_row_data(i, item);
                        }
                    }
                }
            }
            None => {
                let items: Vec<FileItem> =
                    new_view.iter().enumerate().map(|(i, e)| self.item_for(e, i, now)).collect();
                self.files.set_vec(items);
            }
        }
        self.sync_all_selection_flags();
        self.sync_status();
        if let Some(index) = reveal {
            self.ui.invoke_scroll_to(index as i32);
        }
    }

    pub(super) fn item_for(
        &self,
        entry: &FileEntry,
        index: usize,
        now: chrono::NaiveDateTime,
    ) -> FileItem {
        let state = self.state.borrow();
        let location = match state.history.current() {
            Location::Trash => state
                .trash_items
                .get(&entry.path)
                .map(|item| self.pretty_dir(item.original.parent())),
            Location::Dir(root) if state.search_active && state.search_recursive => {
                let parent = entry.path.parent().unwrap_or(root);
                Some(match parent.strip_prefix(root) {
                    Ok(rel) if rel.as_os_str().is_empty() => "This folder".to_string(),
                    Ok(rel) => rel.to_string_lossy().into_owned(),
                    Err(_) => self.pretty_dir(Some(parent)),
                })
            }
            Location::Dir(_) => None,
        };
        let cut = state.clipboard.as_ref().is_some_and(|c| c.cut && c.paths.contains(&entry.path));
        convert::file_item(
            entry,
            ItemContext {
                special: &state.special,
                now,
                location,
                selected: state.selection.is_selected(index),
                cut,
                thumbnail: state.thumbnails.get(&entry.path).cloned(),
                thumbnail_failed: state.thumbnail_failures.contains(&entry.path),
            },
        )
    }

    /// A folder for display, with the home folder shortened to `~`.
    pub(super) fn pretty_dir(&self, dir: Option<&Path>) -> String {
        dir.map(|d| crate::core::properties::pretty_dir(d, &self.env.home)).unwrap_or_default()
    }

    /// Updates the `selected` flag of every row from the model, after rows were replaced.
    fn sync_all_selection_flags(&self) {
        let mut state = self.state.borrow_mut();
        let mut flags = Vec::with_capacity(self.files.row_count());
        for i in 0..self.files.row_count() {
            let selected = state.selection.is_selected(i);
            flags.push(selected);
            if let Some(mut row) = self.files.row_data(i)
                && row.selected != selected
            {
                row.selected = selected;
                self.files.set_row_data(i, row);
            }
        }
        state.row_selected = flags;
        drop(state);
        self.sync_cursor();
    }

    /// Updates the `selected` flag of rows whose selection changed.
    pub(super) fn sync_selection_flags(&self) {
        let in_sync = self.state.borrow().row_selected.len() == self.files.row_count();
        if !in_sync {
            self.sync_all_selection_flags();
            return;
        }
        {
            let mut guard = self.state.borrow_mut();
            let state = &mut *guard;
            for (i, shown) in state.row_selected.iter_mut().enumerate() {
                let selected = state.selection.is_selected(i);
                if *shown != selected
                    && let Some(mut row) = self.files.row_data(i)
                {
                    row.selected = selected;
                    self.files.set_row_data(i, row);
                    *shown = selected;
                }
            }
        }
        self.sync_cursor();
    }

    fn sync_cursor(&self) {
        let state = self.state.borrow();
        self.ui.set_cursor(state.selection.cursor().map_or(-1, |c| c as i32));
        self.ui.set_has_selection(state.selection.count() > 0);
    }

    /// Applies a change to the selection and updates the rows and status bar.
    pub(super) fn change_selection(&self, change: impl FnOnce(&mut Selection)) {
        change(&mut self.state.borrow_mut().selection);
        self.sync_selection_flags();
        self.sync_status();
    }

    /// Refreshes the `cut` flag of every row after the clipboard changed.
    pub(super) fn sync_cut_flags(&self) {
        let state = self.state.borrow();
        let cut: HashSet<&PathBuf> =
            state.clipboard.iter().filter(|c| c.cut).flat_map(|c| c.paths.iter()).collect();
        for (i, entry) in state.view.iter().enumerate() {
            if let Some(mut row) = self.files.row_data(i) {
                let is_cut = cut.contains(&entry.path);
                if row.cut != is_cut {
                    row.cut = is_cut;
                    self.files.set_row_data(i, row);
                }
            }
        }
    }

    pub(super) fn selected_entries(&self) -> Vec<FileEntry> {
        let state = self.state.borrow();
        state.selection.indices().into_iter().filter_map(|i| state.view.get(i).cloned()).collect()
    }

    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected_entries().into_iter().map(|e| e.path).collect()
    }

    pub(super) fn selected_trash_items(&self) -> Vec<TrashItem> {
        let state = self.state.borrow();
        state
            .selection
            .indices()
            .into_iter()
            .filter_map(|i| state.view.get(i).and_then(|e| state.trash_items.get(&e.path)).cloned())
            .collect()
    }

    fn sync_status(&self) {
        let state = self.state.borrow();
        let total = state.view.len() as u64;
        let selected: Vec<&FileEntry> =
            state.selection.indices().into_iter().filter_map(|i| state.view.get(i)).collect();
        let text = match selected.as_slice() {
            [] if state.loading => "Loading…".to_string(),
            [] => format::item_count(total),
            [one] => {
                let detail = convert::size_label(one);
                if detail.is_empty() {
                    format!("“{}” selected", one.name)
                } else {
                    format!("“{}” selected ({detail})", one.name)
                }
            }
            many => {
                let bytes: u64 = many.iter().filter(|e| !e.is_dir()).map(|e| e.size).sum();
                let folders = many.iter().filter(|e| e.is_dir()).count();
                let mut text = format!("{} selected", format::item_count(many.len() as u64));
                if folders < many.len() {
                    text.push_str(&format!(" ({})", format::size(bytes)));
                }
                text
            }
        };
        let detail = match state.history.current() {
            Location::Trash => {
                let bytes: u64 = state.trash_items.values().map(|i| i.size).sum();
                if state.trash_items.is_empty() {
                    String::new()
                } else {
                    format!("{} in the Trash", format::size(bytes))
                }
            }
            Location::Dir(_) => state
                .free_space
                .map(|(free, _)| format!("{} free", format::size(free)))
                .unwrap_or_default(),
        };
        let in_trash = *state.history.current() == Location::Trash;
        let searching = state.search_active && !state.filter.trim().is_empty();
        let (empty_title, empty_subtitle, empty_kind) = if state.loading || !state.view.is_empty() {
            (String::new(), String::new(), IconKind::Folder)
        } else if let Some(error) = &state.error {
            ("Can't Open This Folder".to_string(), error.clone(), IconKind::Folder)
        } else if searching {
            let running = state.search.as_ref().is_some_and(|s| s.running);
            if running {
                (String::new(), String::new(), IconKind::Folder)
            } else {
                (
                    "No Results".to_string(),
                    "Try a different search, or search the subfolders too.".to_string(),
                    IconKind::File,
                )
            }
        } else if in_trash {
            (
                "Trash Is Empty".to_string(),
                "Files you move to the Trash appear here.".to_string(),
                IconKind::Trash,
            )
        } else if !state.entries.is_empty() {
            (
                "Only Hidden Files".to_string(),
                "Press Ctrl+H to show hidden files.".to_string(),
                IconKind::Folder,
            )
        } else {
            ("Folder Is Empty".to_string(), String::new(), IconKind::Folder)
        };
        let ui = &self.ui;
        ui.set_status_text(text.into());
        ui.set_status_detail(detail.into());
        ui.set_empty_title(empty_title.into());
        ui.set_empty_subtitle(empty_subtitle.into());
        ui.set_empty_kind(empty_kind);
        ui.set_loading(state.loading);
        ui.set_trash_empty(in_trash && state.trash_items.is_empty());
    }

    /// Updates the header, sidebar selection, and title for the current location.
    pub(super) fn sync_location(&self) {
        let state = self.state.borrow();
        let location = state.history.current().clone();
        let ui = &self.ui;
        ui.set_can_go_back(state.history.can_go_back());
        ui.set_can_go_forward(state.history.can_go_forward());
        let (crumbs, title, can_up) = match &location {
            Location::Dir(dir) => {
                let segments = pathbar::segments(dir, &self.env.home);
                let title = segments.last().map(|s| s.label.clone()).unwrap_or_else(|| "/".into());
                (convert::crumbs(&segments), title, dir.parent().is_some())
            }
            Location::Trash => (
                vec![crate::Crumb { label: "Trash".into(), kind: IconKind::Trash }],
                "Trash".to_string(),
                false,
            ),
        };
        ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
        ui.set_can_go_up(can_up);
        ui.set_window_title(format!("{title} – Files").into());
        let current = state.places.iter().position(|p| match (&p.target, &location) {
            (Target::Dir(a), Location::Dir(b)) => a == b,
            (Target::Trash, Location::Trash) => true,
            _ => false,
        });
        ui.set_current_place(current.map_or(-1, |i| i as i32));
        ui.set_location_text(match &location {
            Location::Dir(dir) => dir.to_string_lossy().as_ref().into(),
            Location::Trash => "trash:///".into(),
        });
    }

    pub(super) fn sync_places(&self) {
        let items = convert::place_items(&self.state.borrow().places);
        self.ui.set_places(ModelRc::new(VecModel::from(items)));
        self.sync_location();
    }

    pub(super) fn sync_prefs(&self) {
        let state = self.state.borrow();
        let prefs = &state.prefs;
        let ui = &self.ui;
        ui.set_list_mode(prefs.view == ViewMode::List);
        ui.set_zoom(i32::from(prefs.zoom));
        ui.set_sort_key(match prefs.sort.key {
            SortKey::Name => 0,
            SortKey::Size => 1,
            SortKey::Modified => 2,
            SortKey::Type => 3,
        });
        ui.set_sort_descending(prefs.sort.descending);
        ui.set_folders_first(prefs.sort.folders_first);
        ui.set_show_hidden(prefs.show_hidden);
        ui.set_search_recursive(state.search_recursive);
    }

    /// Applies a change to the preferences, saves them, and refreshes the view.
    pub(super) fn change_prefs(&self, change: impl FnOnce(&mut Preferences)) {
        let prefs = {
            let mut state = self.state.borrow_mut();
            change(&mut state.prefs);
            state.prefs.clone()
        };
        if let Some(path) = self.env.prefs_path.clone() {
            // Saving is a small write, but it still belongs off the UI thread.
            std::thread::spawn(move || {
                if let Err(error) = prefs.save_to(&path) {
                    tracing::warn!("couldn't save preferences: {error}");
                }
            });
        }
        self.sync_prefs();
        self.rebuild_view();
    }

    pub(super) fn request_thumbnail(&self, index: usize) {
        let mut state = self.state.borrow_mut();
        let Some(entry) = state.view.get(index) else { return };
        if entry.kind != EntryKind::File || !mime::is_thumbnailable(&entry.mime) {
            return;
        }
        let (path, mime) = (entry.path.clone(), entry.mime.clone());
        if state.thumbnails.contains_key(&path)
            || state.thumbnail_failures.contains(&path)
            || !state.thumbnails_requested.insert(path.clone())
        {
            return;
        }
        drop(state);
        // Grid icons at the largest zoom are 96 logical pixels; thumbnails may be wider than that.
        let logical = if self.ui.get_list_mode() { 20.0 } else { 104.0 };
        let size = ThumbnailSize::for_display(logical, self.ui.window().scale_factor());
        self.thumbnails.request(path, mime, size);
    }

    fn on_thumbnail(
        &self,
        path: PathBuf,
        pixels: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
    ) {
        let index = {
            let mut state = self.state.borrow_mut();
            state.thumbnails_requested.remove(&path);
            let Some(&index) = state.index.get(&path) else { return };
            match pixels {
                Some(pixels) => {
                    state.thumbnails.insert(path, Image::from_rgba8(pixels));
                }
                None => {
                    state.thumbnail_failures.insert(path);
                }
            }
            index
        };
        let now = format::now();
        let entry = self.state.borrow().view.get(index).cloned();
        if let Some(entry) = entry {
            let item = self.item_for(&entry, index, now);
            self.files.set_row_data(index, item);
        }
    }

    /// Runs an operation in the background.
    pub(super) fn start_job(self: &Rc<Self>, operation: Operation, select_after: bool) {
        let cancel = Arc::new(AtomicBool::new(false));
        let id = {
            let mut state = self.state.borrow_mut();
            let id = state.next_job;
            state.next_job += 1;
            state.jobs.push(JobState {
                id,
                operation: operation.clone(),
                cancel: cancel.clone(),
                started: Instant::now(),
                progress: Progress::default(),
                select_after,
            });
            id
        };
        workers::run_operation(self.tx.clone(), id, operation, self.trash(), cancel);
        let weak = Rc::downgrade(self);
        self.jobs_timer.start(slint::TimerMode::Repeated, Duration::from_millis(200), move || {
            if let Some(this) = weak.upgrade() {
                this.sync_jobs();
            }
        });
    }

    fn on_job_progress(&self, id: i32, progress: Progress) {
        if let Some(job) = self.state.borrow_mut().jobs.iter_mut().find(|j| j.id == id) {
            job.progress = progress;
        }
        self.sync_jobs();
    }

    pub(super) fn sync_jobs(&self) {
        let state = self.state.borrow();
        let items: Vec<JobItem> = state
            .jobs
            .iter()
            .filter(|job| !job.operation.is_instant() && job.started.elapsed() >= JOB_PANEL_DELAY)
            .map(job_item)
            .collect();
        if state.jobs.is_empty() {
            self.jobs_timer.stop();
        }
        drop(state);
        if items.len() != self.jobs_model.row_count() {
            self.jobs_model.set_vec(items);
        } else {
            for (i, item) in items.into_iter().enumerate() {
                self.jobs_model.set_row_data(i, item);
            }
        }
    }

    pub(super) fn cancel_job(&self, id: i32) {
        let mut state = self.state.borrow_mut();
        if let Some(job) = state.jobs.iter().find(|j| j.id == id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
        // A conflict waiting for this job is answered by the cancellation.
        let before = state.conflicts.len();
        state.conflicts.retain(|c| c.job != id);
        let removed_front = before != state.conflicts.len();
        drop(state);
        if removed_front && matches!(self.state.borrow().dialog, Dialog::Conflict) {
            self.close_dialog();
            self.show_next_conflict();
        }
    }

    fn on_job_done(self: &Rc<Self>, id: i32, outcome: crate::core::ops::Outcome) {
        let job = {
            let mut state = self.state.borrow_mut();
            let Some(pos) = state.jobs.iter().position(|j| j.id == id) else { return };
            state.conflicts.retain(|c| c.job != id);
            state.jobs.remove(pos)
        };
        if matches!(self.state.borrow().dialog, Dialog::Conflict) {
            if self.state.borrow().conflicts.is_empty() {
                self.close_dialog();
            } else {
                self.show_next_conflict();
            }
        }
        self.sync_jobs();
        if job.select_after && !outcome.created.is_empty() {
            self.state.borrow_mut().pending_select = outcome.created.clone();
        }
        let summary = job.operation.summary(&outcome);
        let undo = matches!(job.operation, Operation::Trash { .. })
            && !outcome.trashed.is_empty()
            && outcome.errors.is_empty();
        if let Some(text) = summary {
            if undo {
                self.show_toast(
                    text,
                    ToastAction::UndoTrash(outcome.trashed.clone()),
                    Some("Undo"),
                );
            } else {
                self.show_toast(text, ToastAction::None, None);
            }
        }
        self.reload();
    }

    pub(super) fn show_toast(&self, text: String, action: ToastAction, label: Option<&str>) {
        self.state.borrow_mut().toast_action = action;
        self.ui.set_toast_text(text.into());
        self.ui.set_toast_action(label.unwrap_or_default().into());
        self.ui.set_toast_visible(true);
        let weak = self.ui.as_weak();
        self.toast_timer.start(slint::TimerMode::SingleShot, TOAST_DURATION, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_toast_visible(false);
            }
        });
    }

    pub(super) fn hide_toast(&self) {
        self.toast_timer.stop();
        self.ui.set_toast_visible(false);
        self.state.borrow_mut().toast_action = ToastAction::None;
    }

    pub(super) fn close_dialog(&self) {
        let previous = std::mem::replace(&mut self.state.borrow_mut().dialog, Dialog::None);
        if let Dialog::Properties { cancel, .. } = previous {
            cancel.store(true, Ordering::Relaxed);
        }
        self.ui.set_dialog(DialogKind::None);
        self.ui.set_dialog_error("".into());
        self.ui.invoke_focus_view();
    }

    pub(super) fn show_next_conflict(&self) {
        let state = self.state.borrow();
        if !matches!(state.dialog, Dialog::None | Dialog::Conflict) {
            return;
        }
        let Some(next) = state.conflicts.front() else { return };
        let name = next
            .conflict
            .target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let folder =
            next.conflict.target.parent().map(|p| self.pretty_dir(Some(p))).unwrap_or_default();
        let merge = next.conflict.is_merge();
        let (title, message) = if merge {
            (
                format!("Merge Folder “{name}”?"),
                format!(
                    "A folder with this name already exists in “{folder}”. Merging adds the incoming files to it."
                ),
            )
        } else {
            (
                format!("Replace “{name}”?"),
                format!(
                    "An item with this name already exists in “{folder}”. Replacing it overwrites its content."
                ),
            )
        };
        let ui = &self.ui;
        ui.set_dialog_title(title.into());
        ui.set_dialog_message(message.into());
        ui.set_conflict_existing(next.details.existing.as_str().into());
        ui.set_conflict_incoming(next.details.incoming.as_str().into());
        ui.set_conflict_merge(merge);
        drop(state);
        self.state.borrow_mut().dialog = Dialog::Conflict;
        // Re-creates the dialog so a second conflict starts with a fresh "apply to all" box.
        ui.set_dialog(DialogKind::None);
        ui.set_dialog(DialogKind::Conflict);
    }

    pub(super) fn resolve_conflict(&self, decision: Option<Decision>) {
        let pending = self.state.borrow_mut().conflicts.pop_front();
        if let Some(pending) = pending {
            let _ = pending.reply.send(decision);
            if decision.is_none() {
                self.cancel_job(pending.job);
            }
        }
        self.state.borrow_mut().dialog = Dialog::None;
        self.ui.set_dialog(DialogKind::None);
        if self.state.borrow().conflicts.is_empty() {
            self.ui.invoke_focus_view();
        } else {
            self.show_next_conflict();
        }
    }

    fn on_properties(&self, id: u64, rows: Vec<(String, String)>, done: bool) {
        if !matches!(self.state.borrow().dialog, Dialog::Properties { id: current, .. } if current == id)
        {
            return;
        }
        let rows: Vec<crate::PropertyRow> = rows
            .into_iter()
            .map(|(label, value)| crate::PropertyRow { label: label.into(), value: value.into() })
            .collect();
        self.ui.set_props_rows(ModelRc::new(VecModel::from(rows)));
        self.ui.set_props_computing(!done);
    }

    fn on_apps(&self, id: u64, apps: Vec<AppInfo>, default: Option<&str>) {
        let mut state = self.state.borrow_mut();
        let Dialog::OpenWith { id: current, apps: stored, .. } = &mut state.dialog else { return };
        if *current != id {
            return;
        }
        let choices: Vec<crate::AppChoice> = apps
            .iter()
            .map(|app| crate::AppChoice {
                name: app.name.as_str().into(),
                detail: if Some(app.id.as_str()) == default {
                    "Default application".into()
                } else {
                    app.id.trim_end_matches(".desktop").into()
                },
            })
            .collect();
        *stored = apps;
        drop(state);
        self.ui.set_apps(ModelRc::new(VecModel::from(choices)));
        self.ui.set_apps_loading(false);
    }

    /// Whether new files can be created in the current folder.
    pub(super) fn writable(&self) -> bool {
        let state = self.state.borrow();
        state.writable && *state.history.current() != Location::Trash
    }
}

/// Forgets thumbnails of files whose size or modification time changed, so they're generated again.
fn invalidate_changed_thumbnails(state: &mut State, new_view: &[FileEntry]) {
    if state.thumbnails.is_empty() && state.thumbnail_failures.is_empty() {
        return;
    }
    let old: HashMap<&PathBuf, (Option<SystemTime>, u64)> =
        state.view.iter().map(|e| (&e.path, (e.modified, e.size))).collect();
    let stale: Vec<PathBuf> = new_view
        .iter()
        .filter(|e| {
            old.get(&e.path)
                .is_some_and(|&(modified, size)| modified != e.modified || size != e.size)
        })
        .map(|e| e.path.clone())
        .collect();
    for path in stale {
        state.thumbnails.remove(&path);
        state.thumbnail_failures.remove(&path);
        state.thumbnails_requested.remove(&path);
    }
}

fn job_item(job: &JobState) -> JobItem {
    let p = &job.progress;
    let mut detail = if p.total_bytes > 0 {
        format!("{} of {}", format::size(p.done_bytes), format::size(p.total_bytes))
    } else if p.total_items > 0 {
        format!("{} of {}", p.done_items.min(p.total_items), format::item_count(p.total_items))
    } else {
        "Preparing…".to_string()
    };
    if !p.current.is_empty() {
        detail.push_str(" · ");
        detail.push_str(&p.current);
    }
    JobItem {
        id: job.id,
        title: job.operation.title().into(),
        detail: detail.into(),
        progress: p.fraction(),
        indeterminate: p.total_bytes == 0 && p.total_items == 0,
    }
}

/// A trashed item as a view row; its date is the deletion date.
pub(super) fn trash_entry(item: &TrashItem) -> FileEntry {
    let mime = if item.is_dir {
        mime::DIRECTORY.to_string()
    } else {
        mime::guess(Path::new(&item.name), false)
    };
    let modified = item.deleted.and_then(|d| {
        use chrono::TimeZone as _;
        chrono::Local.from_local_datetime(&d).earliest().map(SystemTime::from)
    });
    FileEntry {
        path: item.file.clone(),
        name: item.name.clone(),
        kind: if item.is_dir { EntryKind::Directory } else { EntryKind::File },
        is_symlink: false,
        size: if item.is_dir { 0 } else { item.size },
        modified,
        category: mime::category(&mime),
        mime,
        mode: 0o644,
        child_count: None,
        hidden: false,
    }
}
