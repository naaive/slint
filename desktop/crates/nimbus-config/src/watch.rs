// SPDX-License-Identifier: MIT

use crate::{Config, Error};
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Editors and `Config::save_to` touch the file several times per save.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// Keeps a file watch alive; dropping it stops the callbacks.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// Calls `on_change` with the freshly loaded configuration whenever `path` changes.
///
/// Watches the parent directory, so editors that save by renaming and files created later both work.
/// The callback runs on the watcher's thread; invalid files are logged and skipped.
pub fn watch(
    path: &Path,
    on_change: impl Fn(Config) + Send + 'static,
) -> Result<ConfigWatcher, Error> {
    let path = path.to_path_buf();
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let file_name = path.file_name().map(OsString::from).ok_or_else(|| Error::Io {
        path: path.clone(),
        source: std::io::ErrorKind::InvalidInput.into(),
    })?;
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io { path: dir.clone(), source })?;

    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result {
            Ok(event) if is_relevant(&event, &file_name) => {
                // The receiver only disappears while the watcher is being dropped.
                let _ = tx.send(());
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("watching the Nimbus configuration failed: {error}"),
        })
        .map_err(|e| notify_error(&dir, e))?;
    watcher.watch(&dir, RecursiveMode::NonRecursive).map_err(|e| notify_error(&dir, e))?;

    let baseline = Config::load_from(&path).ok();
    std::thread::Builder::new()
        .name("nimbus-config-watch".into())
        .spawn(move || reload_loop(&path, &rx, baseline, on_change))
        .map_err(|source| Error::Io { path: dir, source })?;

    Ok(ConfigWatcher { _watcher: watcher })
}

/// Runs until the watcher, and with it the channel's sender, is dropped.
fn reload_loop(
    path: &Path,
    rx: &mpsc::Receiver<()>,
    mut last: Option<Config>,
    on_change: impl Fn(Config),
) {
    while rx.recv().is_ok() {
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(()) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        match Config::load_from(path) {
            Ok(config) if last.as_ref() == Some(&config) => {}
            Ok(config) => {
                last = Some(config.clone());
                on_change(config);
            }
            Err(error) => tracing::warn!("ignoring configuration change: {error}"),
        }
    }
}

/// Reading the file ourselves produces open and close events, which must not trigger another reload.
fn is_relevant(event: &notify::Event, file_name: &OsString) -> bool {
    let kind_matters = match event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    };
    kind_matters && event.paths.iter().any(|p| p.file_name() == Some(file_name.as_os_str()))
}

fn notify_error(dir: &Path, error: notify::Error) -> Error {
    let source = match error.kind {
        notify::ErrorKind::Io(io) => io,
        other => std::io::Error::other(format!("{other:?}")),
    };
    Error::Io { path: dir.to_path_buf(), source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    const WAIT: Duration = Duration::from_secs(5);

    fn start(path: &Path) -> (ConfigWatcher, Receiver<Config>) {
        let (tx, rx) = mpsc::channel();
        let watcher = watch(path, move |config| {
            let _ = tx.send(config);
        })
        .unwrap();
        (watcher, rx)
    }

    fn assert_quiet(rx: &Receiver<Config>) {
        assert!(rx.recv_timeout(DEBOUNCE * 5).is_err(), "unexpected callback");
    }

    #[test]
    fn write_triggers_callback_and_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nimbus/config.toml");
        let (_watcher, rx) = start(&path);
        assert!(path.parent().unwrap().is_dir());

        std::fs::write(&path, "[panel]\nheight = 48\n").unwrap();
        let config = rx.recv_timeout(WAIT).unwrap();
        assert_eq!(config.panel.height, 48);
        assert_quiet(&rx);
    }

    #[test]
    fn atomic_rename_save_triggers_callback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save_to(&path).unwrap();
        let (_watcher, rx) = start(&path);

        let config = Config { favorites: vec!["org.nimbus.Files".into()], ..Config::default() };
        config.save_to(&path).unwrap();
        assert_eq!(rx.recv_timeout(WAIT).unwrap(), config);
    }

    #[test]
    fn invalid_file_is_skipped_until_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (_watcher, rx) = start(&path);

        std::fs::write(&path, "[panel\nheight = ").unwrap();
        assert_quiet(&rx);
        std::fs::write(&path, "[workspaces]\ncount = 6\n").unwrap();
        assert_eq!(rx.recv_timeout(WAIT).unwrap().workspaces.count, 6);
    }

    #[test]
    fn unchanged_content_and_other_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[panel]\nheight = 40\n").unwrap();
        let (_watcher, rx) = start(&path);

        std::fs::write(&path, "# same values\n[panel]\nheight = 40\n").unwrap();
        std::fs::write(dir.path().join("other.toml"), "[panel]\nheight = 99\n").unwrap();
        assert_quiet(&rx);
    }

    #[test]
    fn dropping_the_watcher_stops_callbacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let (watcher, rx) = start(&path);
        drop(watcher);
        std::fs::write(&path, "[panel]\nheight = 50\n").unwrap();
        assert_quiet(&rx);
    }
}
