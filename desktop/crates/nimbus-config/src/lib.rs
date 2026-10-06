// SPDX-License-Identifier: MIT

//! Nimbus user configuration, stored as TOML in `$XDG_CONFIG_HOME/nimbus/config.toml`.
//!
//! Every field has a default, so a partial or missing file is valid.
//! Unknown keys are ignored so that older builds can read newer files.
//! Saving over an existing file keeps its comments and unknown keys.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub mod chord;
pub mod geometry;
mod outputs;
mod update;
mod watch;
pub use outputs::{InvalidOutputMode, OutputConfig, OutputId, OutputMode, Transform, format_hz};
pub use update::{update, update_with};
pub use watch::{ConfigWatcher, watch};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Display settings, which the compositor saves whenever a display configuration is applied.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            panel: Panel::default(),
            workspaces: Workspaces::default(),
            input: Input::default(),
            keybindings: Keybindings::default(),
            power: Power::default(),
            autostart: Vec::new(),
            favorites: [
                "org.nimbus.Files",
                "org.nimbus.Terminal",
                "org.nimbus.Monitor",
                "org.nimbus.Settings",
            ]
            .map(String::from)
            .to_vec(),
            outputs: Vec::new(),
        }
    }
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
            icon_theme: "Adwaita".into(),
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

/// Maps key chords such as `Super+Shift+Q` to actions; see [`chord`] for their grammar.
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
    ///
    /// Reserved: the compositor doesn't blank outputs yet and ignores this value.
    pub blank_after_minutes: u32,
    /// Reserved: the compositor doesn't handle the lid switch yet and ignores this value.
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
            Ok(text) => Self::parse(path, &text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io { path: path.into(), source }),
        }
    }

    fn parse(path: &Path, text: &str) -> Result<Self, Error> {
        toml::from_str(text).map_err(|source| Error::Parse { path: path.into(), source })
    }

    pub fn load() -> Result<Self, Error> {
        Self::load_from(&default_path()?)
    }

    /// Loads `path`, or [`default_path`] when it's `None`, and returns the path with the configuration.
    ///
    /// Errors are logged and give the defaults.
    /// The path is `None` only when there's no default location.
    pub fn load_or_default(path: Option<PathBuf>) -> (Option<PathBuf>, Self) {
        match path.map_or_else(default_path, Ok) {
            Ok(path) => {
                let config = Self::load_from_or_default(&path);
                (Some(path), config)
            }
            Err(error) => {
                tracing::warn!("{error}; using the default configuration");
                (None, Self::default())
            }
        }
    }

    /// Loads `path`; an error is logged and gives the defaults.
    pub fn load_from_or_default(path: &Path) -> Self {
        Self::load_from(path).unwrap_or_else(|error| {
            tracing::warn!("{error}; using the default configuration");
            Self::default()
        })
    }

    /// Writes atomically through a temporary file next to `path`, synced to disk before it replaces `path`.
    ///
    /// When `path` already holds a valid configuration, only the values that differ are rewritten,
    /// so comments and keys this version doesn't know about are kept.
    /// To change a few values without losing another program's concurrent edits, use [`update`].
    pub fn save_to(&self, path: &Path) -> Result<(), Error> {
        let io = |source| Error::Io { path: path.into(), source };
        let dir = parent_dir(path);
        std::fs::create_dir_all(dir).map_err(io)?;
        let text = toml::to_string_pretty(self)?;
        let (text, permissions) = match std::fs::read_to_string(path) {
            Ok(existing) => {
                let permissions = std::fs::metadata(path).map_err(io)?.permissions();
                (merge_into(&existing, &text).unwrap_or(text), permissions)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (text, std::fs::Permissions::from_mode(0o666))
            }
            Err(source) => return Err(io(source)),
        };
        let mut prefix = OsString::from(".");
        prefix.push(path.file_name().unwrap_or_default());
        prefix.push(".");
        let mut tmp = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".tmp")
            .permissions(permissions)
            .tempfile_in(dir)
            .map_err(io)?;
        tmp.write_all(text.as_bytes()).map_err(io)?;
        tmp.as_file().sync_all().map_err(io)?;
        tmp.persist(path).map_err(|e| io(e.error))?;
        // Some file systems can't sync a directory; the rename has happened either way.
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    pub fn save(&self) -> Result<(), Error> {
        self.save_to(&default_path()?)
    }
}

