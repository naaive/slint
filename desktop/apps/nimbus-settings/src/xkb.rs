// SPDX-License-Identifier: MIT

//! XKB keyboard layouts, variants, and options: parsing the rules list, and editing the comma-separated
//! `keyboard_layout`, `keyboard_variant`, and `keyboard_options` settings.

use std::collections::BTreeMap;
use std::path::Path;

/// The XKB rules list shipped by `xkeyboard-config`.
pub const EVDEV_LST: &str = "/usr/share/X11/xkb/rules/evdev.lst";

/// XKB supports at most four layout groups.
pub const MAX_SOURCES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    /// Sorted by description.
    pub layouts: Vec<Item>,
    /// Variants by layout name, in file order.
    pub variants: BTreeMap<String, Vec<Item>>,
}

impl Rules {
    /// Parses the `! layout` and `! variant` sections of an `evdev.lst` file.
    pub fn parse(text: &str) -> Rules {
        let mut rules = Rules::default();
        let mut section = "";
        for line in text.lines() {
            if let Some(name) = line.strip_prefix('!') {
                section = match name.trim() {
                    "layout" => "layout",
                    "variant" => "variant",
                    _ => "",
                };
                continue;
            }
            let line = line.trim();
            let Some((name, rest)) = line.split_once(char::is_whitespace) else { continue };
            let rest = rest.trim();
            match section {
                "layout" => {
                    rules.layouts.push(Item { name: name.into(), description: rest.into() })
                }
                "variant" => {
                    if let Some((layout, description)) = rest.split_once(':') {
                        rules.variants.entry(layout.trim().into()).or_default().push(Item {
                            name: name.into(),
                            description: description.trim().into(),
                        });
                    }
                }
                _ => {}
            }
        }
        rules.layouts.sort_by_cached_key(|item| item.description.to_lowercase());
        rules
    }

    /// Loads `path`, or a short built-in list of common layouts when it can't be read.
    pub fn load(path: &Path) -> Rules {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let rules = Rules::parse(&text);
                if !rules.layouts.is_empty() {
                    return rules;
                }
                tracing::warn!("{} lists no layouts; using built-in layouts", path.display());
            }
            Err(error) => {
                tracing::warn!("cannot read {}: {error}; using built-in layouts", path.display())
            }
        }
        Rules::builtin()
    }

    pub fn builtin() -> Rules {
        const COMMON: [(&str, &str); 16] = [
            ("cn", "Chinese"),
            ("cz", "Czech"),
            ("de", "German"),
            ("dk", "Danish"),
            ("es", "Spanish"),
            ("fi", "Finnish"),
            ("fr", "French"),
            ("gb", "English (UK)"),
            ("it", "Italian"),
            ("jp", "Japanese"),
            ("nl", "Dutch"),
            ("no", "Norwegian"),
            ("pl", "Polish"),
            ("pt", "Portuguese"),
            ("ru", "Russian"),
            ("us", "English (US)"),
        ];
        let mut layouts: Vec<Item> = COMMON
            .iter()
            .map(|(n, d)| Item { name: (*n).into(), description: (*d).into() })
            .collect();
        layouts.sort_by_cached_key(|item| item.description.to_lowercase());
        Rules { layouts, variants: BTreeMap::new() }
    }

    pub fn layout_description(&self, layout: &str) -> String {
        self.layouts
            .iter()
            .find(|i| i.name == layout)
            .map_or_else(|| layout.to_string(), |i| i.description.clone())
    }

    pub fn variant_description(&self, layout: &str, variant: &str) -> String {
        if variant.is_empty() {
            return "Default".into();
        }
        self.variants
            .get(layout)
            .and_then(|items| items.iter().find(|i| i.name == variant))
            .map_or_else(|| variant.to_string(), |i| i.description.clone())
    }

    pub fn variants_of(&self, layout: &str) -> &[Item] {
        self.variants.get(layout).map_or(&[], Vec::as_slice)
    }
}

