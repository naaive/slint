// SPDX-License-Identifier: MIT

//! Decoration colors and sizes from `[appearance]`, matching the tokens of `nimbus-theme`.

use super::frame::Metrics;
use nimbus_config::{Appearance, ColorScheme};

/// A straight-alpha color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    const fn hex(rgb: u32, alpha: u8) -> Self {
        Self((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, alpha)
    }

    /// Relative luminance, from 0 for black to 1 for white.
    fn luminance(self) -> f32 {
        let channel = |c: u8| {
            let c = f32::from(c) / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }
}

const DEFAULT_ACCENT: Rgba = Rgba::hex(0x3584e4, 255);

/// The resolved appearance of decorations.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub accent: Rgba,
    pub font_family: String,
    /// In logical pixels.
    pub font_size: f32,
    /// The radius of a floating titlebar's top corners, in logical pixels.
    pub corner_radius: f32,
}

impl Theme {
    /// Resolves the user's choices like `nimbus_theme::ThemeSettings::from_config`.
    ///
    /// `ColorScheme::System` is dark: the portal answers it with no preference, which Nimbus shows as dark.
    pub fn from_config(appearance: &Appearance) -> Self {
        let sanitize = |value: f32, min: f32, max: f32, fallback: f32| {
            if value.is_finite() { value.clamp(min, max) } else { fallback }
        };
        Self {
            dark: appearance.color_scheme != ColorScheme::Light,
            accent: parse_hex_color(&appearance.accent).unwrap_or(DEFAULT_ACCENT),
            font_family: appearance.font_family.trim().to_owned(),
            font_size: sanitize(appearance.font_size, 6.0, 48.0, 11.0) * 96.0 / 72.0,
            // Titlebars are smaller than the shell's cards, so they round less.
            corner_radius: sanitize(appearance.corner_radius, 0.0, 32.0, 12.0).min(10.0),
        }
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::for_font_size(self.font_size)
    }

    pub fn palette(&self) -> Palette {
        let dark = self.dark;
        let pick =
            |dark_color: Rgba, light_color: Rgba| if dark { dark_color } else { light_color };
        let on_accent = if self.accent.luminance() > 0.4 {
            Rgba::hex(0x1c1c21, 255)
        } else {
            Rgba::hex(0xffffff, 255)
        };
        Palette {
            background: pick(Rgba::hex(0x2f2f34, 255), Rgba::hex(0xffffff, 255)),
            background_inactive: pick(Rgba::hex(0x252529, 255), Rgba::hex(0xf4f4f6, 255)),
            text: pick(Rgba::hex(0xf3f3f6, 255), Rgba::hex(0x1c1c21, 255)),
            text_inactive: pick(Rgba::hex(0xa9a9b4, 255), Rgba::hex(0x5c5c68, 255)),
            separator: pick(Rgba::hex(0x000000, 0x66), Rgba::hex(0x000000, 0x17)),
            control: pick(Rgba::hex(0xffffff, 0x17), Rgba::hex(0x000000, 0x0f)),
            control_hover: pick(Rgba::hex(0xffffff, 0x33), Rgba::hex(0x000000, 0x29)),
            close_hover: pick(Rgba::hex(0xd8363f, 255), Rgba::hex(0xd22f39, 255)),
            on_close_hover: Rgba::hex(0xffffff, 255),
            accent: self.accent,
            on_accent,
        }
    }
}

impl Default for Metrics {
    /// The metrics of the default appearance.
    fn default() -> Self {
        Theme::from_config(&Appearance::default()).metrics()
    }
}

/// The colors decorations use; the names follow `nimbus-theme`'s tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Rgba,
    pub background_inactive: Rgba,
    pub text: Rgba,
    pub text_inactive: Rgba,
    pub separator: Rgba,
    pub control: Rgba,
    pub control_hover: Rgba,
    pub close_hover: Rgba,
    pub on_close_hover: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
}

/// Parses `#rrggbb` or `#rgb`, with or without the `#`.
fn parse_hex_color(text: &str) -> Option<Rgba> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some(Rgba::hex(value, 255)),
        3 => {
            let expand = |shift: u32| ((value >> shift) & 0xf) * 0x11;
            Some(Rgba::hex(expand(8) << 16 | expand(4) << 8 | expand(0), 255))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_resolves_scheme_accent_and_font() {
        let appearance = Appearance {
            color_scheme: ColorScheme::Light,
            accent: "#e01b24".into(),
            font_size: 12.0,
            ..Appearance::default()
        };
        let theme = Theme::from_config(&appearance);
        assert!(!theme.dark);
        assert_eq!(theme.accent, Rgba(0xe0, 0x1b, 0x24, 255));
        assert!((theme.font_size - 16.0).abs() < 1e-4);
        assert_ne!(theme.palette(), Theme::from_config(&Appearance::default()).palette());

        let system = Theme::from_config(&Appearance::default());
        assert!(system.dark);
        assert_eq!(system.accent, DEFAULT_ACCENT);
    }

    #[test]
    fn bad_values_fall_back() {
        let appearance = Appearance {
            accent: "blue".into(),
            font_size: f32::NAN,
            corner_radius: 99.0,
            ..Appearance::default()
        };
        let theme = Theme::from_config(&appearance);
        assert_eq!(theme.accent, DEFAULT_ACCENT);
        assert!((theme.font_size - 11.0 * 4.0 / 3.0).abs() < 1e-4);
        assert_eq!(theme.corner_radius, 10.0);
    }

    #[test]
    fn hex_colors_parse() {
        assert_eq!(parse_hex_color("#fff"), Some(Rgba(255, 255, 255, 255)));
        assert_eq!(parse_hex_color("123456"), Some(Rgba(0x12, 0x34, 0x56, 255)));
        assert_eq!(parse_hex_color("+12345"), None);
        assert_eq!(parse_hex_color("#12345"), None);
    }

    #[test]
    fn text_on_the_accent_stays_readable() {
        let light_accent = Theme {
            accent: Rgba(0xf6, 0xd3, 0x2d, 255),
            ..Theme::from_config(&Appearance::default())
        };
        assert_eq!(light_accent.palette().on_accent, Rgba::hex(0x1c1c21, 255));
        let dark_accent = Theme::from_config(&Appearance::default());
        assert_eq!(dark_accent.palette().on_accent, Rgba::hex(0xffffff, 255));
    }
}
