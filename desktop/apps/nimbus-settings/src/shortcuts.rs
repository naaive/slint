// SPDX-License-Identifier: MIT

//! Keyboard shortcuts: chord syntax, capturing chords from key events, describing actions,
//! and editing [`Keybindings`] with conflict detection.

use std::collections::BTreeMap;

use nimbus_config::chord::Chord;
pub use nimbus_config::chord::Modifiers;
use nimbus_config::{Action, Keybindings};
use xkbcommon::xkb;

/// Rewrites a chord such as `shift+super+q` as `Super+Shift+Q`, using the compositor's grammar.
///
/// Returns `None` unless the chord parses with [`Chord::parse`] and its key is an XKB keysym name.
pub fn normalize_chord(chord: &str) -> Option<String> {
    let Chord { modifiers, key } = Chord::parse(chord).ok()?;
    Some(Chord { modifiers, key: normalize_key(&key)? }.to_string())
}

/// Resolves a keysym name the way the compositor does, exactly first and then ignoring case,
/// and returns its canonical name.
fn normalize_key(token: &str) -> Option<String> {
    // xkbcommon's wrapper panics on interior NUL bytes.
    if token.contains('\0') {
        return None;
    }
    let sym = [xkb::KEYSYM_NO_FLAGS, xkb::KEYSYM_CASE_INSENSITIVE]
        .into_iter()
        .map(|flags| xkb::keysym_from_name(token, flags))
        .find(|sym| sym.raw() != 0)?;
    Some(keysym_name(sym))
}

/// The name of `sym`, folded to its unshifted letter, with ASCII letters in upper case.
///
/// The compositor matches letters regardless of case, so `Super+E` and `Super+e` are the same chord.
fn keysym_name(sym: xkb::Keysym) -> String {
    let lower = char::from_u32(xkb::keysym_to_utf32(sym)).and_then(|c| {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(l), None) if l != c => Some(l),
            _ => None,
        }
    });
    let sym = lower.map_or(sym, |l| xkb::utf32_to_keysym(u32::from(l)));
    let name = xkb::keysym_get_name(sym);
    if name.len() == 1 && name.bytes().all(|b| b.is_ascii_lowercase()) {
        name.to_ascii_uppercase()
    } else {
        name
    }
}

/// Builds a chord string from a Slint key event's `text` and modifiers.
///
/// Returns `None` for modifier-only presses and keys without a keysym name.
/// Shifted symbols map back to their US base key, so Shift+1 is `Shift+1` rather than `Shift+exclam`.
pub fn chord_from_key_event(text: &str, modifiers: Modifiers) -> Option<String> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(Chord { modifiers, key: keysym_for(c)? }.to_string())
}

/// The XKB keysym name for a character of Slint's key event text.
fn keysym_for(c: char) -> Option<String> {
    // Slint's private-use codes for special keys, from `internal/common/key_codes.rs`.
    let special = match c {
        '\u{0008}' => "BackSpace",
        '\u{0009}' | '\u{0019}' => "Tab",
        '\u{000a}' | '\u{000d}' => "Return",
        '\u{001b}' => "Escape",
        '\u{007f}' => "Delete",
        ' ' => "space",
        '\u{F700}' => "Up",
        '\u{F701}' => "Down",
        '\u{F702}' => "Left",
        '\u{F703}' => "Right",
        '\u{F727}' => "Insert",
        '\u{F729}' => "Home",
        '\u{F72B}' => "End",
        '\u{F72C}' => "Page_Up",
        '\u{F72D}' => "Page_Down",
        '\u{F72F}' => "Scroll_Lock",
        '\u{F730}' => "Pause",
        '\u{F731}' => "Print",
        '\u{F735}' => "Menu",
        // Modifiers and lock keys on their own.
        '\u{0010}'..='\u{0018}' => return None,
        '\u{F704}'..='\u{F71B}' => return Some(format!("F{}", u32::from(c) - 0xF704 + 1)),
        '-' | '_' => "minus",
        '=' | '+' => "equal",
        ',' | '<' => "comma",
        '.' | '>' => "period",
        '/' | '?' => "slash",
        ';' | ':' => "semicolon",
        '\'' | '"' => "apostrophe",
        '[' | '{' => "bracketleft",
        ']' | '}' => "bracketright",
        '\\' | '|' => "backslash",
        '`' | '~' => "grave",
        _ => "",
    };
    if !special.is_empty() {
        return Some(special.into());
    }
    let shifted_digits = ")!@#$%^&*(";
    if let Some(digit) = shifted_digits.chars().position(|s| s == c) {
        return Some(digit.to_string());
    }
    if c.is_control() || ('\u{E000}'..='\u{F8FF}').contains(&c) {
        return None;
    }
    let sym = xkb::utf32_to_keysym(u32::from(c));
    (sym.raw() != 0).then(|| keysym_name(sym))
}

