// SPDX-License-Identifier: MIT

//! Data for the properties dialog: recursive sizes and file ownership.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeepSize {
    pub bytes: u64,
    pub files: u64,
    pub folders: u64,
}

impl DeepSize {
    /// "4.2 MB (12 files, 3 folders)", or just the size for a single file.
    pub fn describe(&self) -> String {
        let size = super::format::size(self.bytes);
        let plural = |n: u64, one: &str, many: &str| {
            if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
        };
        match (self.files, self.folders) {
            (_, 0) if self.files <= 1 => size,
            (files, 0) => format!("{size} ({})", plural(files, "file", "files")),
            (files, folders) => format!(
                "{size} ({}, {})",
                plural(files, "file", "files"),
                plural(folders, "folder", "folders")
            ),
        }
    }
}

/// Sums the sizes below `paths` without following symlinks, reporting partial totals as it goes.
///
/// Returns `None` when cancelled. The given folders themselves don't count as folders.
pub fn deep_size(
    paths: &[PathBuf],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(DeepSize),
) -> Option<DeepSize> {
    let mut total = DeepSize::default();
    let mut last = Instant::now();
    for path in paths {
        for entry in walkdir::WalkDir::new(path).follow_links(false) {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_dir() {
                if entry.depth() > 0 {
                    total.folders += 1;
                }
            } else {
                total.files += 1;
                total.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
            if last.elapsed() >= REPORT_INTERVAL {
                on_progress(total);
                last = Instant::now();
            }
        }
    }
    Some(total)
}

/// A folder for display, with the home folder shortened to `~`.
pub fn pretty_dir(dir: &Path, home: &Path) -> String {
    match dir.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => format!("~/{}", rel.to_string_lossy()),
        Err(_) => dir.to_string_lossy().into_owned(),
    }
}

/// The rows of the properties dialog for one or more paths, without recursive sizes.
pub fn describe(paths: &[PathBuf], home: &Path) -> Vec<(String, String)> {
    use super::entry::{self, ListOptions};
    use super::format;
    use std::os::unix::fs::MetadataExt as _;
    let location = |p: &Path| p.parent().map_or_else(|| "/".to_string(), |d| pretty_dir(d, home));
    let mut rows: Vec<(String, String)> = Vec::new();
    let [path] = paths else {
        rows.push(("Selection".into(), format::item_count(paths.len() as u64)));
        if let Some(first) = paths.first() {
            rows.push(("Location".into(), location(first)));
        }
        return rows;
    };
    if let Ok(entry) = entry::read_entry(path, ListOptions { sniff: true, count_children: true }) {
        rows.push(("Type".into(), super::mime::describe(&entry.mime)));
        if !entry.is_dir() {
            rows.push((
                "Size".into(),
                format!("{} ({} bytes)", format::size(entry.size), entry.size),
            ));
        } else if let Some(count) = entry.child_count {
            rows.push(("Contents".into(), format::item_count(count)));
        }
    }
    rows.push(("Location".into(), location(path)));
    if let Ok(target) = std::fs::read_link(path) {
        rows.push(("Link Target".into(), target.to_string_lossy().into_owned()));
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else { return rows };
    let time = |t: std::io::Result<std::time::SystemTime>| {
        t.ok().map(|t| format::long_date(format::local_time(t)))
    };
    for (label, value) in [
        ("Modified", time(meta.modified())),
        ("Accessed", time(meta.accessed())),
        ("Created", time(meta.created())),
    ] {
        if let Some(value) = value {
            rows.push((label.into(), value));
        }
    }
    let (mode, is_dir) = (meta.mode(), meta.is_dir());
    rows.push((
        "Owner".into(),
        format!("{} — {}", user_name(meta.uid()), format::access(mode, 6, is_dir)),
    ));
    rows.push((
        "Group".into(),
        format!("{} — {}", group_name(meta.gid()), format::access(mode, 3, is_dir)),
    ));
    rows.push(("Others".into(), format::access(mode, 0, is_dir).to_string()));
    rows.push((
        "Permissions".into(),
        format!("{} ({:04o})", format::permissions(mode), mode & 0o7777),
    ));
    rows
}

/// The rows with a "Size" row for a recursive total, after "Type" or replacing an existing one.
pub fn with_size(rows: &[(String, String)], size: DeepSize) -> Vec<(String, String)> {
    let mut rows = rows.to_vec();
    let row = ("Size".to_string(), size.describe());
    match rows.iter().position(|(k, _)| k == "Size") {
        Some(i) => rows[i] = row,
        None => {
            let after =
                rows.iter().position(|(k, _)| k == "Type" || k == "Selection").map_or(0, |i| i + 1);
            rows.insert(after, row);
        }
    }
    rows
}

/// The rows of the properties dialog for an item in the trash.
pub fn describe_trashed(item: &super::trash::TrashItem, home: &Path) -> Vec<(String, String)> {
    use super::format;
    let mime = if item.is_dir {
        super::mime::DIRECTORY.to_string()
    } else {
        super::mime::guess(Path::new(&item.name), false)
    };
    let mut rows = vec![
        ("Type".to_string(), super::mime::describe(&mime)),
        ("Size".to_string(), format::size(item.size)),
        (
            "Original Location".to_string(),
            item.original.parent().map_or_else(|| "/".to_string(), |d| pretty_dir(d, home)),
        ),
    ];
    if let Some(deleted) = item.deleted {
        rows.push(("Deleted".to_string(), format::long_date(deleted)));
    }
    rows
}

/// Looks up a name by numeric id in a `passwd` or `group` style file.
pub fn lookup_name(table: &str, id: u32) -> Option<String> {
    table.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let numeric: u32 = fields.nth(1)?.parse().ok()?;
        (numeric == id && !name.is_empty()).then(|| name.to_string())
    })
}

