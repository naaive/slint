// SPDX-License-Identifier: MIT

//! Color schemes, and the resolution of cell colors against a scheme and the terminal's overrides.

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// The id of the scheme that follows the desktop's light or dark preference.
pub const SYSTEM_SCHEME: &str = "nimbus";

/// The factor applied to colors with the dim attribute.
const DIM_FACTOR: f32 = 0.66;

/// A terminal color scheme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scheme {
    /// A stable identifier, stored in the preferences.
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Blended over the cell background with `selection_alpha`.
    pub selection: Rgb,
    pub selection_alpha: f32,
    /// The 16 ANSI colors: black, red, green, yellow, blue, magenta, cyan, white, then their bright variants.
    pub ansi: [Rgb; 16],
}

const fn rgb(hex: u32) -> Rgb {
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

const fn ansi(hex: [u32; 16]) -> [Rgb; 16] {
    let mut out = [Rgb { r: 0, g: 0, b: 0 }; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = rgb(hex[i]);
        i += 1;
    }
    out
}

const SOLARIZED: [u32; 16] = [
    0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x586e75,
    0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
];

/// The built-in schemes, excluding the [`SYSTEM_SCHEME`] alias.
pub const SCHEMES: &[Scheme] = &[
    Scheme {
        id: "nimbus-dark",
        name: "Nimbus Dark",
        dark: true,
        foreground: rgb(0xe6e6eb),
        background: rgb(0x1c1c20),
        cursor: rgb(0xe6e6eb),
        selection: rgb(0x3584e4),
        selection_alpha: 0.45,
        ansi: ansi([
            0x2e2e35, 0xf0605a, 0x5cc98a, 0xe8bd52, 0x5b9cf2, 0xb880e8, 0x45c4d2, 0xc4c4cc,
            0x66666f, 0xff8580, 0x80e3a8, 0xf6d47a, 0x86b8ff, 0xd2a3ff, 0x72e1ec, 0xf6f6f9,
        ]),
    },
    Scheme {
        id: "nimbus-light",
        name: "Nimbus Light",
        dark: false,
        foreground: rgb(0x1c1c21),
        background: rgb(0xfcfcfd),
        cursor: rgb(0x1c1c21),
        selection: rgb(0x3584e4),
        selection_alpha: 0.28,
        ansi: ansi([
            0x1c1c21, 0xc4262e, 0x1d8a4e, 0x9a6700, 0x1c6bd0, 0x8b3fb6, 0x0e8494, 0xa8a8b2,
            0x5c5c68, 0xe0353d, 0x26a269, 0xb87d00, 0x3584e4, 0xa54bc0, 0x16a0b2, 0xd8d8de,
        ]),
    },
    Scheme {
        id: "solarized-dark",
        name: "Solarized Dark",
        dark: true,
        foreground: rgb(0x839496),
        background: rgb(0x002b36),
        cursor: rgb(0x93a1a1),
        selection: rgb(0x586e75),
        selection_alpha: 0.6,
        ansi: ansi(SOLARIZED),
    },
    Scheme {
        id: "solarized-light",
        name: "Solarized Light",
        dark: false,
        foreground: rgb(0x657b83),
        background: rgb(0xfdf6e3),
        cursor: rgb(0x586e75),
        selection: rgb(0x93a1a1),
        selection_alpha: 0.45,
        ansi: ansi(SOLARIZED),
    },
    Scheme {
        id: "dracula",
        name: "Dracula",
        dark: true,
        foreground: rgb(0xf8f8f2),
        background: rgb(0x282a36),
        cursor: rgb(0xf8f8f2),
        selection: rgb(0x44475a),
        selection_alpha: 1.0,
        ansi: ansi([
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ]),
    },
    Scheme {
        id: "gruvbox-dark",
        name: "Gruvbox Dark",
        dark: true,
        foreground: rgb(0xebdbb2),
        background: rgb(0x282828),
        cursor: rgb(0xebdbb2),
        selection: rgb(0x665c54),
        selection_alpha: 0.8,
        ansi: ansi([
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ]),
    },
    Scheme {
        id: "nord",
        name: "Nord",
        dark: true,
        foreground: rgb(0xd8dee9),
        background: rgb(0x2e3440),
        cursor: rgb(0xd8dee9),
        selection: rgb(0x4c566a),
        selection_alpha: 0.9,
        ansi: ansi([
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ]),
    },
];

/// The ids offered in the preferences, starting with [`SYSTEM_SCHEME`].
pub fn scheme_ids() -> impl Iterator<Item = &'static str> {
    std::iter::once(SYSTEM_SCHEME).chain(SCHEMES.iter().map(|s| s.id))
}

/// The display name for a scheme id, or `None` for unknown ids.
pub fn scheme_name(id: &str) -> Option<&'static str> {
    if id == SYSTEM_SCHEME {
        return Some("Nimbus");
    }
    SCHEMES.iter().find(|s| s.id == id).map(|s| s.name)
}

