// SPDX-License-Identifier: MIT

//! The compositor state and its Smithay protocol handlers.

mod compositor;
mod ime;
mod layer;
mod protocols;
mod seat;
mod xdg;

use crate::backend::Backend;
use crate::capture::CaptureState;
use crate::config::ConfigManager;
use crate::cursor::CursorThemeManager;
use crate::ipc::IpcServer;
use crate::keybindings::Bindings;
use crate::lock::SessionLock;
use crate::lock_marker::LockMarker;
use crate::outputs::OutputManagementState;
use crate::process::Children;
use crate::render::Wallpaper;
use crate::wm::layout::{self, Rect};
use crate::wm::{OutputArea, Wm};
use nimbus_ipc::{CompositorState as IpcState, Event, OutputInfo, ShellCommand, WindowInfo};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::{PopupGrab, PopupManager, WindowSurfaceType, layer_map_for_output};
use smithay::input::keyboard::{KeyboardHandle, Keycode, XkbConfig};
use smithay::input::pointer::{CursorImageStatus, PointerHandle};
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::calloop::{LoopHandle, LoopSignal};
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, GlobalId,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::utils::{Clock, Logical, Monotonic, Point, Size};
use smithay::wayland::compositor::{
    CompositorClientState, CompositorState, TraversalAction, send_surface_state,
    with_surface_tree_downward,
};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelListState;
use smithay::wayland::fractional_scale::{FractionalScaleManagerState, with_fractional_scale};
use smithay::wayland::idle_inhibit::IdleInhibitManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::input_method::{InputMethodKeyboardGrab, InputMethodManagerState};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::ext_data_control::DataControlState as ExtDataControlState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::selection::wlr_data_control::DataControlState as WlrDataControlState;
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer, WlrLayerShellState};
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::single_pixel_buffer::SinglePixelBufferState;
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::XdgActivationState;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// The calloop data: the backend and everything else, split so backends can borrow both.
pub struct State {
    pub backend: Backend,
    pub nimbus: Nimbus,
}

pub struct Nimbus {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub clock: Clock<Monotonic>,
    pub start_time: Instant,
    pub running: bool,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    /// Clipboard managers and command-line clipboard tools use data control instead of keyboard focus.
    pub ext_data_control_state: ExtDataControlState,
    pub wlr_data_control_state: WlrDataControlState,
    pub xdg_activation_state: XdgActivationState,
    _presentation_state: PresentationState,
    _viewporter_state: ViewporterState,
    _fractional_scale_state: FractionalScaleManagerState,
    _single_pixel_buffer_state: SinglePixelBufferState,
    _cursor_shape_state: CursorShapeManagerState,
    pub idle_notifier_state: IdleNotifierState<State>,
    _idle_inhibit_state: IdleInhibitManagerState,
    pub shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub foreign_toplevel_state: ForeignToplevelListState,
    _text_input_state: TextInputManagerState,
    /// None without a seat keyboard, which smithay's input method and virtual keyboard unwrap.
    _input_method_state: Option<(InputMethodManagerState, VirtualKeyboardManagerState)>,
    pub popups: PopupManager,
    /// The latest xdg popup grab; see [`State::refresh_keyboard_grab`].
    pub popup_grab: Option<PopupGrab<State>>,
    /// The input method's keyboard grab, while it holds one; see [`State::refresh_keyboard_grab`].
    pub input_method_grab: Option<InputMethodKeyboardGrab>,

    pub seat: Seat<State>,
    pub keyboard: Option<KeyboardHandle<State>>,
    pub pointer: PointerHandle<State>,
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,
    pub cursor_theme: CursorThemeManager,
    pub dnd_icon: Option<WlSurface>,
    /// Keys whose press triggered a compositor action; their releases don't reach clients.
    pub suppressed_keys: HashSet<Keycode>,
    /// The layer surface that took keyboard focus on click.
    pub layer_focus: Option<WlSurface>,

