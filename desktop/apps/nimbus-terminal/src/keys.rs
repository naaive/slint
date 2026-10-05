// SPDX-License-Identifier: MIT

//! Translation of key presses into the byte sequences a terminal application expects (xterm style).

use alacritty_terminal::term::TermMode;

/// Modifier keys held during an input event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    pub const NONE: Self = Self { shift: false, ctrl: false, alt: false, meta: false };
    pub const SHIFT: Self = Self { shift: true, ..Self::NONE };
    pub const CTRL: Self = Self { ctrl: true, ..Self::NONE };
    pub const ALT: Self = Self { alt: true, ..Self::NONE };
    pub const CTRL_SHIFT: Self = Self { ctrl: true, shift: true, ..Self::NONE };

    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// The xterm modifier parameter: 1 plus a bit mask of shift, alt, ctrl, and meta.
    fn xterm_param(self) -> u8 {
        1 + u8::from(self.shift)
            + 2 * u8::from(self.alt)
            + 4 * u8::from(self.ctrl)
            + 8 * u8::from(self.meta)
    }
}

/// A key press, independent of the toolkit that reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Text produced by the key, with shift and the layout applied but not control.
    Text(String),
    Enter,
    Tab,
    /// Shift+Tab as reported by toolkits that give it its own key.
    Backtab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    /// A function key, `F(1)` to `F(24)`.
    F(u8),
    /// A modifier key on its own, such as Shift.
    Modifier,
    /// A key with no terminal meaning, such as Pause.
    Unsupported,
}

impl Key {
    /// Interprets the `text` of a Slint key event, which encodes special keys as private-use characters.
    pub fn from_slint_text(text: &str) -> Self {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.clone().next()) else {
            return if text.is_empty() { Key::Unsupported } else { Key::Text(text.to_string()) };
        };
        match c {
            '\u{8}' => Key::Backspace,
            '\t' => Key::Tab,
            '\n' | '\r' => Key::Enter,
            '\u{1b}' => Key::Escape,
            '\u{19}' => Key::Backtab,
            '\u{7f}' => Key::Delete,
            '\u{10}'..='\u{18}' => Key::Modifier,
            '\u{F700}' => Key::Up,
            '\u{F701}' => Key::Down,
            '\u{F702}' => Key::Left,
            '\u{F703}' => Key::Right,
            '\u{F704}'..='\u{F71B}' => Key::F((c as u32 - 0xF704 + 1) as u8),
            '\u{F727}' => Key::Insert,
            '\u{F729}' => Key::Home,
            '\u{F72B}' => Key::End,
            '\u{F72C}' => Key::PageUp,
            '\u{F72D}' => Key::PageDown,
            '\u{F700}'..='\u{F8FF}' => Key::Unsupported,
            c if c.is_control() => Key::Unsupported,
            c => Key::Text(c.to_string()),
        }
    }
}

