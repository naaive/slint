// SPDX-License-Identifier: MIT

//! The freedesktop.org key file format shared by desktop entries, `index.theme`, `mimeapps.list`, and `mimeinfo.cache`.

/// A parsed key file. Malformed lines are skipped; for duplicate groups and keys the first one wins.
#[derive(Debug, Default)]
pub(crate) struct KeyFile {
    groups: Vec<Group>,
}

#[derive(Debug, Default)]
pub(crate) struct Group {
    pub name: String,
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    key: String,
    locale: Option<String>,
    value: String,
}

impl KeyFile {
    pub fn parse(contents: &str) -> Self {
        let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
        let mut groups: Vec<Group> = Vec::new();
        // Index into `groups` of the group receiving entries; `None` before the first header or inside a duplicate group.
        let mut current: Option<usize> = None;
        for line in contents.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let Some(name) = rest.trim_end().strip_suffix(']') else {
                    current = None;
                    continue;
                };
                if groups.iter().any(|g| g.name == name) {
                    current = None;
                } else {
                    groups.push(Group { name: name.to_owned(), entries: Vec::new() });
                    current = Some(groups.len() - 1);
                }
                continue;
            }
            let Some(group) = current.and_then(|i| groups.get_mut(i)) else { continue };
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim_end();
            let (key, locale) = match key.split_once('[') {
                Some((base, rest)) => match rest.strip_suffix(']') {
                    Some(locale) if !locale.is_empty() => (base, Some(locale.to_owned())),
                    _ => continue,
                },
                None => (key, None),
            };
            if key.is_empty() {
                continue;
            }
            if group.entries.iter().any(|e| e.key == key && e.locale == locale) {
                continue;
            }
            group.entries.push(Entry {
                key: key.to_owned(),
                locale,
                value: value.trim_start().to_owned(),
            });
        }
        Self { groups }
    }

    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    pub fn groups(&self) -> impl Iterator<Item = &Group> {
        self.groups.iter()
    }
}

impl Group {
    /// The unlocalized raw value, without unescaping.
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.key == key && e.locale.is_none()).map(|e| e.value.as_str())
    }

    /// The raw value for the first matching locale in `candidates`, else the unlocalized one.
    pub fn raw_localized(&self, key: &str, candidates: &[String]) -> Option<&str> {
        candidates
            .iter()
            .find_map(|candidate| {
                self.entries
                    .iter()
                    .find(|e| e.key == key && e.locale.as_deref() == Some(candidate.as_str()))
            })
            .map(|e| e.value.as_str())
            .or_else(|| self.raw(key))
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.raw(key).map(unescape)
    }

    pub fn locale_string(&self, key: &str, candidates: &[String]) -> Option<String> {
        self.raw_localized(key, candidates).map(unescape)
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        self.raw(key).map(split_list).unwrap_or_default()
    }

    pub fn locale_strings(&self, key: &str, candidates: &[String]) -> Vec<String> {
        self.raw_localized(key, candidates).map(split_list).unwrap_or_default()
    }

    /// Accepts `true`/`false` and the legacy `1`/`0`.
    pub fn boolean(&self, key: &str) -> Option<bool> {
        match self.raw(key)?.trim() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }

    pub fn integer(&self, key: &str) -> Option<i64> {
        self.raw(key)?.trim().parse().ok()
    }

    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter(|e| e.locale.is_none()).map(|e| (e.key.as_str(), e.value.as_str()))
    }
}

/// Applies the string escapes `\s`, `\n`, `\t`, `\r`, and `\\`; other backslashes are kept literally.
pub(crate) fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits a `;`-separated list, honoring `\;`, and unescapes each element. Empty elements are dropped.
pub(crate) fn split_list(raw: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(';') => current.push(';'),
                Some(next) => {
                    current.push('\\');
                    current.push(next);
                }
                None => current.push('\\'),
            },
            ';' => {
                let item = unescape(&current);
                if !item.is_empty() {
                    items.push(item);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let item = unescape(&current);
    if !item.is_empty() {
        items.push(item);
    }
    items
}

/// A POSIX locale such as `sr_RS.UTF-8@latin`, reduced to the parts the key file lookup uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Locale {
    lang: String,
    country: Option<String>,
    modifier: Option<String>,
}