    pub wm: Wm,
    /// Every connected display; the enabled ones are also in `wm.space`.
    heads: Vec<Output>,
    pub output_globals: HashMap<String, GlobalId>,
    pub output_management: OutputManagementState,
    pub wallpaper: Wallpaper,
    pub config: ConfigManager,
    pub bindings: Bindings,
    pub ipc: IpcServer,
    pub children: Children,
    pub lock_marker: LockMarker,
    pub lock: SessionLock,
    pub capture: CaptureState,
    pub idle_inhibitors: HashSet<WlSurface>,
    pub pending_redraws: HashSet<String>,
    /// Outputs with a queued frame that isn't on screen yet.
    pub presenting: HashSet<String>,

    /// The `locked` and `held` of the last [`Event::LockState`].
    reported_lock: (bool, bool),
    window_snapshot: Vec<WindowInfo>,
    events: Vec<Event>,
}

/// Inputs to [`Nimbus::new`] that come from the command line and environment.
pub struct NimbusInit {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub seat_name: String,
    pub config: ConfigManager,
    pub ipc: IpcServer,
    pub lock_marker: LockMarker,
    pub locked: bool,
}

impl Nimbus {
    pub fn new(init: NimbusInit) -> Self {
        let NimbusInit {
            display_handle: dh,
            loop_handle,
            loop_signal,
            seat_name,
            config,
            ipc,
            lock_marker,
            locked,
        } = init;
        let clock = Clock::<Monotonic>::new();
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, seat_name);
        let keyboard = add_keyboard(&mut seat, &config.current().input);
        let pointer = seat.add_pointer();
        let wm = Wm::new(&config.current().workspaces);
        let bindings = Bindings::from_config(&config.current().keybindings);
        let wallpaper = Wallpaper::new(config.current().appearance.wallpaper.clone());

