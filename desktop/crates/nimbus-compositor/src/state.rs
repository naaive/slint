// SPDX-License-Identifier: MIT

//! The compositor state and its Smithay protocol handlers.

mod compositor;
mod layer;
mod protocols;
mod seat;
mod xdg;

use crate::backend::Backend;
use crate::config::ConfigManager;
use crate::cursor::CursorThemeManager;
use crate::ipc::IpcServer;
use crate::keybindings::Bindings;
use crate::process::Children;
use crate::render::Wallpaper;
use crate::shell_host::ShellHost;
use crate::wm::layout::{self, Rect};
use crate::wm::{OutputArea, Wm};
use nimbus_ipc::{CompositorState as IpcState, Event, OutputInfo, WindowInfo};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::{PopupManager, WindowSurfaceType, layer_map_for_output};
use smithay::input::keyboard::{KeyboardHandle, Keycode, XkbConfig};
use smithay::input::pointer::{CursorImageStatus, PointerHandle};
use smithay::input::{Seat, SeatState};
use smithay::output::Output;
use smithay::reexports::calloop::{LoopHandle, LoopSignal};
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, GlobalId,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::utils::{Clock, Logical, Monotonic, Point};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufState};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelListState;
use smithay::wayland::fractional_scale::FractionalScaleManagerState;
use smithay::wayland::idle_inhibit::IdleInhibitManagerState;
use smithay::wayland::idle_notify::IdleNotifierState;
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState;
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::ext_data_control::DataControlState as ExtDataControlState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::selection::wlr_data_control::DataControlState as WlrDataControlState;
use smithay::wayland::session_lock::{LockSurface, SessionLockManagerState, SessionLocker};
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer, WlrLayerShellState};
use smithay::wayland::shell::xdg::XdgShellState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shm::ShmState;
use smithay::wayland::single_pixel_buffer::SinglePixelBufferState;
use smithay::wayland::viewporter::ViewporterState;
use smithay::wayland::xdg_activation::XdgActivationState;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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

/// ext-session-lock state.
#[derive(Default)]
pub enum SessionLock {
    #[default]
    Unlocked,
    /// Locked once every output rendered a frame without client content.
    Pending(SessionLocker),
    Locked,
}

/// Where keyboard input goes.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyboardTarget {
    /// The in-process shell, which takes keys through Slint.
    Shell,
    Surface(WlSurface),
    None,
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
    pub session_lock_state: SessionLockManagerState,
    pub shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub foreign_toplevel_state: ForeignToplevelListState,
    pub popups: PopupManager,

    pub seat: Seat<State>,
    pub keyboard: Option<KeyboardHandle<State>>,
    pub pointer: PointerHandle<State>,
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,
    pub cursor_theme: CursorThemeManager,
    pub dnd_icon: Option<WlSurface>,
    /// Keys whose press triggered a compositor action; their releases don't reach clients.
    pub suppressed_keys: HashSet<Keycode>,
    /// Keys whose press went to the shell; their releases go there too.
    pub shell_keys: HashSet<Keycode>,
    /// The layer surface that took keyboard focus on click.
    pub layer_focus: Option<WlSurface>,
    pub input: crate::input::InputState,

    pub wm: Wm,
    pub output_globals: HashMap<String, GlobalId>,
    pub wallpaper: Wallpaper,
    pub config: ConfigManager,
    pub bindings: Bindings,
    pub ipc: IpcServer,
    pub shell: Option<ShellHost>,
    pub children: Children,
    pub session_lock: SessionLock,
    /// The ext-session-lock that is pending or active.
    pub lock_owner: Option<ExtSessionLockV1>,
    pub lock_surfaces: HashMap<String, LockSurface>,
    pub idle_inhibitors: HashSet<WlSurface>,
    pub last_activity: Instant,
    pub pending_redraws: HashSet<String>,

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
}

