// SPDX-License-Identifier: MIT

//! Mouse reporting to terminal applications, click counting, and wheel accumulation.

use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;

use crate::keys::Modifiers;

/// Mouse buttons, including the wheel directions xterm reports as buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl Button {
    fn code(self) -> u8 {
        match self {
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
            Button::WheelUp => 64,
            Button::WheelDown => 65,
        }
    }
}

/// A pointer event in grid coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Press(Button),
    Release(Button),
    /// Motion, with the button held during it, if any.
    Motion(Option<Button>),
}

/// Whether the application wants pointer events instead of the terminal handling selection.
pub fn reporting(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Encodes a pointer event at zero-based `(column, line)` of the viewport as the application requested,
/// or returns `None` when the application doesn't want it.
pub fn report(
    event: MouseEvent,
    column: usize,
    line: usize,
    mods: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !reporting(mode) {
        return None;
    }
    let (mut code, pressed) = match event {
        MouseEvent::Press(button) => (button.code(), true),
        MouseEvent::Release(Button::WheelUp | Button::WheelDown) => return None,
        MouseEvent::Release(button) => (button.code(), false),
        MouseEvent::Motion(button) => {
            let wanted = mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && button.is_some());
            if !wanted {
                return None;
            }
            // Motion without a button reports as button 3.
            (32 + button.map_or(3, Button::code), true)
        }
    };
    if mods.shift {
        code += 4;
    }
    if mods.alt || mods.meta {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }

    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", column + 1, line + 1).into_bytes());
    }

    // The legacy encodings can't tell which button was released.
    if !pressed {
        code = (code & !3) | 3;
    }
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + code);
    for position in [column, line] {
        let value = position + 1 + 32;
        if mode.contains(TermMode::UTF8_MOUSE) {
            let c = char::from_u32(u32::try_from(value).ok()?).filter(|_| value < 2048)?;
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(u8::try_from(value).ok()?);
        }
    }
    Some(out)
}

/// The arrow-key presses that stand in for wheel scrolling in full-screen applications such as `less`,
/// when they don't request mouse reports. Positive `lines` scroll up.
pub fn alternate_scroll(lines: i32, mode: TermMode) -> Vec<u8> {
    let letter = if lines > 0 { b'A' } else { b'B' };
    let intro = if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' };
    (0..lines.unsigned_abs()).flat_map(|_| [0x1b, intro, letter]).collect()
}

/// The longest pause between clicks that still counts as a double or triple click.
pub const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);

/// Counts consecutive clicks on the same cell: 1, 2, 3, then 1 again.
#[derive(Debug, Default)]
pub struct ClickCounter {
    last: Option<(Instant, (i32, usize))>,
    count: u8,
}

impl ClickCounter {
    /// Registers a press at `cell` and returns the click count it completes.
    pub fn press(&mut self, now: Instant, cell: (i32, usize)) -> u8 {
        let continues = self.last.is_some_and(|(at, last_cell)| {
            last_cell == cell && now.saturating_duration_since(at) <= MULTI_CLICK_INTERVAL
        });
        self.count = if continues && self.count < 3 { self.count + 1 } else { 1 };
        self.last = Some((now, cell));
        self.count
    }
}

/// Turns pixel deltas from wheels and touchpads into whole lines, keeping the remainder.
#[derive(Debug, Default)]
pub struct ScrollAccumulator {
    remainder: f32,
}

impl ScrollAccumulator {
    /// Adds `delta` pixels and returns the whole lines of height `line_height` to scroll; positive is up.
    pub fn add(&mut self, delta: f32, line_height: f32) -> i32 {
        if !delta.is_finite() || line_height <= 0.0 {
            return 0;
        }
        self.remainder += delta / line_height;
        let lines = self.remainder.trunc();
        self.remainder -= lines;
        lines as i32
    }

