// SPDX-License-Identifier: MIT

//! Pointer and tablet protocols beyond `wl_seat`; `input/` routes the events they carry.

use super::State;
use smithay::backend::input::TabletToolDescriptor;
use smithay::input::pointer::{CursorImageStatus, PointerHandle};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::pointer_constraints::PointerConstraintsHandler;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{
    delegate_pointer_constraints, delegate_pointer_gestures, delegate_relative_pointer,
    delegate_tablet_manager,
};

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        self.refresh_pointer_constraint();
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        self.apply_cursor_hint(surface, location);
    }
}

impl TabletSeatHandler for State {
    fn tablet_tool_image(&mut self, _tool: &TabletToolDescriptor, image: CursorImageStatus) {
        self.nimbus.cursor_status = image;
        self.nimbus.queue_redraw_all();
    }
}

delegate_relative_pointer!(State);
delegate_pointer_gestures!(State);
delegate_pointer_constraints!(State);
delegate_tablet_manager!(State);
