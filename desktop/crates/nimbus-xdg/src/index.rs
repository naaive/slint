// SPDX-License-Identifier: MIT

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::entry::{DesktopEntry, ParseOptions};
use crate::keyfile::locale_from_env;

/// Limits recursion below `applications/` in case of symlink loops the canonical-path check misses.
const MAX_DEPTH: usize = 16;

/// `$XDG_DATA_HOME`, or `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
}

/// `$XDG_DATA_DIRS`, or `/usr/local/share:/usr/share`.
pub fn system_data_dirs() -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_DIRS")
        .map(|v| std::env::split_paths(&v).filter(|p| p.is_absolute()).collect())
        .unwrap_or_default();
    if dirs.is_empty() {
        vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]
    } else {
        dirs
    }
}

/// The XDG data directories, highest precedence first: `$XDG_DATA_HOME`, `$XDG_DATA_DIRS`,
/// then the user and system Flatpak exports when they exist. Duplicates are removed.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = data_home().into_iter().collect();
    dirs.extend(system_data_dirs());
    let flatpak_user = dirs::home_dir().map(|home| home.join(".local/share/flatpak/exports/share"));
    let flatpak_system = PathBuf::from("/var/lib/flatpak/exports/share");
    dirs.extend(flatpak_user.into_iter().chain([flatpak_system]).filter(|p| p.is_dir()));
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

/// Collects `(desktop file id, path)` for every `.desktop` file below `applications`,
/// where the id is the relative path with `/` replaced by `-`, without the suffix.
pub(crate) fn desktop_files(applications: &Path) -> Vec<(String, PathBuf)> {
    fn walk(
        dir: &Path,
        prefix: &str,
        depth: usize,
        visited: &mut HashSet<PathBuf>,
        out: &mut Vec<(String, PathBuf)>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        if let Ok(canonical) = dir.canonicalize()
            && !visited.insert(canonical)
        {
            return;
        }
        let Ok(read_dir) = std::fs::read_dir(dir) else { return };
        let mut children: Vec<_> = read_dir.filter_map(Result::ok).collect();
        children.sort_by_key(|c| c.file_name());
        for child in children {
            let Some(name) = child.file_name().to_str().map(str::to_owned) else { continue };
            let path = child.path();
            // Follows symlinks, as both files and directories may be linked in.
            let Ok(metadata) = path.metadata() else { continue };
            if metadata.is_dir() {
                walk(&path, &format!("{prefix}{name}-"), depth + 1, visited, out);
            } else if let Some(stem) = name.strip_suffix(".desktop")
                && !stem.is_empty()
                && metadata.is_file()
            {
                out.push((format!("{prefix}{stem}"), path));
            }
        }
    }
    let mut out = Vec::new();
    walk(applications, "", 0, &mut HashSet::new(), &mut out);
    out
}

/// Maps each desktop file id to its highest-precedence file below `applications/` of `data_dirs`.
pub(crate) fn desktop_file_map(data_dirs: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for dir in data_dirs {
        for (id, path) in desktop_files(&dir.join("applications")) {
            map.entry(id).or_insert(path);
        }
    }
    map
}

/// All visible applications, deduplicated by id with `$XDG_DATA_HOME` taking precedence over `$XDG_DATA_DIRS`.
#[derive(Clone, Debug, Default)]
pub struct AppIndex {
    pub entries: Vec<DesktopEntry>,
}

impl AppIndex {
    /// Scans `applications/` below each XDG data directory.
    pub fn scan() -> Self {
        Self::scan_dirs(&data_dirs(), locale_from_env().as_deref())
    }

    /// Scans `applications/` below each of `data_dirs`, highest precedence first.
    pub fn scan_dirs(data_dirs: &[PathBuf], locale: Option<&str>) -> Self {
        Self::scan_dirs_with(data_dirs, &ParseOptions::from_env(locale))
    }

