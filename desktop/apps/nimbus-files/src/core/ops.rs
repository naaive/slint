// SPDX-License-Identifier: MIT

//! File operations with progress, cancellation, and conflict resolution.
//!
//! [`run`] blocks, so call it on a worker thread; the [`Observer`] reports back and answers conflicts.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use super::names::{self, Style};
use super::trash::{self, Trash, TrashItem};

const CHUNK: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Copy {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    /// Renames within a drive, and copies then deletes across drives.
    Move {
        sources: Vec<PathBuf>,
        dest: PathBuf,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    Delete {
        paths: Vec<PathBuf>,
    },
    Restore {
        items: Vec<TrashItem>,
    },
    /// Deletes trashed items permanently, as emptying the trash does.
    DeleteTrashed {
        items: Vec<TrashItem>,
    },
    Rename {
        path: PathBuf,
        new_name: String,
    },
    CreateFolder {
        parent: PathBuf,
        name: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub total_items: u64,
    pub done_items: u64,
    /// The name of the file being processed.
    pub current: String,
}

impl Progress {
    /// Completion from 0 to 1, by bytes when there are any, else by items.
    pub fn fraction(&self) -> f32 {
        let (done, total) = if self.total_bytes > 0 {
            (self.done_bytes, self.total_bytes)
        } else {
            (self.done_items, self.total_items)
        };
        if total == 0 { 0.0 } else { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Skip,
    /// Overwrites a file, or merges a folder into an existing folder.
    Replace,
    /// Gives the incoming item a numbered name.
    KeepBoth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub resolution: Resolution,
    /// Applies the resolution to the remaining conflicts of the same kind (file or folder).
    pub apply_to_all: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub source: PathBuf,
    pub target: PathBuf,
    pub source_is_dir: bool,
    pub target_is_dir: bool,
}

impl Conflict {
    pub fn is_merge(&self) -> bool {
        self.source_is_dir && self.target_is_dir
    }
}

/// The worker's link to whoever started the operation.
pub trait Observer {
    fn is_cancelled(&self) -> bool;
    fn progress(&mut self, progress: &Progress);
    /// Asks how to resolve a name collision; `None` cancels the operation.
    fn resolve(&mut self, conflict: &Conflict) -> Option<Decision>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Top-level items created at the destination, to select afterwards.
    pub created: Vec<PathBuf>,
    /// Items moved to the trash, to undo the operation.
    pub trashed: Vec<TrashItem>,
    pub errors: Vec<(PathBuf, String)>,
    pub skipped: usize,
    pub cancelled: bool,
    /// Items processed successfully.
    pub done: usize,
}

/// Unwinds an operation when the user cancels.
struct Cancelled;

type Step<T = ()> = Result<T, Cancelled>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Copy,
    Move,
}

/// Runs an operation to completion or cancellation; per-item failures are collected, not fatal.
pub fn run(operation: &Operation, trash: &Trash, observer: &mut dyn Observer) -> Outcome {
    let mut engine = Engine {
        observer,
        progress: Progress::default(),
        remembered_file: None,
        remembered_dir: None,
        outcome: Outcome::default(),
        buffer: Vec::new(),
    };
    let result = match operation {
        Operation::Copy { sources, dest } => engine.transfer(sources, dest, Mode::Copy),
        Operation::Move { sources, dest } => engine.transfer(sources, dest, Mode::Move),
        Operation::Trash { paths } => engine.trash(paths, trash),
        Operation::Delete { paths } => engine.delete(paths),
        Operation::Restore { items } => engine.restore(items, trash),
        Operation::DeleteTrashed { items } => engine.delete_trashed(items, trash),
        Operation::Rename { path, new_name } => {
            engine.rename(path, new_name);
            Ok(())
        }
        Operation::CreateFolder { parent, name } => {
            engine.create_folder(parent, name);
            Ok(())
        }
    };
    engine.outcome.cancelled = result.is_err();
    engine.outcome
}

struct Engine<'a> {
    observer: &'a mut dyn Observer,
    progress: Progress,
    remembered_file: Option<Resolution>,
    remembered_dir: Option<Resolution>,
    outcome: Outcome,
    buffer: Vec<u8>,
}

impl Engine<'_> {
    fn check(&self) -> Step {
        if self.observer.is_cancelled() { Err(Cancelled) } else { Ok(()) }
    }

    fn report(&mut self) {
        self.observer.progress(&self.progress);
    }

    fn advance(&mut self, bytes: u64, items: u64) {
        self.progress.done_bytes += bytes;
        self.progress.done_items += items;
        self.report();
    }

    fn set_current(&mut self, path: &Path) {
        self.progress.current = display_name(path);
    }

    fn fail(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.outcome.errors.push((path.to_path_buf(), error.to_string()));
    }

    fn fail_io(&mut self, path: &Path, error: &io::Error) {
        self.fail(path, error_message(error));
    }

    fn decide(&mut self, conflict: &Conflict) -> Step<Resolution> {
        let remembered =
            if conflict.is_merge() { self.remembered_dir } else { self.remembered_file };
        if let Some(resolution) = remembered {
            return Ok(resolution);
        }
        let decision = self.observer.resolve(conflict).ok_or(Cancelled)?;
        if decision.apply_to_all {
            if conflict.is_merge() {
                self.remembered_dir = Some(decision.resolution);
            } else {
                self.remembered_file = Some(decision.resolution);
            }
        }
        Ok(decision.resolution)
    }

    fn transfer(&mut self, sources: &[PathBuf], dest: &Path, mode: Mode) -> Step {
        let dest_dev = fs::metadata(dest).map(|m| m.dev()).ok();
        let mut stats = HashMap::new();
        for source in sources {
            self.check()?;
            let same_device = fs::symlink_metadata(source).ok().map(|m| m.dev()) == dest_dev;
            let stat = if mode == Mode::Move && same_device { (0, 1) } else { scan(source) };
            self.progress.total_bytes += stat.0;
            self.progress.total_items += stat.1;
            stats.insert(source.clone(), stat);
        }
        self.report();
        for source in sources {
            self.check()?;
            let stat = stats.get(source).copied().unwrap_or((0, 1));
            let Some(name) = source.file_name() else {
                self.fail(source, "Invalid file name");
                continue;
            };
            let is_real_dir = fs::symlink_metadata(source).is_ok_and(|m| m.is_dir());
            if is_real_dir && dest.starts_with(source) {
                let verb = if mode == Mode::Copy { "copy" } else { "move" };
                self.fail(source, format!("You can't {verb} a folder into itself"));
                self.advance(stat.0, stat.1);
                continue;
            }
            let mut target = dest.join(name);
            if target == *source {
                if mode == Mode::Move {
                    self.outcome.skipped += 1;
                    self.advance(stat.0, stat.1);
                    continue;
                }
                let file_name = name.to_string_lossy();
                target = dest.join(names::unique(&file_name, Style::Copy, |n| {
                    fs::symlink_metadata(dest.join(n)).is_ok()
                }));
            }
            if let Some(created) = self.place(source, target, mode, stat)? {
                self.outcome.created.push(created);
                self.outcome.done += 1;
            }
        }
        Ok(())
    }

    /// Puts `source` at `target`, resolving a collision first; returns where it ended up.
    fn place(
        &mut self,
        source: &Path,
        mut target: PathBuf,
        mode: Mode,
        stat: (u64, u64),
    ) -> Step<Option<PathBuf>> {
        self.set_current(source);
        let source_meta = match fs::symlink_metadata(source) {
            Ok(meta) => meta,
            Err(error) => {
                self.fail_io(source, &error);
                self.advance(stat.0, stat.1);
                return Ok(None);
            }
        };
        if let Ok(target_meta) = fs::symlink_metadata(&target) {
            let conflict = Conflict {
                source: source.to_path_buf(),
                target: target.clone(),
                source_is_dir: source_meta.is_dir(),
                target_is_dir: target_meta.is_dir(),
            };
            match self.decide(&conflict)? {
                Resolution::Skip => {
                    self.outcome.skipped += 1;
                    self.advance(stat.0, stat.1);
                    return Ok(None);
                }
                Resolution::KeepBoth => {
                    let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
                    let name = target
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    target = parent.join(names::unique(&name, Style::Numbered, |n| {
                        fs::symlink_metadata(parent.join(n)).is_ok()
                    }));
                }
                Resolution::Replace if conflict.is_merge() => {
                    self.merge(source, &target, mode)?;
                    return Ok(Some(target));
                }
                Resolution::Replace => {
                    if let Err(error) = trash::remove_any(&target) {
                        self.fail_io(&target, &error);
                        self.advance(stat.0, stat.1);
                        return Ok(None);
                    }
                }
            }
        }
        if mode == Mode::Move {
            match fs::rename(source, &target) {
                Ok(()) => {
                    self.advance(stat.0, stat.1);
                    return Ok(Some(target));
                }
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                    if stat.0 == 0 && stat.1 == 1 {
                        // The source was assumed to be on the destination's drive and wasn't scanned.
                        let (bytes, items) = scan(source);
                        self.progress.total_bytes += bytes;
                        self.progress.total_items += items.saturating_sub(1);
                    }
                }
                Err(error) => {
                    self.fail_io(source, &error);
                    self.advance(stat.0, stat.1);
                    return Ok(None);
                }
            }
        }
        if !self.copy_tree(source, &target)? {
            return Ok(None);
        }
        if mode == Mode::Move
            && let Err(error) = trash::remove_any(source)
        {
            self.fail_io(source, &error);
        }
        Ok(Some(target))
    }

    /// Moves or copies the children of `source` into the existing folder `target`.
    fn merge(&mut self, source: &Path, target: &Path, mode: Mode) -> Step {
        let children = match fs::read_dir(source) {
            Ok(entries) => entries.flatten().map(|e| e.path()).collect::<Vec<_>>(),
            Err(error) => {
                self.fail_io(source, &error);
                return Ok(());
            }
        };
        for child in children {
            self.check()?;
            let Some(name) = child.file_name() else { continue };
            let stat = if mode == Mode::Copy { scan(&child) } else { (0, 0) };
            self.place(&child, target.join(name), mode, stat)?;
        }
        if mode == Mode::Move {
            // Fails harmlessly when skipped children remain.
            let _ = fs::remove_dir(source);
        }
        self.advance(0, 1);
        Ok(())
    }

    /// Copies a file, symlink, or folder tree to a path that doesn't exist; false when anything failed.
    fn copy_tree(&mut self, source: &Path, target: &Path) -> Step<bool> {
        self.check()?;
        let meta = match fs::symlink_metadata(source) {
            Ok(meta) => meta,
            Err(error) => {
                self.fail_io(source, &error);
                return Ok(false);
            }
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            let result =
                fs::read_link(source).and_then(|link| std::os::unix::fs::symlink(link, target));
            self.advance(0, 1);
            if let Err(error) = result {
                self.fail_io(source, &error);
                return Ok(false);
            }
            Ok(true)
        } else if file_type.is_dir() {
            if let Err(error) = fs::create_dir(target) {
                self.fail_io(target, &error);
                return Ok(false);
            }
            let children = match fs::read_dir(source) {
                Ok(entries) => entries.flatten().map(|e| e.path()).collect::<Vec<_>>(),
                Err(error) => {
                    self.fail_io(source, &error);
                    return Ok(false);
                }
            };
            let mut ok = true;
            for child in children {
                let Some(name) = child.file_name() else { continue };
                self.set_current(&child);
                ok &= self.copy_tree(&child, &target.join(name))?;
            }
            let _ = fs::set_permissions(target, fs::Permissions::from_mode(meta.mode() & 0o7777));
            self.advance(0, 1);
            Ok(ok)
        } else if file_type.is_file() {
            match self.copy_file(source, target, &meta)? {
                Ok(()) => {
                    self.advance(0, 1);
                    Ok(true)
                }
                Err(error) => {
                    self.fail_io(source, &error);
                    self.advance(meta.len(), 1);
                    Ok(false)
                }
            }
        } else {
            self.fail(source, "Special files can't be copied");
            self.advance(0, 1);
            Ok(false)
        }
    }

    /// Copies file contents in chunks, reporting bytes and removing the partial copy on cancel or error.
    fn copy_file(
        &mut self,
        source: &Path,
        target: &Path,
        meta: &fs::Metadata,
    ) -> Step<io::Result<()>> {
        let mut input = match File::open(source) {
            Ok(file) => file,
            Err(error) => return Ok(Err(error)),
        };
        let mut output = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(meta.mode() & 0o777)
            .open(target)
        {
            Ok(file) => file,
            Err(error) => return Ok(Err(error)),
        };
        if self.buffer.len() != CHUNK {
            self.buffer = vec![0; CHUNK];
        }
        let mut copied = 0u64;
        let result = loop {
            if self.observer.is_cancelled() {
                drop(output);
                let _ = fs::remove_file(target);
                self.progress.done_bytes = self.progress.done_bytes.saturating_sub(copied);
                return Err(Cancelled);
            }
            let read = match input.read(&mut self.buffer) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            };
            if let Err(e) = output.write_all(&self.buffer[..read]) {
                break Err(e);
            }
            copied += read as u64;
            self.advance(read as u64, 0);
        };
        let result = result.and_then(|()| {
            output.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))?;
            if let Ok(modified) = meta.modified() {
                output.set_modified(modified)?;
            }
            Ok(())
        });
        if result.is_err() {
            drop(output);
            let _ = fs::remove_file(target);
            self.progress.done_bytes = self.progress.done_bytes.saturating_sub(copied);
        }
        Ok(result)
    }

    fn trash(&mut self, paths: &[PathBuf], trash: &Trash) -> Step {
        self.progress.total_items = paths.len() as u64;
        self.report();
        for path in paths {
            self.check()?;
            self.set_current(path);
            match trash.trash(path) {
                Ok(item) => {
                    self.outcome.trashed.push(item);
                    self.outcome.done += 1;
                }
                Err(error) => self.fail(path, error),
            }
            self.advance(0, 1);
        }
        Ok(())
    }

    fn delete(&mut self, paths: &[PathBuf]) -> Step {
        for path in paths {
            self.check()?;
            self.progress.total_items += scan(path).1;
        }
        self.report();
        for path in paths {
            self.check()?;
            self.set_current(path);
            let mut ok = true;
            for entry in walkdir::WalkDir::new(path).contents_first(true).follow_links(false) {
                self.check()?;
                let result = match &entry {
                    Ok(entry) if entry.file_type().is_dir() => fs::remove_dir(entry.path()),
                    Ok(entry) => fs::remove_file(entry.path()),
                    Err(error) => Err(io::Error::other(error.to_string())),
                };
                if let Err(error) = result {
                    ok = false;
                    let failed = entry
                        .as_ref()
                        .map(|e| e.path().to_path_buf())
                        .unwrap_or_else(|_| path.clone());
                    self.fail_io(&failed, &error);
                }
                self.advance(0, 1);
            }
            if ok {
                self.outcome.done += 1;
            }
        }
        Ok(())
    }

    fn restore(&mut self, items: &[TrashItem], trash: &Trash) -> Step {
        self.progress.total_items = items.len() as u64;
        self.report();
        for item in items {
            self.check()?;
            self.progress.current = item.name.clone();
            let mut target = item.original.clone();
            if let Ok(existing) = fs::symlink_metadata(&target) {
                let conflict = Conflict {
                    source: item.file.clone(),
                    target: target.clone(),
                    source_is_dir: item.is_dir,
                    target_is_dir: existing.is_dir(),
                };
                match self.decide(&conflict)? {
                    Resolution::Skip => {
                        self.outcome.skipped += 1;
                        self.advance(0, 1);
                        continue;
                    }
                    Resolution::KeepBoth => {
                        let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
                        target = parent.join(names::unique(&item.name, Style::Numbered, |n| {
                            fs::symlink_metadata(parent.join(n)).is_ok()
                        }));
                    }
                    Resolution::Replace => {
                        if let Err(error) = trash::remove_any(&target) {
                            self.fail_io(&target, &error);
                            self.advance(0, 1);
                            continue;
                        }
                    }
                }
            }
            match trash.restore(item, &target) {
                Ok(()) => {
                    self.outcome.created.push(target);
                    self.outcome.done += 1;
                }
                Err(error) => self.fail_io(&item.original, &error),
            }
            self.advance(0, 1);
        }
        Ok(())
    }

    fn delete_trashed(&mut self, items: &[TrashItem], trash: &Trash) -> Step {
        self.progress.total_items = items.len() as u64;
        self.report();
        for item in items {
            self.check()?;
            self.progress.current = item.name.clone();
            match trash.delete(item) {
                Ok(()) => self.outcome.done += 1,
                Err(error) => self.fail_io(&item.original, &error),
            }
            self.advance(0, 1);
        }
        Ok(())
    }

    fn rename(&mut self, path: &Path, new_name: &str) {
        if let Err(error) = names::validate(new_name) {
            self.fail(path, error);
            return;
        }
        let target = path.with_file_name(new_name);
        if target == path {
            return;
        }
        if fs::symlink_metadata(&target).is_ok() {
            self.fail(path, format!("A file named “{new_name}” already exists"));
            return;
        }
        match fs::rename(path, &target) {
            Ok(()) => {
                self.outcome.created.push(target);
                self.outcome.done += 1;
            }
            Err(error) => self.fail_io(path, &error),
        }
    }

    fn create_folder(&mut self, parent: &Path, name: &str) {
        if let Err(error) = names::validate(name) {
            self.fail(parent, error);
            return;
        }
        let target = parent.join(name);
        match fs::create_dir(&target) {
            Ok(()) => {
                self.outcome.created.push(target);
                self.outcome.done += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.fail(&target, format!("A file named “{name}” already exists"));
            }
            Err(error) => self.fail_io(&target, &error),
        }
    }
}

