// SPDX-License-Identifier: MIT

//! Touchpad gestures: the workspace swipe, and `pointer-gestures` for everything else.

use crate::state::State;
use nimbus_config::Action;
use smithay::backend::input::{
    Event, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as _,
    GestureSwipeUpdateEvent as _, InputBackend,
};
use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

/// How far, in touchpad units, a swipe goes before it switches workspaces.
const SWIPE_DISTANCE: f64 = 100.0;

/// A swipe the compositor took for itself, so clients never see it.
#[derive(Debug, Default)]
pub struct WorkspaceSwipe {
    travel: Point<f64, Logical>,
}

impl WorkspaceSwipe {
    pub fn update(&mut self, delta: Point<f64, Logical>) {
        self.travel += delta;
    }

    /// The action of a finished swipe: the fingers move the workspaces along, as on a touchscreen.
    pub fn finish(self, cancelled: bool) -> Option<Action> {
        let Point { x, y, .. } = self.travel;
        if cancelled || x.abs() < SWIPE_DISTANCE || x.abs() < y.abs() {
            return None;
        }
        Some(if x < 0.0 { Action::NextWorkspace } else { Action::PreviousWorkspace })
    }
}

impl State {
    pub(super) fn on_gesture_swipe_begin<B: InputBackend>(
        &mut self,
        event: B::GestureSwipeBeginEvent,
    ) {
        let fingers = self.nimbus.config.current().input.workspace_swipe_fingers;
        if fingers != 0 && event.fingers() == fingers && !self.nimbus.is_locked() {
            self.nimbus.workspace_swipe = Some(WorkspaceSwipe::default());
            return;
        }
        let e = GestureSwipeBeginEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            fingers: event.fingers(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_swipe_begin(self, &e);
    }

    pub(super) fn on_gesture_swipe_update<B: InputBackend>(
        &mut self,
        event: B::GestureSwipeUpdateEvent,
    ) {
        if let Some(swipe) = &mut self.nimbus.workspace_swipe {
            swipe.update(event.delta());
            return;
        }
        let e = GestureSwipeUpdateEvent { time: event.time_msec(), delta: event.delta() };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_swipe_update(self, &e);
    }

    pub(super) fn on_gesture_swipe_end<B: InputBackend>(&mut self, event: B::GestureSwipeEndEvent) {
        if let Some(swipe) = self.nimbus.workspace_swipe.take() {
            let action = swipe.finish(event.cancelled()).filter(|_| !self.nimbus.is_locked());
            if let Some(action) = action {
                self.run_action(action);
            }
            return;
        }
        let e = GestureSwipeEndEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            cancelled: event.cancelled(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_swipe_end(self, &e);
    }

    pub(super) fn on_gesture_pinch_begin<B: InputBackend>(
        &mut self,
        event: B::GesturePinchBeginEvent,
    ) {
        let e = GesturePinchBeginEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            fingers: event.fingers(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_pinch_begin(self, &e);
    }

    pub(super) fn on_gesture_pinch_update<B: InputBackend>(
        &mut self,
        event: B::GesturePinchUpdateEvent,
    ) {
        let e = GesturePinchUpdateEvent {
            time: event.time_msec(),
            delta: event.delta(),
            scale: event.scale(),
            rotation: event.rotation(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_pinch_update(self, &e);
    }

    pub(super) fn on_gesture_pinch_end<B: InputBackend>(&mut self, event: B::GesturePinchEndEvent) {
        let e = GesturePinchEndEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            cancelled: event.cancelled(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_pinch_end(self, &e);
    }

    pub(super) fn on_gesture_hold_begin<B: InputBackend>(
        &mut self,
        event: B::GestureHoldBeginEvent,
    ) {
        let e = GestureHoldBeginEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            fingers: event.fingers(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_hold_begin(self, &e);
    }

    pub(super) fn on_gesture_hold_end<B: InputBackend>(&mut self, event: B::GestureHoldEndEvent) {
        let e = GestureHoldEndEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: event.time_msec(),
            cancelled: event.cancelled(),
        };
        let pointer = self.nimbus.pointer.clone();
        pointer.gesture_hold_end(self, &e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swipe(deltas: &[(f64, f64)]) -> WorkspaceSwipe {
        let mut swipe = WorkspaceSwipe::default();
        for &delta in deltas {
            swipe.update(delta.into());
        }
        swipe
    }

    #[test]
    fn swiping_left_goes_to_the_next_workspace() {
        let action = swipe(&[(-60.0, 5.0), (-60.0, -5.0)]).finish(false);
        assert_eq!(action, Some(Action::NextWorkspace));
        assert_eq!(swipe(&[(120.0, 0.0)]).finish(false), Some(Action::PreviousWorkspace));
    }

    #[test]
    fn short_vertical_or_cancelled_swipes_do_nothing() {
        assert_eq!(swipe(&[(-50.0, 0.0)]).finish(false), None);
        assert_eq!(swipe(&[(-120.0, -200.0)]).finish(false), None);
        assert_eq!(swipe(&[(-300.0, 0.0)]).finish(true), None);
    }
}
