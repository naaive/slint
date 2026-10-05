// SPDX-License-Identifier: MIT

//! The freedesktop.org Trash specification: the home trash and per-drive trash folders.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use chrono::{NaiveDateTime, Timelike as _};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

use super::{names, uri};

const INFO_EXT: &str = "trashinfo";
const DATE_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";
const DIR_FLAGS: OFlags =
    OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// One trashed file or folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrashItem {
    /// The original file name.
    pub name: String,
    /// Where the file is now, inside a trash `files` folder.
    pub file: PathBuf,
    /// Its `.trashinfo` file.
    pub info: PathBuf,
    /// Where it was before it was trashed.
    pub original: PathBuf,
    pub deleted: Option<NaiveDateTime>,
    pub is_dir: bool,
    /// Size in bytes, recursive for folders.
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum TrashError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("The trash can't be moved to the trash")]
    ContainsTrash,
    #[error("This drive has no trash")]
    NoTrashOnDevice,
}

/// The trash folders of one user.
#[derive(Clone, Debug)]
pub struct Trash {
    home: PathBuf,
    uid: u32,
    mount_points: Vec<PathBuf>,
}

/// A trash folder, and the folder its relative `Path=` entries start from.
struct TrashDir {
    root: PathBuf,
    topdir: Option<PathBuf>,
}

impl Trash {
    /// `home` is `$XDG_DATA_HOME/Trash`; `mount_points` are searched for per-drive trash folders, deepest first.
    pub fn new(home: PathBuf, uid: u32, mount_points: Vec<PathBuf>) -> Self {
        Self { home, uid, mount_points }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Moves a file or folder to the trash on its own drive.
    pub fn trash(&self, path: &Path) -> Result<TrashItem, TrashError> {
        let path = absolute(path)?;
        if self.home.starts_with(&path) {
            return Err(TrashError::ContainsTrash);
        }
        let meta = fs::symlink_metadata(&path)?;
        let dir = self.trash_dir_for(&path, meta.dev())?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let stored = match &dir.topdir {
            Some(top) => path.strip_prefix(top).unwrap_or(&path).to_path_buf(),
            None => path.clone(),
        };
        let now = chrono::Local::now().naive_local();
        let deleted = now.with_nanosecond(0).unwrap_or(now);
        let info_text = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            uri::escape_path(&stored),
            deleted.format(DATE_FORMAT)
        );
        let (file, info) = reserve(&dir.root, &name, info_text.as_bytes())?;
        if let Err(error) = fs::rename(&path, &file) {
            let _ = fs::remove_file(&info);
            return Err(error.into());
        }
        Ok(TrashItem {
            name,
            file,
            info,
            original: path,
            deleted: Some(deleted),
            is_dir: meta.is_dir(),
            size: 0,
        })
    }

    fn trash_dir_for(&self, path: &Path, device: u64) -> Result<TrashDir, TrashError> {
        ensure_trash_dirs(&self.home)?;
        if fs::metadata(&self.home)?.dev() == device {
            return Ok(TrashDir { root: self.home.clone(), topdir: None });
        }
        let topdir = self
            .mount_points
            .iter()
            .find(|m| path.starts_with(m) && fs::metadata(m).is_ok_and(|meta| meta.dev() == device))
            .ok_or(TrashError::NoTrashOnDevice)?;
        let root = self.topdir_trash(topdir).ok_or(TrashError::NoTrashOnDevice)?;
        Ok(TrashDir { root, topdir: Some(topdir.clone()) })
    }

    /// The trash folder of this user in `topdir`, created if needed.
    fn topdir_trash(&self, topdir: &Path) -> Option<PathBuf> {
        let [shared, own] = self.topdir_candidates(topdir);
        if fs::symlink_metadata(topdir.join(".Trash")).is_ok_and(|m| is_shared_trash(&m))
            && self.create_topdir_trash(&shared).is_ok()
        {
            return Some(shared);
        }
        self.create_topdir_trash(&own).is_ok().then_some(own)
    }

    fn topdir_candidates(&self, topdir: &Path) -> [PathBuf; 2] {
        [
            topdir.join(".Trash").join(self.uid.to_string()),
            topdir.join(format!(".Trash-{}", self.uid)),
        ]
    }

