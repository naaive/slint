// SPDX-License-Identifier: MIT

//! The lock marker, [`nimbus_ipc::lock_marker_path`], which `nimbus-session` checks before restarting the compositor.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::ErrorKind;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a failed marker update waits before [`LockMarker::sync`] retries it.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

pub struct LockMarker {
    path: PathBuf,
    present: bool,
    retry_at: Option<Instant>,
}

impl LockMarker {
    pub fn new(runtime_dir: &Path) -> Self {
        let path = nimbus_ipc::lock_marker_path(runtime_dir);
        let present = path.exists();
        Self { path, present, retry_at: None }
    }

    /// Whether the marker existed at startup or was created since.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// Creates or removes the marker to match `locked`.
    ///
    /// A failed update logs once and is retried at most every [`RETRY_INTERVAL`] while `locked` stays unchanged.
    pub fn sync(&mut self, locked: bool) {
        if locked == self.present {
            self.retry_at = None;
            return;
        }
        let first_attempt = self.retry_at.is_none();
        if self.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        match if locked { self.create() } else { self.remove() } {
            Ok(()) => {
                self.present = locked;
                self.retry_at = None;
            }
            Err(err) => {
                if first_attempt {
                    tracing::error!(path = %self.path.display(), locked, "cannot update the lock marker: {err}");
                }
                self.retry_at = Some(Instant::now() + RETRY_INTERVAL);
            }
        }
    }

    fn create(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
            std::fs::set_permissions(dir, Permissions::from_mode(0o700))?;
        }
        OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&self.path)?;
        Ok(())
    }

    fn remove(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(err) if err.kind() != ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn marker_follows_the_lock_state() {
        let runtime = tempfile::tempdir().unwrap();
        let path = nimbus_ipc::lock_marker_path(runtime.path());
        let mut marker = LockMarker::new(runtime.path());
        assert!(!marker.is_present());
        marker.sync(true);
        assert!(path.exists());
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        marker.sync(false);
        assert!(!path.exists());
    }

    #[test]
    fn a_marker_left_by_a_crash_is_present_at_startup() {
        let runtime = tempfile::tempdir().unwrap();
        LockMarker::new(runtime.path()).sync(true);
        let mut marker = LockMarker::new(runtime.path());
        assert!(marker.is_present());
        marker.sync(false);
        assert!(!nimbus_ipc::lock_marker_path(runtime.path()).exists());
    }

    #[test]
    fn a_failed_creation_is_retried() {
        let runtime = tempfile::tempdir().unwrap();
        let dir = nimbus_ipc::lock_marker_path(runtime.path()).parent().unwrap().to_owned();
        // A file where the marker directory belongs makes creation fail.
        std::fs::write(&dir, "").unwrap();
        let mut marker = LockMarker::new(runtime.path());
        marker.sync(true);
        assert!(!marker.is_present());

        std::fs::remove_file(&dir).unwrap();
        marker.retry_at = Some(Instant::now());
        marker.sync(true);
        assert!(marker.is_present());
        assert!(nimbus_ipc::lock_marker_path(runtime.path()).exists());
    }
}
