// SPDX-License-Identifier: MIT

//! Edits of single settings, as sent by the UI: a dotted key such as `panel.height` and a typed value.
//! Values are validated and clamped here, so the UI never writes an out-of-range configuration.

use std::path::PathBuf;
use std::str::FromStr;

use nimbus_config::{ColorScheme, Config, DefaultLayout, PanelPosition};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    ColorScheme,
    Accent,
    Wallpaper,
    FontFamily,
    FontSize,
    Scale,
    Animations,
    CornerRadius,
    PanelPosition,
    PanelHeight,
    ShowDock,
    DockAutohide,
    ClockFormat,
    BatteryPercentage,
    WorkspaceCount,
    WindowLayout,
    Gaps,
    FocusFollowsMouse,
    KeyboardOptions,
    RepeatDelay,
    RepeatRate,
    NaturalScroll,
    TapToClick,
    PointerSpeed,
    LockAfter,
    BlankAfter,
    SuspendOnLidClose,
}

impl Key {
    pub const ALL: [Key; 27] = [
        Key::ColorScheme,
        Key::Accent,
        Key::Wallpaper,
        Key::FontFamily,
        Key::FontSize,
        Key::Scale,
        Key::Animations,
        Key::CornerRadius,
        Key::PanelPosition,
        Key::PanelHeight,
        Key::ShowDock,
        Key::DockAutohide,
        Key::ClockFormat,
        Key::BatteryPercentage,
        Key::WorkspaceCount,
        Key::WindowLayout,
        Key::Gaps,
        Key::FocusFollowsMouse,
        Key::KeyboardOptions,
        Key::RepeatDelay,
        Key::RepeatRate,
        Key::NaturalScroll,
        Key::TapToClick,
        Key::PointerSpeed,
        Key::LockAfter,
        Key::BlankAfter,
        Key::SuspendOnLidClose,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Key::ColorScheme => "appearance.color-scheme",
            Key::Accent => "appearance.accent",
            Key::Wallpaper => "appearance.wallpaper",
            Key::FontFamily => "appearance.font-family",
            Key::FontSize => "appearance.font-size",
            Key::Scale => "appearance.scale",
            Key::Animations => "appearance.animations",
            Key::CornerRadius => "appearance.corner-radius",
            Key::PanelPosition => "panel.position",
            Key::PanelHeight => "panel.height",
            Key::ShowDock => "panel.show-dock",
            Key::DockAutohide => "panel.dock-autohide",
            Key::ClockFormat => "panel.clock-format",
            Key::BatteryPercentage => "panel.show-battery-percentage",
            Key::WorkspaceCount => "workspaces.count",
            Key::WindowLayout => "workspaces.layout",
            Key::Gaps => "workspaces.gaps",
            Key::FocusFollowsMouse => "workspaces.focus-follows-mouse",
            Key::KeyboardOptions => "input.keyboard-options",
            Key::RepeatDelay => "input.repeat-delay",
            Key::RepeatRate => "input.repeat-rate",
            Key::NaturalScroll => "input.natural-scroll",
            Key::TapToClick => "input.tap-to-click",
            Key::PointerSpeed => "input.pointer-speed",
            Key::LockAfter => "power.lock-after",
            Key::BlankAfter => "power.blank-after",
            Key::SuspendOnLidClose => "power.suspend-on-lid-close",
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    #[error("unknown setting '{0}'")]
    UnknownKey(String),
    #[error("'{key}' doesn't take that kind of value")]
    WrongType { key: &'static str },
    #[error("'{0}' isn't a color; use #rrggbb")]
    InvalidColor(String),
    #[error("'{0}' isn't a valid clock format")]
    InvalidClockFormat(String),
}

impl FromStr for Key {
    type Err = SettingError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Key::ALL
            .into_iter()
            .find(|k| k.name() == s)
            .ok_or_else(|| SettingError::UnknownKey(s.into()))
    }
}

pub const PANEL_HEIGHT: (u32, u32) = (24, 48);
pub const WORKSPACE_COUNT: (u32, u32) = (1, 10);
pub const GAPS: (u32, u32) = (0, 32);
pub const FONT_SIZE: (f64, f64) = (8.0, 16.0);
pub const SCALE: (f64, f64) = (1.0, 2.0);
pub const CORNER_RADIUS: (f64, f64) = (0.0, 24.0);
pub const REPEAT_DELAY: (u32, u32) = (150, 1000);
pub const REPEAT_RATE: (u32, u32) = (5, 80);
/// Presets for the lock and blank timeouts, in minutes; 0 means never.
pub const TIMEOUT_PRESETS: [u32; 8] = [0, 1, 2, 5, 10, 15, 30, 60];

fn clamp_u32(value: i64, (min, max): (u32, u32)) -> u32 {
    // The clamp keeps the value inside `u32`, so the cast is exact.
    value.clamp(i64::from(min), i64::from(max)) as u32
}

fn finite(value: f64, (min, max): (f64, f64), step: f64) -> Option<f64> {
    value.is_finite().then(|| ((value / step).round() * step).clamp(min, max))
}

/// Applies one edit to `config`.
///
/// A non-finite number leaves the setting unchanged.
pub fn apply(config: &mut Config, key: Key, value: Value) -> Result<(), SettingError> {
    let wrong = || SettingError::WrongType { key: key.name() };
    let appearance = &mut config.appearance;
    match (key, value) {
        (Key::ColorScheme, Value::Int(i)) => appearance.color_scheme = color_scheme_from_index(i),
        (Key::Accent, Value::Text(text)) => {
            let (r, g, b) =
                nimbus_theme::parse_hex_color(&text).ok_or(SettingError::InvalidColor(text))?;
            appearance.accent = format!("#{r:02x}{g:02x}{b:02x}");
        }
        (Key::Wallpaper, Value::Text(text)) => {
            appearance.wallpaper = (!text.trim().is_empty()).then(|| PathBuf::from(text));
        }
        (Key::FontFamily, Value::Text(text)) => {
            let family = text.trim();
            if !family.is_empty() {
                appearance.font_family = family.to_string();
            }
        }
        (Key::FontSize, Value::Float(f)) => {
            if let Some(size) = finite(f, FONT_SIZE, 0.5) {
                appearance.font_size = size as f32;
            }
        }
        (Key::Scale, Value::Float(f)) => {
            if let Some(scale) = finite(f, SCALE, 0.25) {
                appearance.scale = scale;
            }
        }
        (Key::Animations, Value::Bool(b)) => appearance.animations = b,
        (Key::CornerRadius, Value::Float(f)) => {
            if let Some(radius) = finite(f, CORNER_RADIUS, 1.0) {
                appearance.corner_radius = radius as f32;
            }
        }
        (Key::PanelPosition, Value::Int(i)) => {
            config.panel.position = if i == 1 { PanelPosition::Bottom } else { PanelPosition::Top };
        }
        (Key::PanelHeight, Value::Int(i)) => config.panel.height = clamp_u32(i, PANEL_HEIGHT),
        (Key::ShowDock, Value::Bool(b)) => config.panel.show_dock = b,
        (Key::DockAutohide, Value::Bool(b)) => config.panel.dock_autohide = b,
        (Key::ClockFormat, Value::Text(text)) => {
            if !crate::clock::is_valid_format(&text) {
                return Err(SettingError::InvalidClockFormat(text));
            }
            config.panel.clock_format = text;
        }
        (Key::BatteryPercentage, Value::Bool(b)) => config.panel.show_battery_percentage = b,
        (Key::WorkspaceCount, Value::Int(i)) => {
            config.workspaces.count = clamp_u32(i, WORKSPACE_COUNT)
        }
        (Key::WindowLayout, Value::Int(i)) => {
            config.workspaces.layout =
                if i == 1 { DefaultLayout::Tiling } else { DefaultLayout::Floating };
        }
        (Key::Gaps, Value::Int(i)) => config.workspaces.gaps = clamp_u32(i, GAPS),
        (Key::FocusFollowsMouse, Value::Bool(b)) => config.workspaces.focus_follows_mouse = b,
        (Key::KeyboardOptions, Value::Text(text)) => {
            config.input.keyboard_options = crate::xkb::normalize_options(&text);
        }
        (Key::RepeatDelay, Value::Int(i)) => {
            config.input.repeat_delay_ms = clamp_u32(i, REPEAT_DELAY)
        }
        (Key::RepeatRate, Value::Int(i)) => config.input.repeat_rate = clamp_u32(i, REPEAT_RATE),
        (Key::NaturalScroll, Value::Bool(b)) => config.input.natural_scroll = b,
        (Key::TapToClick, Value::Bool(b)) => config.input.tap_to_click = b,
        (Key::PointerSpeed, Value::Float(f)) => {
            if let Some(speed) = finite(f, (-1.0, 1.0), 0.05) {
                config.input.pointer_speed = speed;
            }
        }
        (Key::LockAfter, Value::Int(i)) => {
            config.power.lock_after_minutes = clamp_u32(i, (0, 24 * 60))
        }
        (Key::BlankAfter, Value::Int(i)) => {
            config.power.blank_after_minutes = clamp_u32(i, (0, 24 * 60))
        }
        (Key::SuspendOnLidClose, Value::Bool(b)) => config.power.suspend_on_lid_close = b,
        _ => return Err(wrong()),
    }
    Ok(())
}

/// The UI's order: light, dark, system.
pub fn color_scheme_index(scheme: ColorScheme) -> i32 {
    match scheme {
        ColorScheme::Light => 0,
        ColorScheme::Dark => 1,
        ColorScheme::System => 2,
    }
}

pub fn color_scheme_from_index(index: i64) -> ColorScheme {
    match index {
        0 => ColorScheme::Light,
        1 => ColorScheme::Dark,
        _ => ColorScheme::System,
    }
}

/// The index of `scale` among 100%, 125%, ..., 200%.
pub fn scale_index(scale: f64) -> i32 {
    if !scale.is_finite() {
        return 0;
    }
    // Clamped to 0..=4 first, so the cast is exact.
    ((scale.clamp(SCALE.0, SCALE.1) - SCALE.0) / 0.25).round() as i32
}

/// A labeled list of timeout choices that always contains `current`, in ascending order.
pub fn timeout_choices(current: u32) -> Vec<(u32, String)> {
    let mut values: Vec<u32> = TIMEOUT_PRESETS.to_vec();
    if !values.contains(&current) {
        values.push(current);
        values.sort_unstable();
    }
    values.into_iter().map(|v| (v, timeout_label(v))).collect()
}

pub fn timeout_label(minutes: u32) -> String {
    match minutes {
        0 => "Never".into(),
        1 => "1 minute".into(),
        60 => "1 hour".into(),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

/// The well-known GNOME accent palette, as `(name, #rrggbb)`.
pub const ACCENTS: [(&str, &str); 9] = [
    ("Blue", "#3584e4"),
    ("Teal", "#2190a4"),
    ("Green", "#3a944a"),
    ("Yellow", "#c88800"),
    ("Orange", "#ed5b00"),
    ("Red", "#e62d42"),
    ("Pink", "#d56199"),
    ("Purple", "#9141ac"),
    ("Slate", "#6f8396"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for key in Key::ALL {
            assert_eq!(key.name().parse::<Key>(), Ok(key));
        }
        assert_eq!(
            "panel.width".parse::<Key>(),
            Err(SettingError::UnknownKey("panel.width".into()))
        );
    }

    #[test]
    fn numbers_are_clamped_and_snapped() {
        let mut c = Config::default();
        apply(&mut c, Key::PanelHeight, Value::Int(500)).unwrap();
        assert_eq!(c.panel.height, 48);
        apply(&mut c, Key::WorkspaceCount, Value::Int(-3)).unwrap();
        assert_eq!(c.workspaces.count, 1);
        apply(&mut c, Key::FontSize, Value::Float(11.3)).unwrap();
        assert_eq!(c.appearance.font_size, 11.5);
        apply(&mut c, Key::Scale, Value::Float(1.4)).unwrap();
        assert_eq!(c.appearance.scale, 1.5);
        apply(&mut c, Key::Scale, Value::Float(f64::NAN)).unwrap();
        assert_eq!(c.appearance.scale, 1.5);
        apply(&mut c, Key::PointerSpeed, Value::Float(-7.0)).unwrap();
        assert_eq!(c.input.pointer_speed, -1.0);
        apply(&mut c, Key::RepeatDelay, Value::Int(i64::MAX)).unwrap();
        assert_eq!(c.input.repeat_delay_ms, 1000);
    }

    #[test]
    fn text_values_are_validated() {
        let mut c = Config::default();
        apply(&mut c, Key::Accent, Value::Text(" #E62D42 ".into())).unwrap();
        assert_eq!(c.appearance.accent, "#e62d42");
        apply(&mut c, Key::Accent, Value::Text("#f80".into())).unwrap();
        assert_eq!(c.appearance.accent, "#ff8800");
        assert!(matches!(
            apply(&mut c, Key::Accent, Value::Text("blue".into())),
            Err(SettingError::InvalidColor(_))
        ));
        apply(&mut c, Key::Wallpaper, Value::Text("/a/b.jpg".into())).unwrap();
        assert_eq!(c.appearance.wallpaper, Some(PathBuf::from("/a/b.jpg")));
        apply(&mut c, Key::Wallpaper, Value::Text(String::new())).unwrap();
        assert_eq!(c.appearance.wallpaper, None);
        apply(&mut c, Key::FontFamily, Value::Text("  ".into())).unwrap();
        assert_eq!(c.appearance.font_family, "Inter");
        assert!(apply(&mut c, Key::ClockFormat, Value::Text("%H:%Q".into())).is_err());
        apply(&mut c, Key::ClockFormat, Value::Text("%H:%M".into())).unwrap();
        assert_eq!(c.panel.clock_format, "%H:%M");
    }

    #[test]
    fn enums_and_type_errors() {
        let mut c = Config::default();
        apply(&mut c, Key::ColorScheme, Value::Int(1)).unwrap();
        assert_eq!(c.appearance.color_scheme, ColorScheme::Dark);
        assert_eq!(color_scheme_index(c.appearance.color_scheme), 1);
        apply(&mut c, Key::PanelPosition, Value::Int(1)).unwrap();
        assert_eq!(c.panel.position, PanelPosition::Bottom);
        apply(&mut c, Key::WindowLayout, Value::Int(1)).unwrap();
        assert_eq!(c.workspaces.layout, DefaultLayout::Tiling);
        assert_eq!(
            apply(&mut c, Key::ShowDock, Value::Int(1)),
            Err(SettingError::WrongType { key: "panel.show-dock" })
        );
    }

    #[test]
    fn presets() {
        assert_eq!(scale_index(1.0), 0);
        assert_eq!(scale_index(1.75), 3);
        assert_eq!(scale_index(9.0), 4);
        assert_eq!(scale_index(f64::INFINITY), 0);
        let choices = timeout_choices(7);
        assert_eq!(
            choices.iter().map(|c| c.0).collect::<Vec<_>>(),
            [0, 1, 2, 5, 7, 10, 15, 30, 60]
        );
        assert_eq!(choices[4].1, "7 minutes");
        assert_eq!(timeout_choices(10).len(), TIMEOUT_PRESETS.len());
        assert_eq!(timeout_label(0), "Never");
        assert_eq!(timeout_label(120), "2 hours");
        for (_, hex) in ACCENTS {
            assert!(nimbus_theme::parse_hex_color(hex).is_some());
        }
    }
}