/// The user name for a uid from `/etc/passwd`, or the number.
pub fn user_name(uid: u32) -> String {
    std::fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|t| lookup_name(&t, uid))
        .unwrap_or_else(|| uid.to_string())
}

/// The group name for a gid from `/etc/group`, or the number.
pub fn group_name(gid: u32) -> String {
    std::fs::read_to_string("/etc/group")
        .ok()
        .and_then(|t| lookup_name(&t, gid))
        .unwrap_or_else(|| gid.to_string())
}

/// Free and total bytes of the file system holding `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let block = stat.f_frsize.max(1);
    Some((stat.f_bavail.saturating_mul(block), stat.f_blocks.saturating_mul(block)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sums_trees() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        fs::create_dir_all(root.join("a/b")).expect("mkdir");
        fs::write(root.join("a/one"), "12345").expect("write");
        fs::write(root.join("a/b/two"), "123").expect("write");
        fs::write(root.join("three"), "1").expect("write");
        let total =
            deep_size(&[root.join("a"), root.join("three")], &AtomicBool::new(false), |_| {})
                .expect("not cancelled");
        assert_eq!(total, DeepSize { bytes: 9, files: 3, folders: 1 });
        assert_eq!(total.describe(), "9 bytes (3 files, 1 folder)");
        assert_eq!(deep_size(&[root.to_path_buf()], &AtomicBool::new(true), |_| {}), None);
    }

    #[test]
    fn descriptions() {
        assert_eq!(DeepSize { bytes: 1500, files: 1, folders: 0 }.describe(), "1.5 kB");
        assert_eq!(DeepSize { bytes: 0, files: 0, folders: 0 }.describe(), "0 bytes");
        assert_eq!(DeepSize { bytes: 10, files: 2, folders: 0 }.describe(), "10 bytes (2 files)");
        assert_eq!(
            DeepSize { bytes: 10, files: 0, folders: 2 }.describe(),
            "10 bytes (0 files, 2 folders)"
        );
    }

    #[test]
    fn name_tables() {
        let passwd =
            "root:x:0:0:root:/root:/bin/bash\nada:x:1000:1000::/home/ada:/bin/sh\nbroken\n";
        assert_eq!(lookup_name(passwd, 1000).as_deref(), Some("ada"));
        assert_eq!(lookup_name(passwd, 0).as_deref(), Some("root"));
        assert_eq!(lookup_name(passwd, 7), None);
        assert!(!user_name(0).is_empty());
        assert!(!group_name(0).is_empty());
    }

    #[test]
    fn rows_for_files_folders_and_selections() {
        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path();
        fs::create_dir_all(home.join("docs/sub")).expect("mkdir");
        fs::write(home.join("docs/a.txt"), "hello").expect("write");
        let file = describe(&[home.join("docs/a.txt")], home);
        let get = |rows: &[(String, String)], key: &str| {
            rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        assert_eq!(get(&file, "Type").as_deref(), Some("Plain text document"));
        assert_eq!(get(&file, "Size").as_deref(), Some("5 bytes (5 bytes)"));
        assert_eq!(get(&file, "Location").as_deref(), Some("~/docs"));
        assert!(get(&file, "Permissions").is_some_and(|p| p.starts_with("rw")));

        let folder = describe(&[home.join("docs")], home);
        assert_eq!(get(&folder, "Contents").as_deref(), Some("2 items"));
        let sized = with_size(&folder, DeepSize { bytes: 5, files: 1, folders: 1 });
        assert_eq!(sized[1], ("Size".to_string(), "5 bytes (1 file, 1 folder)".to_string()));
        assert_eq!(with_size(&sized, DeepSize::default()).len(), sized.len());

        let many = describe(&[home.join("docs"), home.join("docs/a.txt")], home);
        assert_eq!(many[0], ("Selection".to_string(), "2 items".to_string()));
        assert_eq!(with_size(&many, DeepSize::default())[1].0, "Size");
        assert_eq!(pretty_dir(Path::new("/etc"), home), "/etc");
        assert_eq!(pretty_dir(home, home), "~");
    }

    #[test]
    fn rows_for_trashed_items() {
        let item = crate::core::trash::TrashItem {
            name: "a.png".into(),
            file: "/t/files/a.png".into(),
            info: "/t/info/a.png.trashinfo".into(),
            original: "/home/ada/Pictures/a.png".into(),
            deleted: chrono::NaiveDate::from_ymd_opt(2024, 3, 12)
                .and_then(|d| d.and_hms_opt(14, 5, 0)),
            is_dir: false,
            size: 1500,
        };
        let rows = describe_trashed(&item, Path::new("/home/ada"));
        assert_eq!(rows[0].1, "PNG image");
        assert_eq!(rows[2].1, "~/Pictures");
        assert_eq!(rows[3].1, "Tue 12 Mar 2024, 14:05");
    }

    #[test]
    fn free_space() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (free, total) = disk_space(dir.path()).expect("statvfs");
        assert!(free <= total);
        assert_eq!(disk_space(Path::new("/nonexistent/path")), None);
    }
}