    /// Like [`AppIndex::scan_dirs`] with an explicit environment.
    /// A file shadows lower-precedence files with the same id even when it's hidden or invalid,
    /// so users can hide system applications with `Hidden=true`.
    pub fn scan_dirs_with(data_dirs: &[PathBuf], options: &ParseOptions) -> Self {
        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        for dir in data_dirs {
            for (id, path) in desktop_files(&dir.join("applications")) {
                if seen.contains(&id) {
                    continue;
                }
                let bytes = match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        tracing::debug!(path = %path.display(), %err, "skipping unreadable desktop file");
                        continue;
                    }
                };
                seen.insert(id.clone());
                let contents = String::from_utf8_lossy(&bytes);
                match DesktopEntry::parse_with(&id, &path, &contents, options) {
                    Ok(entry) => entries.push(entry),
                    Err(reason) => tracing::trace!(%id, %reason, "desktop entry not shown"),
                }
            }
        }
        entries.sort_by(crate::search::compare_names);
        Self { entries }
    }

    pub fn get(&self, id: &str) -> Option<&DesktopEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Finds the entry for a running window's `app_id`, also matching `StartupWMClass` and case-insensitive ids.
    ///
    /// Tries, in order: the exact id, the id ignoring case, `StartupWMClass`,
    /// and finally the last component of a reverse-DNS id on either side, such as `nautilus` for `org.gnome.Nautilus`.
    pub fn find_by_app_id(&self, app_id: &str) -> Option<&DesktopEntry> {
        let app_id = app_id.trim();
        let app_id = app_id.strip_suffix(".desktop").unwrap_or(app_id);
        if app_id.is_empty() {
            return None;
        }
        let last = |id: &str| id.rsplit('.').next().unwrap_or(id).to_owned();
        let app_last = last(app_id);
        self.get(app_id)
            .or_else(|| self.entries.iter().find(|e| e.id.eq_ignore_ascii_case(app_id)))
            .or_else(|| self.entries.iter().find(|e| e.startup_wm_class.as_deref() == Some(app_id)))
            .or_else(|| {
                self.entries.iter().find(|e| {
                    e.startup_wm_class.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(app_id))
                })
            })
            .or_else(|| self.entries.iter().find(|e| last(&e.id).eq_ignore_ascii_case(app_id)))
            .or_else(|| self.entries.iter().find(|e| e.id.eq_ignore_ascii_case(&app_last)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, contents).expect("write");
    }

    fn app(name: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec={}\n", name.to_lowercase())
    }

    fn options() -> ParseOptions {
        ParseOptions { desktops: vec!["Nimbus".into()], ..Default::default() }
    }

    #[test]
    fn ids_from_subdirectories() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "applications/kde/org.kate.desktop", &app("Kate"));
        write(dir.path(), "applications/top.desktop", &app("Top"));
        write(dir.path(), "applications/readme.txt", "x");
        let index = AppIndex::scan_dirs_with(&[dir.path().to_path_buf()], &options());
        let ids: Vec<&str> = index.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["kde-org.kate", "top"]);
    }

    #[test]
    fn earlier_directories_win_and_hidden_masks() {
        let home = tempfile::tempdir().expect("tempdir");
        let system = tempfile::tempdir().expect("tempdir");
        write(home.path(), "applications/editor.desktop", &app("My Editor"));
        write(system.path(), "applications/editor.desktop", &app("Editor"));
        write(home.path(), "applications/game.desktop", &format!("{}Hidden=true\n", app("Game")));
        write(system.path(), "applications/game.desktop", &app("Game"));
        write(system.path(), "applications/other.desktop", &app("Other"));
        let index = AppIndex::scan_dirs_with(
            &[home.path().to_path_buf(), system.path().to_path_buf()],
            &options(),
        );
        assert_eq!(index.get("editor").map(|e| e.name.as_str()), Some("My Editor"));
        assert!(index.get("game").is_none());
        assert!(index.get("other").is_some());
        assert_eq!(index.entries.len(), 2);
    }

    #[test]
    fn missing_directories_are_empty() {
        let index =
            AppIndex::scan_dirs_with(&[PathBuf::from("/nonexistent/nimbus-test")], &options());
        assert!(index.entries.is_empty());
    }

    #[test]
    fn find_by_app_id_order() {
        let entry = |id: &str, wm: Option<&str>| DesktopEntry {
            id: id.into(),
            name: id.into(),
            startup_wm_class: wm.map(str::to_owned),
            ..Default::default()
        };
        let index = AppIndex {
            entries: vec![
                entry("org.gnome.Nautilus", None),
                entry("firefox", None),
                entry("code", Some("Code-OSS")),
                entry("Alacritty", None),
            ],
        };
        let found = |app_id: &str| index.find_by_app_id(app_id).map(|e| e.id.as_str());
        assert_eq!(found("firefox"), Some("firefox"));
        assert_eq!(found("firefox.desktop"), Some("firefox"));
        assert_eq!(found("alacritty"), Some("Alacritty"));
        assert_eq!(found("Code-OSS"), Some("code"));
        assert_eq!(found("code-oss"), Some("code"));
        assert_eq!(found("nautilus"), Some("org.gnome.Nautilus"));
        assert_eq!(found("org.mozilla.firefox"), Some("firefox"));
        assert_eq!(found("unknown"), None);
        assert_eq!(found(""), None);
    }
}
