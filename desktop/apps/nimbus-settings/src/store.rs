// SPDX-License-Identifier: MIT

//! The configuration model: the loaded [`Config`], edits with debounced background saves,
//! and reconciliation with changes made by other programs.
//!
//! Edits travel as [`Patch`]es rather than whole configurations.
//! Each write reloads the file and replays the unwritten patches onto it,
//! so changes other programs made in the meantime survive.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nimbus_config::Config;
use toml::{Table, Value};

/// How long edits settle before they're written.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// The longest an edit waits while further edits keep arriving, such as during a slider drag.
pub const MAX_DELAY: Duration = Duration::from_millis(1000);

/// The outcome of one background write, reported to the status callback on the saver's thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveStatus {
    Saved,
    Failed(String),
}

/// Values set or removed by one edit, keyed by their TOML path.
///
/// Paths stop at the second level, such as `panel.height` or a chord in `keybindings`.
type Patch = Vec<(Vec<String>, Option<Value>)>;

/// The deepest level [`diff`] descends to; values there are replaced whole.
const PATCH_DEPTH: usize = 2;

enum Message {
    Save(u64, Patch),
    Flush(mpsc::Sender<()>),
}

/// What the saver last wrote.
struct Written {
    config: Config,
    /// The sequence number of the newest patch on disk.
    seq: u64,
}

/// The configuration being edited.
///
/// Lives on the UI thread; writes happen on a background thread so the UI never waits for the disk.
pub struct ConfigStore {
    path: PathBuf,
    config: Config,
    /// Patches sent to the saver and not known to be written, with their sequence numbers.
    pending: Vec<(u64, Patch)>,
    next_seq: u64,
    written: Arc<Mutex<Written>>,
    sender: Option<mpsc::Sender<Message>>,
    thread: Option<JoinHandle<()>>,
}

