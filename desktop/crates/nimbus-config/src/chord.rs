// SPDX-License-Identifier: MIT

//! The key chord grammar of [`Keybindings`](crate::Keybindings), such as `Super+Shift+Q`.
//!
//! A chord is any number of modifiers followed by one key, joined by `+`.
//! Modifiers match in any order and case, with aliases such as `Control` and `Mod4`.
//! The key is an XKB keysym name, kept as text here; resolving it needs xkbcommon,
//! so programs that bind keys resolve [`Chord::key`] themselves.
//! `Super++` binds the plus key.

use std::fmt;

/// The modifiers of a chord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// The Super, Logo, or Windows key.
    pub logo: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// One modifier key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Super,
    Ctrl,
    Alt,
    Shift,
}

impl Modifier {
    /// The modifiers in the order a canonical chord lists them.
    pub const ALL: [Modifier; 4] =
        [Modifier::Super, Modifier::Ctrl, Modifier::Alt, Modifier::Shift];

    /// Resolves a modifier name or alias, ignoring case.
    pub fn from_name(name: &str) -> Option<Modifier> {
        match name.to_ascii_lowercase().as_str() {
            "super" | "logo" | "mod4" | "win" | "meta" => Some(Modifier::Super),
            "ctrl" | "control" => Some(Modifier::Ctrl),
            "alt" | "mod1" => Some(Modifier::Alt),
            "shift" => Some(Modifier::Shift),
            _ => None,
        }
    }

    /// The canonical name, such as `Ctrl`.
    pub fn name(self) -> &'static str {
        match self {
            Modifier::Super => "Super",
            Modifier::Ctrl => "Ctrl",
            Modifier::Alt => "Alt",
            Modifier::Shift => "Shift",
        }
    }
}

impl Modifiers {
    pub fn contains(self, modifier: Modifier) -> bool {
        match modifier {
            Modifier::Super => self.logo,
            Modifier::Ctrl => self.ctrl,
            Modifier::Alt => self.alt,
            Modifier::Shift => self.shift,
        }
    }

    pub fn insert(&mut self, modifier: Modifier) {
        match modifier {
            Modifier::Super => self.logo = true,
            Modifier::Ctrl => self.ctrl = true,
            Modifier::Alt => self.alt = true,
            Modifier::Shift => self.shift = true,
        }
    }
}

/// A parsed chord: modifiers and a keysym name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub modifiers: Modifiers,
    /// The keysym name as written, such as `Return` or `q`.
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChordError {
    #[error("the chord has no key")]
    Empty,
    #[error("'{0}' is not a modifier; use Super, Ctrl, Alt, or Shift")]
    UnknownModifier(String),
    #[error("'{0}' is a modifier; end the chord with a key")]
    ModifierAsKey(String),
}

impl Chord {
    /// Parses a chord such as `shift+super+q`; see the [module documentation](self) for the grammar.
    pub fn parse(text: &str) -> Result<Chord, ChordError> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, modifiers)) = parts.split_last() else { return Err(ChordError::Empty) };
        let (key, modifiers) = match (key.is_empty(), modifiers.split_last()) {
            (true, Some((&"", rest))) => ("plus", rest),
            _ => (*key, modifiers),
        };
        if key.is_empty() {
            return Err(ChordError::Empty);
        }
        if Modifier::from_name(key).is_some() {
            return Err(ChordError::ModifierAsKey(key.to_owned()));
        }
        let mut parsed = Modifiers::default();
        for name in modifiers {
            let modifier = Modifier::from_name(name)
                .ok_or_else(|| ChordError::UnknownModifier((*name).to_owned()))?;
            parsed.insert(modifier);
        }
        Ok(Chord { modifiers: parsed, key: key.to_owned() })
    }
}

/// Writes the canonical form: modifiers in [`Modifier::ALL`] order with their canonical names, then the key.
impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for modifier in Modifier::ALL.into_iter().filter(|m| self.modifiers.contains(*m)) {
            write!(f, "{}+", modifier.name())?;
        }
        f.write_str(&self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(text: &str) -> Result<String, ChordError> {
        Chord::parse(text).map(|chord| chord.to_string())
    }

    #[test]
    fn modifiers_are_canonicalized() {
        assert_eq!(canonical("shift+super+q").as_deref(), Ok("Super+Shift+q"));
        assert_eq!(canonical("Control + Alt + Delete").as_deref(), Ok("Ctrl+Alt+Delete"));
        assert_eq!(canonical("Mod1+mod4+Return").as_deref(), Ok("Super+Alt+Return"));
        assert_eq!(canonical("WIN+META+x").as_deref(), Ok("Super+x"));
        assert_eq!(canonical("Print").as_deref(), Ok("Print"));
        assert_eq!(canonical("Super++").as_deref(), Ok("Super+plus"));
        assert_eq!(canonical("+").as_deref(), Ok("plus"));
    }

    #[test]
    fn parts_are_kept_apart() {
        let chord = Chord::parse("Ctrl+Shift+F5").unwrap();
        assert_eq!(chord.modifiers, Modifiers { ctrl: true, shift: true, ..Modifiers::default() });
        assert_eq!(chord.key, "F5");
    }

    #[test]
    fn invalid_chords_are_rejected() {
        assert_eq!(Chord::parse(""), Err(ChordError::Empty));
        assert_eq!(Chord::parse("Super+"), Err(ChordError::Empty));
        assert_eq!(Chord::parse("Super+Shift"), Err(ChordError::ModifierAsKey("Shift".into())));
        assert_eq!(Chord::parse("Primary+Q"), Err(ChordError::UnknownModifier("Primary".into())));
        assert_eq!(Chord::parse("Q+Super"), Err(ChordError::ModifierAsKey("Super".into())));
        assert_eq!(Chord::parse("Super+A+B"), Err(ChordError::UnknownModifier("A".into())));
    }

    #[test]
    fn default_bindings_parse_and_are_canonical() {
        for chord in crate::Keybindings::default().0.keys() {
            assert_eq!(canonical(chord).as_deref(), Ok(chord.as_str()));
        }
    }
}