/// One configured layout with its variant; an empty variant is the layout's default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub layout: String,
    pub variant: String,
}

/// Splits the parallel `keyboard_layout` and `keyboard_variant` lists into sources.
pub fn sources(layouts: &str, variants: &str) -> Vec<Source> {
    let mut variants = variants.split(',').map(str::trim);
    layouts
        .split(',')
        .map(|layout| (layout.trim(), variants.next().unwrap_or("")))
        .filter(|(layout, _)| !layout.is_empty())
        .map(|(layout, variant)| Source { layout: layout.into(), variant: variant.into() })
        .collect()
}

/// Joins sources back into `(keyboard_layout, keyboard_variant)`; the variant list is empty when all are defaults.
pub fn join(sources: &[Source]) -> (String, String) {
    let layouts = sources.iter().map(|s| s.layout.as_str()).collect::<Vec<_>>().join(",");
    let variants = if sources.iter().all(|s| s.variant.is_empty()) {
        String::new()
    } else {
        sources.iter().map(|s| s.variant.as_str()).collect::<Vec<_>>().join(",")
    };
    (layouts, variants)
}

/// Adds a source unless it's already configured or the maximum is reached; returns whether it did.
pub fn add_source(sources: &mut Vec<Source>, layout: &str, variant: &str) -> bool {
    let exists = sources.iter().any(|s| s.layout == layout && s.variant == variant);
    if exists || layout.is_empty() || sources.len() >= MAX_SOURCES {
        return false;
    }
    sources.push(Source { layout: layout.into(), variant: variant.into() });
    true
}

/// Removes the source at `index`, keeping at least one; returns whether it did.
pub fn remove_source(sources: &mut Vec<Source>, index: usize) -> bool {
    if sources.len() <= 1 || index >= sources.len() {
        return false;
    }
    sources.remove(index);
    true
}

/// Moves the source at `index` one place earlier, making the first one the default layout.
pub fn move_up(sources: &mut [Source], index: usize) -> bool {
    if index == 0 || index >= sources.len() {
        return false;
    }
    sources.swap(index - 1, index);
    true
}

/// Trims, deduplicates, and rejoins a comma-separated option list.
pub fn normalize_options(options: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for option in options.split(',').map(str::trim).filter(|o| !o.is_empty()) {
        if !seen.contains(&option) {
            seen.push(option);
        }
    }
    seen.join(",")
}

/// Mutually exclusive XKB options offered as one choice; the first choice clears them all.
pub struct OptionGroup {
    pub title: &'static str,
    pub choices: &'static [(&'static str, Option<&'static str>)],
}

pub const OPTION_GROUPS: [OptionGroup; 3] = [
    OptionGroup {
        title: "Switch layouts with",
        choices: &[
            ("Shortcut only", None),
            ("Alt+Shift", Some("grp:alt_shift_toggle")),
            ("Ctrl+Shift", Some("grp:ctrl_shift_toggle")),
            ("Caps Lock", Some("grp:caps_toggle")),
            ("Right Alt", Some("grp:toggle")),
        ],
    },
    OptionGroup {
        title: "Caps Lock behavior",
        choices: &[
            ("Caps Lock", None),
            ("Ctrl", Some("ctrl:nocaps")),
            ("Escape", Some("caps:escape")),
            ("Swap with Escape", Some("caps:swapescape")),
            ("Disabled", Some("caps:none")),
        ],
    },
    OptionGroup {
        title: "Compose key",
        choices: &[
            ("None", None),
            ("Right Alt", Some("compose:ralt")),
            ("Right Ctrl", Some("compose:rctrl")),
            ("Menu", Some("compose:menu")),
            ("Caps Lock", Some("compose:caps")),
        ],
    },
];

