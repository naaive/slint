// SPDX-License-Identifier: MIT

//! Input routing: compositor shortcuts, the in-process shell, and Wayland clients.

use crate::keybindings::Mods;
use crate::state::{KeyboardTarget, Nimbus, State};
use crate::wm::grabs::{self, MoveGrab, ResizeGrab};
use nimbus_ipc::WindowId;
use slint::SharedString;
use slint::platform::{Key, PointerEventButton};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
    GestureEndEvent, GesturePinchUpdateEvent as _, GestureSwipeUpdateEvent as _, InputBackend,
    InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    PointerMotionEvent,
};
use smithay::desktop::{WindowSurfaceType, layer_map_for_output};
use smithay::input::keyboard::{FilterResult, Keysym, KeysymHandle, ModifiersState, keysyms};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
    GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, RelativeMotionEvent,
};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer};
use std::time::Duration;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// Logical pixels per discrete wheel step, matching common toolkits.
const WHEEL_STEP: f64 = 15.0;

/// Pointer and keyboard routing state that outlives single events.
#[derive(Default)]
pub struct InputState {
    /// The output whose shell currently has the pointer.
    shell_pointer: Option<String>,
    /// A button press went to the shell on this output; motion and release follow it there.
    shell_grab: Option<(String, u32)>,
    shell_repeat: Option<(u32, RegistrationToken)>,
}

/// What a key press does after filtering.
enum KeyAction {
    None,
    Action(nimbus_config::Action),
    SwitchVt(i32),
    EmergencyQuit,
    Shell { keycode: u32, text: SharedString, pressed: bool },
}

/// Where the pointer is.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerTarget {
    Shell { output: String, local: Point<f64, Logical> },
    Surface { surface: WlSurface, location: Point<f64, Logical> },
    None,
}

impl Nimbus {
    /// Finds what is under `pos`, in the same order the scene is drawn.
    pub fn pointer_target(&self, pos: Point<f64, Logical>) -> PointerTarget {
        if let Some((output, _)) = &self.input.shell_grab
            && let Some(geo) = self.output_by_name(output).and_then(|o| self.output_geometry(&o))
        {
            return PointerTarget::Shell { output: output.clone(), local: pos - geo.loc.to_f64() };
        }
        let Some(output) = self.output_at(pos) else {
            return PointerTarget::None;
        };
        let Some(output_geo) = self.output_geometry(&output) else {
            return PointerTarget::None;
        };
        let name = output.name();
        let local = pos - output_geo.loc.to_f64();

        if self.is_session_locked() {
            return self.lock_surfaces.get(&name).map_or(PointerTarget::None, |lock| {
                PointerTarget::Surface {
                    surface: lock.wl_surface().clone(),
                    location: output_geo.loc.to_f64(),
                }
            });
        }
        let shell = |name: &str| PointerTarget::Shell { output: name.to_owned(), local };
        if self.is_shell_locked() {
            return shell(&name);
        }

        let layer_hit = |layers: &[Layer]| -> Option<PointerTarget> {
            let map = layer_map_for_output(&output);
            layers.iter().find_map(|&layer| {
                let surface = map.layer_under(layer, local)?;
                let geo = map.layer_geometry(surface)?;
                let (hit, loc) =
                    surface.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL)?;
                Some(PointerTarget::Surface {
                    surface: hit,
                    location: (loc + geo.loc + output_geo.loc).to_f64(),
                })
            })
        };
        if let Some(hit) = layer_hit(&[Layer::Overlay]) {
            return hit;
        }

        let fullscreen = self.wm.fullscreen_on(&name).map(|w| w.window.clone());
        let shell_on_top =
            fullscreen.is_none() || self.shell.as_ref().is_some_and(|s| s.wants_keyboard());
        if shell_on_top && self.shell.as_ref().is_some_and(|s| s.accepts_pointer(&name, local)) {
            return shell(&name);
        }
        let window_hit = |window: &smithay::desktop::Window| -> Option<PointerTarget> {
            let location = self.wm.space.element_location(window)? - window.geometry().loc;
            let (surface, loc) =
                window.surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)?;
            Some(PointerTarget::Surface { surface, location: (loc + location).to_f64() })
        };
        if let Some(window) = &fullscreen
            && let Some(hit) = window_hit(window)
        {
            return hit;
        }
        if let Some(hit) = layer_hit(&[Layer::Top]) {
            return hit;
        }
        if let Some(hit) = self.wm.space.elements().rev().find_map(window_hit) {
            return hit;
        }
        layer_hit(&[Layer::Bottom, Layer::Background]).unwrap_or(PointerTarget::None)
    }

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

    pub fn notify_activity(&mut self) {
        self.last_activity = std::time::Instant::now();
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
    }
}