impl Nimbus {
    pub fn new(init: NimbusInit) -> Self {
        let NimbusInit { display_handle: dh, loop_handle, loop_signal, seat_name, config, ipc } =
            init;
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
            session_lock_state: SessionLockManagerState::new::<State, _>(&dh, |_| true),
            shortcuts_inhibit_state: KeyboardShortcutsInhibitState::new::<State>(&dh),
            foreign_toplevel_state: ForeignToplevelListState::new::<State>(&dh),
            popups: PopupManager::default(),
            seat_state,
            seat,
            keyboard,
            pointer,
            pointer_location: Point::from((0.0, 0.0)),
            cursor_status: CursorImageStatus::default_named(),
            cursor_theme: CursorThemeManager::from_env(),
            dnd_icon: None,
            suppressed_keys: HashSet::new(),
            shell_keys: HashSet::new(),
            layer_focus: None,
            input: crate::input::InputState::default(),
            wm,
            output_globals: HashMap::new(),
            wallpaper,
            config,
            bindings,
            ipc,
            shell: None,
            children: Children::default(),
            session_lock: SessionLock::Unlocked,
            lock_owner: None,
            lock_surfaces: HashMap::new(),
            idle_inhibitors: HashSet::new(),
            last_activity: Instant::now(),
            pending_redraws: HashSet::new(),
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

    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.wm.space.outputs()
    }

    pub fn output_by_name(&self, name: &str) -> Option<Output> {
        self.outputs().find(|o| o.name() == name).cloned()
    }

    /// Adds an output to the right of the existing ones and advertises it to clients.
    pub fn add_output(&mut self, output: Output) {
        let x = self
            .outputs()
            .filter_map(|o| self.wm.space.output_geometry(o))
            .map(|g| g.loc.x + g.size.w)
            .max()
            .unwrap_or(0);
        let scale = self.config.current().appearance.scale;
        if scale.is_finite() && scale > 0.0 {
            output.change_current_state(
                None,
                None,
                Some(smithay::output::Scale::Fractional(scale)),
                Some((x, 0).into()),
            );
        } else {
            output.change_current_state(None, None, None, Some((x, 0).into()));
        }
        let global = output.create_global::<State>(&self.display_handle);
        self.output_globals.insert(output.name(), global);
        self.wm.space.map_output(&output, (x, 0));
        if self.pointer_location == Point::from((0.0, 0.0))
            && let Some(geo) = self.wm.space.output_geometry(&output)
        {
            self.pointer_location = layout::center(geo).into();
        }
        let wm_state = self.wm_state();
        if let Some(shell) = self.shell.as_mut() {
            shell.add_output(&output, self.config.current(), &wm_state);
        }
        tracing::info!(name = %output.name(), "output added");
        self.outputs_changed();
    }

    pub fn remove_output(&mut self, output: &Output) {
        self.wm.space.unmap_output(output);
        if let Some(global) = self.output_globals.remove(&output.name()) {
            self.display_handle.remove_global::<State>(global);
        }
        {
            let mut map = layer_map_for_output(output);
            for layer in map.layers().cloned().collect::<Vec<_>>() {
                layer.layer_surface().send_close();
                map.unmap_layer(&layer);
            }
        }
        self.lock_surfaces.remove(&output.name());
        self.pending_redraws.remove(&output.name());
        if let Some(shell) = self.shell.as_mut() {
            shell.remove_output(&output.name());
        }
        tracing::info!(name = %output.name(), "output removed");
        self.outputs_changed();
    }

