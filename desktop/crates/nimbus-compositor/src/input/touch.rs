// SPDX-License-Identifier: MIT

//! Touchscreens, through `wl_touch`.

use crate::state::{Nimbus, State};
use smithay::backend::input::{AbsolutePositionEvent, Event, InputBackend, TouchEvent};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::output::Output;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

/// Connector name prefixes of built-in panels, the usual home of a touchscreen.
const BUILT_IN_CONNECTORS: [&str; 3] = ["eDP", "LVDS", "DSI"];

impl Nimbus {
    /// The output a touchscreen covers: a built-in panel, or else the first output.
    fn touch_output(&self) -> Option<Output> {
        self.outputs()
            .find(|o| BUILT_IN_CONNECTORS.iter().any(|prefix| o.name().starts_with(prefix)))
            .or_else(|| self.outputs().next())
            .cloned()
    }

    /// Maps a touch position onto its output; the screen turns with the output, and its touches with it.
    fn touch_location<B: InputBackend>(
        &self,
        event: &impl AbsolutePositionEvent<B>,
    ) -> Option<Point<f64, Logical>> {
        let output = self.touch_output()?;
        let geo = self.output_geometry(&output)?;
        let transform = output.current_transform();
        let size = transform.invert().transform_size(geo.size);
        let local = transform.transform_point_in(event.position_transformed(size), &size.to_f64());
        Some(local + geo.loc.to_f64())
    }
}

impl State {
    pub(super) fn on_touch_down<B: InputBackend>(&mut self, event: B::TouchDownEvent) {
        let Some(touch) = self.nimbus.seat.get_touch() else {
            return;
        };
        let Some(location) = self.nimbus.touch_location(&event) else {
            return;
        };
        let focus = self.nimbus.pointer_target(location);
        if let Some((surface, _)) = &focus {
            self.focus_on_press(surface);
        }
        let serial = SERIAL_COUNTER.next_serial();
        let down = DownEvent { slot: event.slot(), location, serial, time: event.time_msec() };
        touch.down(self, focus, &down);
    }

    pub(super) fn on_touch_motion<B: InputBackend>(&mut self, event: B::TouchMotionEvent) {
        let Some(touch) = self.nimbus.seat.get_touch() else {
            return;
        };
        let Some(location) = self.nimbus.touch_location(&event) else {
            return;
        };
        let focus = self.nimbus.pointer_target(location);
        let motion = MotionEvent { slot: event.slot(), location, time: event.time_msec() };
        touch.motion(self, focus, &motion);
    }

    pub(super) fn on_touch_up<B: InputBackend>(&mut self, event: B::TouchUpEvent) {
        if let Some(touch) = self.nimbus.seat.get_touch() {
            let serial = SERIAL_COUNTER.next_serial();
            touch.up(self, &UpEvent { slot: event.slot(), serial, time: event.time_msec() });
        }
    }

    pub(super) fn on_touch_frame(&mut self) {
        if let Some(touch) = self.nimbus.seat.get_touch() {
            touch.frame(self);
        }
    }

    pub(super) fn on_touch_cancel(&mut self) {
        if let Some(touch) = self.nimbus.seat.get_touch() {
            touch.cancel(self);
        }
    }
}
