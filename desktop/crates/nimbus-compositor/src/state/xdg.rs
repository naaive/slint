// SPDX-License-Identifier: MIT

use super::{Nimbus, State};
use crate::wm::WindowMode;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window, WindowSurfaceType,
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output,
};
use smithay::input::Seat;
use smithay::input::pointer::Focus;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::utils::{Logical, Rectangle, Serial};
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState};
use smithay::{delegate_xdg_decoration, delegate_xdg_shell};

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.nimbus.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Nimbus draws no title bars, so clients decorate themselves.
        surface
            .with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ClientSide));
        self.nimbus.wm.add(Window::new_wayland_window(surface));
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| state.geometry = positioner.get_geometry());
        self.nimbus.unconstrain_popup(&surface);
        if let Err(err) = self.nimbus.popups.track_popup(PopupKind::from(surface)) {
            tracing::debug!("popup vanished before it was tracked: {err}");
        }
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: WlSeat, serial: Serial) {
        if Seat::<State>::from_resource(&seat).is_some_and(|s| s == self.nimbus.seat) {
            self.start_client_move(surface.wl_surface(), serial);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: WlSeat,
        serial: Serial,
        edges: ResizeEdge,
    ) {
        if Seat::<State>::from_resource(&seat).is_some_and(|s| s == self.nimbus.seat) {
            self.start_client_resize(surface.wl_surface(), serial, edges);
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let is_window = self.nimbus.wm.find_surface(&root).is_some();
        let is_layer = self.nimbus.outputs().any(|o| {
            layer_map_for_output(o).layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).is_some()
        });
        if !is_window && !is_layer {
            return;
        }
        let Ok(mut grab) = self.nimbus.popups.grab_popup(root, kind, &seat, serial) else {
            return;
        };
        if self.nimbus.is_locked() {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !super::ime::is_input_method_grab(&keyboard)
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            self.nimbus.remember_input_method_grab(&keyboard);
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
            self.nimbus.popup_grab = Some(grab.clone());
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.nimbus.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.request_window_mode(&surface, WindowMode::Maximized);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.request_window_mode(&surface, WindowMode::Normal);
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        if let Some(name) = output.as_ref().and_then(Output::from_resource).map(|o| o.name())
            && let Some(id) = self.nimbus.wm.find_surface(surface.wl_surface())
            && let Some(w) = self.nimbus.wm.get_mut(id)
        {
            w.output = Some(name);
        }
        self.request_window_mode(&surface, WindowMode::Fullscreen);
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.request_window_mode(&surface, WindowMode::Normal);
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.nimbus.wm.find_surface(surface.wl_surface()) {
            self.nimbus.wm.set_minimized(id, true);
            self.nimbus.arrange();
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.nimbus.wm.find_surface(surface.wl_surface())
            && let Some(removed) = self.nimbus.wm.remove(id)
        {
            if let Some(handle) = removed.foreign {
                self.nimbus.foreign_toplevel_state.remove_toplevel(&handle);
            }
            self.nimbus.arrange();
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.nimbus.wm.find_surface(surface.wl_surface()) {
            self.nimbus.wm.refresh_metadata(id);
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.nimbus.wm.find_surface(surface.wl_surface()) {
            self.nimbus.wm.refresh_metadata(id);
        }
    }

    fn parent_changed(&mut self, _surface: ToplevelSurface) {
        self.nimbus.arrange();
    }
}

impl State {
    fn request_window_mode(&mut self, surface: &ToplevelSurface, mode: WindowMode) {
        if let Some(id) = self.nimbus.wm.find_surface(surface.wl_surface()) {
            self.nimbus.wm.set_mode(id, mode);
            self.nimbus.arrange();
        }
        // The protocol requires a configure in reply, even when nothing changed.
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }
}

impl Nimbus {
    /// Keeps a popup inside the output its parent is on, flipping or sliding it as its positioner allows.
    pub fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let popup_offset = get_popup_toplevel_coords(&kind);
        if let Some(window_geo) = self
            .wm
            .find_surface(&root)
            .and_then(|id| self.wm.get(id))
            .and_then(|w| self.wm.space.element_geometry(&w.window))
        {
            let outputs: Vec<Rectangle<i32, Logical>> =
                self.outputs().filter_map(|o| self.wm.space.output_geometry(o)).collect();
            let Some(output_geo) = best_output(&outputs, window_geo) else {
                return;
            };
            let mut target = output_geo;
            target.loc -= popup_offset;
            target.loc -= window_geo.loc;
            popup.with_pending_state(|state| {
                state.geometry = state.positioner.get_unconstrained_geometry(target);
            });
            return;
        }
        for output in self.outputs() {
            let map = layer_map_for_output(output);
            let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL) else {
                continue;
            };
            let Some(layer_geo) = map.layer_geometry(layer) else {
                return;
            };
            let Some(output_geo) = self.wm.space.output_geometry(output) else {
                return;
            };
            let mut target = Rectangle::from_size(output_geo.size);
            target.loc -= layer_geo.loc;
            target.loc -= popup_offset;
            popup.with_pending_state(|state| {
                state.geometry = state.positioner.get_unconstrained_geometry(target);
            });
            return;
        }
    }
}

/// The output that shows most of `window`, or the first one when it's on none.
fn best_output(
    outputs: &[Rectangle<i32, Logical>],
    window: Rectangle<i32, Logical>,
) -> Option<Rectangle<i32, Logical>> {
    outputs
        .iter()
        .filter_map(|g| {
            g.intersection(window).map(|i| (*g, i64::from(i.size.w) * i64::from(i.size.h)))
        })
        .max_by_key(|(_, area)| *area)
        .map(|(g, _)| g)
        .or_else(|| outputs.first().copied())
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel
            .with_pending_state(|state| state.decoration_mode = Some(DecorationMode::ClientSide));
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.new_decoration(toplevel.clone());
        if toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        XdgDecorationHandler::request_mode(self, toplevel, DecorationMode::ClientSide);
    }
}

delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_unconstrain_to_the_output_with_most_of_the_window() {
        let a = Rectangle::new((0, 0).into(), (1920, 1080).into());
        let b = Rectangle::new((1920, 0).into(), (1920, 1080).into());
        let window = Rectangle::new((1820, 100).into(), (800, 600).into());
        assert_eq!(best_output(&[a, b], window), Some(b));
        let off_screen = Rectangle::new((-900, -900).into(), (100, 100).into());
        assert_eq!(best_output(&[a, b], off_screen), Some(a));
    }
}
