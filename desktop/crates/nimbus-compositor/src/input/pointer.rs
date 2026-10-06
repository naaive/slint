// SPDX-License-Identifier: MIT

//! Pointer focus, focus on click or hover, scrolling, and interactive move and resize.

use super::constraints::Constraint;
use crate::decoration::Hit;
use crate::state::{Nimbus, State};
use crate::wm::grabs::{self, MoveGrab, ResizeGrab};
use nimbus_ipc::WindowId;
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, PointerAxisEvent,
    PointerButtonEvent, PointerMotionEvent,
};
use smithay::desktop::{Window, WindowSurfaceType, layer_map_for_output};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GrabStartData, MotionEvent, RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::shell::wlr_layer::Layer;

pub(super) const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// Logical pixels per discrete wheel step, matching common toolkits.
const WHEEL_STEP: f64 = 15.0;

/// A surface and the global position of its origin, as Smithay's pointer focus takes it.
type PointerFocus = (WlSurface, Point<f64, Logical>);

/// What's under the pointer.
enum PointerHit {
    Surface(PointerFocus),
    Decoration(WindowId, Hit),
}

impl PointerHit {
    fn surface(self) -> Option<PointerFocus> {
        match self {
            Self::Surface(focus) => Some(focus),
            Self::Decoration(..) => None,
        }
    }

    fn decoration(&self) -> Option<(WindowId, Hit)> {
        match *self {
            Self::Decoration(id, hit) => Some((id, hit)),
            Self::Surface(_) => None,
        }
    }
}

impl Nimbus {
    /// Finds the surface under `pos`, in the same order the scene is drawn.
    pub fn pointer_target(&self, pos: Point<f64, Logical>) -> Option<PointerFocus> {
        self.pointer_hit(pos).and_then(PointerHit::surface)
    }

    /// Finds the surface or decoration under `pos`, in the same order the scene is drawn.
    fn pointer_hit(&self, pos: Point<f64, Logical>) -> Option<PointerHit> {
        let output = self.output_at(pos)?;
        let output_geo = self.output_geometry(&output)?;
        let name = output.name();
        let local = pos - output_geo.loc.to_f64();
        if self.is_locked() {
            let lock = self.lock.client()?.surface(&name)?;
            return Some(PointerHit::Surface((lock.wl_surface().clone(), output_geo.loc.to_f64())));
        }

        let layer_hit = |layers: &[Layer]| {
            let map = layer_map_for_output(&output);
            layers.iter().find_map(|&layer| {
                let surface = map.layer_under(layer, local)?;
                let geo = map.layer_geometry(surface)?;
                let (hit, loc) =
                    surface.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL)?;
                Some(PointerHit::Surface((hit, (loc + geo.loc + output_geo.loc).to_f64())))
            })
        };
        let window_hit = |window: &Window| {
            let location = self.wm.space.element_location(window)? - window.geometry().loc;
            let (surface, loc) =
                window.surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)?;
            Some(PointerHit::Surface((surface, (loc + location).to_f64())))
        };
        let decoration_hit = |window: &Window| {
            let id = self.wm.find_window(window)?;
            let hit = self.wm.frame(id)?.hit(pos)?;
            Some(PointerHit::Decoration(id, hit))
        };
        layer_hit(&[Layer::Overlay])
            .or_else(|| self.wm.fullscreen_on(&name).and_then(|w| window_hit(&w.window)))
            .or_else(|| layer_hit(&[Layer::Top]))
            .or_else(|| {
                self.wm
                    .space
                    .elements()
                    .rev()
                    .find_map(|w| window_hit(w).or_else(|| decoration_hit(w)))
            })
            .or_else(|| layer_hit(&[Layer::Bottom, Layer::Background]))
    }
}

