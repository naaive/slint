// SPDX-License-Identifier: MIT

//! Directory listing.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::mime::{self, Category};

/// Folders larger than this report their item count as at least this many.
const MAX_COUNTED_CHILDREN: u64 = 10_000;
/// A listing sniffs at most this many files, so folders full of extensionless files stay fast.
const SNIFF_BUDGET: usize = 256;
/// A listing counts the children of at most this many subfolders.
const COUNT_BUDGET: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    /// Devices, sockets, pipes, and broken symlinks.
    Other,
}

/// One file or folder as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: PathBuf,
    /// The file name, lossily converted to UTF-8 for display.
    pub name: String,
    pub kind: EntryKind,
    pub is_symlink: bool,
    /// Size in bytes; zero for folders.
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mime: String,
    pub category: Category,
    /// Unix permission bits.
    pub mode: u32,
    /// The number of entries in a folder, when it could be read.
    pub child_count: Option<u64>,
    /// Hidden by a leading dot, a trailing tilde, or the folder's `.hidden` file.
    pub hidden: bool,
}

impl FileEntry {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    pub fn is_executable(&self) -> bool {
        self.kind == EntryKind::File && self.mode & 0o111 != 0
    }
}

/// What to compute beyond the cheap metadata.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListOptions {
    /// Reads the content of files whose name doesn't reveal their type.
    pub sniff: bool,
    /// Counts the entries of each subfolder.
    pub count_children: bool,
}

/// Whether a file name is hidden by convention.
pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || name.ends_with('~')
}

/// Describes one path, following symlinks for the type and size but remembering that it's a link.
pub fn read_entry(path: &Path, options: ListOptions) -> io::Result<FileEntry> {
    let link_meta = fs::symlink_metadata(path)?;
    let is_symlink = link_meta.file_type().is_symlink();
    let meta = if is_symlink { fs::metadata(path).ok() } else { Some(link_meta.clone()) };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    Ok(build(path.to_path_buf(), name, is_symlink, meta.as_ref(), &link_meta, options))
}

fn build(
    path: PathBuf,
    name: String,
    is_symlink: bool,
    meta: Option<&Metadata>,
    link_meta: &Metadata,
    options: ListOptions,
) -> FileEntry {
    let (kind, mime) = match meta {
        Some(m) if m.is_dir() => (EntryKind::Directory, mime::DIRECTORY.to_string()),
        Some(m) if m.is_file() => {
            let sniff = options.sniff && m.len() > 0;
            let mut mime = mime::guess(&path, sniff);
            if mime == mime::UNKNOWN && m.permissions().mode() & 0o111 != 0 {
                mime = "application/x-executable".to_string();
            }
            (EntryKind::File, mime)
        }
        Some(_) => (EntryKind::Other, mime::UNKNOWN.to_string()),
        None => (EntryKind::Other, mime::SYMLINK_BROKEN.to_string()),
    };
    let meta = meta.unwrap_or(link_meta);
    let child_count = (kind == EntryKind::Directory && options.count_children)
        .then(|| count_children(&path))
        .flatten();
    FileEntry {
        category: mime::category(&mime),
        hidden: is_hidden_name(&name),
        size: if kind == EntryKind::File { meta.len() } else { 0 },
        modified: meta.modified().ok(),
        mode: meta.mode() & 0o7777,
        path,
        name,
        kind,
        is_symlink,
        mime,
        child_count,
    }
}

fn count_children(dir: &Path) -> Option<u64> {
    let entries = fs::read_dir(dir).ok()?;
    Some(entries.take(MAX_COUNTED_CHILDREN as usize).count() as u64)
}

/// Names listed in a folder's `.hidden` file, which GNOME and KDE honor.
fn hidden_file_names(dir: &Path) -> HashSet<String> {
    fs::read_to_string(dir.join(".hidden"))
        .map(|text| {
            text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
        })
        .unwrap_or_default()
}

