// SPDX-License-Identifier: MIT

//! View preferences remembered between sessions, in `$XDG_CONFIG_HOME/nimbus/files.toml`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

use super::sort::SortOptions;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    #[default]
    Grid,
    List,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub view: ViewMode,
    pub sort: SortOptions,
    pub show_hidden: bool,
    /// Grid icon zoom, from 0 (smallest) to 2 (largest).
    pub zoom: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { view: ViewMode::Grid, sort: SortOptions::default(), show_hidden: false, zoom: 1 }
    }
}

impl Preferences {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("nimbus").join("files.toml"))
    }

    /// Loads preferences, falling back to defaults for a missing or invalid file.
    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        match toml::from_str::<Self>(&text) {
            Ok(mut prefs) => {
                prefs.zoom = prefs.zoom.min(2);
                prefs
            }
            Err(error) => {
                tracing::warn!("ignoring invalid {}: {error}", path.display());
                Self::default()
            }
        }
    }

    /// Saves atomically, so a crash never leaves a truncated file.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let dir = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        std::fs::create_dir_all(dir)?;
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".files.toml.{}.{n}.tmp", std::process::id()));
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sort::SortKey;

    #[test]
    fn round_trip_and_fallbacks() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nimbus/files.toml");
        assert_eq!(Preferences::load_from(&path), Preferences::default());
        let prefs = Preferences {
            view: ViewMode::List,
            sort: SortOptions { key: SortKey::Modified, descending: true, folders_first: false },
            show_hidden: true,
            zoom: 2,
        };
        prefs.save_to(&path).expect("saved");
        assert_eq!(Preferences::load_from(&path), prefs);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("view = \"list\""));
        assert!(text.contains("key = \"modified\""));

        std::fs::write(&path, "view = \"list\"\nzoom = 9\nunknown = 1\n").expect("write");
        let partial = Preferences::load_from(&path);
        assert_eq!(partial.view, ViewMode::List);
        assert_eq!(partial.zoom, 2);
        assert!(partial.sort.folders_first);

        std::fs::write(&path, "view = 3").expect("write");
        assert_eq!(Preferences::load_from(&path), Preferences::default());
    }

    #[test]
    fn concurrent_saves_never_mix() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("files.toml");
        let snapshots: Vec<Preferences> = (0..16)
            .map(|i| Preferences {
                show_hidden: i % 2 == 0,
                zoom: (i % 3) as u8,
                view: if i % 4 < 2 { ViewMode::List } else { ViewMode::Grid },
                sort: SortOptions {
                    key: if i % 5 == 0 { SortKey::Modified } else { SortKey::Name },
                    descending: i % 3 == 0,
                    folders_first: i % 7 != 0,
                },
            })
            .collect();
        std::thread::scope(|scope| {
            for prefs in &snapshots {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..20 {
                        prefs.save_to(path).expect("saved");
                    }
                });
            }
        });
        assert!(snapshots.contains(&Preferences::load_from(&path)));
    }
}
