// SPDX-License-Identifier: MIT

//! The Nimbus design system.
//!
//! The Slint sources in `ui/` are a library imported as `@nimbus`, for example
//! `import { Theme, Button } from "@nimbus/theme.slint";`.
//! Consumers call [`library_paths`] from their `build.rs` and [`apply_theme!`] at runtime.
//! See `README.md` for the tokens and components.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nimbus_config::{Appearance, ColorScheme};

#[cfg(feature = "headless")]
pub mod headless;

/// Library paths for `slint_build::CompilerConfiguration::with_library_paths`.
pub fn library_paths() -> HashMap<String, PathBuf> {
    HashMap::from([("nimbus".to_string(), PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"))])
}

/// The accent used when the configured one doesn't parse, as `(red, green, blue)`.
pub const DEFAULT_ACCENT: (u8, u8, u8) = (0x35, 0x84, 0xe4);

/// The font family used when the configured one is empty.
pub const DEFAULT_FONT_FAMILY: &str = "Inter";

/// Runtime theme values resolved from the user's configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeSettings {
    pub dark: bool,
    /// Accent as `(red, green, blue)`.
    pub accent: (u8, u8, u8),
    /// Base corner radius in logical pixels.
    pub corner_radius: f32,
    pub font_family: String,
    /// Base font size in logical pixels.
    pub font_size: f32,
    pub animations: bool,
}

impl Default for ThemeSettings {
    /// Dark scheme, the default accent, and the defaults of [`Appearance`].
    fn default() -> Self {
        Self::from_config_with_system(&Appearance::default(), || true)
    }
}

impl ThemeSettings {
    /// Resolves `ColorScheme::System` through the XDG desktop portal's `color-scheme` setting, defaulting to dark.
    ///
    /// The configured font size is in points and becomes logical pixels at 96 DPI.
    /// Out-of-range sizes and radii are clamped, and an accent that isn't `#rrggbb` or `#rgb` becomes [`DEFAULT_ACCENT`].
    /// The portal query takes at most about a second, and never fails: without a portal the result is dark.
    pub fn from_config(appearance: &Appearance) -> Self {
        Self::from_config_with_system(appearance, || system_prefers_dark().unwrap_or(true))
    }

    /// Like [`ThemeSettings::from_config`], with `system_dark` answering for `ColorScheme::System`.
    ///
    /// `system_dark` runs only when the scheme is `System`.
    pub fn from_config_with_system(
        appearance: &Appearance,
        system_dark: impl FnOnce() -> bool,
    ) -> Self {
        let dark = match appearance.color_scheme {
            ColorScheme::Dark => true,
            ColorScheme::Light => false,
            ColorScheme::System => system_dark(),
        };
        let font_family = appearance.font_family.trim();
        Self {
            dark,
            accent: parse_hex_color(&appearance.accent).unwrap_or(DEFAULT_ACCENT),
            corner_radius: sanitize(appearance.corner_radius, 0.0, 32.0, 12.0),
            font_family: if font_family.is_empty() { DEFAULT_FONT_FAMILY } else { font_family }
                .to_string(),
            font_size: sanitize(appearance.font_size, 6.0, 48.0, 11.0) * 96.0 / 72.0,
            animations: appearance.animations,
        }
    }
}

fn sanitize(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() { value.clamp(min, max) } else { fallback }
}

/// Parses `#rrggbb` or `#rgb`, with or without the `#`, into `(red, green, blue)`.
pub fn parse_hex_color(text: &str) -> Option<(u8, u8, u8)> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    // `from_str_radix` alone would accept a sign, such as `+1`.
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(hex.get(range)?, 16).ok();
    match hex.len() {
        6 => Some((channel(0..2)?, channel(2..4)?, channel(4..6)?)),
        3 => {
            let expand = |i: usize| channel(i..i + 1).map(|v| v * 0x11);
            Some((expand(0)?, expand(1)?, expand(2)?))
        }
        _ => None,
    }
}

/// Asks the XDG desktop portal whether the user prefers a dark scheme.
///
/// Returns `None` when there's no portal, no D-Bus tool, or the user has no preference.
pub fn system_prefers_dark() -> Option<bool> {
    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const NAMESPACE: &str = "org.freedesktop.appearance";
    const KEY: &str = "color-scheme";
    let busctl = |method: &str| {
        let mut command = Command::new("busctl");
        command.args(["--user", "--timeout=500ms", "call", PORTAL, PATH]);
        command.args(["org.freedesktop.portal.Settings", method, "ss", NAMESPACE, KEY]);
        command
    };
    let gdbus = |method: &str| {
        let mut command = Command::new("gdbus");
        command.args([
            "call",
            "--session",
            "--timeout",
            "1",
            "--dest",
            PORTAL,
            "--object-path",
            PATH,
        ]);
        command.args([
            "--method",
            &format!("org.freedesktop.portal.Settings.{method}"),
            NAMESPACE,
            KEY,
        ]);
        command
    };
    // `ReadOne` needs portal version 2; `Read` wraps the value in an extra variant but parses the same.
    let deadline = Instant::now() + Duration::from_millis(1200);
    [busctl("ReadOne"), busctl("Read"), gdbus("ReadOne"), gdbus("Read")]
        .into_iter()
        .take_while(|_| Instant::now() < deadline)
        .find_map(|command| run_with_deadline(command, deadline))
        .and_then(|output| parse_portal_color_scheme(&output))
        .and_then(|scheme| match scheme {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        })
}

fn run_with_deadline(mut command: Command, deadline: Instant) -> Option<String> {
    let mut child =
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    Some(output)
}

/// Extracts the `color-scheme` value from `busctl` output such as `v u 1`,
/// or `gdbus` output such as `(<uint32 1>,)`.
pub fn parse_portal_color_scheme(output: &str) -> Option<u32> {
    output.split_whitespace().last()?.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok()
}

/// Sets the `Theme` global of a generated component from a [`ThemeSettings`].
///
/// Each crate compiles its own copy of the `Theme` global, so this is a macro rather than a function.
/// `apply_theme!(component, settings)` uses the `Theme` type in scope at the call site,
/// so import the generated global first, for example `use crate::Theme;`.
/// `apply_theme!(component, settings, path::to::Theme)` names it explicitly.
/// The caller also needs `slint` as a dependency.
///
/// ```ignore
/// slint::include_modules!();
/// let ui = MainWindow::new()?;
/// let settings = nimbus_theme::ThemeSettings::from_config(&config.appearance);
/// nimbus_theme::apply_theme!(ui, settings);
/// ```
#[macro_export]
macro_rules! apply_theme {
    ($component:expr, $settings:expr) => {
        $crate::apply_theme!($component, $settings, Theme)
    };
    ($component:expr, $settings:expr, $theme:ty) => {{
        use slint::ComponentHandle as _;
        let settings: &$crate::ThemeSettings = &$settings;
        let theme = (&$component).global::<$theme>();
        let (red, green, blue) = settings.accent;
        theme.set_dark(settings.dark);
        theme.set_accent(slint::Color::from_rgb_u8(red, green, blue));
        theme.set_corner_radius(settings.corner_radius);
        theme.set_font_family(settings.font_family.as_str().into());
        theme.set_font_size(settings.font_size);
        theme.set_animations(settings.animations);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appearance(scheme: ColorScheme) -> Appearance {
        Appearance { color_scheme: scheme, ..Appearance::default() }
    }

    #[test]
    fn explicit_schemes_skip_the_system_query() {
        let light = ThemeSettings::from_config_with_system(&appearance(ColorScheme::Light), || {
            panic!("queried the system")
        });
        assert!(!light.dark);
        let dark = ThemeSettings::from_config_with_system(&appearance(ColorScheme::Dark), || {
            panic!("queried the system")
        });
        assert!(dark.dark);
        assert!(!ThemeSettings::from_config(&appearance(ColorScheme::Light)).dark);
    }

    #[test]
    fn system_scheme_uses_the_answer() {
        let a = appearance(ColorScheme::System);
        assert!(!ThemeSettings::from_config_with_system(&a, || false).dark);
        assert!(ThemeSettings::from_config_with_system(&a, || true).dark);
    }

    #[test]
    fn default_settings() {
        let settings = ThemeSettings::default();
        assert!(settings.dark);
        assert_eq!(settings.accent, DEFAULT_ACCENT);
        assert_eq!(settings.corner_radius, 12.0);
        assert_eq!(settings.font_family, "Inter");
        assert!((settings.font_size - 11.0 * 4.0 / 3.0).abs() < 1e-4);
        assert!(settings.animations);
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#3584e4"), Some((0x35, 0x84, 0xe4)));
        assert_eq!(parse_hex_color(" #FF00aA "), Some((0xff, 0x00, 0xaa)));
        assert_eq!(parse_hex_color("e01b24"), Some((0xe0, 0x1b, 0x24)));
        assert_eq!(parse_hex_color("#f80"), Some((0xff, 0x88, 0x00)));
        for bad in ["", "#", "#12345", "#1234567", "#gg0000", "#+12345", "#ü1234", "red"] {
            assert_eq!(parse_hex_color(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bad_values_fall_back() {
        let a = Appearance {
            color_scheme: ColorScheme::Dark,
            accent: "not a color".into(),
            font_family: "  ".into(),
            font_size: f32::NAN,
            corner_radius: 400.0,
            animations: false,
            ..Appearance::default()
        };
        let settings = ThemeSettings::from_config(&a);
        assert_eq!(settings.accent, DEFAULT_ACCENT);
        assert_eq!(settings.font_family, DEFAULT_FONT_FAMILY);
        assert!((settings.font_size - 11.0 * 4.0 / 3.0).abs() < 1e-4);
        assert_eq!(settings.corner_radius, 32.0);
        assert!(!settings.animations);

        let tiny = Appearance { corner_radius: -3.0, font_size: 1.0, ..a };
        let settings = ThemeSettings::from_config(&tiny);
        assert_eq!(settings.corner_radius, 0.0);
        assert_eq!(settings.font_size, 8.0);
    }

    #[test]
    fn portal_output() {
        assert_eq!(parse_portal_color_scheme("v u 1\n"), Some(1));
        assert_eq!(parse_portal_color_scheme("v v u 2"), Some(2));
        assert_eq!(parse_portal_color_scheme("(<uint32 1>,)\n"), Some(1));
        assert_eq!(parse_portal_color_scheme("(<<uint32 0>>,)"), Some(0));
        assert_eq!(parse_portal_color_scheme(""), None);
        assert_eq!(parse_portal_color_scheme("Call failed: no such method"), None);
    }

    #[test]
    fn portal_query_is_bounded() {
        let start = Instant::now();
        let _ = system_prefers_dark();
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn missing_tools_time_out_gracefully() {
        let deadline = Instant::now() + Duration::from_millis(200);
        assert_eq!(run_with_deadline(Command::new("/nonexistent/nimbus-tool"), deadline), None);
        let mut sleeper = Command::new("sleep");
        sleeper.arg("5");
        let start = Instant::now();
        assert_eq!(run_with_deadline(sleeper, deadline), None);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
