// SPDX-License-Identifier: MIT

//! Input routing: compositor shortcuts, focus, and delivery to Wayland clients.

mod keyboard;
mod pointer;

use crate::state::{Nimbus, State};
use nimbus_ipc::WindowId;
use smithay::backend::input::{
    Event, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
    GestureSwipeUpdateEvent as _, InputBackend, InputEvent,
};
use smithay::desktop::{PopupManager, layer_map_for_output};
use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use std::time::Duration;

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
        self.nimbus.notify_activity();
        if self.nimbus.is_locked() && self.has_grabs() {
            self.break_grabs_for_lock();
        }
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => self.on_pointer_motion::<B>(event),
            InputEvent::PointerMotionAbsolute { event } => {
                self.on_pointer_motion_absolute::<B>(event)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            InputEvent::GestureSwipeBegin { event } => {
                let e = GestureSwipeBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_swipe_begin(self, &e);
            }
            InputEvent::GestureSwipeUpdate { event } => {
                let e = GestureSwipeUpdateEvent { time: event.time_msec(), delta: event.delta() };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_swipe_update(self, &e);
            }
            InputEvent::GestureSwipeEnd { event } => {
                let e = GestureSwipeEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_swipe_end(self, &e);
            }
            InputEvent::GesturePinchBegin { event } => {
                let e = GesturePinchBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_pinch_begin(self, &e);
            }
            InputEvent::GesturePinchUpdate { event } => {
                let e = GesturePinchUpdateEvent {
                    time: event.time_msec(),
                    delta: event.delta(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_pinch_update(self, &e);
            }
            InputEvent::GesturePinchEnd { event } => {
                let e = GesturePinchEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_pinch_end(self, &e);
            }
            InputEvent::GestureHoldBegin { event } => {
                let e = GestureHoldBeginEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    fingers: event.fingers(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_hold_begin(self, &e);
            }
            InputEvent::GestureHoldEnd { event } => {
                let e = GestureHoldEndEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: event.time_msec(),
                    cancelled: event.cancelled(),
                };
                let pointer = self.nimbus.pointer.clone();
                pointer.gesture_hold_end(self, &e);
            }
            _ => {}
        }
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
