// SPDX-License-Identifier: MIT

//! Nimbus user configuration, stored as TOML in `$XDG_CONFIG_HOME/nimbus/config.toml`.
//!
//! Every field has a default, so a partial or missing file is valid.
//! Unknown keys are ignored so that older builds can read newer files.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod watch;
pub use watch::{ConfigWatcher, watch};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub panel: Panel,
    pub workspaces: Workspaces,
    pub input: Input,
    pub keybindings: Keybindings,
    pub power: Power,
    /// Command lines started once when the session starts.
    pub autostart: Vec<String>,
    /// Desktop entry ids pinned to the dock, in order, without the `.desktop` suffix.
    pub favorites: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorScheme {
    Light,
    Dark,
    #[default]
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub color_scheme: ColorScheme,
    /// Accent color as `#rrggbb`.
    pub accent: String,
    pub wallpaper: Option<PathBuf>,
    pub icon_theme: String,
    pub font_family: String,
    pub font_size: f32,
    pub scale: f64,
    pub animations: bool,
    pub corner_radius: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            color_scheme: ColorScheme::System,
            accent: "#3584e4".into(),
            wallpaper: None,
            icon_theme: "hicolor".into(),
            font_family: "Inter".into(),
            font_size: 11.0,
            scale: 1.0,
            animations: true,
            corner_radius: 12.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanelPosition {
    #[default]
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Panel {
    pub position: PanelPosition,
    pub height: u32,
    pub show_dock: bool,
    pub dock_autohide: bool,
    /// A `chrono`-style format string such as `%a %d %b  %H:%M`.
    pub clock_format: String,
    pub show_battery_percentage: bool,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            position: PanelPosition::Top,
            height: 32,
            show_dock: true,
            dock_autohide: false,
            clock_format: "%a %d %b  %H:%M".into(),
            show_battery_percentage: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefaultLayout {
    #[default]
    Floating,
    Tiling,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspaces {
    pub count: u32,
    pub layout: DefaultLayout,
    /// Gap between tiled windows, in logical pixels.
    pub gaps: u32,
    pub focus_follows_mouse: bool,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self { count: 4, layout: DefaultLayout::Floating, gaps: 8, focus_follows_mouse: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    /// XKB layout names, such as `us` or `de,us`.
    pub keyboard_layout: String,
    pub keyboard_variant: String,
    pub keyboard_options: String,
    pub repeat_delay_ms: u32,
    pub repeat_rate: u32,
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    pub pointer_speed: f64,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            keyboard_layout: "us".into(),
            keyboard_variant: String::new(),
            keyboard_options: String::new(),
            repeat_delay_ms: 300,
            repeat_rate: 30,
            natural_scroll: true,
            tap_to_click: true,
            pointer_speed: 0.0,
        }
    }
}

/// Maps key chords such as `Super+Shift+Q` to actions.
///
/// Modifier names are `Super`, `Ctrl`, `Alt`, and `Shift`; the key is an XKB keysym name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Keybindings(pub BTreeMap<String, Action>);

/// An action bound to a key chord.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Spawn(String),
    CloseWindow,
    ToggleMaximize,
    ToggleFullscreen,
    Minimize,
    ToggleLayout,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    Workspace(u32),
    MoveToWorkspace(u32),
    NextWorkspace,
    PreviousWorkspace,
    ToggleLauncher,
    ToggleOverview,
    Lock,
    Screenshot,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    BrightnessUp,
    BrightnessDown,
    Quit,
}

impl Default for Keybindings {
    fn default() -> Self {
        use Action::*;
        let mut map = BTreeMap::new();
        let mut bind = |chord: &str, action: Action| {
            map.insert(chord.to_string(), action);
        };
        bind("Super+Return", Spawn("nimbus-terminal".into()));
        bind("Super+E", Spawn("nimbus-files".into()));
        bind("Super+Space", ToggleLauncher);
        bind("Super+Tab", ToggleOverview);
        bind("Super+Q", CloseWindow);
        bind("Super+Up", ToggleMaximize);
        bind("Super+F", ToggleFullscreen);
        bind("Super+H", Minimize);
        bind("Super+T", ToggleLayout);
        bind("Super+Left", FocusLeft);
        bind("Super+Right", FocusRight);
        bind("Super+Ctrl+Left", PreviousWorkspace);
        bind("Super+Ctrl+Right", NextWorkspace);
        bind("Super+L", Lock);
        bind("Print", Screenshot);
        bind("XF86AudioRaiseVolume", VolumeUp);
        bind("XF86AudioLowerVolume", VolumeDown);
        bind("XF86AudioMute", ToggleMute);
        bind("XF86MonBrightnessUp", BrightnessUp);
        bind("XF86MonBrightnessDown", BrightnessDown);
        bind("Super+Shift+E", Quit);
        for n in 1..=9u32 {
            bind(&format!("Super+{n}"), Workspace(n - 1));
            bind(&format!("Super+Shift+{n}"), MoveToWorkspace(n - 1));
        }
        Self(map)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Power {
    /// Minutes of inactivity before the screen locks; 0 disables locking.
    pub lock_after_minutes: u32,
    /// Minutes of inactivity before outputs blank; 0 disables blanking.
    pub blank_after_minutes: u32,
    pub suspend_on_lid_close: bool,
}

impl Default for Power {
    fn default() -> Self {
        Self { lock_after_minutes: 10, blank_after_minutes: 5, suspend_on_lid_close: true }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid configuration in {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("cannot serialize the configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigDir,
}

/// Returns `$XDG_CONFIG_HOME/nimbus/config.toml`.
pub fn default_path() -> Result<PathBuf, Error> {
    Ok(dirs::config_dir().ok_or(Error::NoConfigDir)?.join("nimbus").join("config.toml"))
}

impl Config {
    /// Loads `path`, or returns the defaults when it doesn't exist.
    pub fn load_from(path: &Path) -> Result<Self, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| Error::Parse { path: path.into(), source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io { path: path.into(), source }),
        }
    }

    pub fn load() -> Result<Self, Error> {
        Self::load_from(&default_path()?)
    }

    /// Writes atomically through a temporary file next to `path`.
    pub fn save_to(&self, path: &Path) -> Result<(), Error> {
        let io = |source| Error::Io { path: path.into(), source };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }

    pub fn save(&self) -> Result<(), Error> {
        self.save_to(&default_path()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config: Config = toml::from_str("[panel]\nheight = 40\n[keybindings]\n\"Super+X\" = \"lock\"\n").unwrap();
        assert_eq!(config.panel.height, 40);
        assert_eq!(config.panel.position, PanelPosition::Top);
        assert_eq!(config.keybindings.0.get("Super+X"), Some(&Action::Lock));
        assert_eq!(config.workspaces, Workspaces::default());
    }

    #[test]
    fn defaults_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.toml");
        assert_eq!(Config::load_from(&path).unwrap(), Config::default());
        let mut config = Config::default();
        config.favorites = vec!["org.nimbus.Files".into()];
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn parameterized_actions_parse() {
        let config: Config =
            toml::from_str("[keybindings]\n\"Super+Return\" = { spawn = \"foot\" }\n\"Super+3\" = { workspace = 2 }\n")
                .unwrap();
        assert_eq!(config.keybindings.0["Super+Return"], Action::Spawn("foot".into()));
        assert_eq!(config.keybindings.0["Super+3"], Action::Workspace(2));
    }
}
