// SPDX-License-Identifier: MIT

use std::cmp::Ordering;

use crate::entry::DesktopEntry;
use crate::index::AppIndex;

const EXACT: u32 = 1000;
const PREFIX: u32 = 800;
const WORD_PREFIX: u32 = 600;
const SUBSTRING: u32 = 400;
const FUZZY_BASE: u32 = 100;
const FUZZY_MAX: u32 = SUBSTRING - FUZZY_BASE - 1;

/// Field weights in percent.
const NAME: u32 = 100;
const GENERIC_NAME: u32 = 70;
const KEYWORD: u32 = 60;
const ID: u32 = 50;
const CATEGORY: u32 = 40;

/// Orders entries by name ignoring case, then by id.
pub(crate) fn compare_names(a: &DesktopEntry, b: &DesktopEntry) -> Ordering {
    a.name
        .to_lowercase()
        .cmp(&b.name.to_lowercase())
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.id.cmp(&b.id))
}

fn is_boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|c| !c.is_alphanumeric())
}

/// Scores `needle` as a subsequence of `haystack` (both lowercase), favoring word starts and consecutive runs.
fn fuzzy(haystack: &[char], needle: &[char]) -> Option<u32> {
    const MATCH: i64 = 16;
    const BOUNDARY: i64 = 24;
    const CONSECUTIVE: i64 = 12;
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let n = haystack.len();
    let bonus = |j: usize| {
        MATCH + if is_boundary(j.checked_sub(1).map(|p| haystack[p])) { BOUNDARY } else { 0 }
    };
    // best[j]: best score with the current needle char matched at haystack[j].
    let mut best: Vec<Option<i64>> =
        (0..n).map(|j| (haystack[j] == needle[0]).then(|| bonus(j) - j as i64 / 4)).collect();
    for &c in &needle[1..] {
        let mut next = vec![None; n];
        // Running max of best[k] + k over k < j - 1, so the gap penalty (j - k - 1) is linear.
        let mut running: Option<i64> = None;
        for j in 1..n {
            if j >= 2
                && let Some(score) = best[j - 2]
            {
                let candidate = score + (j as i64 - 2);
                running = Some(running.map_or(candidate, |r| r.max(candidate)));
            }
            if haystack[j] != c {
                continue;
            }
            let gapped = running.map(|r| r - (j as i64 - 1) + bonus(j));
            let consecutive = best[j - 1].map(|s| s + bonus(j) + CONSECUTIVE);
            next[j] = gapped.max(consecutive);
        }
        best = next;
    }
    best.into_iter()
        .flatten()
        .max()
        .map(|score| u32::try_from(score.max(0)).unwrap_or(0).min(FUZZY_MAX))
}

fn match_text(haystack: &str, needle: &str, allow_fuzzy: bool) -> Option<u32> {
    if haystack.is_empty() || needle.is_empty() {
        return None;
    }
    if haystack == needle {
        return Some(EXACT);
    }
    // Shorter texts rank higher within one tier.
    let closeness = |tier: u32| tier + (99 * needle.len() / haystack.len()) as u32;
    if haystack.starts_with(needle) {
        return Some(closeness(PREFIX));
    }
    let mut prev = None;
    for (i, c) in haystack.char_indices() {
        if is_boundary(prev) && c.is_alphanumeric() && haystack[i..].starts_with(needle) {
            return Some(closeness(WORD_PREFIX));
        }
        prev = Some(c);
    }
    if haystack.contains(needle) {
        return Some(closeness(SUBSTRING));
    }
    if allow_fuzzy {
        let hay: Vec<char> = haystack.chars().collect();
        let pattern: Vec<char> = needle.chars().collect();
        return fuzzy(&hay, &pattern).map(|s| FUZZY_BASE + s);
    }
    None
}

struct Fields {
    name: String,
    generic_name: String,
    id: String,
    keywords: Vec<String>,
    categories: Vec<String>,
}

impl Fields {
    fn new(entry: &DesktopEntry) -> Self {
        let lower_all = |items: &[String]| items.iter().map(|s| s.to_lowercase()).collect();
        Self {
            name: entry.name.to_lowercase(),
            generic_name: entry.generic_name.as_deref().unwrap_or_default().to_lowercase(),
            id: entry.id.to_lowercase(),
            keywords: lower_all(&entry.keywords),
            categories: lower_all(&entry.categories),
        }
    }

