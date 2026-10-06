// SPDX-License-Identifier: MIT

//! Input routing: compositor shortcuts, focus, and delivery to Wayland clients.

mod constraints;
mod decoration;
mod devices;
mod gestures;
mod keyboard;
mod pointer;
mod tablet;
mod touch;

pub use decoration::DecorationInput;
pub use gestures::WorkspaceSwipe;

use crate::state::{Nimbus, State};
use nimbus_ipc::WindowId;
use smithay::backend::input::{ButtonState, InputBackend, InputEvent, KeyState};
use smithay::desktop::{PopupManager, layer_map_for_output};
use smithay::input::keyboard::Keycode;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use std::time::Duration;

/// The difference between Linux input event codes and XKB keycodes.
const XKB_KEYCODE_OFFSET: u32 = 8;

impl Nimbus {
    /// The managed window owning `surface` or one of its subsurfaces and popups.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.wm
            .visible_stack()
            .find(|w| {
                let mut found = false;
                w.window.with_surfaces(|s, _| found |= s == surface);
                found
            })
            .map(|w| w.id)
    }

    /// Sends `popup_done` to every popup of every window and layer surface.
    fn dismiss_all_popups(&self) {
        let mut roots: Vec<WlSurface> = self
            .wm
            .space
            .elements()
            .filter_map(|w| w.toplevel().map(|t| t.wl_surface().clone()))
            .collect();
        for output in self.outputs() {
            roots.extend(layer_map_for_output(output).layers().map(|l| l.wl_surface().clone()));
        }
        for root in roots {
            for (popup, _) in PopupManager::popups_for_surface(&root) {
                if let Err(err) = PopupManager::dismiss_popup(&root, &popup) {
                    tracing::debug!("cannot dismiss a popup: {err}");
                }
            }
        }
    }

    pub fn notify_activity(&mut self) {
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
    }
}

impl State {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        self.input_arrived();
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => self.on_pointer_motion::<B>(event),
            InputEvent::PointerMotionAbsolute { event } => {
                self.on_pointer_motion_absolute::<B>(event)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            InputEvent::GestureSwipeBegin { event } => self.on_gesture_swipe_begin::<B>(event),
            InputEvent::GestureSwipeUpdate { event } => self.on_gesture_swipe_update::<B>(event),
            InputEvent::GestureSwipeEnd { event } => self.on_gesture_swipe_end::<B>(event),
            InputEvent::GesturePinchBegin { event } => self.on_gesture_pinch_begin::<B>(event),
            InputEvent::GesturePinchUpdate { event } => self.on_gesture_pinch_update::<B>(event),
            InputEvent::GesturePinchEnd { event } => self.on_gesture_pinch_end::<B>(event),
            InputEvent::GestureHoldBegin { event } => self.on_gesture_hold_begin::<B>(event),
            InputEvent::GestureHoldEnd { event } => self.on_gesture_hold_end::<B>(event),
            InputEvent::TouchDown { event } => self.on_touch_down::<B>(event),
            InputEvent::TouchMotion { event } => self.on_touch_motion::<B>(event),
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchFrame { .. } => self.on_touch_frame(),
            InputEvent::TouchCancel { .. } => self.on_touch_cancel(),
            InputEvent::TabletToolAxis { event } => self.on_tablet_tool_axis::<B>(event),
            InputEvent::TabletToolProximity { event } => self.on_tablet_tool_proximity::<B>(event),
            InputEvent::TabletToolTip { event } => self.on_tablet_tool_tip::<B>(event),
            InputEvent::TabletToolButton { event } => self.on_tablet_tool_button::<B>(event),
            InputEvent::DeviceAdded { device } => self.on_device_added(&device),
            InputEvent::DeviceRemoved { device } => self.on_device_removed(&device),
            _ => {}
        }
    }

    fn input_arrived(&mut self) {
        self.nimbus.notify_activity();
        if self.nimbus.is_locked() && self.has_grabs() {
            self.break_grabs_for_lock();
        }
    }

    /// Moves the pointer to `location` and clicks the left button, as a user would.
    pub fn click(&mut self, location: Point<f64, Logical>) {
        let time = self.input_time();
        self.input_arrived();
        self.pointer_moved(location, time, None);
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.pointer_button(pointer::BTN_LEFT, state, time);
        }
    }

    /// Presses and releases the key with the Linux input event `code`, as a user would.
    pub fn press_key(&mut self, code: u32) {
        self.key(code, true);
        self.key(code, false);
    }

    /// Presses or releases the key with the Linux input event `code`, as a user would.
    pub fn key(&mut self, code: u32, pressed: bool) {
        let time = self.input_time();
        self.input_arrived();
        let keycode = Keycode::new(code + XKB_KEYCODE_OFFSET);
        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
        self.keyboard_key(keycode, state, time);
    }

    /// Milliseconds since the compositor started, the clock of input events.
    fn input_time(&self) -> u32 {
        u32::try_from(self.nimbus.start_time.elapsed().as_millis()).unwrap_or(u32::MAX)
    }

    fn has_grabs(&self) -> bool {
        self.nimbus.pointer.is_grabbed()
            || self.nimbus.keyboard.as_ref().is_some_and(|k| k.is_grabbed())
    }

    /// Ends client grabs and popups once a lock screen is up, so it gets all input.
    pub fn break_grabs_for_lock(&mut self) {
        if !self.nimbus.is_locked() {
            return;
        }
        self.nimbus.dismiss_all_popups();
        let serial = SERIAL_COUNTER.next_serial();
        let time = u32::try_from(Duration::from(self.nimbus.clock.now()).as_millis() % (1 << 32))
            .unwrap_or(0);
        if let Some(keyboard) = self.nimbus.keyboard.clone()
            && keyboard.is_grabbed()
        {
            keyboard.unset_grab(self);
            keyboard.set_focus(self, None, serial);
        }
        let pointer = self.nimbus.pointer.clone();
        if pointer.is_grabbed() {
            pointer.unset_grab(self, serial, time);
        }
        // Moves pointer focus off client windows, onto the lock screen.
        self.pointer_moved(self.nimbus.pointer_location, time, None);
        self.apply_keyboard_focus();
    }
}

pub fn root_surface(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
        root = parent;
    }
    root
}
