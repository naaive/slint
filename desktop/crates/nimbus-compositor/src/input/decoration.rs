// SPDX-License-Identifier: MIT

//! Pointer input on server-side decorations: hover, buttons, moving by the titlebar, and resizing by the borders.

use super::pointer::BTN_LEFT;
use crate::decoration::{Button, Hit};
use crate::state::State;
use crate::wm::WindowMode;
use nimbus_ipc::{Request, WindowId};
use smithay::backend::input::ButtonState;
use smithay::input::pointer::{CursorIcon, CursorImageStatus, GrabStartData};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::utils::{Logical, Point, Serial};

/// How far the pointer moves with the button down on a titlebar before the window follows.
const DRAG_THRESHOLD: f64 = 4.0;
/// The longest time between the presses of a double click, in milliseconds.
const DOUBLE_CLICK_MS: u32 = 400;

/// The pointer's dealings with decorations.
#[derive(Default)]
pub struct DecorationInput {
    hover: Option<(WindowId, Hit)>,
    press: Option<Press>,
    /// The window and time of the last left click on a titlebar.
    last_click: Option<(WindowId, u32)>,
}

struct Press {
    window: WindowId,
    hit: Hit,
    button: u32,
    location: Point<f64, Logical>,
    serial: Serial,
}

impl DecorationInput {
    /// The button of window `id` under the pointer.
    pub fn hovered_button(&self, id: WindowId) -> Option<Button> {
        match self.hover {
            Some((window, Hit::Button(button))) if window == id => Some(button),
            _ => None,
        }
    }
}

fn cursor_for(hit: Hit) -> CursorIcon {
    let Hit::Edge(edge) = hit else {
        return CursorIcon::Default;
    };
    match edge {
        ResizeEdge::Top => CursorIcon::NResize,
        ResizeEdge::Bottom => CursorIcon::SResize,
        ResizeEdge::Left => CursorIcon::WResize,
        ResizeEdge::Right => CursorIcon::EResize,
        ResizeEdge::TopLeft => CursorIcon::NwResize,
        ResizeEdge::TopRight => CursorIcon::NeResize,
        ResizeEdge::BottomLeft => CursorIcon::SwResize,
        ResizeEdge::BottomRight => CursorIcon::SeResize,
        _ => CursorIcon::Default,
    }
}

impl State {
    /// Follows the pointer onto `hit`, or off decorations with `None`, and starts a titlebar drag past the threshold.
    pub(super) fn decoration_motion(
        &mut self,
        hit: Option<(WindowId, Hit)>,
        location: Point<f64, Logical>,
    ) {
        let grabbed = self.nimbus.pointer.is_grabbed();
        let input = &mut self.nimbus.decoration_input;
        if input.hover != hit && !grabbed {
            let left = input.hover.is_some();
            input.hover = hit;
            match hit {
                Some((_, part)) => {
                    self.nimbus.cursor_status = CursorImageStatus::Named(cursor_for(part));
                }
                None if left => self.nimbus.cursor_status = CursorImageStatus::default_named(),
                None => {}
            }
            self.nimbus.queue_redraw_all();
        }

        let input = &mut self.nimbus.decoration_input;
        let dragging = input.press.as_ref().is_some_and(|press| {
            let moved = location - press.location;
            press.hit == Hit::Titlebar && moved.x.hypot(moved.y) > DRAG_THRESHOLD
        });
        if dragging && let Some(press) = input.press.take() {
            let start =
                GrabStartData { focus: None, button: press.button, location: press.location };
            self.begin_move(press.window, start, press.serial);
        }
    }

    /// Handles a button on a decoration: `hit` is what's under the pointer, if it's a decoration.
    pub(super) fn decoration_button(
        &mut self,
        hit: Option<(WindowId, Hit)>,
        button: u32,
        state: ButtonState,
        serial: Serial,
        time: u32,
    ) {
        if state == ButtonState::Released {
            let Some(press) = self.nimbus.decoration_input.press.take() else {
                return;
            };
            if let Hit::Button(pressed) = press.hit
                && press.button == button
                && hit == Some((press.window, press.hit))
            {
                self.press_decoration_button(press.window, pressed);
            }
            return;
        }
        let Some((id, part)) = hit else {
            return;
        };
        self.nimbus.layer_focus = None;
        self.nimbus.wm.focus(Some(id));
        self.nimbus.arrange();
        if button != BTN_LEFT {
            return;
        }
        let location = self.nimbus.pointer_location;
        let input = &mut self.nimbus.decoration_input;
        match part {
            Hit::Edge(edges) => {
                let start = GrabStartData { focus: None, button, location };
                self.begin_resize(id, start, serial, Some(edges));
            }
            Hit::Titlebar
                if input.last_click.is_some_and(|(window, at)| {
                    window == id && time.wrapping_sub(at) <= DOUBLE_CLICK_MS
                }) =>
            {
                input.last_click = None;
                self.nimbus.wm.toggle_mode(id, WindowMode::Maximized);
                self.nimbus.arrange();
            }
            Hit::Titlebar | Hit::Button(_) => {
                if part == Hit::Titlebar {
                    input.last_click = Some((id, time));
                }
                input.press = Some(Press { window: id, hit: part, button, location, serial });
            }
        }
    }

    fn press_decoration_button(&mut self, id: WindowId, button: Button) {
        match button {
            Button::Close => {
                self.handle_request(Request::Close { id });
            }
            Button::Maximize => {
                self.nimbus.wm.toggle_mode(id, WindowMode::Maximized);
                self.nimbus.arrange();
            }
            Button::Minimize => {
                self.handle_request(Request::SetMinimized { id, minimized: true });
            }
        }
    }
}