/// The control character for `Ctrl+c`, following xterm, or `None` when the key has none.
fn control_char(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '~' | '6' => 0x1e,
        '_' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// A CSI sequence for a cursor-style key: `ESC [ x`, `ESC O x` in application mode, or `ESC [ 1 ; m x` with modifiers.
fn cursor_key(letter: u8, mods: Modifiers, application: bool) -> Vec<u8> {
    if mods.is_empty() {
        let intro = if application { b'O' } else { b'[' };
        vec![0x1b, intro, letter]
    } else {
        format!("\x1b[1;{}{}", mods.xterm_param(), letter as char).into_bytes()
    }
}

/// A tilde sequence such as `ESC [ 5 ~`, or `ESC [ 5 ; m ~` with modifiers.
fn tilde_key(code: u8, mods: Modifiers) -> Vec<u8> {
    if mods.is_empty() {
        format!("\x1b[{code}~").into_bytes()
    } else {
        format!("\x1b[{code};{}~", mods.xterm_param()).into_bytes()
    }
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// The bytes to send to the PTY for `key`, or `None` when the key produces nothing.
pub fn encode(key: &Key, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let bytes = match key {
        Key::Text(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if mods.ctrl => match control_char(c) {
                    Some(byte) => with_alt(vec![byte], mods.alt),
                    None => with_alt(text.as_bytes().to_vec(), mods.alt),
                },
                (Some(_), None) => with_alt(text.as_bytes().to_vec(), mods.alt),
                (Some(_), Some(_)) => text.as_bytes().to_vec(),
                (None, _) => return None,
            }
        }
        Key::Enter => {
            let enter: &[u8] =
                if mode.contains(TermMode::LINE_FEED_NEW_LINE) { b"\r\n" } else { b"\r" };
            with_alt(enter.to_vec(), mods.alt)
        }
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => with_alt(b"\t".to_vec(), mods.alt),
        Key::Backtab => b"\x1b[Z".to_vec(),
        Key::Backspace if mods.ctrl => with_alt(vec![0x08], mods.alt),
        Key::Backspace => with_alt(vec![0x7f], mods.alt),
        Key::Escape => with_alt(vec![0x1b], mods.alt),
        Key::Up => cursor_key(b'A', mods, app_cursor),
        Key::Down => cursor_key(b'B', mods, app_cursor),
        Key::Right => cursor_key(b'C', mods, app_cursor),
        Key::Left => cursor_key(b'D', mods, app_cursor),
        Key::Home => cursor_key(b'H', mods, app_cursor),
        Key::End => cursor_key(b'F', mods, app_cursor),
        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),
        Key::F(n @ 1..=4) => {
            let letter = b"PQRS"[usize::from(*n - 1)];
            if mods.is_empty() {
                vec![0x1b, b'O', letter]
            } else {
                format!("\x1b[1;{}{}", mods.xterm_param(), letter as char).into_bytes()
            }
        }
        Key::F(n @ 5..=24) => {
            const CODES: [u8; 20] =
                [15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34, 42, 43, 44, 45];
            tilde_key(CODES[usize::from(*n - 5)], mods)
        }
        Key::F(_) | Key::Modifier | Key::Unsupported => return None,
    };
    Some(bytes)
}