impl State {
    pub(super) fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let delta = event.delta();
        let from = self.nimbus.pointer_location;
        let relative = RelativeMotionEvent {
            delta,
            delta_unaccel: event.delta_unaccel(),
            utime: event.time(),
        };
        let location = match self.active_constraint() {
            Some(Constraint::Locked) => {
                let pointer = self.nimbus.pointer.clone();
                let focus = self.nimbus.pointer_target(from);
                pointer.relative_motion(self, focus, &relative);
                pointer.frame(self);
                return;
            }
            Some(Constraint::Confined { surface, origin, region }) => {
                self.confine(from, from + delta, &surface, origin, region.as_ref())
            }
            None => from + delta,
        };
        self.pointer_moved(location, event.time_msec(), Some(relative));
    }

    pub(super) fn on_pointer_motion_absolute<B: InputBackend>(
        &mut self,
        event: B::PointerMotionAbsoluteEvent,
    ) {
        let Some(geo) =
            self.nimbus.outputs().next().cloned().and_then(|o| self.nimbus.output_geometry(&o))
        else {
            return;
        };
        let location = event.position_transformed(geo.size) + geo.loc.to_f64();
        self.pointer_moved(location, event.time_msec(), None);
    }

    pub(super) fn pointer_moved(
        &mut self,
        location: Point<f64, Logical>,
        time: u32,
        relative: Option<RelativeMotionEvent>,
    ) {
        self.nimbus.pointer_location = location;
        self.nimbus.clamp_pointer();
        let location = self.nimbus.pointer_location;
        let hit = self.nimbus.pointer_hit(location);
        self.decoration_motion(hit.as_ref().and_then(PointerHit::decoration), location);
        let focus = hit.and_then(PointerHit::surface);
        if let Some((surface, _)) = &focus {
            self.focus_follows_mouse(surface);
        }
        let pointer = self.nimbus.pointer.clone();
        let serial = SERIAL_COUNTER.next_serial();
        pointer.motion(self, focus.clone(), &MotionEvent { location, serial, time });
        if let Some(relative) = relative {
            pointer.relative_motion(self, focus, &relative);
        }
        pointer.frame(self);
        self.refresh_pointer_constraint();
        self.nimbus.queue_redraw_all();
    }

    fn focus_follows_mouse(&mut self, surface: &WlSurface) {
        if !self.nimbus.config.current().workspaces.focus_follows_mouse
            || self.nimbus.pointer.is_grabbed()
        {
            return;
        }
        if let Some(id) = self.nimbus.window_for_surface(surface)
            && self.nimbus.wm.focused() != Some(id)
        {
            self.nimbus.layer_focus = None;
            self.nimbus.wm.focus_with(Some(id), false);
            self.nimbus.arrange();
        }
    }

    pub(super) fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        self.pointer_button(event.button_code(), event.state(), event.time_msec());
    }

    pub(super) fn pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.nimbus.pointer.clone();
        let location = self.nimbus.pointer_location;
        let hit = self.nimbus.pointer_hit(location);

        let decoration = hit.as_ref().and_then(PointerHit::decoration);
        if state == ButtonState::Released || !pointer.is_grabbed() {
            self.decoration_button(decoration, button, state, serial, time);
        }
        if state == ButtonState::Pressed
            && !pointer.is_grabbed()
            && let Some((surface, _)) = hit.and_then(PointerHit::surface)
        {
            let logo = self.nimbus.keyboard.as_ref().is_some_and(|k| k.modifier_state().logo);
            let window = self.nimbus.window_for_surface(&surface);
            match window {
                Some(id) if logo && button == BTN_LEFT => {
                    self.begin_move(id, GrabStartData { focus: None, button, location }, serial);
                }
                Some(id) if logo && button == BTN_RIGHT => {
                    let start_data = GrabStartData { focus: None, button, location };
                    self.begin_resize(id, start_data, serial, None);
                }
                _ => self.click_focus(&surface, window),
            }
        }
        pointer.button(self, &ButtonEvent { button, state, serial, time });
        pointer.frame(self);
    }

    /// Focuses what a touch or tablet tool pressed on, as a click would.
    pub(super) fn focus_on_press(&mut self, surface: &WlSurface) {
        let window = self.nimbus.window_for_surface(surface);
        self.click_focus(surface, window);
    }

    fn click_focus(&mut self, surface: &WlSurface, window: Option<WindowId>) {
        if let Some(id) = window {
            self.nimbus.layer_focus = None;
            self.nimbus.wm.focus(Some(id));
            self.nimbus.arrange();
            return;
        }
        let root = super::root_surface(surface);
        if self.nimbus.layer_takes_focus(&root) {
            self.nimbus.layer_focus = Some(root);
        }
    }

    pub(super) fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let source = event.source();
        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let value = event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * WHEEL_STEP / 120.0);
            if value != 0.0 {
                frame = frame
                    .relative_direction(axis, event.relative_direction(axis))
                    .value(axis, value);
                if let Some(discrete) = event.amount_v120(axis) {
                    frame = frame.v120(axis, discrete as i32);
                }
            } else if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                frame = frame.stop(axis);
            }
        }
        let pointer = self.nimbus.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Starts an interactive move requested by a client through `xdg_toplevel.move`.
    pub fn start_client_move(&mut self, surface: &WlSurface, serial: Serial) {
        if let Some((id, start_data)) = self.client_grab_start(surface, serial) {
            self.begin_move(id, start_data, serial);
        }
    }

    /// Starts an interactive resize requested by a client through `xdg_toplevel.resize`.
    pub fn start_client_resize(&mut self, surface: &WlSurface, serial: Serial, edges: ResizeEdge) {
        if let Some((id, start_data)) = self.client_grab_start(surface, serial) {
            self.begin_resize(id, start_data, serial, Some(edges));
        }
    }

    /// Validates that the client's request belongs to its current implicit pointer grab.
    fn client_grab_start(
        &self,
        surface: &WlSurface,
        serial: Serial,
    ) -> Option<(WindowId, GrabStartData<State>)> {
        let pointer = &self.nimbus.pointer;
        if !pointer.has_grab(serial) {
            return None;
        }
        let start_data = pointer.grab_start_data()?;
        let same_client = start_data
            .focus
            .as_ref()
            .is_some_and(|(focus, _)| focus.id().same_client_as(&surface.id()));
        if !same_client {
            return None;
        }
        Some((self.nimbus.wm.find_surface(surface)?, start_data))
    }

    pub(super) fn begin_move(
        &mut self,
        id: WindowId,
        start_data: GrabStartData<State>,
        serial: Serial,
    ) {
        let location = self.nimbus.pointer_location;
        let Some(rect) = self.nimbus.wm.detach_for_grab(id, location) else {
            return;
        };
        self.nimbus.wm.focus(Some(id));
        self.nimbus.arrange();
        let grab = MoveGrab { start_data, window: id, initial_location: rect.loc };
        let pointer = self.nimbus.pointer.clone();
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    pub(super) fn begin_resize(
        &mut self,
        id: WindowId,
        start_data: GrabStartData<State>,
        serial: Serial,
        edges: Option<ResizeEdge>,
    ) {
        let location = self.nimbus.pointer_location;
        let Some(rect) = self.nimbus.wm.detach_for_grab(id, location) else {
            return;
        };
        let edges = edges.unwrap_or_else(|| grabs::edges_for_point(rect, location));
        self.nimbus.wm.focus(Some(id));
        self.nimbus.arrange();
        let grab = ResizeGrab { start_data, window: id, edges, initial: rect };
        let pointer = self.nimbus.pointer.clone();
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }
}