impl State {
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        self.nimbus.notify_activity();
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

    fn on_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        let Some(keyboard) = self.nimbus.keyboard.clone() else {
            return;
        };
        let keycode = event.key_code();
        let key_state = event.state();
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let action = keyboard.input::<KeyAction, _>(
            self,
            keycode,
            key_state,
            serial,
            time,
            |state, modifiers, handle| {
                state.filter_key(keycode.raw(), key_state, modifiers, &handle)
            },
        );
        match action {
            Some(KeyAction::Action(action)) => self.run_action(action),
            Some(KeyAction::SwitchVt(vt)) => self.backend.change_vt(vt),
            Some(KeyAction::EmergencyQuit) => {
                tracing::warn!("emergency exit requested with Ctrl+Alt+Backspace");
                self.nimbus.stop();
            }
            Some(KeyAction::Shell { keycode, text, pressed }) => {
                self.shell_key(keycode, text, pressed)
            }
            Some(KeyAction::None) | None => {}
        }
    }

    fn filter_key(
        &mut self,
        keycode: u32,
        key_state: KeyState,
        modifiers: &ModifiersState,
        handle: &KeysymHandle<'_>,
    ) -> FilterResult<KeyAction> {
        let code = smithay::input::keyboard::Keycode::new(keycode);
        if key_state == KeyState::Released {
            if self.nimbus.suppressed_keys.remove(&code) {
                return FilterResult::Intercept(KeyAction::None);
            }
            if self.nimbus.shell_keys.remove(&code) {
                let text = slint_key_text(handle.modified_sym()).unwrap_or_default();
                return FilterResult::Intercept(KeyAction::Shell { keycode, text, pressed: false });
            }
            return FilterResult::Forward;
        }

        let raw_syms = handle.raw_syms();
        let modified = handle.modified_sym();
        if let Some(vt) = vt_for_keysym(modified) {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::SwitchVt(vt));
        }
        let is_backspace = raw_syms.iter().any(|s| s.raw() == keysyms::KEY_BackSpace);
        if modified.raw() == keysyms::KEY_Terminate_Server
            || (modifiers.ctrl && modifiers.alt && is_backspace)
        {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::EmergencyQuit);
        }
        if self.nimbus.is_session_locked() {
            return FilterResult::Forward;
        }

        let target = self.nimbus.keyboard_target();
        let inhibited = matches!(target, KeyboardTarget::Surface(_))
            && self.nimbus.seat.keyboard_shortcuts_inhibited();
        let locked = self.nimbus.is_shell_locked();
        if !inhibited
            && let Some(action) =
                self.nimbus.bindings.lookup(Mods::from(modifiers), &raw_syms).cloned()
            && (!locked || allowed_while_locked(&action))
        {
            self.nimbus.suppressed_keys.insert(code);
            return FilterResult::Intercept(KeyAction::Action(action));
        }
        if target == KeyboardTarget::Shell {
            let text = slint_key_text(modified).unwrap_or_default();
            if text.is_empty() {
                return FilterResult::Intercept(KeyAction::None);
            }
            self.nimbus.shell_keys.insert(code);
            return FilterResult::Intercept(KeyAction::Shell { keycode, text, pressed: true });
        }
        FilterResult::Forward
    }

    fn shell_key(&mut self, keycode: u32, text: SharedString, pressed: bool) {
        if let Some((_, token)) = self.nimbus.input.shell_repeat.take() {
            self.nimbus.loop_handle.remove(token);
        }
        let Some(shell) = self.nimbus.shell.as_ref() else {
            return;
        };
        shell.dispatch_key(text.clone(), pressed, false);
        let is_modifier = text.chars().next().is_some_and(is_modifier_char);
        if !pressed || is_modifier {
            return;
        }
        let input = &self.nimbus.config.current().input;
        if input.repeat_rate == 0 {
            return;
        }
        let delay = Duration::from_millis(u64::from(input.repeat_delay_ms));
        let interval = Duration::from_secs_f64(1.0 / f64::from(input.repeat_rate));
        let timer = Timer::from_duration(delay);
        match self.nimbus.loop_handle.insert_source(timer, move |_, _, state| {
            if let Some(shell) = state.nimbus.shell.as_ref() {
                shell.dispatch_key(text.clone(), true, true);
            }
            TimeoutAction::ToDuration(interval)
        }) {
            Ok(token) => self.nimbus.input.shell_repeat = Some((keycode, token)),
            Err(err) => tracing::debug!("cannot start key repeat: {err}"),
        }
    }

    fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let delta = event.delta();
        let location = self.nimbus.pointer_location + delta;
        let relative = RelativeMotionEvent {
            delta,
            delta_unaccel: event.delta_unaccel(),
            utime: event.time(),
        };
        self.pointer_moved(location, event.time_msec(), Some(relative));
    }

    fn on_pointer_motion_absolute<B: InputBackend>(
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

    fn pointer_moved(
        &mut self,
        location: Point<f64, Logical>,
        time: u32,
        relative: Option<RelativeMotionEvent>,
    ) {
        self.nimbus.pointer_location = location;
        self.nimbus.clamp_pointer();
        let location = self.nimbus.pointer_location;
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.nimbus.pointer.clone();
        let target = if pointer.is_grabbed() {
            self.client_target(location)
        } else {
            self.nimbus.pointer_target(location)
        };

        let focus = match target {
            PointerTarget::Shell { output, local } => {
                self.shell_pointer_enter(&output);
                if let Some(shell) = self.nimbus.shell.as_ref() {
                    shell.pointer_moved(&output, local);
                }
                None
            }
            PointerTarget::Surface { surface, location } => {
                self.shell_pointer_leave();
                self.focus_follows_mouse(&surface);
                Some((surface, location))
            }
            PointerTarget::None => {
                self.shell_pointer_leave();
                None
            }
        };
        pointer.motion(self, focus.clone(), &MotionEvent { location, serial, time });
        if let Some(relative) = relative {
            pointer.relative_motion(self, focus, &relative);
        }
        pointer.frame(self);
        self.nimbus.queue_redraw_all();
    }

    /// The client surface under the pointer, ignoring the shell, for use while a client grab is active.
    fn client_target(&self, location: Point<f64, Logical>) -> PointerTarget {
        match self.nimbus.pointer_target(location) {
            PointerTarget::Shell { .. } => PointerTarget::None,
            other => other,
        }
    }

    fn shell_pointer_enter(&mut self, output: &str) {
        if self.nimbus.input.shell_pointer.as_deref() != Some(output) {
            self.shell_pointer_leave();
            self.nimbus.input.shell_pointer = Some(output.to_owned());
        }
    }

    fn shell_pointer_leave(&mut self) {
        if let Some(output) = self.nimbus.input.shell_pointer.take()
            && let Some(shell) = self.nimbus.shell.as_ref()
        {
            shell.pointer_exited(&output);
        }
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

    fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let button = event.button_code();
        let state = event.state();
        let time = event.time_msec();
        let pointer = self.nimbus.pointer.clone();
        let location = self.nimbus.pointer_location;

        if let Some((output, pressed)) = self.nimbus.input.shell_grab.clone() {
            let local = self.shell_local(&output, location);
            if let Some(shell) = self.nimbus.shell.as_ref() {
                shell.pointer_button(
                    &output,
                    local,
                    slint_button(button),
                    state == ButtonState::Pressed,
                );
            }
            let pressed =
                if state == ButtonState::Pressed { pressed + 1 } else { pressed.saturating_sub(1) };
            self.nimbus.input.shell_grab = (pressed > 0).then_some((output, pressed));
            return;
        }

        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            match self.nimbus.pointer_target(location) {
                PointerTarget::Shell { output, local } => {
                    if let Some(shell) = self.nimbus.shell.as_ref() {
                        shell.pointer_button(&output, local, slint_button(button), true);
                    }
                    self.nimbus.input.shell_grab = Some((output, 1));
                    return;
                }
                PointerTarget::Surface { surface, .. } => {
                    let logo =
                        self.nimbus.keyboard.as_ref().is_some_and(|k| k.modifier_state().logo);
                    let window = self.nimbus.window_for_surface(&surface);
                    if logo
                        && let Some(id) = window
                        && (button == BTN_LEFT || button == BTN_RIGHT)
                    {
                        let start_data = GrabStartData { focus: None, button, location };
                        if button == BTN_LEFT {
                            self.begin_move(id, start_data, serial);
                        } else {
                            self.begin_resize(id, start_data, serial, None);
                        }
                    } else {
                        self.click_focus(&surface, window);
                    }
                }
                PointerTarget::None => {}
            }
        }
        pointer.button(self, &ButtonEvent { button, state, serial, time });
        pointer.frame(self);
    }

    fn shell_local(&self, output: &str, location: Point<f64, Logical>) -> Point<f64, Logical> {
        let origin = self
            .nimbus
            .output_by_name(output)
            .and_then(|o| self.nimbus.output_geometry(&o))
            .map(|g| g.loc);
        location - origin.unwrap_or_default().to_f64()
    }

    fn click_focus(&mut self, surface: &WlSurface, window: Option<WindowId>) {
        if let Some(id) = window {
            self.nimbus.layer_focus = None;
            self.nimbus.wm.focus(Some(id));
            self.nimbus.arrange();
            return;
        }
        let layer_wants_focus = self.nimbus.outputs().any(|o| {
            layer_map_for_output(o).layer_for_surface(surface, WindowSurfaceType::ALL).is_some_and(
                |l| l.cached_state().keyboard_interactivity != KeyboardInteractivity::None,
            )
        });
        if layer_wants_focus {
            let root = root_surface(surface);
            self.nimbus.layer_focus = Some(root);
        }
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let source = event.source();
        let amount = |axis: Axis| {
            event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * WHEEL_STEP / 120.0)
        };
        let horizontal = amount(Axis::Horizontal);
        let vertical = amount(Axis::Vertical);
        let location = self.nimbus.pointer_location;

        let shell_target = match &self.nimbus.input.shell_grab {
            Some((output, _)) => Some((output.clone(), self.shell_local(output, location))),
            None if !self.nimbus.pointer.is_grabbed() => match self.nimbus.pointer_target(location)
            {
                PointerTarget::Shell { output, local } => Some((output, local)),
                _ => None,
            },
            None => None,
        };
        if let Some((output, local)) = shell_target {
            if let Some(shell) = self.nimbus.shell.as_ref() {
                // Wayland axes grow downward and rightward; Slint's deltas move the content.
                shell.pointer_scrolled(&output, local, -horizontal as f32, -vertical as f32);
            }
            return;
        }

        let mut frame = AxisFrame::new(event.time_msec()).source(source);
        for (axis, value) in [(Axis::Horizontal, horizontal), (Axis::Vertical, vertical)] {
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
    pub fn start_client_resize(
        &mut self,
        surface: &WlSurface,
        serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
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

    fn begin_move(&mut self, id: WindowId, start_data: GrabStartData<State>, serial: Serial) {
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

    fn begin_resize(
        &mut self,
        id: WindowId,
        start_data: GrabStartData<State>,
        serial: Serial,
        edges: Option<
            smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
        >,
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

fn root_surface(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
        root = parent;
    }
    root
}

fn vt_for_keysym(sym: Keysym) -> Option<i32> {
    let raw = sym.raw();
    (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12)
        .contains(&raw)
        .then(|| i32::try_from(raw - keysyms::KEY_XF86Switch_VT_1 + 1).unwrap_or(1))
}

/// Hardware keys keep working on the lock screen.
fn allowed_while_locked(action: &nimbus_config::Action) -> bool {
    use nimbus_config::Action::*;
    matches!(action, VolumeUp | VolumeDown | ToggleMute | BrightnessUp | BrightnessDown)
}

fn is_modifier_char(c: char) -> bool {
    [
        Key::Shift,
        Key::ShiftR,
        Key::Control,
        Key::ControlR,
        Key::Alt,
        Key::AltGr,
        Key::Meta,
        Key::MetaR,
        Key::CapsLock,
    ]
    .into_iter()
    .any(|k| char::from(k) == c)
}

pub fn slint_button(button: u32) -> PointerEventButton {
    match button {
        BTN_LEFT => PointerEventButton::Left,
        BTN_RIGHT => PointerEventButton::Right,
        BTN_MIDDLE => PointerEventButton::Middle,
        BTN_SIDE => PointerEventButton::Back,
        BTN_EXTRA => PointerEventButton::Forward,
        _ => PointerEventButton::Other,
    }
}

/// The text Slint expects for a key: a [`Key`] code for special keys, otherwise the produced character.
pub fn slint_key_text(sym: Keysym) -> Option<SharedString> {
    let key = match sym.raw() {
        keysyms::KEY_BackSpace => Key::Backspace,
        keysyms::KEY_Tab => Key::Tab,
        keysyms::KEY_ISO_Left_Tab => Key::Backtab,
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => Key::Return,
        keysyms::KEY_Escape => Key::Escape,
        keysyms::KEY_Delete | keysyms::KEY_KP_Delete => Key::Delete,
        keysyms::KEY_Shift_L => Key::Shift,
        keysyms::KEY_Shift_R => Key::ShiftR,
        keysyms::KEY_Control_L => Key::Control,
        keysyms::KEY_Control_R => Key::ControlR,
        keysyms::KEY_Alt_L | keysyms::KEY_Alt_R => Key::Alt,
        keysyms::KEY_ISO_Level3_Shift | keysyms::KEY_Mode_switch => Key::AltGr,
        keysyms::KEY_Super_L | keysyms::KEY_Meta_L => Key::Meta,
        keysyms::KEY_Super_R | keysyms::KEY_Meta_R => Key::MetaR,
        keysyms::KEY_Caps_Lock => Key::CapsLock,
        keysyms::KEY_space | keysyms::KEY_KP_Space => Key::Space,
        keysyms::KEY_Up | keysyms::KEY_KP_Up => Key::UpArrow,
        keysyms::KEY_Down | keysyms::KEY_KP_Down => Key::DownArrow,
        keysyms::KEY_Left | keysyms::KEY_KP_Left => Key::LeftArrow,
        keysyms::KEY_Right | keysyms::KEY_KP_Right => Key::RightArrow,
        keysyms::KEY_Home | keysyms::KEY_KP_Home => Key::Home,
        keysyms::KEY_End | keysyms::KEY_KP_End => Key::End,
        keysyms::KEY_Page_Up | keysyms::KEY_KP_Page_Up => Key::PageUp,
        keysyms::KEY_Page_Down | keysyms::KEY_KP_Page_Down => Key::PageDown,
        keysyms::KEY_Insert | keysyms::KEY_KP_Insert => Key::Insert,
        keysyms::KEY_Menu => Key::Menu,
        keysyms::KEY_Pause => Key::Pause,
        keysyms::KEY_Scroll_Lock => Key::ScrollLock,
        raw @ keysyms::KEY_F1..=keysyms::KEY_F24 => {
            FUNCTION_KEYS[usize::try_from(raw - keysyms::KEY_F1).unwrap_or(0)]
        }
        _ => {
            let c = sym.key_char().filter(|c| !c.is_control())?;
            return Some(SharedString::from(c.to_string().as_str()));
        }
    };
    Some(key.into())
}

const FUNCTION_KEYS: [Key; 24] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
    Key::F13,
    Key::F14,
    Key::F15,
    Key::F16,
    Key::F17,
    Key::F18,
    Key::F19,
    Key::F20,
    Key::F21,
    Key::F22,
    Key::F23,
    Key::F24,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn text(raw: u32) -> Option<SharedString> {
        slint_key_text(Keysym::new(raw))
    }

    #[test]
    fn special_keys_map_to_slint_codes() {
        assert_eq!(text(keysyms::KEY_BackSpace), Some(Key::Backspace.into()));
        assert_eq!(text(keysyms::KEY_KP_Enter), Some(Key::Return.into()));
        assert_eq!(text(keysyms::KEY_F12), Some(Key::F12.into()));
        assert_eq!(text(keysyms::KEY_Super_L), Some(Key::Meta.into()));
        assert_eq!(text(keysyms::KEY_ISO_Left_Tab), Some(Key::Backtab.into()));
    }

    #[test]
    fn printable_keys_produce_their_character() {
        assert_eq!(text(keysyms::KEY_a).as_deref(), Some("a"));
        assert_eq!(text(keysyms::KEY_A).as_deref(), Some("A"));
        assert_eq!(text(keysyms::KEY_adiaeresis).as_deref(), Some("ä"));
        assert_eq!(text(keysyms::KEY_XF86AudioMute), None);
    }

    #[test]
    fn vt_keysyms_map_to_numbers() {
        assert_eq!(vt_for_keysym(Keysym::new(keysyms::KEY_XF86Switch_VT_1)), Some(1));
        assert_eq!(vt_for_keysym(Keysym::new(keysyms::KEY_XF86Switch_VT_12)), Some(12));
        assert_eq!(vt_for_keysym(Keysym::new(keysyms::KEY_F1)), None);
    }

    #[test]
    fn buttons_map_to_slint() {
        assert_eq!(slint_button(BTN_LEFT), PointerEventButton::Left);
        assert_eq!(slint_button(BTN_EXTRA), PointerEventButton::Forward);
        assert_eq!(slint_button(0x200), PointerEventButton::Other);
    }

    #[test]
    fn modifiers_are_recognized() {
        assert!(is_modifier_char(char::from(Key::Shift)));
        assert!(!is_modifier_char('a'));
    }
}