impl Locale {
    /// Returns `None` for an empty locale and for `C` and `POSIX`, which have no translations.
    pub fn parse(locale: &str) -> Option<Self> {
        let locale = locale.trim();
        let (rest, modifier) = match locale.split_once('@') {
            Some((rest, modifier)) => (rest, Some(modifier)),
            None => (locale, None),
        };
        let rest = rest.split_once('.').map_or(rest, |(before, _)| before);
        let (lang, country) = match rest.split_once('_') {
            Some((lang, country)) => (lang, Some(country)),
            None => (rest, None),
        };
        if lang.is_empty() || lang == "C" || lang == "POSIX" {
            return None;
        }
        let non_empty = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(str::to_owned);
        Some(Self { lang: lang.to_owned(), country: non_empty(country), modifier: non_empty(modifier) })
    }

    /// The localized key suffixes to try, in the Desktop Entry Specification's order.
    pub fn candidates(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(4);
        let lang = &self.lang;
        if let (Some(country), Some(modifier)) = (&self.country, &self.modifier) {
            out.push(format!("{lang}_{country}@{modifier}"));
        }
        if let Some(country) = &self.country {
            out.push(format!("{lang}_{country}"));
        }
        if let Some(modifier) = &self.modifier {
            out.push(format!("{lang}@{modifier}"));
        }
        out.push(lang.clone());
        out
    }
}

/// The locale candidates for `locale`, empty when it has no translations.
pub(crate) fn locale_candidates(locale: Option<&str>) -> Vec<String> {
    locale.and_then(Locale::parse).map(|l| l.candidates()).unwrap_or_default()
}

/// The message locale from `LC_ALL`, `LC_MESSAGES`, or `LANG`, whichever is set first.
pub fn locale_from_env() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.is_empty())
        .filter(|value| Locale::parse(value).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_keys_and_comments() {
        let kf = KeyFile::parse(
            "\u{feff}# comment\nIgnored=1\n[A]\nKey = value \nName[de]=Wert\n[B]\nKey=b\n[A]\nKey=dup\nbroken line\n",
        );
        let a = kf.group("A").expect("group A");
        assert_eq!(a.raw("Key"), Some("value "));
        assert_eq!(a.raw_localized("Name", &["de".into()]), Some("Wert"));
        assert_eq!(kf.group("B").and_then(|g| g.raw("Key")), Some("b"));
        assert_eq!(kf.groups().count(), 2);
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r"a\sb\nc\td\re\\f\x"), "a b\nc\td\re\\f\\x");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn lists() {
        assert_eq!(split_list(r"a;b\;c;;d\s e;"), vec!["a", "b;c", "d e"]);
        assert_eq!(split_list(r"x\\;y"), vec!["x\\", "y"]);
        assert!(split_list("").is_empty());
    }

    #[test]
    fn locale_candidates_follow_spec_order() {
        assert_eq!(
            locale_candidates(Some("sr_RS.UTF-8@latin")),
            vec!["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
        assert_eq!(locale_candidates(Some("de_DE.UTF-8")), vec!["de_DE", "de"]);
        assert_eq!(locale_candidates(Some("fr")), vec!["fr"]);
        assert!(locale_candidates(Some("C.UTF-8")).is_empty());
        assert!(locale_candidates(Some("POSIX")).is_empty());
        assert!(locale_candidates(None).is_empty());
    }

    #[test]
    fn booleans() {
        let kf = KeyFile::parse("[G]\nA=true\nB=0\nC=yes\n");
        let g = kf.group("G").expect("group");
        assert_eq!(g.boolean("A"), Some(true));
        assert_eq!(g.boolean("B"), Some(false));
        assert_eq!(g.boolean("C"), None);
    }
}
