// SPDX-License-Identifier: MIT

//! Searching settings from the sidebar, and the text matching shared with the pickers.

use crate::page::Page;

/// How well `text` matches `query`, higher is better; `None` when it doesn't match.
///
/// Every whitespace-separated query term must occur in `text`, ignoring case.
/// Terms at the start of `text` score highest, then terms at a word start, then anywhere.
pub fn score(query: &str, text: &str) -> Option<u32> {
    let text = text.to_lowercase();
    let mut total = 0;
    let mut any = false;
    for term in query.split_whitespace() {
        any = true;
        let term = term.to_lowercase();
        let best = text
            .match_indices(term.as_str())
            .map(|(at, _)| {
                if at == 0 {
                    3
                } else if text[..at].ends_with(|c: char| !c.is_alphanumeric()) {
                    2
                } else {
                    1
                }
            })
            .max()?;
        total += best;
    }
    any.then_some(total)
}

/// Indices of `items` matching `query`, best first, keeping the original order among equals.
///
/// Each item is matched through its `primary` text, and through `secondary` at a lower rank.
/// An empty query returns every index in order.
pub fn rank<'a>(query: &str, items: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<usize> {
    let query = query.trim();
    let mut hits: Vec<(u32, usize)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, (primary, secondary))| {
            if query.is_empty() {
                return Some((0, i));
            }
            let best = score(query, primary).map(|s| s * 2).max(score(query, secondary));
            best.map(|s| (s, i))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// One searchable setting: where it lives, its visible name, and extra words people may type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub page: Page,
    pub title: &'static str,
    pub keywords: &'static str,
}

const fn entry(page: Page, title: &'static str, keywords: &'static str) -> Entry {
    Entry { page, title, keywords }
}

pub const ENTRIES: &[Entry] = &[
    entry(Page::Appearance, "Style", "color scheme dark light mode theme night system"),
    entry(Page::Appearance, "Accent color", "colour highlight tint hex custom"),
    entry(Page::Appearance, "Background", "wallpaper picture image desktop background"),
    entry(Page::Appearance, "Font", "typeface text family size fonts"),
    entry(Page::Appearance, "Interface scale", "zoom hidpi dpi size scaling display"),
    entry(Page::Appearance, "Animations", "motion effects reduce transitions"),
    entry(Page::Appearance, "Corner radius", "rounded corners roundness"),
    entry(Page::Panel, "Panel position", "top bottom bar placement"),
    entry(Page::Panel, "Panel height", "bar size taskbar"),
    entry(Page::Panel, "Show dock", "dash launcher favorites taskbar"),
    entry(Page::Panel, "Hide dock automatically", "autohide intellihide dock"),
    entry(Page::Panel, "Clock format", "time date 24-hour 12-hour am pm"),
    entry(Page::Panel, "Battery percentage", "power charge level"),
    entry(Page::Workspaces, "Number of workspaces", "virtual desktops count"),
    entry(Page::Workspaces, "Default window layout", "tiling floating tile master stack"),
    entry(Page::Workspaces, "Window gaps", "spacing tiling margin"),
    entry(Page::Workspaces, "Focus follows mouse", "hover pointer sloppy focus"),
    entry(Page::Input, "Input sources", "keyboard layout language xkb"),
    entry(Page::Input, "Keyboard options", "caps lock compose key switch layout xkb options"),
    entry(Page::Input, "Key repeat", "delay rate typing"),
    entry(Page::Input, "Natural scrolling", "touchpad mouse wheel reverse direction"),
    entry(Page::Input, "Tap to click", "touchpad trackpad"),
    entry(Page::Input, "Pointer speed", "mouse touchpad acceleration sensitivity cursor"),
    entry(Page::Shortcuts, "Keyboard shortcuts", "keybindings hotkeys keys bindings chord"),
    entry(Page::Power, "Automatic screen lock", "lock screen idle timeout security"),
    entry(
        Page::Displays,
        "Displays",
        "monitors screens resolution refresh rate scale rotation arrangement outputs",
    ),
    entry(Page::Notifications, "Do not disturb", "notifications banners popups quiet"),
    entry(
        Page::About,
        "About this system",
        "system information version hostname os kernel cpu memory disk",
    ),
];

/// Settings matching `query`, best first; empty for a blank query.
pub fn search(query: &str) -> Vec<Entry> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    rank(query, ENTRIES.iter().map(|e| (e.title, e.keywords)))
        .into_iter()
        .filter_map(|i| ENTRIES.get(i).copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_prefers_starts() {
        assert_eq!(score("dark", "Dark style"), Some(3));
        assert_eq!(score("style", "Dark style"), Some(2));
        assert_eq!(score("ty", "Dark style"), Some(1));
        assert_eq!(score("dark mode", "dark style"), None);
        assert_eq!(score("  ", "anything"), None);
        assert_eq!(score("É", "écran"), Some(3));
    }

    #[test]
    fn ranking() {
        let items = [("English (US)", "us"), ("German", "de"), ("English (UK)", "gb")];
        assert_eq!(rank("", items), [0, 1, 2]);
        assert_eq!(rank("english", items), [0, 2]);
        assert_eq!(rank("de", items), [1]);
        assert_eq!(rank("uk", items), [2]);
    }

    #[test]
    fn settings_search() {
        assert!(search(" ").is_empty());
        assert_eq!(search("wallpaper")[0].title, "Background");
        assert_eq!(search("dark")[0].page, Page::Appearance);
        assert_eq!(search("tiling")[0].page, Page::Workspaces);
        assert_eq!(search("keybindings")[0].page, Page::Shortcuts);
        assert_eq!(search("kernel")[0].page, Page::About);
        assert!(search("zzzz").is_empty());
        assert!(search("suspend").is_empty(), "lid suspend isn't implemented");
        assert!(search("dpms").is_empty(), "screen blanking isn't implemented");
        for page in Page::ALL {
            assert!(ENTRIES.iter().any(|e| e.page == page), "{page} has no search entries");
        }
    }
}
