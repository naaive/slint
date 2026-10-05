// SPDX-License-Identifier: MIT

//! Preferences saved in `$XDG_CONFIG_HOME/nimbus/monitor.toml`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::processes::SortColumn;

pub const MIN_INTERVAL_MS: u64 = 500;
pub const MAX_INTERVAL_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Page {
    #[default]
    Processes,
    Resources,
    FileSystems,
}

impl Page {
    pub const ALL: [Page; 3] = [Self::Processes, Self::Resources, Self::FileSystems];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "processes" => Some(Self::Processes),
            "resources" => Some(Self::Resources),
            "file-systems" | "filesystems" => Some(Self::FileSystems),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub interval_ms: u64,
    pub page: Page,
    pub sort: SortColumn,
    pub descending: bool,
    pub tree: bool,
    pub all_users: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            interval_ms: 2000,
            page: Page::default(),
            sort: SortColumn::Cpu,
            descending: true,
            tree: false,
            all_users: false,
        }
    }
}

impl Prefs {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("nimbus").join("monitor.toml"))
    }

    /// Reads the preferences, falling back to the defaults for a missing or unreadable file.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str::<Prefs>(&text).map(Prefs::sanitized).unwrap_or_else(|err| {
                    tracing::warn!("ignoring {}: {err}", path.display());
                    Prefs::default()
                })
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Prefs::default(),
            Err(err) => {
                tracing::warn!("cannot read {}: {err}", path.display());
                Prefs::default()
            }
        }
    }

    /// Writes the preferences atomically.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, path)
    }

    pub fn sanitized(mut self) -> Self {
        self.interval_ms = self.interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
        self
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_sanitize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("monitor.toml");
        assert_eq!(Prefs::load(&path), Prefs::default());
        let prefs = Prefs {
            interval_ms: 1000,
            page: Page::FileSystems,
            sort: SortColumn::Memory,
            tree: true,
            ..Prefs::default()
        };
        prefs.save(&path).unwrap();
        assert_eq!(Prefs::load(&path), prefs);

        std::fs::write(&path, "interval_ms = 1\npage = \"resources\"\n").unwrap();
        let loaded = Prefs::load(&path);
        assert_eq!(loaded.interval_ms, MIN_INTERVAL_MS);
        assert_eq!(loaded.page, Page::Resources);

        std::fs::write(&path, "page = 7").unwrap();
        assert_eq!(Prefs::load(&path), Prefs::default());
    }

    #[test]
    fn page_names() {
        assert_eq!(Page::from_name("file-systems"), Some(Page::FileSystems));
        assert_eq!(Page::from_name("nope"), None);
        assert_eq!(Page::from_index(Page::Resources.index()), Some(Page::Resources));
    }
}