        let primary_selection_state = PrimarySelectionState::new::<State>(&dh);
        let ext_data_control_state =
            ExtDataControlState::new::<State, _>(&dh, Some(&primary_selection_state), |_| true);
        let wlr_data_control_state =
            WlrDataControlState::new::<State, _>(&dh, Some(&primary_selection_state), |_| true);
        dh.create_global::<State, ExtSessionLockManagerV1, _>(1, ());
        let input_method_state = keyboard.is_some().then(|| {
            (
                InputMethodManagerState::new::<State, _>(&dh, |_| true),
                VirtualKeyboardManagerState::new::<State, _>(&dh, |_| true),
            )
        });
        Self {
            compositor_state: CompositorState::new::<State>(&dh),
            xdg_shell_state: XdgShellState::new::<State>(&dh),
            _xdg_decoration_state: XdgDecorationState::new::<State>(&dh),
            layer_shell_state: WlrLayerShellState::new::<State>(&dh),
            shm_state: ShmState::new::<State>(&dh, Vec::new()),
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            _output_manager_state: OutputManagerState::new_with_xdg_output::<State>(&dh),
            data_device_state: DataDeviceState::new::<State>(&dh),
            primary_selection_state,
            ext_data_control_state,
            wlr_data_control_state,
            xdg_activation_state: XdgActivationState::new::<State>(&dh),
            _presentation_state: PresentationState::new::<State>(&dh, clock.id() as u32),
            _viewporter_state: ViewporterState::new::<State>(&dh),
            _fractional_scale_state: FractionalScaleManagerState::new::<State>(&dh),
            _single_pixel_buffer_state: SinglePixelBufferState::new::<State>(&dh),
            _cursor_shape_state: CursorShapeManagerState::new::<State>(&dh),
            idle_notifier_state: IdleNotifierState::new(&dh, loop_handle.clone()),
            _idle_inhibit_state: IdleInhibitManagerState::new::<State>(&dh),
            shortcuts_inhibit_state: KeyboardShortcutsInhibitState::new::<State>(&dh),
            foreign_toplevel_state: ForeignToplevelListState::new::<State>(&dh),
            _text_input_state: TextInputManagerState::new::<State>(&dh),
            _input_method_state: input_method_state,
            popups: PopupManager::default(),
            popup_grab: None,
            input_method_grab: None,
            seat_state,
            seat,
            keyboard,
            pointer,
            pointer_location: Point::from((0.0, 0.0)),
            cursor_status: CursorImageStatus::default_named(),
            cursor_theme: CursorThemeManager::from_env(),
            dnd_icon: None,
            suppressed_keys: HashSet::new(),
            layer_focus: None,
            wm,
            heads: Vec::new(),
            output_globals: HashMap::new(),
            output_management: OutputManagementState::new(&dh),
            wallpaper,
            config,
            bindings,
            ipc,
            children: Children::default(),
            lock_marker,
            lock: if locked { SessionLock::Locked(None) } else { SessionLock::Unlocked },
            capture: CaptureState::new(&dh),
            idle_inhibitors: HashSet::new(),
            pending_redraws: HashSet::new(),
            presenting: HashSet::new(),
            reported_lock: (locked, false),
            window_snapshot: Vec::new(),
            events: Vec::new(),
            display_handle: dh,
            loop_handle,
            loop_signal,
            clock,
            start_time: Instant::now(),
            running: true,
        }
    }

    /// Every connected head, enabled or not.
    pub fn heads(&self) -> &[Output] {
        &self.heads
    }

    /// Adds a head made by [`HeadDescription::into_output`]; [`Nimbus::reconfigure_outputs`] then enables it.
    ///
    /// [`HeadDescription::into_output`]: crate::outputs::HeadDescription::into_output
    pub fn connect_head(&mut self, output: Output) {
        tracing::info!(name = %output.name(), description = %output.description(), "display connected");
        self.heads.push(output);
    }

    /// Removes a head; [`Nimbus::reconfigure_outputs`] then arranges the rest.
    pub fn disconnect_head(&mut self, output: &Output) {
        tracing::info!(name = %output.name(), "display disconnected");
        self.heads.retain(|o| o != output);
        if self.outputs().any(|o| o == output) {
            self.unmap_output(output);
            self.outputs_changed(&[]);
        }
    }

    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.wm.space.outputs()
    }

    pub fn output_by_name(&self, name: &str) -> Option<Output> {
        self.outputs().find(|o| o.name() == name).cloned()
    }

    /// Re-arranges after outputs were enabled, disabled, moved by `moved`, or changed their mode or scale.
    pub fn outputs_changed(&mut self, moved: &[(String, Point<i32, Logical>)]) {
        for output in self.outputs() {
            layer_map_for_output(output).arrange();
        }
        let areas = self.output_areas();
        self.wm.reassign_outputs(&areas, moved);
        self.clamp_pointer();
        self.sync_lock_surfaces();
        self.arrange();
        self.events.push(Event::OutputsChanged { outputs: self.output_infos() });
        self.refresh_output_management();
    }

    pub fn output_infos(&self) -> Vec<OutputInfo> {
        self.outputs()
            .map(|o| {
                let mode = o.current_mode();
                OutputInfo {
                    name: o.name(),
                    width: mode.map_or(0, |m| m.size.w),
                    height: mode.map_or(0, |m| m.size.h),
                    scale: o.current_scale().fractional_scale(),
                    refresh_mhz: mode.map_or(0, |m| u32::try_from(m.refresh).unwrap_or(0)),
                }
            })
            .collect()
    }

    /// Every output's geometry and usable area, in global logical coordinates.
    pub fn output_areas(&self) -> Vec<OutputArea> {
        self.outputs()
            .filter_map(|output| {
                let geometry = self.wm.space.output_geometry(output)?;
                let mut zone = layer_map_for_output(output).non_exclusive_zone();
                zone.loc += geometry.loc;
                let usable = layout::intersect(geometry, zone);
                Some(OutputArea { name: output.name(), geometry, usable })
            })
            .collect()
    }

    pub fn output_area_at(&self, point: Point<f64, Logical>) -> Option<OutputArea> {
        let areas = self.output_areas();
        let p = point.to_i32_floor();
        areas.iter().find(|a| a.geometry.contains(p)).cloned().or_else(|| areas.into_iter().next())
    }

    pub fn output_at(&self, point: Point<f64, Logical>) -> Option<Output> {
        self.wm.space.output_under(point).next().cloned().or_else(|| self.outputs().next().cloned())
    }

    /// The output the user is working on: the one under the pointer.
    pub fn active_output(&self) -> Option<Output> {
        self.output_at(self.pointer_location)
    }

    /// The output showing `surface`: the one its lock surface or layer surface is on,
    /// or the first one its window is on; otherwise the active one.
    pub fn output_of(&self, surface: &WlSurface) -> Option<Output> {
        let root = crate::input::root_surface(surface);
        if let Some(name) = self.lock.client().and_then(|c| c.output_of(&root)) {
            return self.output_by_name(name);
        }
        let layer_output = self.outputs().find(|o| {
            layer_map_for_output(o).layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).is_some()
        });
        let window_output = || {
            let window = &self.wm.get(self.wm.find_surface(&root)?)?.window;
            self.wm.space.outputs_for_element(window).into_iter().next()
        };
        layer_output.cloned().or_else(window_output).or_else(|| self.active_output())
    }

    /// Sizes each lock surface to its output, and sends it the output's `enter`, buffer scale, transform,
    /// and fractional scale.
    pub fn sync_lock_surfaces(&self) {
        let Some(client) = self.lock.client() else {
            return;
        };
        for output in self.outputs() {
            let (Some(surface), Some(geometry)) =
                (client.surface(&output.name()), self.output_geometry(output))
            else {
                continue;
            };
            let size = Size::<u32, Logical>::from((
                u32::try_from(geometry.size.w).unwrap_or(0),
                u32::try_from(geometry.size.h).unwrap_or(0),
            ));
            surface.configure(size);
            let scale = output.current_scale();
            let transform = output.current_transform();
            with_surface_tree_downward(
                surface.wl_surface(),
                (),
                |_, _, _| TraversalAction::DoChildren(()),
                |surface, data, _| {
                    output.enter(surface);
                    send_surface_state(surface, data, scale.integer_scale(), transform);
                    with_fractional_scale(data, |fractional| {
                        fractional.set_preferred_scale(scale.fractional_scale());
                    });
                },
                |_, _, _| true,
            );
        }
    }

    pub fn output_geometry(&self, output: &Output) -> Option<Rect> {
        self.wm.space.output_geometry(output)
    }

    /// Keeps the pointer inside the union of outputs.
    pub fn clamp_pointer(&mut self) {
        let p = self.pointer_location;
        let inside = self
            .outputs()
            .filter_map(|o| self.wm.space.output_geometry(o))
            .any(|g| g.to_f64().contains(p));
        if inside {
            return;
        }
        let nearest =
            self.outputs().filter_map(|o| self.wm.space.output_geometry(o)).min_by(|a, b| {
                let da = distance_to(*a, p);
                let db = distance_to(*b, p);
                da.total_cmp(&db)
            });
        if let Some(geo) = nearest {
            let max_x = f64::from(geo.loc.x + geo.size.w) - 1.0;
            let max_y = f64::from(geo.loc.y + geo.size.h) - 1.0;
            self.pointer_location = Point::from((
                p.x.clamp(f64::from(geo.loc.x), max_x),
                p.y.clamp(f64::from(geo.loc.y), max_y),
            ));
        }
    }

    pub fn arrange(&mut self) {
        let areas = self.output_areas();
        self.wm.arrange(&areas);
        self.queue_redraw_all();
    }

    pub fn queue_redraw(&mut self, output: &Output) {
        self.pending_redraws.insert(output.name());
    }

    pub fn queue_redraw_all(&mut self) {
        let names: Vec<String> = self.outputs().map(|o| o.name()).collect();
        self.pending_redraws.extend(names);
    }

    /// Queues a redraw of the outputs that show `surface` or any of its windows.
    pub fn queue_redraw_for_surface(&mut self, _surface: &WlSurface) {
        // Damage tracking makes unchanged outputs nearly free, so the precise output set isn't worth computing.
        self.queue_redraw_all();
    }

    pub fn wm_state(&self) -> IpcState {
        IpcState {
            windows: self.wm.infos(),
            outputs: self.output_infos(),
            workspace_count: self.wm.workspaces().count(),
            active_workspace: self.wm.workspaces().active(),
            layout: self.wm.layout(),
        }
    }

    /// Collects window changes since the last call, plus queued events.
    pub fn take_events(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.events);
        events.extend(self.wm.take_events());
        let current = self.wm.infos();
        for info in &current {
            match self.window_snapshot.iter().find(|old| old.id == info.id) {
                None => events.push(Event::WindowOpened(info.clone())),
                Some(old) if old != info => events.push(Event::WindowChanged(info.clone())),
                Some(_) => {}
            }
        }
        for old in &self.window_snapshot {
            if !current.iter().any(|info| info.id == old.id) {
                events.push(Event::WindowClosed { id: old.id });
            }
        }
        self.window_snapshot = current;
        events
    }

    pub fn is_locked(&self) -> bool {
        self.lock.is_locked()
    }

    /// Brings the lock marker and control socket subscribers in line with the lock state.
    pub fn sync_lock_state(&mut self) {
        let locked = self.is_locked();
        self.lock_marker.sync(locked);
        let held = self.lock.client().is_some();
        if self.reported_lock != (locked, held) {
            self.reported_lock = (locked, held);
            self.events.push(Event::LockState { locked, held });
        }
    }

    /// Asks the shell, through control socket subscribers, to carry out `command` on the active output.
    pub fn shell_command(&mut self, command: ShellCommand) {
        let output = self.active_output().map(|o| o.name());
        self.events.push(Event::ShellCommand { command, output });
    }

    /// Where keyboard input should go, by priority: lock screens, grabbing popups, exclusive layer surfaces, windows.
    pub fn keyboard_target(&self) -> Option<WlSurface> {
        if self.is_locked() {
            let client = self.lock.client()?;
            let output = self.active_output().map(|o| o.name());
            let surface = output
                .and_then(|name| client.surface(&name))
                .or_else(|| client.surfaces().next())?;
            return Some(surface.wl_surface().clone());
        }
        if let Some(popup) = self
            .popup_grab
            .as_ref()
            .filter(|grab| !grab.has_ended())
            .and_then(PopupGrab::current_grab)
        {
            return Some(popup);
        }
        for output in self.outputs() {
            let map = layer_map_for_output(output);
            for layer in [Layer::Overlay, Layer::Top] {
                if let Some(surface) = map.layers_on(layer).find(|l| {
                    l.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                        && is_mapped(l.wl_surface())
                }) {
                    return Some(surface.wl_surface().clone());
                }
            }
        }
        if let Some(surface) = self.layer_focus.as_ref().filter(|s| self.layer_takes_focus(s)) {
            return Some(surface.clone());
        }
        self.wm.focused_window().and_then(|w| w.wl_surface())
    }

    /// Whether `surface` is a mapped layer surface that accepts keyboard focus.
    pub fn layer_takes_focus(&self, surface: &WlSurface) -> bool {
        surface.is_alive()
            && is_mapped(surface)
            && self.outputs().any(|o| {
                layer_map_for_output(o)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some_and(|l| {
                        l.cached_state().keyboard_interactivity != KeyboardInteractivity::None
                    })
            })
    }

    /// Confirms a pending ext-session-lock once every output presented a frame without client content.
    pub fn confirm_session_lock(&mut self) {
        let live: HashSet<String> = self.outputs().map(|o| o.name()).collect();
        self.pending_redraws.retain(|name| live.contains(name));
        self.presenting.retain(|name| live.contains(name));
        if self.pending_redraws.is_empty() && self.presenting.is_empty() {
            self.lock.confirm();
        }
    }

    /// Whether a visible surface inhibits idling; the idle-inhibit protocol ignores hidden ones.
    pub fn idle_inhibited(&self) -> bool {
        self.idle_inhibitors.iter().any(|s| s.is_alive() && self.surface_visible(s))
    }

    /// Drops inhibitors of destroyed surfaces and tells ext-idle-notify clients whether idling is inhibited.
    pub fn refresh_idle_inhibit(&mut self) {
        self.idle_inhibitors.retain(|s| s.is_alive());
        let inhibited = self.idle_inhibited();
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }

    /// Whether `surface` belongs to a shown window or a layer surface on an output.
    fn surface_visible(&self, surface: &WlSurface) -> bool {
        let root = crate::input::root_surface(surface);
        if let Some(id) = self.wm.find_surface(&root) {
            return self.wm.is_visible(id);
        }
        self.outputs().any(|o| {
            layer_map_for_output(o).layer_for_surface(&root, WindowSurfaceType::TOPLEVEL).is_some()
        })
    }
}

