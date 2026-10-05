// SPDX-License-Identifier: MIT

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use super::entry::{EntryKind, FileEntry};
use super::mime;

/// An entry in `/x`; `size` is the item count for folders, and `age` counts seconds back from a fixed time.
pub fn entry(name: &str, dir: bool, size: u64, age: u64) -> FileEntry {
    let mime = if dir { mime::DIRECTORY.to_string() } else { mime::guess(name.as_ref(), false) };
    FileEntry {
        path: PathBuf::from("/x").join(name),
        name: name.to_string(),
        kind: if dir { EntryKind::Directory } else { EntryKind::File },
        is_symlink: false,
        size: if dir { 0 } else { size },
        modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age)),
        category: mime::category(&mime),
        mime,
        mode: 0o644,
        child_count: dir.then_some(size),
        hidden: name.starts_with('.'),
    }
}