    fn create_topdir_trash(&self, root: &Path) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        for dir in [root.to_path_buf(), root.join("files"), root.join("info")] {
            match builder.create(&dir) {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            }
        }
        self.validate_topdir_trash(root)
    }

    /// Checks a per-drive trash folder as the Trash spec requires, so other users can't redirect it.
    fn validate_topdir_trash(&self, root: &Path) -> io::Result<()> {
        let denied = || io::Error::from(io::ErrorKind::PermissionDenied);
        let owned_dir = |path: &Path| {
            let meta = fs::symlink_metadata(path)?;
            if meta.file_type().is_dir() && meta.uid() == self.uid {
                Ok(meta)
            } else {
                Err(denied())
            }
        };
        if root.parent().and_then(Path::file_name) == Some(".Trash".as_ref())
            && !root
                .parent()
                .is_some_and(|p| fs::symlink_metadata(p).is_ok_and(|m| is_shared_trash(&m)))
        {
            return Err(denied());
        }
        if owned_dir(root)?.mode() & 0o077 != 0 {
            return Err(denied());
        }
        owned_dir(&root.join("files"))?;
        owned_dir(&root.join("info"))?;
        Ok(())
    }

    /// Every trash folder that exists: the home trash and the valid ones on mounted drives.
    fn dirs(&self) -> Vec<TrashDir> {
        let mut dirs = vec![TrashDir { root: self.home.clone(), topdir: None }];
        for top in &self.mount_points {
            for root in self.topdir_candidates(top) {
                if root != self.home && self.validate_topdir_trash(&root).is_ok() {
                    dirs.push(TrashDir { root, topdir: Some(top.clone()) });
                }
            }
        }
        dirs
    }

    /// The trash folder holding `item`, which must be one of [`Trash::dirs`].
    fn dir_of(&self, item: &TrashItem) -> io::Result<TrashDir> {
        self.dirs()
            .into_iter()
            .find(|dir| {
                item.file.parent() == Some(&dir.root.join("files"))
                    && item.info.parent() == Some(&dir.root.join("info"))
            })
            .ok_or_else(|| io::ErrorKind::PermissionDenied.into())
    }

    /// Lists every trashed item, computing folder sizes; info files without a trashed file are skipped.
    pub fn list(&self) -> Vec<TrashItem> {
        let mut items = Vec::new();
        for dir in self.dirs() {
            let Ok(entries) = fs::read_dir(dir.root.join("info")) else { continue };
            for entry in entries.flatten() {
                let info = entry.path();
                if info.extension().and_then(|e| e.to_str()) != Some(INFO_EXT) {
                    continue;
                }
                let Some(stem) = info.file_stem() else { continue };
                let file = dir.root.join("files").join(stem);
                let Ok(meta) = fs::symlink_metadata(&file) else { continue };
                let Some((stored, deleted)) =
                    fs::read_to_string(&info).ok().as_deref().and_then(parse_info)
                else {
                    continue;
                };
                let original = match &dir.topdir {
                    Some(top) => match restore_target(top, &stored) {
                        Some(original) => original,
                        None => continue,
                    },
                    None => stored,
                };
                let name = original
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| stem.to_string_lossy().into_owned());
                let size = if meta.is_dir() { tree_size(&file) } else { meta.len() };
                items.push(TrashItem {
                    name,
                    file,
                    info,
                    original,
                    deleted,
                    is_dir: meta.is_dir(),
                    size,
                });
            }
        }
        items
    }

    /// Whether any trash folder holds an item, without computing sizes.
    pub fn is_empty(&self) -> bool {
        self.dirs().iter().all(|dir| {
            fs::read_dir(dir.root.join("files"))
                .map_or(true, |mut entries| entries.next().is_none())
        })
    }

    /// Moves an item back to `target`, usually its original location, creating missing parent folders.
    ///
    /// Fails with `AlreadyExists` when `target` exists, so the caller can resolve the conflict.
    pub fn restore(&self, item: &TrashItem, target: &Path) -> io::Result<()> {
        self.dir_of(item)?;
        if fs::symlink_metadata(target).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&item.file, target)?;
        let _ = fs::remove_file(&item.info);
        Ok(())
    }

    /// Deletes an item permanently.
    ///
    /// The item is removed relative to its `files` folder, never following a symlink to another place.
    pub fn delete(&self, item: &TrashItem) -> io::Result<()> {
        let dir = self.dir_of(item)?;
        let name = item.file.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let root = rustix::fs::open(&dir.root, DIR_FLAGS, Mode::empty())?;
        let files = rustix::fs::openat(&root, "files", DIR_FLAGS, Mode::empty())?;
        if dir.topdir.is_some()
            && (rustix::fs::fstat(&root)?.st_uid != self.uid
                || rustix::fs::fstat(&files)?.st_uid != self.uid)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        remove_at(&files, name)?;
        match fs::remove_file(&item.info) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Where an item from the trash in `top` goes back to, or `None` when its stored path leaves `top`.
fn restore_target(top: &Path, stored: &Path) -> Option<PathBuf> {
    let relative = if stored.is_absolute() { stored.strip_prefix(top).ok()? } else { stored };
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then(|| top.join(relative))
}

/// Removes the entry `name` of the folder `dir`, recursively, without following symlinks.
fn remove_at(dir: &OwnedFd, name: &OsStr) -> io::Result<()> {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Ok(rustix::fs::unlinkat(dir, name, AtFlags::empty())?);
    }
    let sub = rustix::fs::openat(dir, name, DIR_FLAGS, Mode::empty())?;
    let children: Vec<OsString> = rustix::fs::Dir::read_from(&sub)?
        .map(|entry| entry.map(|e| OsStr::from_bytes(e.file_name().to_bytes()).to_os_string()))
        .collect::<Result<_, _>>()?;
    for child in children {
        if child != "." && child != ".." {
            remove_at(&sub, &child)?;
        }
    }
    Ok(rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR)?)
}