/// Total bytes of regular files and the number of entries in a tree, without following symlinks.
pub fn scan(path: &Path) -> (u64, u64) {
    walkdir::WalkDir::new(path).follow_links(false).into_iter().flatten().fold(
        (0, 0),
        |(bytes, items), entry| {
            let size = if entry.file_type().is_file() {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };
            (bytes + size, items + 1)
        },
    )
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// A short, human message for an I/O error, without the "(os error N)" suffix.
pub fn error_message(error: &io::Error) -> String {
    use io::ErrorKind::*;
    match error.kind() {
        PermissionDenied => "Permission denied".into(),
        NotFound => "The file no longer exists".into(),
        AlreadyExists => "A file with that name already exists".into(),
        StorageFull => "There's no space left on the drive".into(),
        ReadOnlyFilesystem => "The drive is read-only".into(),
        DirectoryNotEmpty => "The folder isn't empty".into(),
        InvalidFilename => "The name isn't valid on this drive".into(),
        CrossesDevices => "The file is on another drive".into(),
        _ => {
            let text = error.to_string();
            match text.find(" (os error") {
                Some(i) => text[..i].to_string(),
                None => text,
            }
        }
    }
}

fn quoted_name(path: &Path) -> String {
    format!("“{}”", display_name(path))
}

fn count_or_name(paths: &[PathBuf]) -> String {
    match paths {
        [one] => quoted_name(one),
        many => format!("{} items", many.len()),
    }
}

impl Operation {
    /// A description of the running operation, such as "Copying 3 items to “Documents”".
    pub fn title(&self) -> String {
        match self {
            Operation::Copy { sources, dest } => {
                format!("Copying {} to {}", count_or_name(sources), quoted_name(dest))
            }
            Operation::Move { sources, dest } => {
                format!("Moving {} to {}", count_or_name(sources), quoted_name(dest))
            }
            Operation::Trash { paths } => format!("Moving {} to the Trash", count_or_name(paths)),
            Operation::Delete { paths } => format!("Deleting {}", count_or_name(paths)),
            Operation::Restore { items } => match items.as_slice() {
                [one] => format!("Restoring “{}”", one.name),
                many => format!("Restoring {} items", many.len()),
            },
            Operation::DeleteTrashed { items } => match items.as_slice() {
                [one] => format!("Deleting “{}”", one.name),
                many => format!("Deleting {} items from the Trash", many.len()),
            },
            Operation::Rename { path, new_name } => {
                format!("Renaming {} to “{new_name}”", quoted_name(path))
            }
            Operation::CreateFolder { name, .. } => format!("Creating “{name}”"),
        }
    }

    /// Whether the operation is quick enough that it shows no progress, only its result.
    pub fn is_instant(&self) -> bool {
        matches!(self, Operation::Rename { .. } | Operation::CreateFolder { .. })
    }

    /// A one-line summary of a finished operation for a toast, or `None` when nothing is worth saying.
    pub fn summary(&self, outcome: &Outcome) -> Option<String> {
        if let Some((path, error)) = outcome.errors.first() {
            let verb = match self {
                Operation::Copy { .. } => "copy",
                Operation::Move { .. } => "move",
                Operation::Trash { .. } => "move to the Trash",
                Operation::Delete { .. } | Operation::DeleteTrashed { .. } => "delete",
                Operation::Restore { .. } => "restore",
                Operation::Rename { .. } => "rename",
                Operation::CreateFolder { .. } => "create",
            };
            return Some(if outcome.errors.len() == 1 {
                format!("Couldn't {verb} {}: {error}", quoted_name(path))
            } else {
                format!("Couldn't {verb} {} items: {error}", outcome.errors.len())
            });
        }
        if outcome.cancelled {
            return Some("Cancelled".into());
        }
        let n = outcome.done;
        let items = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        match self {
            Operation::Trash { paths } if n > 0 => Some(if paths.len() == 1 && n == 1 {
                format!("{} moved to the Trash", quoted_name(&paths[0]))
            } else {
                format!("{} moved to the Trash", items(n))
            }),
            Operation::Delete { .. } if n > 0 => Some(format!("Deleted {}", items(n))),
            Operation::DeleteTrashed { .. } if n > 0 => {
                Some(format!("Deleted {} permanently", items(n)))
            }
            Operation::Restore { .. } if n > 0 => Some(format!("Restored {}", items(n))),
            Operation::Copy { .. } | Operation::Move { .. } if outcome.skipped > 0 => {
                Some(format!("Skipped {}", items(outcome.skipped)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Answers conflicts from a script and cancels after a number of progress reports.
    struct Script {
        answers: Vec<Option<Decision>>,
        conflicts: Vec<Conflict>,
        reports: usize,
        cancel_after: Option<usize>,
        cancelled: Cell<bool>,
        last: Progress,
    }

    impl Script {
        fn new(answers: Vec<Option<Decision>>) -> Self {
            Self {
                answers,
                conflicts: Vec::new(),
                reports: 0,
                cancel_after: None,
                cancelled: Cell::new(false),
                last: Progress::default(),
            }
        }
    }

    impl Observer for Script {
        fn is_cancelled(&self) -> bool {
            self.cancelled.get()
        }
        fn progress(&mut self, progress: &Progress) {
            self.reports += 1;
            self.last = progress.clone();
            if self.cancel_after.is_some_and(|n| self.reports >= n) {
                self.cancelled.set(true);
            }
        }
        fn resolve(&mut self, conflict: &Conflict) -> Option<Decision> {
            self.conflicts.push(conflict.clone());
            if self.answers.is_empty() { None } else { self.answers.remove(0) }
        }
    }

    fn decision(resolution: Resolution, apply_to_all: bool) -> Option<Decision> {
        Some(Decision { resolution, apply_to_all })
    }

    struct Fixture {
        dir: tempfile::TempDir,
        trash: Trash,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let trash = Trash::new(dir.path().join(".trash"), 1000, vec![PathBuf::from("/")]);
            fs::create_dir(dir.path().join("src")).expect("mkdir");
            fs::create_dir(dir.path().join("dst")).expect("mkdir");
            Self { dir, trash }
        }
        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }
        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let path = self.path(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir");
            }
            fs::write(&path, content).expect("write");
            path
        }
        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.path(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
        }
        fn run(&self, op: Operation, script: &mut Script) -> Outcome {
            run(&op, &self.trash, script)
        }
    }

    #[test]
    fn copies_trees_with_metadata_and_progress() {
        let fx = Fixture::new();
        let a = fx.write("src/a.txt", "hello");
        fs::set_permissions(&a, fs::Permissions::from_mode(0o640)).expect("chmod");
        fx.write("src/tree/inner/b.txt", "world!");
        std::os::unix::fs::symlink("inner/b.txt", fx.path("src/tree/link")).expect("symlink");
        let mut script = Script::new(vec![]);
        let outcome = fx.run(
            Operation::Copy { sources: vec![a.clone(), fx.path("src/tree")], dest: fx.path("dst") },
            &mut script,
        );
        assert_eq!(outcome.errors, []);
        assert_eq!(outcome.created, [fx.path("dst/a.txt"), fx.path("dst/tree")]);
        assert_eq!(outcome.done, 2);
        assert_eq!(fx.read("dst/a.txt"), "hello");
        assert_eq!(fx.read("dst/tree/inner/b.txt"), "world!");
        assert_eq!(
            fs::read_link(fx.path("dst/tree/link")).expect("link"),
            PathBuf::from("inner/b.txt")
        );
        let meta = fs::metadata(fx.path("dst/a.txt")).expect("meta");
        assert_eq!(meta.mode() & 0o777, 0o640);
        assert_eq!(meta.modified().ok(), fs::metadata(&a).and_then(|m| m.modified()).ok());
        assert_eq!(script.last.total_bytes, 11);
        assert_eq!(script.last.done_bytes, 11);
        assert_eq!(script.last.done_items, script.last.total_items);
        assert_eq!(script.last.fraction(), 1.0);
        assert!(a.exists());
    }

    #[test]
    fn copy_into_same_folder_duplicates() {
        let fx = Fixture::new();
        let a = fx.write("src/a.txt", "1");
        let outcome = fx.run(
            Operation::Copy { sources: vec![a.clone()], dest: fx.path("src") },
            &mut Script::new(vec![]),
        );
        assert_eq!(outcome.created, [fx.path("src/a (copy).txt")]);
        let outcome = fx.run(
            Operation::Copy { sources: vec![a], dest: fx.path("src") },
            &mut Script::new(vec![]),
        );
        assert_eq!(outcome.created, [fx.path("src/a (copy 2).txt")]);
    }

    #[test]
    fn refuses_to_copy_a_folder_into_itself() {
        let fx = Fixture::new();
        fx.write("src/tree/x", "");
        let outcome = fx.run(
            Operation::Copy { sources: vec![fx.path("src/tree")], dest: fx.path("src/tree/x") },
            &mut Script::new(vec![]),
        );
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].1.contains("into itself"));
        let outcome = fx.run(
            Operation::Move { sources: vec![fx.path("src/tree")], dest: fx.path("src/tree") },
            &mut Script::new(vec![]),
        );
        assert!(outcome.errors[0].1.contains("move a folder into itself"));
    }

    #[test]
    fn conflicts_skip_replace_keep_both() {
        let fx = Fixture::new();
        let sources: Vec<PathBuf> =
            ["one", "two", "three"].iter().map(|n| fx.write(&format!("src/{n}"), "new")).collect();
        for n in ["one", "two", "three"] {
            fx.write(&format!("dst/{n}"), "old");
        }
        let mut script = Script::new(vec![
            decision(Resolution::Skip, false),
            decision(Resolution::KeepBoth, false),
            decision(Resolution::Replace, false),
        ]);
        let outcome = fx.run(Operation::Copy { sources, dest: fx.path("dst") }, &mut script);
        assert_eq!(script.conflicts.len(), 3);
        assert!(!script.conflicts[0].is_merge());
        assert_eq!(outcome.skipped, 1);
        assert_eq!(fx.read("dst/one"), "old");
        assert_eq!(fx.read("dst/two"), "old");
        assert_eq!(fx.read("dst/two (2)"), "new");
        assert_eq!(fx.read("dst/three"), "new");
        assert_eq!(outcome.created, [fx.path("dst/two (2)"), fx.path("dst/three")]);
    }

    #[test]
    fn apply_to_all_and_cancel_from_dialog() {
        let fx = Fixture::new();
        let sources: Vec<PathBuf> =
            ["a", "b", "c"].iter().map(|n| fx.write(&format!("src/{n}"), "new")).collect();
        for n in ["a", "b", "c"] {
            fx.write(&format!("dst/{n}"), "old");
        }
        let mut script = Script::new(vec![decision(Resolution::Replace, true)]);
        let outcome =
            fx.run(Operation::Copy { sources: sources.clone(), dest: fx.path("dst") }, &mut script);
        assert_eq!(script.conflicts.len(), 1);
        assert_eq!(outcome.done, 3);
        assert_eq!(fx.read("dst/c"), "new");

        let mut script = Script::new(vec![]);
        let outcome = fx.run(Operation::Copy { sources, dest: fx.path("dst") }, &mut script);
        assert!(outcome.cancelled);
        assert_eq!(outcome.done, 0);
    }

    #[test]
    fn merges_folders() {
        let fx = Fixture::new();
        fx.write("src/photos/new.png", "n");
        fx.write("src/photos/same.png", "src");
        fx.write("dst/photos/old.png", "o");
        fx.write("dst/photos/same.png", "dst");
        let mut script = Script::new(vec![
            decision(Resolution::Replace, false),
            decision(Resolution::Skip, false),
        ]);
        let outcome = fx.run(
            Operation::Move { sources: vec![fx.path("src/photos")], dest: fx.path("dst") },
            &mut script,
        );
        assert!(script.conflicts[0].is_merge());
        assert_eq!(script.conflicts.len(), 2);
        assert_eq!(fx.read("dst/photos/new.png"), "n");
        assert_eq!(fx.read("dst/photos/old.png"), "o");
        assert_eq!(fx.read("dst/photos/same.png"), "dst");
        // The skipped file stays behind, so the source folder does too.
        assert!(fx.path("src/photos/same.png").exists());
        assert!(!fx.path("src/photos/new.png").exists());
        assert_eq!(outcome.created, [fx.path("dst/photos")]);
    }

    #[test]
    fn moves_by_renaming() {
        let fx = Fixture::new();
        let a = fx.write("src/a", "x");
        fx.write("src/dir/b", "y");
        let mut script = Script::new(vec![]);
        let outcome = fx.run(
            Operation::Move { sources: vec![a.clone(), fx.path("src/dir")], dest: fx.path("dst") },
            &mut script,
        );
        assert_eq!(outcome.errors, []);
        assert!(!a.exists());
        assert_eq!(fx.read("dst/dir/b"), "y");
        assert_eq!(script.last.total_items, 2);
        assert_eq!(script.last.done_items, 2);
        // Moving onto itself is a no-op.
        let outcome = fx.run(
            Operation::Move { sources: vec![fx.path("dst/a")], dest: fx.path("dst") },
            &mut Script::new(vec![]),
        );
        assert_eq!(outcome.skipped, 1);
        assert!(fx.path("dst/a").exists());
    }

    #[test]
    fn cancel_removes_partial_copy() {
        let fx = Fixture::new();
        let big = fx.path("src/big");
        fs::write(&big, vec![7u8; CHUNK * 3]).expect("write");
        let mut script = Script::new(vec![]);
        // One report after scanning, then one per chunk.
        script.cancel_after = Some(2);
        let outcome =
            fx.run(Operation::Copy { sources: vec![big], dest: fx.path("dst") }, &mut script);
        assert!(outcome.cancelled);
        assert!(!fx.path("dst/big").exists());
    }

    #[test]
    fn delete_and_errors() {
        let fx = Fixture::new();
        fx.write("src/tree/a/b/c", "1");
        let f = fx.write("src/f", "2");
        let mut script = Script::new(vec![]);
        let outcome = fx.run(
            Operation::Delete {
                paths: vec![fx.path("src/tree"), f.clone(), fx.path("src/missing")],
            },
            &mut script,
        );
        assert!(!fx.path("src/tree").exists());
        assert!(!f.exists());
        assert_eq!(outcome.done, 2);
        assert_eq!(outcome.errors.len(), 1);
        let op = Operation::Delete { paths: vec![fx.path("src/missing")] };
        assert!(op.summary(&outcome).is_some_and(|s| s.starts_with("Couldn't delete")));
    }

    #[test]
    fn trash_and_restore_with_conflict() {
        let fx = Fixture::new();
        let a = fx.write("src/a.txt", "first");
        let outcome = fx.run(Operation::Trash { paths: vec![a.clone()] }, &mut Script::new(vec![]));
        assert_eq!(outcome.trashed.len(), 1);
        assert!(!a.exists());
        let op = Operation::Trash { paths: vec![a.clone()] };
        assert_eq!(op.summary(&outcome).as_deref(), Some("“a.txt” moved to the Trash"));

        fx.write("src/a.txt", "second");
        let mut script = Script::new(vec![decision(Resolution::KeepBoth, false)]);
        let outcome = fx.run(Operation::Restore { items: outcome.trashed.clone() }, &mut script);
        assert_eq!(outcome.created, [fx.path("src/a (2).txt")]);
        assert_eq!(fx.read("src/a (2).txt"), "first");
        assert_eq!(fx.read("src/a.txt"), "second");

        let outcome = fx.run(Operation::Trash { paths: vec![a] }, &mut Script::new(vec![]));
        let items = fx.trash.list();
        assert_eq!(items.len(), 1);
        let done = fx.run(Operation::DeleteTrashed { items }, &mut Script::new(vec![]));
        assert_eq!(done.done, 1);
        assert!(fx.trash.list().is_empty());
        assert_eq!(outcome.trashed.len(), 1);
    }

    #[test]
    fn rename_and_create_folder() {
        let fx = Fixture::new();
        let a = fx.write("src/a", "");
        fx.write("src/b", "");
        let mut script = Script::new(vec![]);
        let outcome =
            fx.run(Operation::Rename { path: a.clone(), new_name: "b".into() }, &mut script);
        assert!(outcome.errors[0].1.contains("already exists"));
        let outcome =
            fx.run(Operation::Rename { path: a.clone(), new_name: "x/y".into() }, &mut script);
        assert_eq!(outcome.errors[0].1, "Names can't contain “/”");
        let outcome = fx.run(Operation::Rename { path: a, new_name: "c".into() }, &mut script);
        assert_eq!(outcome.created, [fx.path("src/c")]);

        let outcome = fx.run(
            Operation::CreateFolder { parent: fx.path("src"), name: "New".into() },
            &mut script,
        );
        assert!(fx.path("src/New").is_dir());
        assert_eq!(outcome.done, 1);
        let outcome = fx.run(
            Operation::CreateFolder { parent: fx.path("src"), name: "New".into() },
            &mut script,
        );
        assert!(outcome.errors[0].1.contains("already exists"));
    }

    #[test]
    fn titles_and_messages() {
        let op = Operation::Copy {
            sources: vec![PathBuf::from("/a/x.txt")],
            dest: PathBuf::from("/b/Docs"),
        };
        assert_eq!(op.title(), "Copying “x.txt” to “Docs”");
        let op = Operation::Move {
            sources: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            dest: PathBuf::from("/c"),
        };
        assert_eq!(op.title(), "Moving 2 items to “c”");
        assert!(!op.is_instant());
        assert!(Operation::CreateFolder { parent: "/".into(), name: "x".into() }.is_instant());
        let outcome = Outcome { cancelled: true, ..Outcome::default() };
        assert_eq!(op.summary(&outcome).as_deref(), Some("Cancelled"));
        assert_eq!(op.summary(&Outcome::default()), None);
        assert_eq!(
            error_message(&io::Error::from(io::ErrorKind::PermissionDenied)),
            "Permission denied"
        );
        assert_eq!(error_message(&io::Error::from_raw_os_error(5)), "Input/output error");
        assert_eq!(Progress::default().fraction(), 0.0);
        let p = Progress { total_items: 4, done_items: 1, ..Progress::default() };
        assert_eq!(p.fraction(), 0.25);
    }
}
