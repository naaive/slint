// SPDX-License-Identifier: MIT

//! Keyboard, pointer, and wheel input on the terminal view.

use std::rc::Rc;
use std::time::Instant;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Side;
use alacritty_terminal::term::TermMode;

use super::controller::{Controller, cell_size};
use crate::AppWindow;
use crate::clipboard::ClipboardKind;
use crate::keys::{self, Key, Modifiers};
use crate::mouse::{self, Button, MouseEvent};
use crate::selection::{self, CellGeometry};
use crate::shortcuts;

/// The most wheel steps reported to a program for one event.
const MAX_WHEEL_REPORTS: i32 = 10;

pub(super) fn connect(controller: &Rc<Controller>, window: &AppWindow) {
    let weak = Rc::downgrade(controller);
    window.on_key(move |text, ctrl, alt, shift, meta| {
        let mods = Modifiers { shift, ctrl, alt, meta };
        weak.upgrade().is_some_and(|c| c.key(&text, mods))
    });
    let weak = Rc::downgrade(controller);
    window.on_pointer(move |kind, button, x, y, ctrl, alt, shift| {
        if let Some(c) = weak.upgrade() {
            c.pointer(kind, button, x, y, Modifiers { shift, ctrl, alt, meta: false });
        }
    });
    let weak = Rc::downgrade(controller);
    window.on_scrolled(move |_dx, dy, ctrl, shift| {
        weak.upgrade().is_some_and(|c| c.wheel(dy, Modifiers { shift, ctrl, ..Modifiers::NONE }))
    });
    let weak = Rc::downgrade(controller);
    window.on_scrollbar_moved(move |position| {
        if let Some(c) = weak.upgrade() {
            c.scrollbar_moved(position);
        }
    });
}

fn button_of(code: i32) -> Option<Button> {
    match code {
        1 => Some(Button::Left),
        2 => Some(Button::Middle),
        3 => Some(Button::Right),
        _ => None,
    }
}

impl Controller {
    /// Handles a key press on the terminal; returns whether it was used.
    pub fn key(self: &Rc<Self>, text: &str, mods: Modifiers) -> bool {
        let key = Key::from_slint_text(text);
        if matches!(key, Key::Modifier | Key::Unsupported) {
            return false;
        }
        if let Some(action) = shortcuts::lookup(&key, mods) {
            self.action(action);
            return true;
        }
        let exited =
            self.with_state(|st| st.tabs.get(st.current).map(|t| t.exited.is_some())).flatten();
        match exited {
            None => return false,
            Some(true) => {
                if key == Key::Enter {
                    self.action(shortcuts::Action::CloseTab);
                }
                return true;
            }
            Some(false) => {}
        }
        let written = self.with_state(|st| {
            let current = st.current;
            let tab = st.tabs.get_mut(current)?;
            let bytes = {
                let mut term = tab.session.term.lock();
                let bytes = keys::encode(&key, mods, *term.mode())?;
                if term.grid().display_offset() != 0 {
                    term.scroll_display(Scroll::Bottom);
                }
                bytes
            };
            tab.session.write(bytes);
            Some(())
        });
        if written.flatten().is_none() {
            return false;
        }
        self.restart_blink();
        true
    }

    /// The cell geometry in physical pixels, and the scale factor.
    fn geometry(&self) -> Option<(CellGeometry, f32)> {
        self.with_state(|st| {
            let (width, height) = cell_size(st);
            let geometry = CellGeometry {
                cell_width: width as f32,
                cell_height: height as f32,
                columns: st.grid.columns,
                lines: st.grid.lines,
            };
            (geometry, st.scale)
        })
    }

