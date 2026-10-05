// SPDX-License-Identifier: MIT

//! Ordering and filtering of listed entries.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::entry::FileEntry;
use super::mime;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SortOptions {
    pub key: SortKey,
    pub descending: bool,
    pub folders_first: bool,
}

impl Default for SortOptions {
    fn default() -> Self {
        Self { key: SortKey::Name, descending: false, folders_first: true }
    }
}

/// Compares names the way people expect: case-insensitively, with digit runs compared by value,
/// so "file2" sorts before "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a_chars = a.chars().peekable();
    let mut b_chars = b.chars().peekable();
    loop {
        match (a_chars.peek().copied(), b_chars.peek().copied()) {
            (None, None) => break,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let x_run = take_digits(&mut a_chars);
                let y_run = take_digits(&mut b_chars);
                let x_trim = x_run.trim_start_matches('0');
                let y_trim = y_run.trim_start_matches('0');
                let ordering = x_trim
                    .len()
                    .cmp(&y_trim.len())
                    .then_with(|| x_trim.cmp(y_trim))
                    .then_with(|| y_run.len().cmp(&x_run.len()));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                let ordering = x.to_lowercase().cmp(y.to_lowercase());
                if ordering != Ordering::Equal {
                    return ordering;
                }
                a_chars.next();
                b_chars.next();
            }
        }
    }
    // Equal ignoring case: fall back to a stable, case-sensitive order.
    a.cmp(b)
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
        run.push(c);
        chars.next();
    }
    run
}

/// Leading dots don't affect the name order, so ".config" sorts next to "config".
fn sort_name(name: &str) -> &str {
    let trimmed = name.trim_start_matches('.');
    if trimmed.is_empty() { name } else { trimmed }
}

pub fn compare(a: &FileEntry, b: &FileEntry, options: SortOptions) -> Ordering {
    if options.folders_first && a.is_dir() != b.is_dir() {
        return if a.is_dir() { Ordering::Less } else { Ordering::Greater };
    }
    let by_name = || natural_cmp(sort_name(&a.name), sort_name(&b.name));
    let ordering = match options.key {
        SortKey::Name => by_name(),
        SortKey::Size => {
            let size = |e: &FileEntry| if e.is_dir() { e.child_count.unwrap_or(0) } else { e.size };
            // Folders count items, not bytes, so they never interleave with files by size.
            a.is_dir()
                .cmp(&b.is_dir())
                .reverse()
                .then_with(|| size(a).cmp(&size(b)))
                .then_with(by_name)
        }
        SortKey::Modified => a.modified.cmp(&b.modified).then_with(by_name),
        SortKey::Type => mime::describe(&a.mime)
            .cmp(&mime::describe(&b.mime))
            .then_with(|| a.mime.cmp(&b.mime))
            .then_with(by_name),
    };
    if options.descending { ordering.reverse() } else { ordering }
}

pub fn sort(entries: &mut [FileEntry], options: SortOptions) {
    entries.sort_by(|a, b| compare(a, b, options));
}

/// A case-insensitive filter where every whitespace-separated word must occur in the name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameFilter {
    words: Vec<String>,
}

impl NameFilter {
    pub fn new(query: &str) -> Self {
        Self { words: query.split_whitespace().map(str::to_lowercase).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        if self.words.is_empty() {
            return true;
        }
        let name = name.to_lowercase();
        self.words.iter().all(|w| name.contains(w.as_str()))
    }
}

/// The entries a view shows, in order.
pub fn visible(
    entries: &[FileEntry],
    show_hidden: bool,
    filter: &NameFilter,
    options: SortOptions,
) -> Vec<FileEntry> {
    let mut shown: Vec<FileEntry> = entries
        .iter()
        .filter(|e| (show_hidden || !e.hidden) && filter.matches(&e.name))
        .cloned()
        .collect();
    sort(&mut shown, options);
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testutil::entry;

    fn names(entries: &[FileEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "file02", "apple", "Banana", "file2"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["apple", "Banana", "file1", "file02", "File2", "file2", "file10"]);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
        assert_eq!(natural_cmp("a1", "a"), Ordering::Greater);
        assert_eq!(natural_cmp("x99999999999999999999999", "x1"), Ordering::Greater);
        assert_eq!(natural_cmp("Äpfel", "äpfel"), "Äpfel".cmp("äpfel"));
    }

    #[test]
    fn sorts_by_each_key() {
        let all = vec![
            entry("b.txt", false, 300, 1),
            entry("A.png", false, 100, 3),
            entry("docs", true, 5, 2),
            entry("c.png", false, 200, 0),
            entry(".hidden", false, 1, 4),
        ];
        let none = NameFilter::default();
        let mut options = SortOptions::default();
        assert_eq!(
            names(&visible(&all, false, &none, options)),
            ["docs", "A.png", "b.txt", "c.png"]
        );
        assert_eq!(
            names(&visible(&all, true, &none, options)),
            ["docs", "A.png", "b.txt", "c.png", ".hidden"]
        );

        options.key = SortKey::Size;
        assert_eq!(
            names(&visible(&all, false, &none, options)),
            ["docs", "A.png", "c.png", "b.txt"]
        );
        options.descending = true;
        options.folders_first = false;
        assert_eq!(
            names(&visible(&all, false, &none, options)),
            ["b.txt", "c.png", "A.png", "docs"]
        );

        options = SortOptions { key: SortKey::Modified, descending: true, folders_first: false };
        assert_eq!(
            names(&visible(&all, false, &none, options)),
            ["c.png", "b.txt", "docs", "A.png"]
        );

        options = SortOptions { key: SortKey::Type, descending: false, folders_first: true };
        assert_eq!(
            names(&visible(&all, false, &none, options)),
            ["docs", "A.png", "c.png", "b.txt"]
        );
    }

    #[test]
    fn filters_by_words() {
        let all = vec![entry("Holiday Photo.jpg", false, 1, 1), entry("notes.txt", false, 1, 1)];
        let filter = NameFilter::new("  photo  HOLI ");
        assert!(!filter.is_empty());
        let shown = visible(&all, false, &filter, SortOptions::default());
        assert_eq!(names(&shown), ["Holiday Photo.jpg"]);
        assert!(NameFilter::new("   ").is_empty());
        assert!(NameFilter::new("").matches("anything"));
    }
}
