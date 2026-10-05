// SPDX-License-Identifier: MIT

//! Installed font families, from fontconfig.

use std::process::{Command, Stdio};

/// Families offered when `fc-list` isn't available.
const FALLBACK: [&str; 6] =
    ["Cantarell", "DejaVu Sans", "Inter", "Liberation Sans", "Noto Sans", "Ubuntu"];

/// Parses `fc-list : family` output into sorted, unique family names.
///
/// Each line lists a family's localized names separated by commas, and fontconfig escapes `-` as `\-`.
pub fn parse_fc_list(output: &str) -> Vec<String> {
    let mut families: Vec<String> = output
        .lines()
        .filter_map(|line| line.split(',').next())
        .map(|name| name.replace("\\-", "-").replace("\\\\", "\\").trim().to_string())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .collect();
    families.sort_by_cached_key(|name| name.to_lowercase());
    families.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    families
}

/// Lists installed font families. Blocks while `fc-list` runs.
pub fn installed_families() -> Vec<String> {
    let output = Command::new("fc-list")
        .args([":", "family"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let families = parse_fc_list(&String::from_utf8_lossy(&output.stdout));
            if !families.is_empty() {
                return families;
            }
        }
        Ok(output) => tracing::warn!("fc-list failed with {}", output.status),
        Err(error) => tracing::warn!("cannot run fc-list: {error}"),
    }
    FALLBACK.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_deduplicates() {
        let output = "DejaVu Sans\nNoto Sans CJK JP,Noto Sans CJK JP Regular\nUnifont\\-JP\ndejavu sans\n\n.LastResort\nCantarell\n";
        assert_eq!(
            parse_fc_list(output),
            ["Cantarell", "DejaVu Sans", "Noto Sans CJK JP", "Unifont-JP"]
        );
    }

    #[test]
    fn listing_never_comes_back_empty() {
        assert!(!installed_families().is_empty());
    }
}