/// Human-readable labels for the parts of a chord, for drawing key caps.
pub fn chord_labels(chord: &str) -> Vec<String> {
    chord.split('+').filter(|t| !t.is_empty()).map(key_label).collect()
}

fn key_label(token: &str) -> String {
    let label = match token {
        "Return" => "Enter",
        "BackSpace" => "Backspace",
        "space" => "Space",
        "Up" => "↑",
        "Down" => "↓",
        "Left" => "←",
        "Right" => "→",
        "Page_Up" => "Page Up",
        "Page_Down" => "Page Down",
        "Print" => "Print Screen",
        "minus" => "-",
        "equal" => "=",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "semicolon" => ";",
        "apostrophe" => "'",
        "bracketleft" => "[",
        "bracketright" => "]",
        "backslash" => "\\",
        "grave" => "`",
        "XF86AudioRaiseVolume" => "Volume Up",
        "XF86AudioLowerVolume" => "Volume Down",
        "XF86AudioMute" => "Mute",
        "XF86MonBrightnessUp" => "Brightness Up",
        "XF86MonBrightnessDown" => "Brightness Down",
        other => return other.strip_prefix("XF86").unwrap_or(other).to_string(),
    };
    label.to_string()
}

/// Kinds of actions offered when adding a shortcut, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Spawn,
    ToggleLauncher,
    ToggleOverview,
    CloseWindow,
    ToggleMaximize,
    ToggleFullscreen,
    Minimize,
    ToggleLayout,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    Workspace,
    MoveToWorkspace,
    NextWorkspace,
    PreviousWorkspace,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    BrightnessUp,
    BrightnessDown,
    Lock,
    Screenshot,
    Quit,
}

/// What an [`ActionKind`] needs besides its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parameter {
    None,
    Command,
    Workspace,
}

impl ActionKind {
    pub const ALL: [ActionKind; 24] = [
        ActionKind::Spawn,
        ActionKind::ToggleLauncher,
        ActionKind::ToggleOverview,
        ActionKind::CloseWindow,
        ActionKind::ToggleMaximize,
        ActionKind::ToggleFullscreen,
        ActionKind::Minimize,
        ActionKind::ToggleLayout,
        ActionKind::FocusLeft,
        ActionKind::FocusRight,
        ActionKind::FocusUp,
        ActionKind::FocusDown,
        ActionKind::Workspace,
        ActionKind::MoveToWorkspace,
        ActionKind::NextWorkspace,
        ActionKind::PreviousWorkspace,
        ActionKind::VolumeUp,
        ActionKind::VolumeDown,
        ActionKind::ToggleMute,
        ActionKind::BrightnessUp,
        ActionKind::BrightnessDown,
        ActionKind::Lock,
        ActionKind::Screenshot,
        ActionKind::Quit,
    ];

