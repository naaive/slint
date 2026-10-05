// SPDX-License-Identifier: MIT

use super::State;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::wayland::selection::ext_data_control::{
    DataControlHandler as ExtDataControlHandler, DataControlState as ExtDataControlState,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
};
use smithay::wayland::selection::wlr_data_control::{
    DataControlHandler as WlrDataControlHandler, DataControlState as WlrDataControlState,
};
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{
    delegate_cursor_shape, delegate_data_control as delegate_wlr_data_control,
    delegate_data_device, delegate_ext_data_control, delegate_keyboard_shortcuts_inhibit,
    delegate_primary_selection, delegate_seat,
};

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.nimbus.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.nimbus.display_handle;
        let client = focused.and_then(|surface| dh.get_client(surface.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.nimbus.cursor_status = image;
        self.nimbus.queue_redraw_all();
    }
}

impl TabletSeatHandler for State {}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.nimbus.data_device_state
    }
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.nimbus.dnd_icon = icon;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.nimbus.dnd_icon = None;
        self.nimbus.queue_redraw_all();
    }
}

impl ServerDndGrabHandler for State {}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.nimbus.primary_selection_state
    }
}

impl ExtDataControlHandler for State {
    fn data_control_state(&self) -> &ExtDataControlState {
        &self.nimbus.ext_data_control_state
    }
}

impl WlrDataControlHandler for State {
    fn data_control_state(&self) -> &WlrDataControlState {
        &self.nimbus.wlr_data_control_state
    }
}

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.nimbus.shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Remote desktops and VMs ask for this; VT switching and the emergency exit stay available regardless.
        inhibitor.activate();
    }
}

delegate_seat!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_ext_data_control!(State);
delegate_wlr_data_control!(State);
delegate_cursor_shape!(State);
delegate_keyboard_shortcuts_inhibit!(State);
