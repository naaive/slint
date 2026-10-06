// SPDX-License-Identifier: MIT

//! Drawing tablets, through `tablet-v2`; the pointer follows the tool.

use crate::state::{Nimbus, State};
use smithay::backend::input::{
    AbsolutePositionEvent, Device, Event, InputBackend, ProximityState, TabletToolButtonEvent,
    TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent, TabletToolTipState,
};
use smithay::input::pointer::MotionEvent;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletSeatTrait};

impl Nimbus {
    /// Maps a tool position onto the whole desktop, spanning every output.
    fn tablet_location<B: InputBackend>(
        &self,
        event: &impl AbsolutePositionEvent<B>,
    ) -> Option<Point<f64, Logical>> {
        let area =
            self.outputs().filter_map(|o| self.output_geometry(o)).reduce(|a, b| a.merge(b))?;
        Some(event.position_transformed(area.size) + area.loc.to_f64())
    }
}

impl State {
    pub(super) fn add_tablet<D: Device>(&mut self, device: &D) {
        let dh = self.nimbus.display_handle.clone();
        self.nimbus.seat.tablet_seat().add_tablet::<State>(&dh, &TabletDescriptor::from(device));
    }

    pub(super) fn remove_tablet<D: Device>(&mut self, device: &D) {
        let tablet_seat = self.nimbus.seat.tablet_seat();
        tablet_seat.remove_tablet(&TabletDescriptor::from(device));
        if tablet_seat.count_tablets() == 0 {
            tablet_seat.clear_tools();
        }
    }

    /// Moves the pointer to the tool's position, and returns that position.
    fn tablet_tool_moved<B: InputBackend, E: TabletToolEvent<B> + Event<B>>(
        &mut self,
        event: &E,
    ) -> Option<Point<f64, Logical>> {
        let location = self.nimbus.tablet_location(event)?;
        self.nimbus.pointer_location = location;
        let focus = self.nimbus.pointer_target(location);
        let pointer = self.nimbus.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        pointer.motion(self, focus, &MotionEvent { location, serial, time: event.time_msec() });
        pointer.frame(self);
        self.nimbus.queue_redraw_all();
        Some(location)
    }

    pub(super) fn on_tablet_tool_axis<B: InputBackend>(&mut self, event: B::TabletToolAxisEvent) {
        let Some(location) = self.tablet_tool_moved(&event) else {
            return;
        };
        let tablet_seat = self.nimbus.seat.tablet_seat();
        let tablet = tablet_seat.get_tablet(&TabletDescriptor::from(&event.device()));
        let (Some(tablet), Some(tool)) = (tablet, tablet_seat.get_tool(&event.tool())) else {
            return;
        };
        if event.pressure_has_changed() {
            tool.pressure(event.pressure());
        }
        if event.distance_has_changed() {
            tool.distance(event.distance());
        }
        if event.tilt_has_changed() {
            tool.tilt(event.tilt());
        }
        if event.slider_has_changed() {
            tool.slider_position(event.slider_position());
        }
        if event.rotation_has_changed() {
            tool.rotation(event.rotation());
        }
        if event.wheel_has_changed() {
            tool.wheel(event.wheel_delta(), event.wheel_delta_discrete());
        }
        let focus = self.nimbus.pointer_target(location);
        tool.motion(location, focus, &tablet, SERIAL_COUNTER.next_serial(), event.time_msec());
    }

    pub(super) fn on_tablet_tool_proximity<B: InputBackend>(
        &mut self,
        event: B::TabletToolProximityEvent,
    ) {
        let dh = self.nimbus.display_handle.clone();
        let tablet_seat = self.nimbus.seat.tablet_seat();
        let tool = tablet_seat.add_tool::<State>(self, &dh, &event.tool());
        let Some(location) = self.tablet_tool_moved(&event) else {
            return;
        };
        let Some(tablet) = tablet_seat.get_tablet(&TabletDescriptor::from(&event.device())) else {
            return;
        };
        match event.state() {
            ProximityState::In => {
                if let Some(focus) = self.nimbus.pointer_target(location) {
                    let serial = SERIAL_COUNTER.next_serial();
                    tool.proximity_in(location, focus, &tablet, serial, event.time_msec());
                }
            }
            ProximityState::Out => tool.proximity_out(event.time_msec()),
        }
    }

    pub(super) fn on_tablet_tool_tip<B: InputBackend>(&mut self, event: B::TabletToolTipEvent) {
        let Some(tool) = self.nimbus.seat.tablet_seat().get_tool(&event.tool()) else {
            return;
        };
        match event.tip_state() {
            TabletToolTipState::Down => {
                if let Some((surface, _)) = self.nimbus.pointer_target(self.nimbus.pointer_location)
                {
                    self.focus_on_press(&surface);
                }
                tool.tip_down(SERIAL_COUNTER.next_serial(), event.time_msec());
            }
            TabletToolTipState::Up => tool.tip_up(event.time_msec()),
        }
    }

    pub(super) fn on_tablet_tool_button<B: InputBackend>(
        &mut self,
        event: B::TabletToolButtonEvent,
    ) {
        if let Some(tool) = self.nimbus.seat.tablet_seat().get_tool(&event.tool()) {
            let serial = SERIAL_COUNTER.next_serial();
            tool.button(event.button(), event.button_state(), serial, event.time_msec());
        }
    }
}
