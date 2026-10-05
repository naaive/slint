// SPDX-License-Identifier: MIT

use crate::{Config, Error};
use std::path::Path;

/// Keeps a file watch alive; dropping it stops the callbacks.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// Calls `on_change` with the freshly loaded configuration whenever `path` changes.
///
/// Watches the parent directory, so editors that save by renaming and files created later both work.
/// The callback runs on the watcher's thread; invalid files are logged and skipped.
pub fn watch(path: &Path, on_change: impl Fn(Config) + Send + 'static) -> Result<ConfigWatcher, Error> {
    let _ = (path, on_change);
    todo!("implemented by the nimbus-config agent")
}
