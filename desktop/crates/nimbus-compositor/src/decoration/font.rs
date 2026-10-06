// SPDX-License-Identifier: MIT

//! Finding and loading the titlebar font.

use ab_glyph::FontVec;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Fonts tried when fontconfig has no answer, in the places distributions install them.
const FALLBACKS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
];

/// Loads the font fontconfig picks for `family`, or a common sans-serif font.
///
/// Returns `None` when there's no usable font; titlebars then show no title.
pub fn load(family: &str) -> Option<FontVec> {
    let matched = fc_match(family).and_then(|(path, index)| open(path, index));
    matched.or_else(|| FALLBACKS.iter().find_map(|path| open(PathBuf::from(path), 0))).or_else(
        || {
            tracing::warn!("no font for window titles; install fontconfig or DejaVu Sans");
            None
        },
    )
}

fn open(path: PathBuf, index: u32) -> Option<FontVec> {
    let data = std::fs::read(&path).ok()?;
    FontVec::try_from_vec_and_index(data, index)
        .map_err(|err| tracing::debug!("cannot load the font {}: {err}", path.display()))
        .ok()
}

/// Asks `fc-match` for the file and face index of the best match for `family`.
fn fc_match(family: &str) -> Option<(PathBuf, u32)> {
    let family = if family.is_empty() { "sans-serif" } else { family };
    let output = Command::new("fc-match")
        .arg("--format=%{file}\n%{index}")
        .arg(pattern(family))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| tracing::debug!("cannot run fc-match: {err}"))
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let (file, index) = text.split_once('\n').unwrap_or((text.as_str(), "0"));
    (!file.is_empty()).then(|| (PathBuf::from(file), index.trim().parse().unwrap_or(0)))
}

/// A fontconfig pattern naming `family`; `-`, `:`, `,`, and `\` have meanings there, so they're escaped.
fn pattern(family: &str) -> String {
    let mut pattern = String::with_capacity(family.len());
    for c in family.chars() {
        if matches!(c, '-' | ':' | ',' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_escape_fontconfig_syntax() {
        assert_eq!(pattern("Inter"), "Inter");
        assert_eq!(pattern("Noto Sans-12:bold"), "Noto Sans\\-12\\:bold");
    }

    #[test]
    fn a_font_loads_where_one_is_installed() {
        let installed = fc_match("sans-serif").is_some()
            || FALLBACKS.iter().any(|p| std::path::Path::new(p).exists());
        if !installed {
            eprintln!("skipped: neither fontconfig nor a fallback font is installed");
            return;
        }
        assert!(load("a family that doesn't exist").is_some());
    }
}
