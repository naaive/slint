// SPDX-License-Identifier: MIT

//! Load-modify-save cycles that don't lose concurrent edits from other programs.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};

use crate::{Config, Error};

/// Loads `path`, applies `edit`, and saves the result, returning what it saved.
///
/// The whole cycle holds an advisory lock on `<path>.lock`,
/// so programs that all edit the file through `update` never lose each other's changes.
/// A file that doesn't parse is left alone and its error returned, so the user's other settings survive.
pub fn update(path: &Path, edit: impl FnOnce(&mut Config)) -> Result<Config, Error> {
    let _lock = lock(path)?;
    let mut config = Config::load_from(path)?;
    edit(&mut config);
    config.save_to(path)?;
    Ok(config)
}

/// Like [`update`], but `edit` receives the result of loading `path` and returns what to save.
///
/// Returning `None` saves nothing.
/// Use this to recover from a file that doesn't parse, for example by replacing it.
pub fn update_with(
    path: &Path,
    edit: impl FnOnce(Result<Config, Error>) -> Option<Config>,
) -> Result<Option<Config>, Error> {
    let _lock = lock(path)?;
    let Some(config) = edit(Config::load_from(path)) else { return Ok(None) };
    config.save_to(path)?;
    Ok(Some(config))
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.file_name().unwrap_or_default());
    name.push(".lock");
    path.with_file_name(name)
}

/// Opens `<path>.lock` and blocks until it holds an exclusive lock on it, which lasts until the file is closed.
///
/// `flock` locks an inode, and [`Config::save_to`] replaces the inode of `path`, so the lock is on a sibling.
fn lock(path: &Path) -> Result<File, Error> {
    let lock_path = lock_path(path);
    let io = |source| Error::Io { path: lock_path.clone(), source };
    std::fs::create_dir_all(crate::parent_dir(path)).map_err(io)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io)?;
    loop {
        match flock(&file, FlockOperation::LockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::INTR) => continue,
            Err(errno) => return Err(io(errno.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColorScheme;

    #[test]
    fn concurrent_updates_keep_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nimbus/config.toml");
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        update(&path, |c| c.favorites.push(format!("app-{t}-{i}"))).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let saved = Config::load_from(&path).unwrap();
        let added = saved.favorites.iter().filter(|f| f.starts_with("app-")).count();
        assert_eq!(added, 80, "{:?}", saved.favorites);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name != "config.toml" && name != "config.toml.lock")
            .collect();
        assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
    }

    #[test]
    fn updates_keep_foreign_edits_to_other_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\n[panel]\nheight = 40\n").unwrap();
        let saved = update(&path, |c| c.appearance.color_scheme = ColorScheme::Dark).unwrap();
        assert_eq!(saved.panel.height, 40);
        assert_eq!(Config::load_from(&path).unwrap(), saved);
        assert!(std::fs::read_to_string(&path).unwrap().contains("# mine\n"));
    }

    #[test]
    fn invalid_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "panel = [not toml").unwrap();
        let error = update(&path, |c| c.favorites.clear()).unwrap_err();
        assert!(matches!(error, Error::Parse { .. }), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "panel = [not toml");

        let replaced = update_with(&path, |loaded| {
            assert!(loaded.is_err());
            Some(Config::default())
        })
        .unwrap();
        assert_eq!(replaced, Some(Config::default()));
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
        assert_eq!(update_with(&path, |_| None).unwrap(), None);
    }

    #[test]
    fn lock_files_sit_next_to_the_configuration() {
        assert_eq!(lock_path(Path::new("/a/config.toml")), Path::new("/a/config.toml.lock"));
        assert_eq!(lock_path(Path::new("config.toml")), Path::new("config.toml.lock"));
    }
}