    pub fn of(action: &Action) -> ActionKind {
        match action {
            Action::Spawn(_) => ActionKind::Spawn,
            Action::CloseWindow => ActionKind::CloseWindow,
            Action::ToggleMaximize => ActionKind::ToggleMaximize,
            Action::ToggleFullscreen => ActionKind::ToggleFullscreen,
            Action::Minimize => ActionKind::Minimize,
            Action::ToggleLayout => ActionKind::ToggleLayout,
            Action::FocusLeft => ActionKind::FocusLeft,
            Action::FocusRight => ActionKind::FocusRight,
            Action::FocusUp => ActionKind::FocusUp,
            Action::FocusDown => ActionKind::FocusDown,
            Action::Workspace(_) => ActionKind::Workspace,
            Action::MoveToWorkspace(_) => ActionKind::MoveToWorkspace,
            Action::NextWorkspace => ActionKind::NextWorkspace,
            Action::PreviousWorkspace => ActionKind::PreviousWorkspace,
            Action::ToggleLauncher => ActionKind::ToggleLauncher,
            Action::ToggleOverview => ActionKind::ToggleOverview,
            Action::Lock => ActionKind::Lock,
            Action::Screenshot => ActionKind::Screenshot,
            Action::VolumeUp => ActionKind::VolumeUp,
            Action::VolumeDown => ActionKind::VolumeDown,
            Action::ToggleMute => ActionKind::ToggleMute,
            Action::BrightnessUp => ActionKind::BrightnessUp,
            Action::BrightnessDown => ActionKind::BrightnessDown,
            Action::Quit => ActionKind::Quit,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Spawn => "Run a command",
            ActionKind::ToggleLauncher => "Show the app launcher",
            ActionKind::ToggleOverview => "Show the overview",
            ActionKind::CloseWindow => "Close window",
            ActionKind::ToggleMaximize => "Maximize or restore window",
            ActionKind::ToggleFullscreen => "Toggle fullscreen",
            ActionKind::Minimize => "Minimize window",
            ActionKind::ToggleLayout => "Switch between floating and tiling",
            ActionKind::FocusLeft => "Focus the window to the left",
            ActionKind::FocusRight => "Focus the window to the right",
            ActionKind::FocusUp => "Focus the window above",
            ActionKind::FocusDown => "Focus the window below",
            ActionKind::Workspace => "Switch to a workspace",
            ActionKind::MoveToWorkspace => "Move window to a workspace",
            ActionKind::NextWorkspace => "Switch to the next workspace",
            ActionKind::PreviousWorkspace => "Switch to the previous workspace",
            ActionKind::Lock => "Lock the screen",
            ActionKind::Screenshot => "Take a screenshot",
            ActionKind::VolumeUp => "Volume up",
            ActionKind::VolumeDown => "Volume down",
            ActionKind::ToggleMute => "Mute or unmute",
            ActionKind::BrightnessUp => "Brightness up",
            ActionKind::BrightnessDown => "Brightness down",
            ActionKind::Quit => "Log out",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            ActionKind::Spawn | ActionKind::ToggleLauncher | ActionKind::ToggleOverview => {
                "Launchers"
            }
            ActionKind::CloseWindow
            | ActionKind::ToggleMaximize
            | ActionKind::ToggleFullscreen
            | ActionKind::Minimize
            | ActionKind::ToggleLayout
            | ActionKind::FocusLeft
            | ActionKind::FocusRight
            | ActionKind::FocusUp
            | ActionKind::FocusDown => "Windows",
            ActionKind::Workspace
            | ActionKind::MoveToWorkspace
            | ActionKind::NextWorkspace
            | ActionKind::PreviousWorkspace => "Workspaces",
            ActionKind::VolumeUp
            | ActionKind::VolumeDown
            | ActionKind::ToggleMute
            | ActionKind::BrightnessUp
            | ActionKind::BrightnessDown => "Sound & Brightness",
            ActionKind::Lock | ActionKind::Screenshot | ActionKind::Quit => "System",
        }
    }

    pub fn parameter(self) -> Parameter {
        match self {
            ActionKind::Spawn => Parameter::Command,
            ActionKind::Workspace | ActionKind::MoveToWorkspace => Parameter::Workspace,
            _ => Parameter::None,
        }
    }