/// Lists a folder, including hidden entries, in no particular order.
///
/// Entries that disappear while listing are skipped.
pub fn list_dir(dir: &Path, options: ListOptions) -> io::Result<Vec<FileEntry>> {
    let hidden = hidden_file_names(dir);
    let mut entries = Vec::new();
    let mut sniff_budget = SNIFF_BUDGET;
    let mut count_budget = COUNT_BUDGET;
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let path = item.path();
        let Ok(link_meta) = item.metadata() else { continue };
        let is_symlink = link_meta.file_type().is_symlink();
        let meta = if is_symlink { fs::metadata(&path).ok() } else { Some(link_meta.clone()) };
        let name = item.file_name().to_string_lossy().into_owned();
        let is_dir = meta.as_ref().is_some_and(Metadata::is_dir);
        let needs_sniff = !is_dir && mime_guess::from_path(&path).first_raw().is_none();
        let entry_options = ListOptions {
            sniff: options.sniff && needs_sniff && sniff_budget > 0,
            count_children: options.count_children && is_dir && count_budget > 0,
        };
        sniff_budget -= usize::from(entry_options.sniff);
        count_budget -= usize::from(entry_options.count_children);
        let mut entry = build(path, name, is_symlink, meta.as_ref(), &link_meta, entry_options);
        entry.hidden |= hidden.contains(&entry.name);
        entries.push(entry);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn by_name<'a>(entries: &'a [FileEntry], name: &str) -> &'a FileEntry {
        entries.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("{name} listed"))
    }

    #[test]
    fn lists_files_folders_and_links() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        fs::write(root.join("notes.txt"), "hello").expect("write");
        fs::create_dir(root.join("sub")).expect("mkdir");
        fs::write(root.join("sub/a"), "").expect("write");
        fs::write(root.join("sub/b"), "").expect("write");
        fs::write(root.join(".secret"), "").expect("write");
        fs::write(root.join("backup~"), "").expect("write");
        fs::write(root.join("listed"), "").expect("write");
        fs::write(root.join(".hidden"), "listed\n\n").expect("write");
        symlink(root.join("sub"), root.join("link")).expect("symlink");
        symlink(root.join("missing"), root.join("broken")).expect("symlink");

        let entries =
            list_dir(root, ListOptions { sniff: false, count_children: true }).expect("list");
        assert_eq!(entries.len(), 8);

        let notes = by_name(&entries, "notes.txt");
        assert_eq!(notes.kind, EntryKind::File);
        assert_eq!(notes.size, 5);
        assert_eq!(notes.mime, "text/plain");
        assert_eq!(notes.category, Category::Text);
        assert!(!notes.hidden);
        assert!(notes.modified.is_some());

        let sub = by_name(&entries, "sub");
        assert!(sub.is_dir());
        assert_eq!(sub.child_count, Some(2));
        assert_eq!(sub.size, 0);

        let link = by_name(&entries, "link");
        assert!(link.is_symlink && link.is_dir());

        let broken = by_name(&entries, "broken");
        assert_eq!(broken.kind, EntryKind::Other);
        assert_eq!(broken.mime, mime::SYMLINK_BROKEN);

        assert!(by_name(&entries, ".secret").hidden);
        assert!(by_name(&entries, "backup~").hidden);
        assert!(by_name(&entries, "listed").hidden);
    }

    #[test]
    fn executables_without_extension() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tool");
        fs::write(&path, [0u8, 1, 2]).expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        let entry = read_entry(&path, ListOptions::default()).expect("entry");
        assert!(entry.is_executable());
        assert_eq!(entry.category, Category::Executable);
        assert_eq!(entry.mode, 0o755);
    }

    #[test]
    fn missing_folder_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(list_dir(&dir.path().join("nope"), ListOptions::default()).is_err());
        assert!(read_entry(&dir.path().join("nope"), ListOptions::default()).is_err());
    }
}
