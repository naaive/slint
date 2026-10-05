// SPDX-License-Identifier: MIT

//! Recursive search below a folder, streamed in batches and cancellable.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::entry::{self, FileEntry, ListOptions};
use super::sort::NameFilter;

/// Results beyond this many are dropped; a search this broad needs a narrower query.
pub const MAX_RESULTS: usize = 5_000;
const BATCH_SIZE: usize = 128;
const BATCH_INTERVAL: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchEnd {
    Completed,
    /// Stopped at [`MAX_RESULTS`].
    Truncated,
    Cancelled,
}

/// Walks `root` and reports matching entries to `on_batch` as they're found.
///
/// Hidden files and folders are skipped unless `show_hidden` is set, and symlinks aren't followed.
pub fn search(
    root: &Path,
    filter: &NameFilter,
    show_hidden: bool,
    cancel: &AtomicBool,
    mut on_batch: impl FnMut(Vec<FileEntry>),
) -> SearchEnd {
    let mut batch = Vec::new();
    let mut last_flush = Instant::now();
    let mut found = 0;
    let walker =
        walkdir::WalkDir::new(root).min_depth(1).follow_links(false).into_iter().filter_entry(
            |e| show_hidden || !entry::is_hidden_name(&e.file_name().to_string_lossy()),
        );
    let mut end = SearchEnd::Completed;
    for item in walker {
        if cancel.load(Ordering::Relaxed) {
            end = SearchEnd::Cancelled;
            break;
        }
        let Ok(item) = item else { continue };
        if !filter.matches(&item.file_name().to_string_lossy()) {
            continue;
        }
        if let Ok(found_entry) = entry::read_entry(item.path(), ListOptions::default()) {
            batch.push(found_entry);
            found += 1;
        }
        if found >= MAX_RESULTS {
            end = SearchEnd::Truncated;
            break;
        }
        if batch.len() >= BATCH_SIZE || last_flush.elapsed() >= BATCH_INTERVAL && !batch.is_empty()
        {
            on_batch(std::mem::take(&mut batch));
            last_flush = Instant::now();
        }
    }
    if !batch.is_empty() && end != SearchEnd::Cancelled {
        on_batch(batch);
    }
    end
}

/// The first name at or after `start` that starts with `prefix`, ignoring case, wrapping around.
///
/// Used for type-ahead selection, where typing jumps to a matching item.
pub fn find_by_prefix(names: &[&str], prefix: &str, start: usize) -> Option<usize> {
    if prefix.is_empty() || names.is_empty() {
        return None;
    }
    let prefix = prefix.to_lowercase();
    let start = start.min(names.len());
    (start..names.len()).chain(0..start).find(|&i| names[i].to_lowercase().starts_with(&prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        for path in ["a/report.txt", "a/b/Report-2.pdf", "c/notes.md", ".git/report", "a/.report"] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, "").expect("write");
        }
        fs::create_dir_all(root.join("reports")).expect("mkdir");
        dir
    }

    fn names(root: &Path, query: &str, hidden: bool) -> (Vec<String>, SearchEnd) {
        let mut names = Vec::new();
        let end = search(root, &NameFilter::new(query), hidden, &AtomicBool::new(false), |batch| {
            names.extend(batch.into_iter().map(|e| e.name));
        });
        names.sort();
        (names, end)
    }

    #[test]
    fn finds_matches_recursively() {
        let dir = tree();
        let (found, end) = names(dir.path(), "report", false);
        assert_eq!(found, ["Report-2.pdf", "report.txt", "reports"]);
        assert_eq!(end, SearchEnd::Completed);
        let (found, _) = names(dir.path(), "report", true);
        assert_eq!(found.len(), 5);
        let (found, _) = names(dir.path(), "zzz", false);
        assert!(found.is_empty());
    }

    #[test]
    fn prefix_lookup() {
        let names = ["alpha", "Beta", "beat", "gamma"];
        assert_eq!(find_by_prefix(&names, "be", 0), Some(1));
        assert_eq!(find_by_prefix(&names, "be", 2), Some(2));
        assert_eq!(find_by_prefix(&names, "BE", 3), Some(1));
        assert_eq!(find_by_prefix(&names, "z", 0), None);
        assert_eq!(find_by_prefix(&names, "", 0), None);
        assert_eq!(find_by_prefix(&names, "a", 99), Some(0));
        assert_eq!(find_by_prefix(&[], "a", 0), None);
    }

    #[test]
    fn cancels() {
        let dir = tree();
        let cancel = AtomicBool::new(true);
        let mut batches = 0;
        let end = search(dir.path(), &NameFilter::new("r"), false, &cancel, |_| batches += 1);
        assert_eq!(end, SearchEnd::Cancelled);
        assert_eq!(batches, 0);
    }
}