impl State {
    /// Runs after every event-loop dispatch: focus, events, rendering, and flushing clients.
    pub fn post_dispatch(&mut self) {
        self.nimbus.wm.space.refresh();
        self.nimbus.popups.cleanup();
        for output in self.nimbus.outputs().cloned().collect::<Vec<_>>() {
            layer_map_for_output(&output).cleanup();
        }
        self.nimbus.foreign_toplevel_state.cleanup_closed_handles();
        self.apply_keyboard_focus();
        self.nimbus.sync_lock_state();
        let events = self.nimbus.take_events();
        if !events.is_empty() {
            self.nimbus.ipc.broadcast(&events);
        }
        self.backend.render(&mut self.nimbus);
        self.process_captures();
        self.nimbus.confirm_session_lock();
        self.nimbus.ipc.flush_all();
        if let Err(err) = self.nimbus.display_handle.flush_clients() {
            tracing::warn!("flushing Wayland clients failed: {err}");
        }
    }

    /// Applies [`Nimbus::keyboard_target`] to the seat, unless a popup grab owns the keyboard.
    pub fn apply_keyboard_focus(&mut self) {
        let Some(keyboard) = self.nimbus.keyboard.clone() else {
            return;
        };
        self.refresh_keyboard_grab(&keyboard);
        if keyboard.is_grabbed() && !ime::is_input_method_grab(&keyboard) {
            return;
        }
        let target = self.nimbus.keyboard_target();
        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, smithay::utils::SERIAL_COUNTER.next_serial());
        }
    }
}

