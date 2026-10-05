// SPDX-License-Identifier: MIT

//! Key chords from `nimbus_config::Keybindings`, matched against raw (unshifted) keysyms.

use nimbus_config::{Action, Keybindings};
use smithay::input::keyboard::{Keysym, ModifiersState, xkb};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl From<&ModifiersState> for Mods {
    fn from(state: &ModifiersState) -> Self {
        Self { ctrl: state.ctrl, alt: state.alt, shift: state.shift, logo: state.logo }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub mods: Mods,
    pub keysym: Keysym,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    #[error("the chord is empty")]
    Empty,
    #[error("'{0}' is not a modifier; use Super, Ctrl, Alt, or Shift")]
    UnknownModifier(String),
    #[error("'{0}' is not an XKB keysym name")]
    UnknownKey(String),
}

/// Parses `Super+Shift+Q`: modifiers in any order and case, then one keysym name.
pub fn parse_chord(text: &str) -> Result<Chord, ChordError> {
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return Err(ChordError::Empty);
    };
    // `Super++` binds the plus key.
    let (key, modifiers) = match (key.is_empty(), modifiers.split_last()) {
        (true, Some((&"", rest))) => ("plus", rest),
        _ => (*key, modifiers),
    };
    if key.is_empty() {
        return Err(ChordError::Empty);
    }
    let mut mods = Mods::default();
    for modifier in modifiers {
        match modifier.to_ascii_lowercase().as_str() {
            "super" | "logo" | "mod4" | "win" | "meta" => mods.logo = true,
            "ctrl" | "control" => mods.ctrl = true,
            "alt" | "mod1" => mods.alt = true,
            "shift" => mods.shift = true,
            _ => return Err(ChordError::UnknownModifier((*modifier).to_owned())),
        }
    }
    Ok(Chord {
        mods,
        keysym: keysym_from_name(key).ok_or_else(|| ChordError::UnknownKey(key.to_owned()))?,
    })
}

/// Resolves an XKB keysym name, exactly first and then ignoring case.
pub fn keysym_from_name(name: &str) -> Option<Keysym> {
    // xkbcommon's wrapper panics on interior NUL bytes.
    if name.contains('\0') {
        return None;
    }
    [xkb::KEYSYM_NO_FLAGS, xkb::KEYSYM_CASE_INSENSITIVE]
        .into_iter()
        .map(|flags| xkb::keysym_from_name(name, flags))
        .find(|sym| sym.raw() != 0)
        .map(normalize)
}

/// Folds letter keysyms to lower case, since the raw keysym of a letter key is lower case.
pub fn normalize(sym: Keysym) -> Keysym {
    match sym.key_char() {
        Some(c) if c.is_uppercase() => {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) => Keysym::from_char(l),
                _ => sym,
            }
        }
        _ => sym,
    }
}

/// The parsed keybinding table.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    entries: Vec<(Chord, Action)>,
}

impl Bindings {
    /// Parses every chord, logging and skipping invalid ones.
    pub fn from_config(config: &Keybindings) -> Self {
        let mut entries = Vec::with_capacity(config.0.len());
        for (text, action) in &config.0 {
            match parse_chord(text) {
                Ok(chord) => entries.push((chord, action.clone())),
                Err(err) => tracing::warn!("ignoring keybinding '{text}': {err}"),
            }
        }
        Self { entries }
    }

    /// Finds the action for the pressed key, given its raw keysyms and the active modifiers.
    pub fn lookup(&self, mods: Mods, raw_syms: &[Keysym]) -> Option<&Action> {
        raw_syms.iter().map(|&sym| normalize(sym)).find_map(|sym| {
            self.entries
                .iter()
                .find(|(chord, _)| chord.mods == mods && chord.keysym == sym)
                .map(|(_, a)| a)
        })
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::keyboard::keysyms;

    fn sym(raw: u32) -> Keysym {
        Keysym::new(raw)
    }

    #[test]
    fn parses_modifiers_in_any_order_and_case() {
        let chord = parse_chord("shift+SUPER+q").unwrap();
        assert_eq!(chord.mods, Mods { shift: true, logo: true, ..Mods::default() });
        assert_eq!(chord.keysym, sym(keysyms::KEY_q));
        assert_eq!(parse_chord("Super+Shift+Q").unwrap(), chord);
        assert_eq!(
            parse_chord("Control+Mod1+Delete").unwrap().mods,
            Mods { ctrl: true, alt: true, ..Mods::default() }
        );
    }

    #[test]
    fn resolves_keysym_names() {
        assert_eq!(parse_chord("Super+Return").unwrap().keysym, sym(keysyms::KEY_Return));
        assert_eq!(parse_chord("Print").unwrap().keysym, sym(keysyms::KEY_Print));
        assert_eq!(
            parse_chord("XF86AudioRaiseVolume").unwrap().keysym,
            sym(keysyms::KEY_XF86AudioRaiseVolume)
        );
        assert_eq!(parse_chord("super+return").unwrap().keysym, sym(keysyms::KEY_Return));
        assert_eq!(parse_chord("Super+1").unwrap().keysym, sym(keysyms::KEY_1));
        assert_eq!(parse_chord("Super++").unwrap().keysym, sym(keysyms::KEY_plus));
    }

    #[test]
    fn rejects_invalid_chords() {
        assert_eq!(parse_chord(""), Err(ChordError::Empty));
        assert_eq!(parse_chord("Super+"), Err(ChordError::Empty));
        assert_eq!(parse_chord("Hyper+Q"), Err(ChordError::UnknownModifier("Hyper".into())));
        assert_eq!(parse_chord("Super+NoSuchKey"), Err(ChordError::UnknownKey("NoSuchKey".into())));
        assert!(matches!(parse_chord("Super+a\0b"), Err(ChordError::UnknownKey(_))));
    }

    #[test]
    fn default_bindings_all_parse() {
        let defaults = Keybindings::default();
        let bindings = Bindings::from_config(&defaults);
        assert_eq!(bindings.len(), defaults.0.len());
    }

    #[test]
    fn lookup_matches_exact_modifiers_on_raw_keysyms() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("Super+Shift+1".to_owned(), Action::MoveToWorkspace(0));
        map.insert("Super+1".to_owned(), Action::Workspace(0));
        map.insert("Super+Q".to_owned(), Action::CloseWindow);
        map.insert("bogus+chord".to_owned(), Action::Quit);
        let bindings = Bindings::from_config(&Keybindings(map));
        assert_eq!(bindings.len(), 3);

        let logo = Mods { logo: true, ..Mods::default() };
        let logo_shift = Mods { shift: true, ..logo };
        assert_eq!(bindings.lookup(logo, &[sym(keysyms::KEY_1)]), Some(&Action::Workspace(0)));
        assert_eq!(
            bindings.lookup(logo_shift, &[sym(keysyms::KEY_1)]),
            Some(&Action::MoveToWorkspace(0))
        );
        assert_eq!(bindings.lookup(logo, &[sym(keysyms::KEY_Q)]), Some(&Action::CloseWindow));
        assert_eq!(bindings.lookup(Mods::default(), &[sym(keysyms::KEY_q)]), None);
        assert_eq!(bindings.lookup(logo_shift, &[sym(keysyms::KEY_q)]), None);
    }
}