/// The bytes for pasting `text`: wrapped in bracketed-paste markers when the application asked for them.
///
/// Escape characters are removed from bracketed pastes so the text can't end the paste early.
/// Without bracketed paste, newlines become carriage returns, as typed Enter keys would.
pub fn paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend(text.replace(['\x1b', '\u{9b}'], "").bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Key {
        Key::Text(s.into())
    }

    fn enc(key: Key, mods: Modifiers) -> Vec<u8> {
        encode(&key, mods, TermMode::default()).unwrap_or_default()
    }

    #[test]
    fn slint_text_parsing() {
        assert_eq!(Key::from_slint_text("a"), text("a"));
        assert_eq!(Key::from_slint_text("ü"), text("ü"));
        assert_eq!(Key::from_slint_text("ab"), text("ab"));
        assert_eq!(Key::from_slint_text(""), Key::Unsupported);
        assert_eq!(Key::from_slint_text("\n"), Key::Enter);
        assert_eq!(Key::from_slint_text("\u{8}"), Key::Backspace);
        assert_eq!(Key::from_slint_text("\u{7f}"), Key::Delete);
        assert_eq!(Key::from_slint_text("\u{19}"), Key::Backtab);
        assert_eq!(Key::from_slint_text("\u{10}"), Key::Modifier);
        assert_eq!(Key::from_slint_text("\u{F700}"), Key::Up);
        assert_eq!(Key::from_slint_text("\u{F704}"), Key::F(1));
        assert_eq!(Key::from_slint_text("\u{F70F}"), Key::F(12));
        assert_eq!(Key::from_slint_text("\u{F71B}"), Key::F(24));
        assert_eq!(Key::from_slint_text("\u{F72C}"), Key::PageUp);
        assert_eq!(Key::from_slint_text("\u{F730}"), Key::Unsupported);
        assert_eq!(Key::from_slint_text(" "), text(" "));
    }

    #[test]
    fn plain_and_control_text() {
        assert_eq!(enc(text("a"), Modifiers::NONE), b"a");
        assert_eq!(enc(text("é"), Modifiers::NONE), "é".as_bytes());
        assert_eq!(enc(text("c"), Modifiers::CTRL), [3]);
        assert_eq!(enc(text("C"), Modifiers::CTRL_SHIFT), [3]);
        assert_eq!(enc(text(" "), Modifiers::CTRL), [0]);
        assert_eq!(enc(text("["), Modifiers::CTRL), [0x1b]);
        assert_eq!(enc(text("/"), Modifiers::CTRL), [0x1f]);
        assert_eq!(enc(text("1"), Modifiers::CTRL), b"1");
        assert_eq!(enc(text("x"), Modifiers::ALT), b"\x1bx");
        assert_eq!(
            enc(text("x"), Modifiers { ctrl: true, alt: true, ..Modifiers::NONE }),
            [0x1b, 0x18]
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc(Key::Enter, Modifiers::NONE), b"\r");
        assert_eq!(enc(Key::Enter, Modifiers::ALT), b"\x1b\r");
        let lnm = encode(&Key::Enter, Modifiers::NONE, TermMode::LINE_FEED_NEW_LINE);
        assert_eq!(lnm.as_deref(), Some(&b"\r\n"[..]));
        assert_eq!(enc(Key::Tab, Modifiers::NONE), b"\t");
        assert_eq!(enc(Key::Tab, Modifiers::SHIFT), b"\x1b[Z");
        assert_eq!(enc(Key::Backtab, Modifiers::SHIFT), b"\x1b[Z");
        assert_eq!(enc(Key::Backspace, Modifiers::NONE), [0x7f]);
        assert_eq!(enc(Key::Backspace, Modifiers::CTRL), [0x08]);
        assert_eq!(enc(Key::Backspace, Modifiers::ALT), [0x1b, 0x7f]);
        assert_eq!(enc(Key::Escape, Modifiers::NONE), [0x1b]);
        assert_eq!(enc(Key::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(enc(Key::Insert, Modifiers::NONE), b"\x1b[2~");
        assert_eq!(enc(Key::PageUp, Modifiers::NONE), b"\x1b[5~");
        assert_eq!(enc(Key::PageDown, Modifiers::CTRL), b"\x1b[6;5~");
    }

    #[test]
    fn cursor_keys_follow_application_mode() {
        assert_eq!(enc(Key::Up, Modifiers::NONE), b"\x1b[A");
        assert_eq!(enc(Key::Left, Modifiers::NONE), b"\x1b[D");
        assert_eq!(enc(Key::Home, Modifiers::NONE), b"\x1b[H");
        assert_eq!(enc(Key::End, Modifiers::NONE), b"\x1b[F");
        let app = TermMode::APP_CURSOR;
        assert_eq!(encode(&Key::Up, Modifiers::NONE, app).as_deref(), Some(&b"\x1bOA"[..]));
        assert_eq!(encode(&Key::Home, Modifiers::NONE, app).as_deref(), Some(&b"\x1bOH"[..]));
        // Modified cursor keys always use the CSI form.
        assert_eq!(encode(&Key::Right, Modifiers::CTRL, app).as_deref(), Some(&b"\x1b[1;5C"[..]));
        assert_eq!(enc(Key::Up, Modifiers::SHIFT), b"\x1b[1;2A");
        assert_eq!(
            enc(Key::Down, Modifiers { shift: true, alt: true, ..Modifiers::NONE }),
            b"\x1b[1;4B"
        );
        assert_eq!(enc(Key::Left, Modifiers { meta: true, ..Modifiers::NONE }), b"\x1b[1;9D");
    }

    #[test]
    fn function_keys() {
        assert_eq!(enc(Key::F(1), Modifiers::NONE), b"\x1bOP");
        assert_eq!(enc(Key::F(4), Modifiers::NONE), b"\x1bOS");
        assert_eq!(enc(Key::F(1), Modifiers::SHIFT), b"\x1b[1;2P");
        assert_eq!(enc(Key::F(5), Modifiers::NONE), b"\x1b[15~");
        assert_eq!(enc(Key::F(10), Modifiers::NONE), b"\x1b[21~");
        assert_eq!(enc(Key::F(12), Modifiers::CTRL), b"\x1b[24;5~");
        assert_eq!(enc(Key::F(24), Modifiers::NONE), b"\x1b[45~");
        assert_eq!(encode(&Key::F(0), Modifiers::NONE, TermMode::default()), None);
        assert_eq!(encode(&Key::F(30), Modifiers::NONE, TermMode::default()), None);
        assert_eq!(encode(&Key::Modifier, Modifiers::SHIFT, TermMode::default()), None);
    }

    #[test]
    fn pasting() {
        assert_eq!(paste("a\nb\r\nc", TermMode::default()), b"a\rb\rc");
        assert_eq!(
            paste("ls\x1b[201~rm", TermMode::BRACKETED_PASTE),
            b"\x1b[200~ls[201~rm\x1b[201~"
        );
        assert_eq!(paste("", TermMode::BRACKETED_PASTE), b"\x1b[200~\x1b[201~");
    }
}