/// Whether `surface` has a buffer attached; layer surfaces unmap themselves by attaching none.
pub fn is_mapped(surface: &WlSurface) -> bool {
    with_renderer_surface_state(surface, |s| s.buffer().is_some()).unwrap_or(false)
}

fn distance_to(rect: Rect, p: Point<f64, Logical>) -> f64 {
    let (cx, cy) = layout::center(rect);
    (cx - p.x).hypot(cy - p.y)
}

/// Adds the seat keyboard with the configured layout, falling back to the default keymap.
fn add_keyboard(
    seat: &mut Seat<State>,
    input: &nimbus_config::Input,
) -> Option<KeyboardHandle<State>> {
    let delay = i32::try_from(input.repeat_delay_ms).unwrap_or(300);
    let rate = i32::try_from(input.repeat_rate).unwrap_or(30);
    match seat.add_keyboard(xkb_config(input), delay, rate) {
        Ok(keyboard) => Some(keyboard),
        Err(err) => {
            tracing::warn!(
                "invalid keyboard layout '{}': {err}; using the default",
                input.keyboard_layout
            );
            match seat.add_keyboard(XkbConfig::default(), delay, rate) {
                Ok(keyboard) => Some(keyboard),
                Err(err) => {
                    tracing::error!("cannot create a keymap, keyboard input is disabled: {err}");
                    None
                }
            }
        }
    }
}

pub fn xkb_config(input: &nimbus_config::Input) -> XkbConfig<'_> {
    XkbConfig {
        layout: &input.keyboard_layout,
        variant: &input.keyboard_variant,
        options: (!input.keyboard_options.is_empty()).then(|| input.keyboard_options.clone()),
        ..XkbConfig::default()
    }
}