    pub fn index(self) -> usize {
        ActionKind::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    /// Builds the action from the user's parameter text: a command, or a one-based workspace number.
    pub fn build(self, parameter: &str) -> Option<Action> {
        let workspace =
            || parameter.trim().parse::<u32>().ok().filter(|n| (1..=99).contains(n)).map(|n| n - 1);
        Some(match self {
            ActionKind::Spawn => {
                let command = parameter.trim();
                if command.is_empty() {
                    return None;
                }
                Action::Spawn(command.to_string())
            }
            ActionKind::Workspace => Action::Workspace(workspace()?),
            ActionKind::MoveToWorkspace => Action::MoveToWorkspace(workspace()?),
            ActionKind::CloseWindow => Action::CloseWindow,
            ActionKind::ToggleMaximize => Action::ToggleMaximize,
            ActionKind::ToggleFullscreen => Action::ToggleFullscreen,
            ActionKind::Minimize => Action::Minimize,
            ActionKind::ToggleLayout => Action::ToggleLayout,
            ActionKind::FocusLeft => Action::FocusLeft,
            ActionKind::FocusRight => Action::FocusRight,
            ActionKind::FocusUp => Action::FocusUp,
            ActionKind::FocusDown => Action::FocusDown,
            ActionKind::NextWorkspace => Action::NextWorkspace,
            ActionKind::PreviousWorkspace => Action::PreviousWorkspace,
            ActionKind::ToggleLauncher => Action::ToggleLauncher,
            ActionKind::ToggleOverview => Action::ToggleOverview,
            ActionKind::Lock => Action::Lock,
            ActionKind::Screenshot => Action::Screenshot,
            ActionKind::VolumeUp => Action::VolumeUp,
            ActionKind::VolumeDown => Action::VolumeDown,
            ActionKind::ToggleMute => Action::ToggleMute,
            ActionKind::BrightnessUp => Action::BrightnessUp,
            ActionKind::BrightnessDown => Action::BrightnessDown,
            ActionKind::Quit => Action::Quit,
        })
    }
}

/// A sentence describing what `action` does.
pub fn describe(action: &Action) -> String {
    match action {
        Action::Spawn(command) => format!("Run “{command}”"),
        Action::Workspace(n) => format!("Switch to workspace {}", u64::from(*n) + 1),
        Action::MoveToWorkspace(n) => format!("Move window to workspace {}", u64::from(*n) + 1),
        other => ActionKind::of(other).label().to_string(),
    }
}

/// One binding as the shortcuts page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The chord as stored in the configuration, which identifies the row.
    pub chord: String,
    pub labels: Vec<String>,
    pub description: String,
    pub category: &'static str,
    /// Another binding has the same normalized chord, so only one of them can work.
    pub conflict: bool,
    /// The chord doesn't parse, so the compositor ignores the binding.
    pub invalid: bool,
}

fn sort_key(action: &Action) -> (usize, u64, String) {
    let parameter = match action {
        Action::Workspace(n) | Action::MoveToWorkspace(n) => u64::from(*n),
        _ => 0,
    };
    let command = match action {
        Action::Spawn(command) => command.clone(),
        _ => String::new(),
    };
    (ActionKind::of(action).index(), parameter, command)
}

/// The bindings in display order: by action, then by chord, so each category is contiguous.
pub fn rows(bindings: &Keybindings) -> Vec<Row> {
    let conflicted = conflicts(bindings);
    let mut rows: Vec<(&String, &Action)> = bindings.0.iter().collect();
    rows.sort_by(|a, b| sort_key(a.1).cmp(&sort_key(b.1)).then_with(|| a.0.cmp(b.0)));
    rows.into_iter()
        .map(|(chord, action)| Row {
            chord: chord.clone(),
            labels: chord_labels(&normalize_chord(chord).unwrap_or_else(|| chord.clone())),
            description: describe(action),
            category: ActionKind::of(action).category(),
            conflict: conflicted.iter().any(|c| c == chord),
            invalid: normalize_chord(chord).is_none(),
        })
        .collect()
}

/// The stored chords that collide with another binding after normalization.
pub fn conflicts(bindings: &Keybindings) -> Vec<String> {
    let mut by_chord: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for chord in bindings.0.keys() {
        let normalized = normalize_chord(chord).unwrap_or_else(|| chord.clone());
        by_chord.entry(normalized).or_default().push(chord);
    }
    by_chord.into_values().filter(|group| group.len() > 1).flatten().cloned().collect()
}