impl ConfigStore {
    /// Loads `path` and starts the saver.
    ///
    /// An unreadable or invalid file yields the defaults and the error message.
    /// The invalid file is then copied to `<path>.bak` before the first save replaces it.
    pub fn open(
        path: &Path,
        debounce: Duration,
        on_status: impl Fn(SaveStatus) + Send + 'static,
    ) -> (Self, Option<String>) {
        let (config, error) = match Config::load_from(path) {
            Ok(config) => (config, None),
            Err(error) => (Config::default(), Some(error.to_string())),
        };
        let backup = error.as_ref().map(|_| path.with_extension("toml.bak"));
        let written = Arc::new(Mutex::new(Written { config: config.clone(), seq: 0 }));
        let (sender, receiver) = mpsc::channel();
        let saver = Saver {
            path: path.to_path_buf(),
            debounce,
            max_delay: MAX_DELAY.max(debounce),
            written: written.clone(),
            backup,
            on_status: Box::new(on_status),
        };
        let thread = std::thread::Builder::new()
            .name("nimbus-settings-save".into())
            .spawn(move || saver.run(&receiver));
        let (sender, thread) = match thread {
            Ok(thread) => (Some(sender), Some(thread)),
            Err(error) => {
                tracing::error!("cannot start the configuration saver: {error}");
                (None, None)
            }
        };
        let store = Self {
            path: path.to_path_buf(),
            config,
            pending: Vec::new(),
            next_seq: 1,
            written,
            sender,
            thread,
        };
        (store, error)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Applies `edit` and schedules a save if it changed anything; returns whether it did.
    pub fn update(&mut self, edit: impl FnOnce(&mut Config)) -> bool {
        let mut next = self.config.clone();
        edit(&mut next);
        self.replace(next)
    }

    /// Replaces the whole configuration, scheduling a save if it differs; returns whether it did.
    pub fn replace(&mut self, config: Config) -> bool {
        if config == self.config {
            return false;
        }
        let patch = diff(&self.config, &config);
        self.config = config;
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push((seq, patch.clone()));
        match &self.sender {
            Some(sender) if sender.send(Message::Save(seq, patch)).is_ok() => {}
            _ => tracing::error!("the configuration saver isn't running; changes aren't saved"),
        }
        true
    }

    /// Reconciles a configuration that was loaded after the file changed; returns whether the UI needs a refresh.
    ///
    /// Edits that wait to be written are replayed on top of `config`, since the user made them last.
    pub fn external_change(&mut self, config: Config) -> bool {
        self.forget_written();
        let next = apply(&config, self.pending.iter().map(|(_, patch)| patch)).unwrap_or(config);
        if next == self.config {
            return false;
        }
        self.config = next;
        true
    }

    /// Whether edits are waiting to be written.
    pub fn has_unsaved_changes(&self) -> bool {
        let seq = self.written.lock().unwrap_or_else(PoisonError::into_inner).seq;
        self.pending.iter().any(|(pending, _)| *pending > seq)
    }

    /// Writes pending edits now, waiting at most `timeout`.
    pub fn flush(&self, timeout: Duration) {
        let Some(sender) = &self.sender else { return };
        let (ack, done) = mpsc::channel();
        if sender.send(Message::Flush(ack)).is_ok() && done.recv_timeout(timeout).is_err() {
            tracing::warn!("saving the configuration took longer than {timeout:?}");
        }
    }

    fn forget_written(&mut self) {
        let seq = self.written.lock().unwrap_or_else(PoisonError::into_inner).seq;
        self.pending.retain(|(pending, _)| *pending > seq);
    }
}

impl Drop for ConfigStore {
    /// Writes pending edits before returning.
    fn drop(&mut self) {
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn to_table(config: &Config) -> Option<Table> {
    match Value::try_from(config) {
        Ok(Value::Table(table)) => Some(table),
        Ok(_) => None,
        Err(error) => {
            tracing::error!("cannot serialize the configuration: {error}");
            None
        }
    }
}

/// Returns the patch that turns `from` into `to`.
fn diff(from: &Config, to: &Config) -> Patch {
    fn walk(from: &Table, to: &Table, path: &mut Vec<String>, patch: &mut Patch) {
        let keys = from.keys().chain(to.keys().filter(|key| !from.contains_key(*key)));
        for key in keys {
            let (old, new) = (from.get(key), to.get(key));
            if old == new {
                continue;
            }
            path.push(key.clone());
            match (old, new) {
                (Some(Value::Table(old)), Some(Value::Table(new))) if path.len() < PATCH_DEPTH => {
                    walk(old, new, path, patch);
                }
                _ => patch.push((path.clone(), new.cloned())),
            }
            path.pop();
        }
    }
    let mut patch = Patch::new();
    if let (Some(from), Some(to)) = (to_table(from), to_table(to)) {
        walk(&from, &to, &mut Vec::new(), &mut patch);
    }
    patch
}

/// Replays `patches` onto `config`, or returns `None` if the result isn't a valid configuration.
fn apply<'a>(config: &Config, patches: impl IntoIterator<Item = &'a Patch>) -> Option<Config> {
    let mut root = to_table(config)?;
    for (path, value) in patches.into_iter().flatten() {
        let Some((last, parents)) = path.split_last() else { continue };
        let mut table = &mut root;
        for key in parents {
            let entry = table.entry(key.clone()).or_insert_with(|| Value::Table(Table::new()));
            if !entry.is_table() {
                *entry = Value::Table(Table::new());
            }
            let Value::Table(inner) = entry else { unreachable!() };
            table = inner;
        }
        match value {
            Some(value) => table.insert(last.clone(), value.clone()),
            None => table.remove(last),
        };
    }
    match Value::Table(root).try_into() {
        Ok(config) => Some(config),
        Err(error) => {
            tracing::warn!("cannot replay edits onto the configuration: {error}");
            None
        }
    }
}

struct Saver {
    path: PathBuf,
    debounce: Duration,
    max_delay: Duration,
    written: Arc<Mutex<Written>>,
    backup: Option<PathBuf>,
    on_status: Box<dyn Fn(SaveStatus) + Send>,
}

impl Saver {
    fn run(mut self, receiver: &mpsc::Receiver<Message>) {
        // Patches stay queued after a failed write, and go out with the next one.
        let mut queue: Vec<(u64, Patch)> = Vec::new();
        let mut since: Option<Instant> = None;
        loop {
            let message = match since {
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(since) => {
                    let wait = self.debounce.min(self.max_delay.saturating_sub(since.elapsed()));
                    receiver.recv_timeout(wait)
                }
            };
            match message {
                Ok(Message::Save(seq, patch)) => {
                    queue.push((seq, patch));
                    since.get_or_insert_with(Instant::now);
                }
                Ok(Message::Flush(ack)) => {
                    since = None;
                    self.write(&mut queue);
                    let _ = ack.send(());
                }
                Err(RecvTimeoutError::Timeout) => {
                    since = None;
                    self.write(&mut queue);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.write(&mut queue);
                    return;
                }
            }
        }
    }