/// Looks up `id`, resolving [`SYSTEM_SCHEME`] and unknown ids to Nimbus Dark or Light by `desktop_dark`.
pub fn resolve_scheme(id: &str, desktop_dark: bool) -> &'static Scheme {
    SCHEMES.iter().find(|s| s.id == id).unwrap_or_else(|| {
        let fallback = if desktop_dark { "nimbus-dark" } else { "nimbus-light" };
        SCHEMES.iter().find(|s| s.id == fallback).unwrap_or(&SCHEMES[0])
    })
}

/// Mixes `top` over `bottom` with opacity `alpha` in `0..=1`.
pub fn blend(bottom: Rgb, top: Rgb, alpha: f32) -> Rgb {
    let alpha = alpha.clamp(0.0, 1.0);
    let mix = |b: u8, t: u8| (f32::from(b) + (f32::from(t) - f32::from(b)) * alpha).round() as u8;
    Rgb { r: mix(bottom.r, top.r), g: mix(bottom.g, top.g), b: mix(bottom.b, top.b) }
}

fn scale(color: Rgb, factor: f32) -> Rgb {
    let s = |c: u8| (f32::from(c) * factor).round().clamp(0.0, 255.0) as u8;
    Rgb { r: s(color.r), g: s(color.g), b: s(color.b) }
}

/// The full indexed color table of a terminal: a scheme with the escape-sequence overrides applied.
#[derive(Clone, Debug)]
pub struct ColorTable {
    colors: [Rgb; COUNT],
    /// Brightens bold text in the first eight colors, as most terminals do.
    pub bold_is_bright: bool,
}

impl ColorTable {
    pub fn new(scheme: &Scheme, overrides: &Colors) -> Self {
        let mut colors = [Rgb::default(); COUNT];
        colors[..16].copy_from_slice(&scheme.ansi);
        let levels = [0u8, 95, 135, 175, 215, 255];
        for (i, color) in colors[16..232].iter_mut().enumerate() {
            *color = Rgb { r: levels[i / 36], g: levels[(i / 6) % 6], b: levels[i % 6] };
        }
        for (i, color) in colors[232..256].iter_mut().enumerate() {
            let level = 8 + 10 * i as u8;
            *color = Rgb { r: level, g: level, b: level };
        }
        colors[NamedColor::Foreground as usize] = scheme.foreground;
        colors[NamedColor::Background as usize] = scheme.background;
        colors[NamedColor::Cursor as usize] = scheme.cursor;
        colors[NamedColor::BrightForeground as usize] = scheme.foreground;
        colors[NamedColor::DimForeground as usize] = scale(scheme.foreground, DIM_FACTOR);
        for (index, slot) in colors.iter_mut().enumerate() {
            if let Some(color) = overrides[index] {
                *slot = color;
            }
        }
        // Dim variants follow overridden base colors unless they're overridden themselves.
        for i in 0..8 {
            let dim = NamedColor::DimBlack as usize + i;
            if overrides[dim].is_none() {
                colors[dim] = scale(colors[i], DIM_FACTOR);
            }
        }
        if overrides[NamedColor::DimForeground as usize].is_none() {
            colors[NamedColor::DimForeground as usize] =
                scale(colors[NamedColor::Foreground as usize], DIM_FACTOR);
        }
        Self { colors, bold_is_bright: true }
    }

    /// The color at an index of the table, as used by OSC 4 and its queries.
    pub fn get(&self, index: usize) -> Rgb {
        self.colors.get(index).copied().unwrap_or_default()
    }

    pub fn foreground(&self) -> Rgb {
        self.colors[NamedColor::Foreground as usize]
    }

    pub fn background(&self) -> Rgb {
        self.colors[NamedColor::Background as usize]
    }

    pub fn cursor(&self) -> Rgb {
        self.colors[NamedColor::Cursor as usize]
    }

    fn resolve_fg(&self, color: Color, flags: Flags) -> Rgb {
        let bold = flags.contains(Flags::BOLD) && self.bold_is_bright;
        let dim = flags.contains(Flags::DIM);
        match color {
            Color::Spec(rgb) if dim => scale(rgb, DIM_FACTOR),
            Color::Spec(rgb) => rgb,
            Color::Named(name) => {
                let name = match (bold, dim) {
                    (_, true) => name.to_dim(),
                    (true, false) if (name as usize) < 8 || name == NamedColor::Foreground => {
                        name.to_bright()
                    }
                    _ => name,
                };
                self.get(name as usize)
            }
            Color::Indexed(index) => {
                let index = usize::from(index);
                let index = if bold && !dim && index < 8 { index + 8 } else { index };
                let color = self.get(index);
                if dim { scale(color, DIM_FACTOR) } else { color }
            }
        }
    }