/// The stored chord and action bound to the same keys as `chord`, ignoring the stored chord `except`.
pub fn binding_for<'a>(
    bindings: &'a Keybindings,
    chord: &str,
    except: Option<&str>,
) -> Option<(&'a str, &'a Action)> {
    let wanted = normalize_chord(chord)?;
    bindings.0.iter().find_map(|(stored, action)| {
        let same = normalize_chord(stored).as_deref() == Some(wanted.as_str());
        (same && Some(stored.as_str()) != except).then_some((stored.as_str(), action))
    })
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum BindError {
    #[error("'{0}' isn't a valid shortcut")]
    InvalidChord(String),
    #[error("{chord} is already used by “{description}”")]
    Conflict { chord: String, description: String },
    #[error("there's no shortcut '{0}'")]
    Missing(String),
}

/// Binds `action` to `chord`, replacing the binding at `previous` when editing an existing one.
///
/// A binding that already uses the same keys is an error unless `replace` is set, which removes it.
pub fn bind(
    bindings: &mut Keybindings,
    chord: &str,
    action: Action,
    previous: Option<&str>,
    replace: bool,
) -> Result<(), BindError> {
    let normalized = normalize_chord(chord).ok_or_else(|| BindError::InvalidChord(chord.into()))?;
    if let Some(previous) = previous
        && !bindings.0.contains_key(previous)
    {
        return Err(BindError::Missing(previous.into()));
    }
    if let Some((stored, other)) = binding_for(bindings, &normalized, previous) {
        if !replace {
            return Err(BindError::Conflict { chord: normalized, description: describe(other) });
        }
        let stored = stored.to_string();
        bindings.0.remove(&stored);
    }
    if let Some(previous) = previous {
        bindings.0.remove(previous);
    }
    bindings.0.insert(normalized, action);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(logo: bool, ctrl: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers { logo, ctrl, alt, shift }
    }

    #[test]
    fn chords_normalize() {
        assert_eq!(normalize_chord("shift+super+q").as_deref(), Some("Super+Shift+Q"));
        assert_eq!(normalize_chord("Control + Alt + Delete").as_deref(), Some("Ctrl+Alt+Delete"));
        assert_eq!(normalize_chord("Print").as_deref(), Some("Print"));
        assert_eq!(normalize_chord("Mod4+Return").as_deref(), Some("Super+Return"));
        assert_eq!(normalize_chord("Super+Shift"), None);
        assert_eq!(normalize_chord("Super+A+B"), None);
        assert_eq!(normalize_chord("Super++").as_deref(), Some("Super+plus"));
        assert_eq!(normalize_chord(""), None);
        assert_eq!(normalize_chord("Primary+Q"), None, "the compositor has no 'Primary'");
        assert_eq!(normalize_chord("Q+Super"), None, "the key comes last");
        assert_eq!(normalize_chord("Super+É"), None, "keysym names are ASCII");
        assert_eq!(normalize_chord("Super+Eacute").as_deref(), Some("Super+eacute"));
        assert_eq!(normalize_chord("Super+return").as_deref(), Some("Super+Return"));
        assert_eq!(normalize_chord("Super+NotAKey"), None);
    }

    #[test]
    fn non_ascii_keys_become_keysym_names() {
        let sup = mods(true, false, false, false);
        assert_eq!(chord_from_key_event("é", sup).as_deref(), Some("Super+eacute"));
        assert_eq!(chord_from_key_event("É", sup).as_deref(), Some("Super+eacute"));
        assert_eq!(chord_from_key_event("ф", sup).as_deref(), Some("Super+Cyrillic_ef"));
        let mut b = Keybindings(BTreeMap::new());
        let chord = chord_from_key_event("é", sup).unwrap();
        bind(&mut b, &chord, Action::Lock, None, false).unwrap();
        assert_eq!(b.0.get("Super+eacute"), Some(&Action::Lock));
        assert_eq!(
            bind(&mut b, "Super+É", Action::Lock, None, false),
            Err(BindError::InvalidChord("Super+É".into()))
        );
    }

    #[test]
    fn key_events_become_chords() {
        assert_eq!(
            chord_from_key_event("q", mods(true, false, false, false)).as_deref(),
            Some("Super+Q")
        );
        assert_eq!(
            chord_from_key_event("!", mods(true, false, false, true)).as_deref(),
            Some("Super+Shift+1")
        );
        assert_eq!(
            chord_from_key_event("\n", mods(true, false, false, false)).as_deref(),
            Some("Super+Return")
        );
        assert_eq!(
            chord_from_key_event("\u{F702}", mods(true, true, false, false)).as_deref(),
            Some("Super+Ctrl+Left")
        );
        assert_eq!(
            chord_from_key_event("\u{F70B}", mods(false, false, true, false)).as_deref(),
            Some("Alt+F8")
        );
        assert_eq!(
            chord_from_key_event("\u{F731}", Modifiers::default()).as_deref(),
            Some("Print")
        );
        assert_eq!(
            chord_from_key_event(" ", mods(true, false, false, false)).as_deref(),
            Some("Super+space")
        );
        assert_eq!(
            chord_from_key_event("_", mods(false, true, false, true)).as_deref(),
            Some("Ctrl+Shift+minus")
        );
        assert_eq!(chord_from_key_event("\u{0017}", mods(true, false, false, false)), None);
        assert_eq!(chord_from_key_event("\u{0010}", mods(false, false, false, true)), None);
        assert_eq!(chord_from_key_event("", Modifiers::default()), None);
        assert_eq!(chord_from_key_event("ab", Modifiers::default()), None);
        assert_eq!(chord_from_key_event("\u{F748}", Modifiers::default()), None);
    }

    #[test]
    fn labels() {
        assert_eq!(chord_labels("Super+Shift+Return"), ["Super", "Shift", "Enter"]);
        assert_eq!(chord_labels("XF86AudioMute"), ["Mute"]);
        assert_eq!(chord_labels("XF86Calculator"), ["Calculator"]);
        assert_eq!(chord_labels("Super+Ctrl+Left"), ["Super", "Ctrl", "←"]);
    }

    #[test]
    fn descriptions_and_kinds() {
        assert_eq!(describe(&Action::Spawn("foot".into())), "Run “foot”");
        assert_eq!(describe(&Action::Workspace(0)), "Switch to workspace 1");
        assert_eq!(
            describe(&Action::MoveToWorkspace(u32::MAX)),
            "Move window to workspace 4294967296"
        );
        assert_eq!(describe(&Action::Lock), "Lock the screen");
        for kind in ActionKind::ALL {
            let parameter = match kind.parameter() {
                Parameter::None => "",
                Parameter::Command => "foot",
                Parameter::Workspace => "3",
            };
            let action = kind.build(parameter).expect("valid parameter builds");
            assert_eq!(ActionKind::of(&action), kind);
        }
        assert_eq!(ActionKind::Workspace.build("3"), Some(Action::Workspace(2)));
        assert_eq!(ActionKind::Workspace.build("0"), None);
        assert_eq!(ActionKind::Workspace.build("x"), None);
        assert_eq!(ActionKind::Spawn.build("  "), None);
    }

    #[test]
    fn rows_are_grouped_and_conflicts_flagged() {
        let defaults = Keybindings::default();
        let rows = rows(&defaults);
        assert_eq!(rows.len(), defaults.0.len());
        assert_eq!(rows[0].description, "Run “nimbus-files”");
        assert!(rows.iter().all(|r| !r.conflict && !r.invalid));
        let mut categories: Vec<&str> = rows.iter().map(|r| r.category).collect();
        categories.dedup();
        let mut unique = categories.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(categories.len(), unique.len(), "categories are contiguous: {categories:?}");
        let first_workspace =
            rows.iter().position(|r| r.description == "Switch to workspace 1").unwrap();
        assert_eq!(rows[first_workspace + 1].description, "Switch to workspace 2");

        let mut clash = Keybindings(BTreeMap::new());
        clash.0.insert("Super+Q".into(), Action::CloseWindow);
        clash.0.insert("super+q".into(), Action::Lock);
        clash.0.insert("Super+L".into(), Action::Lock);
        assert_eq!(conflicts(&clash), ["Super+Q", "super+q"]);
        assert_eq!(super::rows(&clash).iter().filter(|r| r.conflict).count(), 2);
        clash.0.insert("Super+É".into(), Action::Lock);
        let invalid: Vec<String> =
            super::rows(&clash).into_iter().filter(|r| r.invalid).map(|r| r.chord).collect();
        assert_eq!(invalid, ["Super+É"]);
    }

    #[test]
    fn binding_detects_and_replaces_conflicts() {
        let mut b = Keybindings::default();
        let err = bind(&mut b, "super+l", Action::Screenshot, None, false).unwrap_err();
        assert_eq!(
            err,
            BindError::Conflict { chord: "Super+L".into(), description: "Lock the screen".into() }
        );
        bind(&mut b, "super+l", Action::Screenshot, None, true).unwrap();
        assert_eq!(b.0.get("Super+L"), Some(&Action::Screenshot));

        // Rebinding a row to its own keys isn't a conflict.
        bind(&mut b, "Super+Q", Action::CloseWindow, Some("Super+Q"), false).unwrap();
        bind(&mut b, "Super+W", Action::CloseWindow, Some("Super+Q"), false).unwrap();
        assert!(!b.0.contains_key("Super+Q"));
        assert_eq!(b.0.get("Super+W"), Some(&Action::CloseWindow));

        assert_eq!(
            bind(&mut b, "Super+", Action::Lock, None, false),
            Err(BindError::InvalidChord("Super+".into()))
        );
        assert_eq!(
            bind(&mut b, "Super+K", Action::Lock, Some("Nope"), false),
            Err(BindError::Missing("Nope".into()))
        );
        assert_eq!(binding_for(&b, "SUPER+w", None).map(|(c, _)| c), Some("Super+W"));
        assert_eq!(binding_for(&b, "Super+W", Some("Super+W")), None);
    }
}