impl OptionGroup {
    /// The index of the choice active in `options`, or 0 when none of the group's options is set.
    pub fn current(&self, options: &str) -> usize {
        let set: Vec<&str> = options.split(',').map(str::trim).collect();
        self.choices
            .iter()
            .position(|(_, option)| option.is_some_and(|o| set.contains(&o)))
            .unwrap_or(0)
    }

    /// Returns `options` with this group's options replaced by choice `index`.
    pub fn select(&self, options: &str, index: usize) -> String {
        let members: Vec<&str> = self.choices.iter().filter_map(|(_, o)| *o).collect();
        let mut kept: Vec<&str> = options
            .split(',')
            .map(str::trim)
            .filter(|o| !o.is_empty() && !members.contains(o))
            .collect();
        if let Some((_, Some(option))) = self.choices.get(index) {
            kept.push(option);
        }
        normalize_options(&kept.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "! model\n  pc105           Generic 105-key PC\n\n! layout\n  us              English (US)\n  de              German\n  af              Dari\n! variant\n  intl            us: English (US, intl., with dead keys)\n  nodeadkeys      de: German (no dead keys)\n  broken-line\n! option\n  grp                  Switching to another layout\n";

    #[test]
    fn parses_layouts_and_variants() {
        let rules = Rules::parse(SAMPLE);
        let names: Vec<&str> = rules.layouts.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["af", "us", "de"], "sorted by description");
        assert_eq!(rules.layout_description("de"), "German");
        assert_eq!(rules.layout_description("xx"), "xx");
        assert_eq!(rules.variant_description("us", "intl"), "English (US, intl., with dead keys)");
        assert_eq!(rules.variant_description("us", ""), "Default");
        assert_eq!(rules.variants_of("de").len(), 1);
        assert!(rules.variants_of("af").is_empty());
    }

    #[test]
    fn missing_rules_fall_back() {
        let rules = Rules::load(Path::new("/nonexistent/evdev.lst"));
        assert!(rules.layouts.iter().any(|i| i.name == "us"));
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("evdev.lst");
        std::fs::write(&empty, "! layout\n").unwrap();
        assert_eq!(Rules::load(&empty), Rules::builtin());
    }

    #[test]
    fn real_rules_parse_when_installed() {
        let path = Path::new(EVDEV_LST);
        if path.exists() {
            let rules = Rules::load(path);
            assert!(rules.layouts.len() > 20);
            assert!(!rules.variants_of("us").is_empty());
        }
    }

    #[test]
    fn sources_round_trip() {
        let mut list = sources("de, us", "nodeadkeys");
        assert_eq!(list[1], Source { layout: "us".into(), variant: String::new() });
        assert_eq!(join(&list), ("de,us".to_string(), "nodeadkeys,".to_string()));
        assert!(move_up(&mut list, 1));
        assert!(!move_up(&mut list, 0));
        assert_eq!(join(&list), ("us,de".to_string(), ",nodeadkeys".to_string()));
        assert!(!add_source(&mut list, "us", ""));
        assert!(add_source(&mut list, "fr", ""));
        assert!(add_source(&mut list, "us", "intl"));
        assert!(!add_source(&mut list, "it", ""), "at most four");
        assert!(remove_source(&mut list, 1));
        assert_eq!(join(&list), ("us,fr,us".to_string(), ",,intl".to_string()));
        let mut single = sources("us", "");
        assert!(!remove_source(&mut single, 0));
        assert_eq!(join(&single), ("us".to_string(), String::new()));
        assert!(sources("", "").is_empty());
    }

    #[test]
    fn option_groups() {
        let caps = &OPTION_GROUPS[1];
        assert_eq!(caps.current(""), 0);
        assert_eq!(caps.current("compose:ralt, caps:escape"), 2);
        let options = caps.select("compose:ralt,caps:escape", 1);
        assert_eq!(options, "compose:ralt,ctrl:nocaps");
        assert_eq!(caps.select(&options, 0), "compose:ralt");
        assert_eq!(normalize_options(" a, ,b,a,"), "a,b");
    }
}