    fn resolve_bg(&self, color: Color) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Named(name) => self.get(name as usize),
            Color::Indexed(index) => self.get(usize::from(index)),
        }
    }

    /// The `(foreground, background)` of a cell, after bold, dim, inverse, and hidden.
    pub fn cell_colors(&self, fg: Color, bg: Color, flags: Flags) -> (Rgb, Rgb) {
        let mut fg = self.resolve_fg(fg, flags);
        let mut bg = self.resolve_bg(bg);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        (fg, bg)
    }

    /// The color of an underline that may have its own color set by SGR 58.
    pub fn underline_color(&self, color: Option<Color>, fg: Rgb, flags: Flags) -> Rgb {
        color.map_or(fg, |color| self.resolve_fg(color, flags & !Flags::BOLD))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(id: &str) -> ColorTable {
        ColorTable::new(resolve_scheme(id, true), &Colors::default())
    }

    #[test]
    fn scheme_lookup() {
        assert_eq!(resolve_scheme("dracula", true).name, "Dracula");
        assert_eq!(resolve_scheme(SYSTEM_SCHEME, true).id, "nimbus-dark");
        assert_eq!(resolve_scheme(SYSTEM_SCHEME, false).id, "nimbus-light");
        assert_eq!(resolve_scheme("no-such-scheme", false).id, "nimbus-light");
        assert_eq!(scheme_ids().next(), Some(SYSTEM_SCHEME));
        assert_eq!(scheme_ids().count(), SCHEMES.len() + 1);
        assert_eq!(scheme_name("nord"), Some("Nord"));
        assert_eq!(scheme_name("bogus"), None);
        let mut ids: Vec<_> = scheme_ids().collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SCHEMES.len() + 1, "scheme ids are unique");
    }

    #[test]
    fn indexed_palette() {
        let t = table("nimbus-dark");
        assert_eq!(t.get(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(t.get(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(t.get(21), Rgb { r: 0, g: 0, b: 255 });
        assert_eq!(t.get(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(t.get(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(t.get(255), Rgb { r: 238, g: 238, b: 238 });
        assert_eq!(t.get(9999), Rgb::default());
    }

    #[test]
    fn named_bold_dim_and_truecolor() {
        let t = table("dracula");
        let flags = Flags::empty();
        let red = Color::Named(NamedColor::Red);
        let bg = Color::Named(NamedColor::Background);
        assert_eq!(t.cell_colors(red, bg, flags).0, rgb(0xff5555));
        assert_eq!(t.cell_colors(red, bg, Flags::BOLD).0, rgb(0xff6e6e));
        assert_eq!(t.cell_colors(Color::Indexed(1), bg, Flags::BOLD).0, rgb(0xff6e6e));
        assert_eq!(t.cell_colors(Color::Indexed(100), bg, Flags::BOLD).0, t.get(100));
        let dim = t.cell_colors(red, bg, Flags::DIM).0;
        assert_eq!(dim, scale(rgb(0xff5555), DIM_FACTOR));
        let spec = Color::Spec(Rgb { r: 10, g: 20, b: 30 });
        assert_eq!(t.cell_colors(spec, bg, flags).0, Rgb { r: 10, g: 20, b: 30 });
        assert_eq!(t.cell_colors(red, bg, flags).1, rgb(0x282a36));
    }

    #[test]
    fn inverse_and_hidden() {
        let t = table("nimbus-dark");
        let fg = Color::Named(NamedColor::Foreground);
        let bg = Color::Named(NamedColor::Background);
        let (f, b) = t.cell_colors(fg, bg, Flags::INVERSE);
        assert_eq!((f, b), (t.background(), t.foreground()));
        let (f, b) = t.cell_colors(fg, bg, Flags::HIDDEN);
        assert_eq!(f, b);
    }

    #[test]
    fn overrides_apply_and_dim_follows() {
        let mut overrides = Colors::default();
        overrides[1] = Some(Rgb { r: 200, g: 0, b: 0 });
        overrides[NamedColor::Background as usize] = Some(Rgb { r: 1, g: 2, b: 3 });
        let t = ColorTable::new(resolve_scheme("nord", true), &overrides);
        assert_eq!(t.get(1), Rgb { r: 200, g: 0, b: 0 });
        assert_eq!(t.background(), Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(t.get(NamedColor::DimRed as usize), Rgb { r: 132, g: 0, b: 0 });
    }

    #[test]
    fn blending() {
        let black = Rgb { r: 0, g: 0, b: 0 };
        let white = Rgb { r: 255, g: 255, b: 255 };
        assert_eq!(blend(black, white, 0.0), black);
        assert_eq!(blend(black, white, 1.0), white);
        assert_eq!(blend(black, white, 0.5), Rgb { r: 128, g: 128, b: 128 });
        assert_eq!(blend(black, white, 7.0), white);
    }
}
