// SPDX-License-Identifier: MIT

//! The terminal's own preferences, stored in `$XDG_CONFIG_HOME/nimbus/terminal.toml`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::palette::SYSTEM_SCHEME;

pub const MIN_FONT_SIZE: f32 = 6.0;
pub const MAX_FONT_SIZE: f32 = 72.0;
/// The largest scrollback `alacritty_terminal` keeps.
pub const MAX_SCROLLBACK: usize = 100_000;
/// The scrollback lengths offered in the preferences.
pub const SCROLLBACK_CHOICES: [usize; 4] = [1_000, 10_000, 50_000, MAX_SCROLLBACK];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorShape {
    #[default]
    Block,
    Beam,
    Underline,
}

impl CursorShape {
    pub const ALL: [CursorShape; 3] =
        [CursorShape::Block, CursorShape::Beam, CursorShape::Underline];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn from_index(index: i32) -> Self {
        usize::try_from(index).ok().and_then(|i| Self::ALL.get(i).copied()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Prefs {
    /// A monospace font family; empty picks the best installed one.
    pub font_family: String,
    /// In points.
    pub font_size: f32,
    /// A scheme id from [`crate::palette`].
    pub color_scheme: String,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
    pub scrollback_lines: usize,
    /// Flashes the terminal on the bell character.
    pub visual_bell: bool,
    /// Brightens bold text in the eight basic colors.
    pub bold_is_bright: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 11.0,
            color_scheme: SYSTEM_SCHEME.into(),
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
            scrollback_lines: 10_000,
            visual_bell: true,
            bold_is_bright: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrefsError {
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid terminal preferences in {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("cannot serialize the terminal preferences: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// `$XDG_CONFIG_HOME/nimbus/terminal.toml`.
pub fn default_path() -> Result<PathBuf, nimbus_config::Error> {
    Ok(nimbus_config::default_path()?.with_file_name("terminal.toml"))
}

impl Prefs {
    /// Clamps out-of-range values into the supported ranges.
    pub fn sanitized(mut self) -> Self {
        self.font_size = clamp_font_size(self.font_size);
        self.scrollback_lines = self.scrollback_lines.min(MAX_SCROLLBACK);
        self.font_family = self.font_family.trim().to_string();
        if crate::palette::scheme_name(&self.color_scheme).is_none() {
            self.color_scheme = SYSTEM_SCHEME.into();
        }
        self
    }

    /// Loads `path`, or returns the defaults when it doesn't exist.
    pub fn load_from(path: &Path) -> Result<Self, PrefsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Prefs>(&text)
                .map(Prefs::sanitized)
                .map_err(|source| PrefsError::Parse { path: path.into(), source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(PrefsError::Io { path: path.into(), source }),
        }
    }

    /// Loads `path`, or returns the defaults when it's missing, unreadable, or invalid.
    ///
    /// A file that exists but fails to load is copied to `<path>.bak`, since the next save replaces it.
    pub fn load_or_default(path: &Path) -> Self {
        Self::load_from(path).unwrap_or_else(|error| {
            tracing::warn!("{error}");
            let backup = path.with_extension("toml.bak");
            if let Err(error) = std::fs::copy(path, &backup) {
                tracing::warn!(
                    "cannot back up {} to {}: {error}",
                    path.display(),
                    backup.display()
                );
            }
            Self::default()
        })
    }

    /// Writes atomically through a temporary file next to `path`.
    pub fn save_to(&self, path: &Path) -> Result<(), PrefsError> {
        static SAVES: AtomicUsize = AtomicUsize::new(0);
        let io = |source| PrefsError::Io { path: path.into(), source };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let text = toml::to_string_pretty(self)?;
        let save = SAVES.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("toml.{}-{save}.tmp", std::process::id()));
        let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(io)
    }
}

/// Clamps a font size in points to the supported range; non-finite sizes become the default.
pub fn clamp_font_size(points: f32) -> f32 {
    if points.is_finite() {
        points.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    } else {
        Prefs::default().font_size
    }
}

/// The index in [`SCROLLBACK_CHOICES`] closest to `lines`.
pub fn scrollback_choice(lines: usize) -> usize {
    SCROLLBACK_CHOICES
        .iter()
        .enumerate()
        .min_by_key(|(_, choice)| choice.abs_diff(lines))
        .map_or(0, |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults_and_round_trips() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("nimbus/terminal.toml");
        assert_eq!(Prefs::load_from(&path).ok(), Some(Prefs::default()));
        let prefs = Prefs {
            font_family: "Fira Code".into(),
            font_size: 13.5,
            color_scheme: "dracula".into(),
            cursor_shape: CursorShape::Beam,
            cursor_blink: false,
            scrollback_lines: 50_000,
            visual_bell: false,
            bold_is_bright: false,
        };
        prefs.save_to(&path).expect("saving works");
        assert_eq!(Prefs::load_from(&path).ok(), Some(prefs));
    }

    #[test]
    fn partial_and_out_of_range_values() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("terminal.toml");
        std::fs::write(&path, "font-size = 500\nscrollback-lines = 9999999\ncolor-scheme = \"bogus\"\ncursor-shape = \"underline\"\n")
            .expect("writable");
        let prefs = Prefs::load_from(&path).expect("parses");
        assert_eq!(prefs.font_size, MAX_FONT_SIZE);
        assert_eq!(prefs.scrollback_lines, MAX_SCROLLBACK);
        assert_eq!(prefs.color_scheme, SYSTEM_SCHEME);
        assert_eq!(prefs.cursor_shape, CursorShape::Underline);
        assert!(prefs.cursor_blink);

        std::fs::write(&path, "font-size = [").expect("writable");
        assert!(matches!(Prefs::load_from(&path), Err(PrefsError::Parse { .. })));
    }

    #[test]
    fn invalid_file_is_backed_up() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("terminal.toml");
        let backup = dir.path().join("terminal.toml.bak");
        assert_eq!(Prefs::load_or_default(&path), Prefs::default());
        assert!(!backup.exists(), "a missing file needs no backup");

        std::fs::write(&path, "font-size = [").expect("writable");
        assert_eq!(Prefs::load_or_default(&path), Prefs::default());
        Prefs::default().save_to(&path).expect("saving works");
        assert_eq!(std::fs::read_to_string(&backup).ok().as_deref(), Some("font-size = ["));
    }

    #[test]
    fn saves_leave_no_temporary_files() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("terminal.toml");
        let prefs = Prefs { font_size: 14.0, ..Prefs::default() };
        prefs.save_to(&path).expect("saving works");
        prefs.save_to(&path).expect("saving again works");
        assert_eq!(Prefs::load_from(&path).ok(), Some(prefs.clone()));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readable")
            .map(|entry| entry.expect("an entry").file_name())
            .collect();
        assert_eq!(names, ["terminal.toml"]);

        let blocked = dir.path().join("blocked.toml");
        std::fs::create_dir_all(blocked.join("child")).expect("writable");
        assert!(prefs.save_to(&blocked).is_err(), "a non-empty directory can't be replaced");
        assert_eq!(std::fs::read_dir(dir.path()).expect("readable").count(), 2);
    }

    #[test]
    fn helpers() {
        assert_eq!(clamp_font_size(f32::NAN), 11.0);
        assert_eq!(clamp_font_size(1.0), MIN_FONT_SIZE);
        assert_eq!(scrollback_choice(10_000), 1);
        assert_eq!(scrollback_choice(0), 0);
        assert_eq!(scrollback_choice(70_000), 2);
        assert_eq!(scrollback_choice(usize::MAX), 3);
        assert_eq!(CursorShape::from_index(1), CursorShape::Beam);
        assert_eq!(CursorShape::from_index(-1), CursorShape::Block);
        assert_eq!(CursorShape::from_index(9), CursorShape::Block);
        assert_eq!(CursorShape::Underline.index(), 2);
    }
}