/// The directory holding `path`, which is `.` for a bare file name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

fn merge_into(existing: &str, new: &str) -> Option<String> {
    let old_config: Config = toml::from_str(existing).ok()?;
    let mut doc: toml_edit::DocumentMut = existing.parse().ok()?;
    let old: toml_edit::DocumentMut = toml::to_string_pretty(&old_config).ok()?.parse().ok()?;
    let new: toml_edit::DocumentMut = new.parse().ok()?;
    merge_table(doc.as_table_mut(), Some(old.as_table()), new.as_table());
    Some(doc.to_string())
}

fn merge_table(
    target: &mut dyn toml_edit::TableLike,
    old: Option<&dyn toml_edit::TableLike>,
    new: &dyn toml_edit::TableLike,
) {
    if let Some(old) = old {
        let removed: Vec<String> = old
            .iter()
            .map(|(key, _)| key.to_string())
            .filter(|key| !new.contains_key(key))
            .collect();
        for key in removed {
            target.remove(&key);
        }
    }
    for (key, new_item) in new.iter() {
        let old_item = old.and_then(|old| old.get(key));
        match target.get_mut(key) {
            Some(item) => merge_item(item, old_item, new_item),
            None => {
                target.insert(key, new_item.clone());
            }
        }
    }
}

fn merge_item(target: &mut toml_edit::Item, old: Option<&toml_edit::Item>, new: &toml_edit::Item) {
    if let (Some(target), Some(new)) = (target.as_table_like_mut(), new.as_table_like()) {
        merge_table(target, old.and_then(toml_edit::Item::as_table_like), new);
        return;
    }
    match (target.as_value_mut(), new.as_value()) {
        (Some(target), Some(new)) => {
            if !same_value(target, new) {
                let decor = target.decor().clone();
                *target = new.clone();
                *target.decor_mut() = decor;
            }
        }
        _ => *target = new.clone(),
    }
}

fn same_value(a: &toml_edit::Value, b: &toml_edit::Value) -> bool {
    let parse =
        |value: &toml_edit::Value| toml::from_str::<toml::Table>(&format!("v = {value}")).ok();
    parse(a).is_some_and(|a| Some(a) == parse(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_other_defaults() {
        let config: Config =
            toml::from_str("[panel]\nheight = 40\n[keybindings]\n\"Super+X\" = \"lock\"\n")
                .unwrap();
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
        let config = Config { favorites: vec!["firefox".into()], ..Config::default() };
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

    #[test]
    fn save_keeps_comments_and_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "# My setup\nfuture-key = 1\n\n[panel]\n# taller panel\nheight = 40 # px\nfuture-panel-key = \"x\"\n\n[keybindings]\n\"Super+X\" = \"lock\"\n\"Super+Y\" = \"quit\"\n\"Super+Return\" = { spawn = \"foot\" }\n";
        std::fs::write(&path, original).unwrap();

        let mut config = Config::load_from(&path).unwrap();
        config.panel.height = 48;
        config.keybindings.0.remove("Super+Y");
        config.save_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# My setup",
            "future-key = 1",
            "# taller panel",
            "height = 48 # px",
            "future-panel-key = \"x\"",
            "\"Super+Return\" = { spawn = \"foot\" }",
        ] {
            assert!(text.contains(kept), "{kept:?} missing from:\n{text}");
        }
        assert!(!text.contains("Super+Y"), "removed keybinding still in:\n{text}");
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn save_removes_option_reset_to_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[appearance]\nwallpaper = \"/a.png\"\nunknown = true\n").unwrap();
        let mut config = Config::load_from(&path).unwrap();
        config.appearance.wallpaper = None;
        config.save_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("wallpaper"), "{text}");
        assert!(text.contains("unknown = true"), "{text}");
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }

    #[test]
    fn concurrent_saves_use_separate_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let threads: Vec<_> = (0..8u32)
            .map(|height| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let config = Config {
                        panel: Panel { height: 30 + height, ..Panel::default() },
                        ..Config::default()
                    };
                    for _ in 0..20 {
                        config.save_to(&path).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!((30..38).contains(&Config::load_from(&path).unwrap().panel.height));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "temporary files remain");
    }

    #[test]
    fn save_keeps_file_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Config::default().save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn save_replaces_invalid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[workspaces]\nlayout = \"tiled\"\n").unwrap();
        let config = Config::default();
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }
}