    fn term_score(&self, term: &str) -> Option<u32> {
        let weighted = |score: Option<u32>, weight: u32| score.map(|s| s * weight / 100);
        let in_list = |list: &[String], weight: u32| {
            list.iter().filter_map(|item| weighted(match_text(item, term, false), weight)).max()
        };
        [
            weighted(match_text(&self.name, term, true), NAME),
            weighted(match_text(&self.generic_name, term, true), GENERIC_NAME),
            in_list(&self.keywords, KEYWORD),
            weighted(match_text(&self.id, term, true), ID),
            in_list(&self.categories, CATEGORY),
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

/// Scores `entry` against the lowercase `query` and its whitespace-separated `terms`.
/// Every term has to match some field.
fn score(entry: &DesktopEntry, query: &str, terms: &[&str]) -> Option<u32> {
    let fields = Fields::new(entry);
    let mut total = 0;
    for term in terms {
        total += fields.term_score(term)?;
    }
    if terms.len() > 1 {
        total += match_text(&fields.name, query, false).unwrap_or(0);
    }
    Some(total)
}

impl AppIndex {
    /// Ranks entries against `query` by fuzzy matching name, generic name, keywords, and id.
    /// An empty query returns all entries sorted by name.
    ///
    /// Exact and prefix matches on the name rank first; generic name, keywords, id, and categories count less.
    /// Equal scores are ordered by name.
    pub fn search(&self, query: &str) -> Vec<&DesktopEntry> {
        let query = query.trim().to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        if terms.is_empty() {
            let mut all: Vec<&DesktopEntry> = self.entries.iter().collect();
            all.sort_by(|a, b| compare_names(a, b));
            return all;
        }
        let mut scored: Vec<(u32, &DesktopEntry)> = self
            .entries
            .iter()
            .filter_map(|entry| Some((score(entry, &query, &terms)?, entry)))
            .collect();
        scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then_with(|| compare_names(a, b)));
        scored.into_iter().map(|(_, entry)| entry).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> DesktopEntry {
        DesktopEntry { id: id.into(), name: name.into(), ..Default::default() }
    }

    fn index() -> AppIndex {
        AppIndex {
            entries: vec![
                DesktopEntry {
                    generic_name: Some("Web Browser".into()),
                    keywords: vec!["internet".into(), "www".into()],
                    categories: vec!["Network".into()],
                    ..entry("org.mozilla.firefox", "Firefox")
                },
                DesktopEntry {
                    generic_name: Some("Terminal".into()),
                    ..entry("org.nimbus.Terminal", "Terminal")
                },
                DesktopEntry { keywords: vec!["shell".into()], ..entry("xterm", "XTerm") },
                entry("files", "Files"),
                entry("filezilla", "FileZilla"),
                entry("profile", "Profile Manager"),
                DesktopEntry {
                    generic_name: Some("Text Editor".into()),
                    ..entry("gedit", "gedit")
                },
                entry("editor", "Text Editor"),
                entry("settings", "Settings"),
            ],
        }
    }

    fn ids(index: &AppIndex, query: &str) -> Vec<String> {
        index.search(query).into_iter().map(|e| e.id.clone()).collect()
    }

    #[test]
    fn empty_query_sorts_by_name() {
        let index = index();
        let names: Vec<&str> = index.search("  ").into_iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.first(), Some(&"Files"));
        assert_eq!(names.len(), index.entries.len());
        assert!(names.windows(2).all(|w| w[0].to_lowercase() <= w[1].to_lowercase()));
    }

    #[test]
    fn exact_then_prefix_then_word_then_substring() {
        let index = index();
        let result = ids(&index, "files");
        assert_eq!(result.first().map(String::as_str), Some("files"));
        let result = ids(&index, "file");
        assert_eq!(&result[..2], &["files", "filezilla"]);
        assert!(result.contains(&"profile".to_owned()));
        let profile = result.iter().position(|id| id == "profile");
        assert!(profile > Some(1), "{result:?}");
    }

    #[test]
    fn fuzzy_matches_subsequences() {
        let index = index();
        assert_eq!(ids(&index, "ffx").first().map(String::as_str), Some("org.mozilla.firefox"));
        assert_eq!(ids(&index, "trm").first().map(String::as_str), Some("org.nimbus.Terminal"));
        assert!(ids(&index, "zzzq").is_empty());
    }

    #[test]
    fn secondary_fields_rank_below_name() {
        let index = index();
        assert_eq!(ids(&index, "browser"), vec!["org.mozilla.firefox"]);
        assert_eq!(ids(&index, "internet"), vec!["org.mozilla.firefox"]);
        assert_eq!(ids(&index, "network"), vec!["org.mozilla.firefox"]);
        assert_eq!(ids(&index, "mozilla"), vec!["org.mozilla.firefox"]);
        let shell = ids(&index, "term");
        assert_eq!(&shell[..2], &["org.nimbus.Terminal", "xterm"]);
        // The name match on "Text Editor" beats the generic-name match on gedit.
        assert_eq!(&ids(&index, "text editor")[..2], &["editor", "gedit"]);
    }

    #[test]
    fn ties_break_by_name() {
        let index = AppIndex { entries: vec![entry("g", "Gamma app"), entry("a", "Alpha app")] };
        assert_eq!(ids(&index, "app"), vec!["a", "g"]);
    }

    #[test]
    fn fuzzy_prefers_boundaries_and_runs() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        let boundary = fuzzy(&chars("visual studio code"), &chars("vsc"));
        let scattered = fuzzy(&chars("overscrolling"), &chars("vsc"));
        assert!(boundary > scattered, "{boundary:?} {scattered:?}");
        assert_eq!(fuzzy(&chars("ab"), &chars("abc")), None);
        assert_eq!(fuzzy(&chars("abc"), &chars("cb")), None);
    }
}