    /// Re-arranges after an output was added, removed, or changed its mode or scale.
    pub fn outputs_changed(&mut self) {
        // Close gaps between outputs left by a removed one.
        let mut x = 0;
        let outputs: Vec<Output> = self.outputs().cloned().collect();
        let mut moved = Vec::new();
        for output in &outputs {
            let Some(geo) = self.wm.space.output_geometry(output) else {
                continue;
            };
            if geo.loc != Point::from((x, 0)) {
                output.change_current_state(None, None, None, Some((x, 0).into()));
                self.wm.space.map_output(output, (x, 0));
                moved.push((output.name(), Point::from((x, 0)) - geo.loc));
            }
            x += geo.size.w;
            layer_map_for_output(output).arrange();
        }
        let areas = self.output_areas();
        self.wm.reassign_outputs(&areas, &moved);
        self.clamp_pointer();
        self.arrange();
        if let Some(shell) = self.shell.as_mut() {
            shell.outputs_changed(&outputs);
        }
        self.events.push(Event::OutputsChanged { outputs: self.output_infos() });
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
                let mut usable = layout::intersect(geometry, zone);
                if let Some(ex) = self.shell.as_ref().and_then(|s| s.exclusive_zone(&output.name()))
                {
                    let px = |v: f32| v.max(0.0).round() as i32;
                    usable =
                        layout::inset(usable, px(ex.top), px(ex.right), px(ex.bottom), px(ex.left));
                }
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

    pub fn is_session_locked(&self) -> bool {
        !matches!(self.session_lock, SessionLock::Unlocked)
    }

    pub fn is_shell_locked(&self) -> bool {
        self.shell.as_ref().is_some_and(ShellHost::is_locked)
    }

    /// Where keyboard input should go, by priority: lock screens, the shell, exclusive layer surfaces, windows.
    pub fn keyboard_target(&self) -> KeyboardTarget {
        if self.is_session_locked() {
            let output = self.active_output().map(|o| o.name());
            let surface = output
                .and_then(|name| self.lock_surfaces.get(&name))
                .or_else(|| self.lock_surfaces.values().next())
                .map(|s| s.wl_surface().clone());
            return surface.map_or(KeyboardTarget::None, KeyboardTarget::Surface);
        }
        if self.is_shell_locked() || self.shell.as_ref().is_some_and(ShellHost::wants_keyboard) {
            return KeyboardTarget::Shell;
        }
        for output in self.outputs() {
            let map = layer_map_for_output(output);
            for layer in [Layer::Overlay, Layer::Top] {
                if let Some(surface) = map.layers_on(layer).find(|l| {
                    l.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                        && is_mapped(l.wl_surface())
                }) {
                    return KeyboardTarget::Surface(surface.wl_surface().clone());
                }
            }
        }
        if let Some(surface) = self.layer_focus.as_ref().filter(|s| s.is_alive() && is_mapped(s)) {
            return KeyboardTarget::Surface(surface.clone());
        }
        self.wm
            .focused_window()
            .and_then(|w| w.wl_surface())
            .map_or(KeyboardTarget::None, KeyboardTarget::Surface)
    }

    /// Confirms a pending ext-session-lock once every output rendered without client content.
    pub fn confirm_session_lock(&mut self) {
        let live: HashSet<String> = self.outputs().map(|o| o.name()).collect();
        self.pending_redraws.retain(|name| live.contains(name));
        if matches!(self.session_lock, SessionLock::Pending(_))
            && self.pending_redraws.is_empty()
            && let SessionLock::Pending(locker) =
                std::mem::replace(&mut self.session_lock, SessionLock::Locked)
        {
            locker.lock();
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
    /// Runs after every event-loop dispatch: shell work, focus, events, rendering, and flushing clients.
    pub fn post_dispatch(&mut self) {
        self.process_shell();
        self.nimbus.wm.space.refresh();
        self.nimbus.popups.cleanup();
        for output in self.nimbus.outputs().cloned().collect::<Vec<_>>() {
            layer_map_for_output(&output).cleanup();
        }
        self.nimbus.foreign_toplevel_state.cleanup_closed_handles();
        self.apply_keyboard_focus();
        let events = self.nimbus.take_events();
        if !events.is_empty() {
            self.nimbus.ipc.broadcast(&events);
            if let Some(shell) = self.nimbus.shell.as_ref() {
                shell.handle_compositor_events(&events);
            }
            // Draws what the events changed, and runs any actions they caused, in this frame.
            self.process_shell();
        }
        self.backend.render(&mut self.nimbus);
        self.nimbus.confirm_session_lock();
        self.nimbus.ipc.flush_all();
        if let Err(err) = self.nimbus.display_handle.flush_clients() {
            tracing::warn!("flushing Wayland clients failed: {err}");
        }
    }

    /// Applies [`Nimbus::keyboard_target`] to the seat, unless a popup grab owns the keyboard outside a lock screen.
    pub fn apply_keyboard_focus(&mut self) {
        let Some(keyboard) = self.nimbus.keyboard.clone() else {
            return;
        };
        if keyboard.is_grabbed() {
            if !self.nimbus.is_locked() {
                return;
            }
            keyboard.unset_grab(self);
        }
        let target = match self.nimbus.keyboard_target() {
            KeyboardTarget::Surface(surface) => Some(surface),
            KeyboardTarget::Shell | KeyboardTarget::None => None,
        };
        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, smithay::utils::SERIAL_COUNTER.next_serial());
        }
    }

    /// The idle time after which the session locks, or `None` when automatic locking is off.
    pub fn lock_timeout(&self) -> Option<Duration> {
        let minutes = self.nimbus.config.current().power.lock_after_minutes;
        (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
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