fn is_shared_trash(meta: &fs::Metadata) -> bool {
    meta.file_type().is_dir() && meta.mode() & 0o1000 != 0
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn ensure_trash_dirs(root: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(root.join("files"))?;
    builder.create(root.join("info"))?;
    let meta = fs::metadata(root.join("info"))?;
    if meta.permissions().mode() & 0o200 == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

/// Atomically claims a name in a trash folder by creating its info file, as the spec requires.
fn reserve(root: &Path, name: &str, info_text: &[u8]) -> io::Result<(PathBuf, PathBuf)> {
    let (stem, ext) = names::split_extension(name);
    for n in 1u32..10_000 {
        let candidate = if n == 1 { name.to_string() } else { format!("{stem}.{n}{ext}") };
        let info = root.join("info").join(format!("{candidate}.{INFO_EXT}"));
        let file = root.join("files").join(&candidate);
        if fs::symlink_metadata(&file).is_ok() {
            continue;
        }
        match OpenOptions::new().write(true).create_new(true).open(&info) {
            Ok(mut handle) => {
                if let Err(error) = handle.write_all(info_text).and_then(|()| handle.sync_all()) {
                    let _ = fs::remove_file(&info);
                    return Err(error);
                }
                return Ok((file, info));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// Parses a `.trashinfo` file into its stored path and deletion date.
pub fn parse_info(text: &str) -> Option<(PathBuf, Option<NaiveDateTime>)> {
    let mut in_group = false;
    let mut path = None;
    let mut date = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Trash Info]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some(value) = line.strip_prefix("Path=") {
            path = Some(uri::unescape_path(value.trim()));
        } else if let Some(value) = line.strip_prefix("DeletionDate=") {
            date = NaiveDateTime::parse_from_str(value.trim(), DATE_FORMAT).ok();
        }
    }
    path.filter(|p| !p.as_os_str().is_empty()).map(|p| (p, date))
}

fn tree_size(dir: &Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| !m.is_dir())
        .map(|m| m.len())
        .sum()
}

/// Removes a file, a symlink, or a folder tree.
pub fn remove_any(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Trash) {
        let dir = tempfile::tempdir().expect("temp dir");
        let trash = Trash::new(dir.path().join("data/Trash"), 1000, vec![PathBuf::from("/")]);
        (dir, trash)
    }

    #[test]
    fn trash_list_restore_delete() {
        let (dir, trash) = setup();
        let docs = dir.path().join("docs");
        fs::create_dir(&docs).expect("mkdir");
        let file = docs.join("report final.txt");
        fs::write(&file, "12345").expect("write");
        let folder = docs.join("photos");
        fs::create_dir(&folder).expect("mkdir");
        fs::write(folder.join("a.png"), "abc").expect("write");

        assert!(trash.is_empty());
        let item = trash.trash(&file).expect("trashed");
        assert!(!file.exists());
        assert!(item.file.exists());
        let info = fs::read_to_string(&item.info).expect("info");
        assert!(info.starts_with("[Trash Info]\nPath="));
        assert!(info.contains("report%20final.txt"));
        assert!(info.contains("DeletionDate="));
        trash.trash(&folder).expect("trashed folder");

        // A second file with the same name gets its own slot.
        fs::write(&file, "x").expect("write");
        let second = trash.trash(&file).expect("trashed again");
        assert_ne!(second.file, item.file);
        assert_eq!(second.file.file_name().and_then(|n| n.to_str()), Some("report final.2.txt"));

        let mut items = trash.list();
        items.sort_by(|a, b| a.file.cmp(&b.file));
        assert_eq!(items.len(), 3);
        assert!(!trash.is_empty());
        let photos = items.iter().find(|i| i.name == "photos").expect("photos listed");
        assert!(photos.is_dir);
        assert_eq!(photos.size, 3);
        assert_eq!(photos.original, folder);
        let report = items.iter().find(|i| i.file == item.file).expect("report listed");
        assert_eq!(report.original, file);
        assert_eq!(report.size, 5);
        assert!(report.deleted.is_some());

        // Restoring over an existing file is a conflict for the caller.
        fs::write(&file, "conflict").expect("write");
        let err = trash.restore(report, &file).expect_err("conflict");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        fs::remove_file(&file).expect("rm");
        trash.restore(report, &file).expect("restored");
        assert_eq!(fs::read_to_string(&file).expect("read"), "12345");
        assert!(!report.info.exists());

        // Restoring recreates missing parents.
        let elsewhere = dir.path().join("new/place/photos");
        trash.restore(photos, &elsewhere).expect("restored elsewhere");
        assert!(elsewhere.join("a.png").exists());

        let rest = trash.list();
        assert_eq!(rest.len(), 1);
        trash.delete(&rest[0]).expect("deleted");
        assert!(trash.list().is_empty());
        assert!(trash.is_empty());
    }

    #[test]
    fn refuses_to_trash_the_trash() {
        let (dir, trash) = setup();
        fs::create_dir_all(dir.path().join("data/Trash/files")).expect("mkdir");
        assert!(matches!(trash.trash(&dir.path().join("data")), Err(TrashError::ContainsTrash)));
        assert!(matches!(trash.trash(&dir.path().join("missing")), Err(TrashError::Io(_))));
    }

    #[test]
    fn skips_orphans_and_bad_info() {
        let (_dir, trash) = setup();
        ensure_trash_dirs(trash.home()).expect("dirs");
        fs::write(trash.home().join("info/ghost.trashinfo"), "[Trash Info]\nPath=/x/ghost\n")
            .expect("write");
        fs::write(trash.home().join("files/bad"), "").expect("write");
        fs::write(trash.home().join("info/bad.trashinfo"), "[Other]\nPath=/x\n").expect("write");
        fs::write(trash.home().join("info/readme.txt"), "").expect("write");
        assert!(trash.list().is_empty());
    }

    /// A trash for the current user with `<temp>/mnt` as the only drive.
    fn topdir_setup(uid_offset: u32) -> (tempfile::TempDir, Trash, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let top = dir.path().join("mnt");
        fs::create_dir(&top).expect("mkdir");
        let uid = rustix::process::getuid().as_raw() + uid_offset;
        let trash = Trash::new(dir.path().join("home/Trash"), uid, vec![top.clone()]);
        (dir, trash, top)
    }

    /// Puts a file named `name` into the trash folder `root`, as another implementation would.
    fn plant(root: &Path, name: &str, stored: &str) -> TrashItem {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(root.join("info")).expect("mkdir");
        if fs::symlink_metadata(root.join("files")).is_err() {
            builder.create(root.join("files")).expect("mkdir");
        }
        let info = root.join("info").join(format!("{name}.{INFO_EXT}"));
        fs::write(&info, format!("[Trash Info]\nPath={stored}\n")).expect("write");
        let file = root.join("files").join(name);
        if fs::symlink_metadata(&file).is_err() {
            fs::write(&file, "x").expect("write");
        }
        TrashItem {
            name: name.into(),
            file,
            info,
            original: PathBuf::new(),
            deleted: None,
            is_dir: false,
            size: 0,
        }
    }

    #[test]
    fn topdir_trash_lists_and_deletes_without_following_symlinks() {
        let (dir, trash, top) = topdir_setup(0);
        let root = top.join(format!(".Trash-{}", trash.uid));
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).expect("mkdir");
        fs::write(outside.join("keep"), "1").expect("write");
        let item = plant(&root, "tree", "docs/tree");
        fs::remove_file(&item.file).expect("rm");
        fs::create_dir_all(item.file.join("sub")).expect("mkdir");
        fs::write(item.file.join("sub/f"), "").expect("write");
        std::os::unix::fs::symlink(&outside, item.file.join("link")).expect("symlink");

        assert_eq!(trash.topdir_trash(&top), Some(root.clone()));
        let items = trash.list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].original, top.join("docs/tree"));
        trash.delete(&items[0]).expect("deleted");
        assert!(trash.list().is_empty());
        assert_eq!(fs::read_to_string(outside.join("keep")).expect("read"), "1");
    }

    #[test]
    fn topdir_trash_with_a_symlinked_files_folder_is_ignored() {
        let (dir, trash, top) = topdir_setup(0);
        let victim = dir.path().join("victim");
        fs::create_dir_all(victim.join("Documents")).expect("mkdir");
        fs::write(victim.join("Documents/secret"), "1").expect("write");
        let root = top.join(format!(".Trash-{}", trash.uid));
        fs::create_dir_all(&root).expect("mkdir");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("chmod");
        std::os::unix::fs::symlink(&victim, root.join("files")).expect("symlink");
        let item = plant(&root, "Documents", "Documents");

        assert!(trash.list().is_empty());
        assert!(trash.is_empty());
        assert!(trash.delete(&item).is_err());
        assert!(trash.restore(&item, &dir.path().join("restored")).is_err());
        assert!(victim.join("Documents/secret").exists());
        assert_eq!(trash.topdir_trash(&top), None);
    }

    #[test]
    fn topdir_trash_must_be_private_and_owned() {
        let (dir, trash, top) = topdir_setup(0);
        let root = top.join(format!(".Trash-{}", trash.uid));
        plant(&root, "a", "a");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).expect("chmod");
        assert!(trash.list().is_empty());
        assert_eq!(trash.topdir_trash(&top), None);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(trash.list().len(), 1);

        // A root that's a symlink to a valid-looking folder is rejected too.
        fs::rename(&root, dir.path().join("elsewhere")).expect("mv");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &root).expect("symlink");
        assert!(trash.list().is_empty());
        assert_eq!(trash.topdir_trash(&top), None);

        // Folders owned by our uid are foreign to another user.
        let (_dir, other, top) = topdir_setup(1);
        plant(&top.join(format!(".Trash-{}", other.uid)), "a", "a");
        assert!(other.list().is_empty());
        assert_eq!(other.topdir_trash(&top), None);
    }

    #[test]
    fn shared_topdir_trash_needs_the_sticky_bit() {
        let (_dir, trash, top) = topdir_setup(0);
        let shared = top.join(".Trash");
        fs::create_dir(&shared).expect("mkdir");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).expect("chmod");
        let root = shared.join(trash.uid.to_string());
        plant(&root, "a", "a");
        assert!(trash.list().is_empty());
        assert_eq!(trash.topdir_trash(&top), Some(top.join(format!(".Trash-{}", trash.uid))));

        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).expect("chmod");
        assert_eq!(trash.list().len(), 1);
        assert_eq!(trash.topdir_trash(&top), Some(root));
    }

    #[test]
    fn topdir_items_must_restore_inside_the_drive() {
        let (_dir, trash, top) = topdir_setup(0);
        let root = top.join(format!(".Trash-{}", trash.uid));
        plant(&root, "up", "../../etc/x");
        plant(&root, "abs", "/etc/x");
        let inside = format!("{}/docs/in", top.display());
        plant(&root, "in", &inside);
        plant(&root, "rel", "docs/rel");
        let mut originals: Vec<PathBuf> = trash.list().into_iter().map(|i| i.original).collect();
        originals.sort();
        assert_eq!(originals, [top.join("docs/in"), top.join("docs/rel")]);
    }

    #[test]
    fn info_parsing() {
        let (path, date) = parse_info(
            "[Trash Info]\nPath=/home/ada/a%20b.txt\nDeletionDate=2024-03-12T14:05:00\n",
        )
        .expect("parsed");
        assert_eq!(path, PathBuf::from("/home/ada/a b.txt"));
        assert_eq!(date.map(|d| d.to_string()).as_deref(), Some("2024-03-12 14:05:00"));
        let (_, date) =
            parse_info("[Trash Info]\nPath=rel/x\nDeletionDate=yesterday\n").expect("parsed");
        assert_eq!(date, None);
        assert_eq!(parse_info("[Trash Info]\nDeletionDate=2024-03-12T14:05:00\n"), None);
        assert_eq!(parse_info("Path=/x\n"), None);
    }
}