    pub fn reset(&mut self) {
        self.remainder = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLICK: TermMode = TermMode::MOUSE_REPORT_CLICK;

    #[test]
    fn nothing_without_reporting() {
        assert_eq!(
            report(MouseEvent::Press(Button::Left), 0, 0, Modifiers::NONE, TermMode::default()),
            None
        );
        assert!(!reporting(TermMode::default()));
        assert!(reporting(TermMode::MOUSE_DRAG));
    }

    #[test]
    fn sgr_reports() {
        let mode = CLICK | TermMode::SGR_MOUSE;
        let r =
            |e, mods| report(e, 4, 9, mods, mode).map(|b| String::from_utf8(b).unwrap_or_default());
        assert_eq!(
            r(MouseEvent::Press(Button::Left), Modifiers::NONE).as_deref(),
            Some("\x1b[<0;5;10M")
        );
        assert_eq!(
            r(MouseEvent::Release(Button::Left), Modifiers::NONE).as_deref(),
            Some("\x1b[<0;5;10m")
        );
        assert_eq!(
            r(MouseEvent::Press(Button::Right), Modifiers::CTRL).as_deref(),
            Some("\x1b[<18;5;10M")
        );
        assert_eq!(
            r(MouseEvent::Press(Button::WheelUp), Modifiers::NONE).as_deref(),
            Some("\x1b[<64;5;10M")
        );
        assert_eq!(
            r(MouseEvent::Press(Button::WheelDown), Modifiers::SHIFT).as_deref(),
            Some("\x1b[<69;5;10M")
        );
        assert_eq!(r(MouseEvent::Release(Button::WheelUp), Modifiers::NONE), None);
        // Click-only mode ignores motion.
        assert_eq!(r(MouseEvent::Motion(Some(Button::Left)), Modifiers::NONE), None);
    }

    #[test]
    fn motion_modes() {
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            report(MouseEvent::Motion(Some(Button::Left)), 0, 0, Modifiers::NONE, drag).as_deref(),
            Some(&b"\x1b[<32;1;1M"[..])
        );
        assert_eq!(report(MouseEvent::Motion(None), 0, 0, Modifiers::NONE, drag), None);
        let any = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(
            report(MouseEvent::Motion(None), 1, 2, Modifiers::NONE, any).as_deref(),
            Some(&b"\x1b[<35;2;3M"[..])
        );
    }

    #[test]
    fn legacy_reports() {
        assert_eq!(
            report(MouseEvent::Press(Button::Left), 0, 0, Modifiers::NONE, CLICK).as_deref(),
            Some(&b"\x1b[M !!"[..])
        );
        assert_eq!(
            report(MouseEvent::Release(Button::Right), 2, 3, Modifiers::NONE, CLICK).as_deref(),
            Some(&b"\x1b[M##$"[..])
        );
        // Positions past 222 don't fit in a byte.
        assert_eq!(report(MouseEvent::Press(Button::Left), 300, 0, Modifiers::NONE, CLICK), None);
        let utf8 = CLICK | TermMode::UTF8_MOUSE;
        let bytes = report(MouseEvent::Press(Button::Left), 300, 0, Modifiers::NONE, utf8)
            .unwrap_or_default();
        assert_eq!(&bytes[..4], b"\x1b[M ");
        assert_eq!(std::str::from_utf8(&bytes[4..]).ok(), Some("\u{14d}!"));
    }

    #[test]
    fn alternate_scroll_keys() {
        assert_eq!(alternate_scroll(2, TermMode::default()), b"\x1b[A\x1b[A");
        assert_eq!(alternate_scroll(-1, TermMode::APP_CURSOR), b"\x1bOB");
        assert!(alternate_scroll(0, TermMode::default()).is_empty());
    }

    #[test]
    fn click_counting() {
        let mut clicks = ClickCounter::default();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(clicks.press(t0, (0, 1)), 1);
        assert_eq!(clicks.press(t0 + ms(100), (0, 1)), 2);
        assert_eq!(clicks.press(t0 + ms(200), (0, 1)), 3);
        assert_eq!(clicks.press(t0 + ms(300), (0, 1)), 1);
        assert_eq!(clicks.press(t0 + ms(400), (0, 2)), 1);
        assert_eq!(clicks.press(t0 + ms(1000), (0, 2)), 1);
    }

    #[test]
    fn scroll_accumulation() {
        let mut acc = ScrollAccumulator::default();
        assert_eq!(acc.add(10.0, 20.0), 0);
        assert_eq!(acc.add(10.0, 20.0), 1);
        assert_eq!(acc.add(-50.0, 20.0), -2);
        acc.reset();
        assert_eq!(acc.add(f32::NAN, 20.0), 0);
        assert_eq!(acc.add(5.0, 0.0), 0);
        assert_eq!(acc.add(60.0, 20.0), 3);
    }
}