    /// Replays `queue` onto the current file and saves the result, emptying `queue` on success.
    fn write(&mut self, queue: &mut Vec<(u64, Patch)>) {
        let Some(&(seq, _)) = queue.last() else { return };
        if let Some(backup) = self.backup.take()
            && let Err(error) = std::fs::copy(&self.path, &backup)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "cannot back up {} to {}: {error}",
                self.path.display(),
                backup.display()
            );
        }
        let patches = || queue.iter().map(|(_, patch)| patch);
        let config = Config::load_from(&self.path)
            .ok()
            .and_then(|current| apply(&current, patches()))
            .or_else(|| {
                let written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
                apply(&written.config, patches())
            });
        let Some(config) = config else {
            queue.clear();
            (self.on_status)(SaveStatus::Failed(
                "the edits don't form a valid configuration".into(),
            ));
            return;
        };
        match config.save_to(&self.path) {
            Ok(()) => {
                *self.written.lock().unwrap_or_else(PoisonError::into_inner) =
                    Written { config, seq };
                queue.clear();
                (self.on_status)(SaveStatus::Saved);
            }
            Err(error) => {
                tracing::error!("{error}");
                (self.on_status)(SaveStatus::Failed(error.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_config::Panel;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SHORT: Duration = Duration::from_millis(40);

    fn counting(path: &Path, debounce: Duration) -> (ConfigStore, Arc<AtomicUsize>) {
        let saves = Arc::new(AtomicUsize::new(0));
        let counter = saves.clone();
        let (store, error) = ConfigStore::open(path, debounce, move |status| {
            if status == SaveStatus::Saved {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        assert_eq!(error, None);
        (store, saves)
    }

    #[test]
    fn edits_are_debounced_into_one_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nimbus/config.toml");
        let (mut store, saves) = counting(&path, SHORT);
        assert_eq!(store.config(), &Config::default());
        for height in 33..40 {
            assert!(store.update(|c| c.panel.height = height));
        }
        assert!(!store.update(|c| c.panel.height = 39), "unchanged edits are no-ops");
        assert!(store.has_unsaved_changes());
        std::thread::sleep(SHORT * 6);
        assert_eq!(saves.load(Ordering::SeqCst), 1);
        assert_eq!(Config::load_from(&path).unwrap().panel.height, 39);
        assert!(!store.has_unsaved_changes());
    }

    #[test]
    fn continuous_edits_are_written_within_the_maximum_delay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (mut store, saves) = counting(&path, Duration::from_millis(200));
        let start = Instant::now();
        let mut gaps = 0;
        while start.elapsed() < MAX_DELAY + Duration::from_millis(400) {
            gaps += 1;
            store.update(|c| c.workspaces.gaps = gaps);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(saves.load(Ordering::SeqCst) >= 1, "a long drag still saves");
    }

    #[test]
    fn drop_and_flush_write_pending_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (mut store, saves) = counting(&path, Duration::from_secs(60));
        store.update(|c| c.power.lock_after_minutes = 3);
        store.flush(Duration::from_secs(5));
        assert_eq!(saves.load(Ordering::SeqCst), 1);
        assert_eq!(Config::load_from(&path).unwrap().power.lock_after_minutes, 3);
        store.update(|c| c.power.lock_after_minutes = 4);
        drop(store);
        assert_eq!(Config::load_from(&path).unwrap().power.lock_after_minutes, 4);
    }

    #[test]
    fn external_changes_are_reconciled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (mut store, _) = counting(&path, SHORT);

        let mut theirs = Config::default();
        theirs.panel.height = 48;
        assert!(store.external_change(theirs.clone()), "a foreign change replaces the model");
        assert_eq!(store.config().panel.height, 48);
        assert!(!store.has_unsaved_changes(), "adopting a change doesn't write it back");

        store.update(|c| c.panel.height = 40);
        store.flush(Duration::from_secs(5));
        assert!(
            !store.external_change(Config::load_from(&path).unwrap()),
            "own writes are ignored"
        );

        store.update(|c| c.panel.show_dock = false);
        let mut newer = Config::load_from(&path).unwrap();
        newer.panel.show_dock = true;
        newer.workspaces.count = 7;
        assert!(store.external_change(newer), "foreign changes merge with pending edits");
        assert!(!store.config().panel.show_dock, "pending local edits win");
        assert_eq!(store.config().workspaces.count, 7);
        assert_eq!(store.config().panel.height, 40);
    }

    #[test]
    fn foreign_changes_survive_pending_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (mut store, _) = counting(&path, Duration::from_secs(60));
        store.update(|c| c.panel.height = 40);

        let mut theirs = Config::load_from(&path).unwrap();
        theirs.workspaces.count = 7;
        theirs.favorites.push("org.example.App".into());
        theirs.save_to(&path).unwrap();
        assert!(store.external_change(theirs));
        assert_eq!(store.config().workspaces.count, 7);
        assert!(store.has_unsaved_changes());

        store.flush(Duration::from_secs(5));
        let saved = Config::load_from(&path).unwrap();
        assert_eq!(saved.panel.height, 40);
        assert_eq!(saved.workspaces.count, 7);
        assert_eq!(saved.favorites.last().map(String::as_str), Some("org.example.App"));
        assert!(!store.has_unsaved_changes());
    }

    #[test]
    fn writes_keep_changes_the_watcher_hasnt_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (mut store, _) = counting(&path, Duration::from_secs(60));
        store.update(|c| {
            c.keybindings.0.remove("Super+Q");
        });
        store.update(|c| c.power.lock_after_minutes = 3);

        let mut theirs = Config::default();
        theirs.appearance.color_scheme = nimbus_config::ColorScheme::Dark;
        theirs.keybindings.0.insert("Super+Q".into(), nimbus_config::Action::Lock);
        theirs.save_to(&path).unwrap();
        store.flush(Duration::from_secs(5));

        let saved = Config::load_from(&path).unwrap();
        assert_eq!(saved.appearance.color_scheme, nimbus_config::ColorScheme::Dark);
        assert_eq!(saved.power.lock_after_minutes, 3);
        assert!(!saved.keybindings.0.contains_key("Super+Q"), "removals are replayed");
        store.external_change(saved.clone());
        assert_eq!(store.config(), &saved);
    }

    #[test]
    fn reverting_an_edit_after_it_was_written_sticks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (mut store, _) = counting(&path, Duration::from_secs(60));
        store.update(|c| c.panel.height = 40);
        store.flush(Duration::from_secs(5));
        store.update(|c| c.panel.height = Panel::default().height);
        assert!(!store.external_change(Config::load_from(&path).unwrap()), "the late echo");
        assert_eq!(store.config().panel.height, Panel::default().height);
        store.flush(Duration::from_secs(5));
        assert_eq!(Config::load_from(&path).unwrap().panel.height, Panel::default().height);
    }

    #[test]
    fn invalid_files_are_backed_up_before_the_first_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[panel\nheight =").unwrap();
        let (mut store, error) = ConfigStore::open(&path, SHORT, |_| {});
        assert!(error.is_some());
        assert_eq!(store.config(), &Config::default());
        store.update(|c| c.panel.height = 30);
        store.flush(Duration::from_secs(5));
        assert_eq!(
            std::fs::read_to_string(path.with_extension("toml.bak")).unwrap(),
            "[panel\nheight ="
        );
        assert_eq!(Config::load_from(&path).unwrap().panel.height, 30);
    }

    #[test]
    fn failed_writes_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        let (sender, receiver) = mpsc::channel();
        let (mut store, _) =
            ConfigStore::open(&blocker.join("config.toml"), SHORT, move |status| {
                let _ = sender.send(status);
            });
        store.update(|c| c.panel.height = 30);
        let status = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(status, SaveStatus::Failed(_)), "{status:?}");
        assert!(store.has_unsaved_changes());
    }
}
