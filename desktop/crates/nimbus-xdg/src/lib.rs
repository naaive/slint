// SPDX-License-Identifier: MIT

//! Freedesktop.org integration: desktop entries, icon themes, application search, and launching.
//!
//! Implements the Desktop Entry Specification 1.5 and the Icon Theme Specification 0.13.

use std::path::{Path, PathBuf};

/// One application from a `.desktop` file of `Type=Application`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesktopEntry {
    /// The desktop file id, such as `org.gnome.Nautilus`, without the `.desktop` suffix.
    pub id: String,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    /// The raw `Icon=` value: an icon name or an absolute path.
    pub icon: Option<String>,
    /// The raw `Exec=` value including field codes.
    pub exec: String,
    pub terminal: bool,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub startup_wm_class: Option<String>,
    pub path: PathBuf,
}

impl DesktopEntry {
    /// Parses one desktop file, picking localized keys for `locale` (such as `de_DE`).
    /// Returns `None` for entries that are hidden, `NoDisplay`, excluded by `OnlyShowIn`/`NotShowIn`
    /// for the `Nimbus` desktop, or fail `TryExec`.
    pub fn parse(id: &str, path: &Path, contents: &str, locale: Option<&str>) -> Option<Self> {
        let _ = (id, path, contents, locale);
        todo!()
    }

    /// Returns the command line for `Exec=` with field codes expanded for `files`, split into argv.
    pub fn command_line(&self, files: &[PathBuf]) -> Vec<String> {
        let _ = files;
        todo!()
    }
}

/// All visible applications, deduplicated by id with `$XDG_DATA_HOME` taking precedence over `$XDG_DATA_DIRS`.
#[derive(Clone, Debug, Default)]
pub struct AppIndex {
    pub entries: Vec<DesktopEntry>,
}

impl AppIndex {
    /// Scans `applications/` below each XDG data directory.
    pub fn scan() -> Self {
        todo!()
    }

    /// Scans `applications/` below each of `data_dirs`, highest precedence first.
    pub fn scan_dirs(data_dirs: &[PathBuf], locale: Option<&str>) -> Self {
        let _ = (data_dirs, locale);
        todo!()
    }

    pub fn get(&self, id: &str) -> Option<&DesktopEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Finds the entry for a running window's `app_id`, also matching `StartupWMClass` and case-insensitive ids.
    pub fn find_by_app_id(&self, app_id: &str) -> Option<&DesktopEntry> {
        let _ = app_id;
        todo!()
    }

    /// Ranks entries against `query` by fuzzy matching name, generic name, keywords, and id.
    /// An empty query returns all entries sorted by name.
    pub fn search(&self, query: &str) -> Vec<&DesktopEntry> {
        let _ = query;
        todo!()
    }
}

/// Icon lookup following the theme inheritance chain, falling back to `hicolor` and `pixmaps`.
#[derive(Clone, Debug)]
pub struct IconResolver {
    _private: (),
}

impl IconResolver {
    pub fn new(theme: &str) -> Self {
        let _ = theme;
        todo!()
    }

    /// Like [`IconResolver::new`], searching `icons/` below `data_dirs` instead of the XDG ones.
    pub fn with_data_dirs(theme: &str, data_dirs: &[PathBuf]) -> Self {
        let _ = (theme, data_dirs);
        todo!()
    }

    /// Returns the best file for `icon` (a name or absolute path) at `size` logical pixels and `scale`.
    /// Prefers PNG and SVG; results are cached.
    pub fn lookup(&self, icon: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let _ = (icon, size, scale);
        todo!()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("the desktop entry '{0}' has an empty Exec line")]
    EmptyExec(String),
    #[error("cannot start '{command}': {source}")]
    Spawn { command: String, source: std::io::Error },
}

/// Starts `entry` detached from the caller, in a terminal when `Terminal=true`,
/// setting `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID` when `activation_token` is given.
pub fn launch(entry: &DesktopEntry, files: &[PathBuf], activation_token: Option<&str>) -> Result<(), LaunchError> {
    let _ = (entry, files, activation_token);
    todo!()
}