    /// Handles a pointer event at `(x, y)` logical pixels from the grid's corner.
    pub fn pointer(self: &Rc<Self>, kind: i32, button: i32, x: f32, y: f32, mods: Modifiers) {
        let Some((geometry, scale)) = self.geometry() else { return };
        let (px, py) = (x * scale, y * scale);
        let cell = selection::cell_at(px, py, geometry);
        let button = button_of(button);
        let mut paste_selection = false;
        let mut copy_selection = None;
        let changed = self.with_state(|st| {
            let current = st.current;
            let tab = st.tabs.get_mut(current)?;
            let mut term = tab.session.term.lock();
            let mode = *term.mode();
            let display_offset = term.grid().display_offset();

            // Programs that track the mouse get the events, unless Shift is held.
            if mouse::reporting(mode) && !mods.shift && tab.exited.is_none() {
                let event = match (kind, button) {
                    (0, Some(button)) => {
                        st.mouse.pressed = Some(button);
                        MouseEvent::Press(button)
                    }
                    (1, Some(button)) => {
                        st.mouse.pressed = None;
                        MouseEvent::Release(button)
                    }
                    (2, _) => {
                        if st.mouse.last_cell == Some((cell.column, cell.line)) {
                            return None;
                        }
                        MouseEvent::Motion(st.mouse.pressed)
                    }
                    _ => return None,
                };
                st.mouse.last_cell = Some((cell.column, cell.line));
                let report = mouse::report(event, cell.column, cell.line, mods, mode);
                drop(term);
                if let Some(bytes) = report {
                    tab.session.write(bytes);
                }
                return Some(false);
            }

            match (kind, button) {
                (0, Some(Button::Left)) => {
                    let point = cell.point(display_offset);
                    if mods.shift && term.selection.is_some() {
                        selection::update(&mut term, point, cell.side);
                    } else {
                        let clicks =
                            st.mouse.clicks.press(Instant::now(), (point.line.0, point.column.0));
                        let ty = selection::selection_type(clicks, mods.ctrl);
                        let side = if clicks > 1 { Side::Left } else { cell.side };
                        selection::start(&mut term, ty, point, side);
                    }
                    st.mouse.pressed = Some(Button::Left);
                    st.mouse.selecting = true;
                    Some(true)
                }
                (0, Some(Button::Middle)) => {
                    paste_selection = true;
                    None
                }
                (2, _) if st.mouse.selecting => {
                    // Dragging past the edges scrolls the history.
                    if py < 0.0 {
                        term.scroll_display(Scroll::Delta(1));
                    } else if py > geometry.cell_height * geometry.lines as f32 {
                        term.scroll_display(Scroll::Delta(-1));
                    }
                    let point = cell.point(term.grid().display_offset());
                    selection::update(&mut term, point, cell.side);
                    Some(true)
                }
                (1, Some(Button::Left)) if st.mouse.selecting => {
                    st.mouse.selecting = false;
                    st.mouse.pressed = None;
                    match selection::text(&term) {
                        Some(text) => copy_selection = Some(text),
                        None => term.selection = None,
                    }
                    Some(true)
                }
                _ => None,
            }
        });
        if let Some(text) = copy_selection {
            self.with_state(|st| st.clipboard.set(ClipboardKind::Selection, text));
        }
        if paste_selection {
            self.request_paste(ClipboardKind::Selection);
        }
        if changed.flatten() == Some(true) {
            self.schedule_render();
        }
    }

    /// Handles a wheel or touchpad scroll of `dy` logical pixels, positive up; returns whether it was used.
    pub fn wheel(self: &Rc<Self>, dy: f32, mods: Modifiers) -> bool {
        let Some((geometry, scale)) = self.geometry() else { return false };
        if mods.ctrl {
            let steps = self.with_state(|st| st.mouse.zoom.add(dy, 40.0)).unwrap_or(0);
            if steps != 0 {
                self.zoom(steps.signum());
            }
            return true;
        }
        let lines = self
            .with_state(|st| st.mouse.scroll.add(dy * scale, geometry.cell_height))
            .unwrap_or(0);
        if lines == 0 {
            return true;
        }
        self.with_state(|st| {
            let current = st.current;
            let column = st.mouse.last_cell.map_or(0, |c| c.0);
            let line = st.mouse.last_cell.map_or(0, |c| c.1);
            let Some(tab) = st.tabs.get_mut(current) else { return };
            let mut term = tab.session.term.lock();
            let mode = *term.mode();
            if mouse::reporting(mode) && !mods.shift {
                let button = if lines > 0 { Button::WheelUp } else { Button::WheelDown };
                let bytes: Vec<u8> = (0..lines.abs().min(MAX_WHEEL_REPORTS))
                    .filter_map(|_| {
                        mouse::report(MouseEvent::Press(button), column, line, mods, mode)
                    })
                    .flatten()
                    .collect();
                drop(term);
                tab.session.write(bytes);
            } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
                && !mods.shift
            {
                let bytes = mouse::alternate_scroll(lines, mode);
                drop(term);
                tab.session.write(bytes);
            } else {
                term.scroll_display(Scroll::Delta(lines));
            }
        });
        self.schedule_render();
        true
    }

    /// Scrolls so the viewport's top is at `position`, a fraction of the whole scrollback.
    pub fn scrollbar_moved(&self, position: f32) {
        self.with_state(|st| {
            if let Some(tab) = st.tabs.get(st.current) {
                let mut term = tab.session.term.lock();
                let history = term.history_size() as f32;
                let total = term.total_lines() as f32;
                let target =
                    (history - (position.clamp(0.0, 1.0) * total).round()).clamp(0.0, history);
                let delta = target as i32 - term.grid().display_offset() as i32;
                if delta != 0 {
                    term.scroll_display(Scroll::Delta(delta));
                }
            }
        });
        self.schedule_render();
    }
}
