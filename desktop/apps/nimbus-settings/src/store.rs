// SPDX-License-Identifier: MIT

//! The configuration model: the loaded [`Config`], edits with debounced background saves,
//! and reconciliation with changes made by other programs.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nimbus_config::Config;

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

enum Message {
    Save(Box<Config>),
    Flush(mpsc::Sender<()>),
}

/// The configuration being edited.
///
/// Lives on the UI thread; writes happen on a background thread so the UI never waits for the disk.
pub struct ConfigStore {
    path: PathBuf,
    config: Config,
    /// The configuration known to be on disk, shared with the saver.
    on_disk: Arc<Mutex<Config>>,
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
        let on_disk = Arc::new(Mutex::new(config.clone()));
        let (sender, receiver) = mpsc::channel();
        let saver = Saver {
            path: path.to_path_buf(),
            debounce,
            max_delay: MAX_DELAY.max(debounce),
            on_disk: on_disk.clone(),
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
        (Self { path: path.to_path_buf(), config, on_disk, sender, thread }, error)
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
        self.config = config;
        match &self.sender {
            Some(sender) if sender.send(Message::Save(Box::new(self.config.clone()))).is_ok() => {}
            _ => tracing::error!("the configuration saver isn't running; changes aren't saved"),
        }
        true
    }

    /// Reconciles a configuration that was loaded after the file changed; returns whether the UI needs a refresh.
    ///
    /// Echoes of this store's own writes are ignored.
    /// While local edits wait to be written they win, since the user made them last.
    pub fn external_change(&mut self, config: Config) -> bool {
        let mut on_disk = self.on_disk.lock().unwrap_or_else(PoisonError::into_inner);
        if config == *on_disk || config == self.config {
            *on_disk = config;
            return false;
        }
        if self.config != *on_disk {
            return false;
        }
        *on_disk = config.clone();
        drop(on_disk);
        self.config = config;
        true
    }

    /// Whether edits are waiting to be written.
    pub fn has_unsaved_changes(&self) -> bool {
        self.config != *self.on_disk.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes pending edits now, waiting at most `timeout`.
    pub fn flush(&self, timeout: Duration) {
        let Some(sender) = &self.sender else { return };
        let (ack, done) = mpsc::channel();
        if sender.send(Message::Flush(ack)).is_ok() && done.recv_timeout(timeout).is_err() {
            tracing::warn!("saving the configuration took longer than {timeout:?}");
        }
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

struct Saver {
    path: PathBuf,
    debounce: Duration,
    max_delay: Duration,
    on_disk: Arc<Mutex<Config>>,
    backup: Option<PathBuf>,
    on_status: Box<dyn Fn(SaveStatus) + Send>,
}

impl Saver {
    fn run(mut self, receiver: &mpsc::Receiver<Message>) {
        let mut pending: Option<(Config, Instant)> = None;
        loop {
            let message = match &pending {
                None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some((_, since)) => {
                    let wait = self.debounce.min(self.max_delay.saturating_sub(since.elapsed()));
                    receiver.recv_timeout(wait)
                }
            };
            match message {
                Ok(Message::Save(config)) => {
                    let since = pending.map_or_else(Instant::now, |(_, since)| since);
                    pending = Some((*config, since));
                }
                Ok(Message::Flush(ack)) => {
                    if let Some((config, _)) = pending.take() {
                        self.write(config);
                    }
                    let _ = ack.send(());
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some((config, _)) = pending.take() {
                        self.write(config);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if let Some((config, _)) = pending.take() {
                        self.write(config);
                    }
                    return;
                }
            }
        }
    }

    fn write(&mut self, config: Config) {
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
        // Recorded before the write, so the watcher's echo is recognized however fast it arrives.
        let previous = std::mem::replace(
            &mut *self.on_disk.lock().unwrap_or_else(PoisonError::into_inner),
            config.clone(),
        );
        match config.save_to(&self.path) {
            Ok(()) => (self.on_status)(SaveStatus::Saved),
            Err(error) => {
                *self.on_disk.lock().unwrap_or_else(PoisonError::into_inner) = previous;
                tracing::error!("{error}");
                (self.on_status)(SaveStatus::Failed(error.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut newer = Config::default();
        newer.workspaces.count = 7;
        assert!(!store.external_change(newer), "pending local edits win");
        assert!(!store.config().panel.show_dock);
        assert_eq!(store.config().workspaces.count, 4);
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
