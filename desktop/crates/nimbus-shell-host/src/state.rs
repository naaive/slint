// SPDX-License-Identifier: MIT

//! The shell process's state, and the work after each event-loop dispatch.

use crate::actions::Apps;
use crate::auth::AuthWorker;
use crate::config::Settings;
use crate::idle::Idle;
use crate::input::Input;
use crate::ipc::Ipc;
use crate::lock::{Lock, spawn_auth};
use crate::output::{Context, OutputShell, WmBase};
use crate::platform::Windows;
use crate::render::{Preference, Renderers};
use crate::services::SystemServices;
use crate::surface::{Scaling, SlintSurface};
use crate::text_input::TextInput;
use anyhow::anyhow;
use nimbus_ipc::{Request, Response};
use nimbus_shell::{ShellAction, ShellModel, ShellView};
use smithay_client_toolkit::activation::ActivationState;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::globals::GlobalData;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event as ChannelEvent, Sender};
use smithay_client_toolkit::reexports::calloop::{LoopHandle, LoopSignal};
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::session_lock::SessionLockState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::Shm;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;

/// How often to redraw while Slint animations run on a surface that isn't waiting for a frame callback.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);

pub struct State {
    pub conn: Connection,
    pub qh: QueueHandle<State>,
    pub loop_handle: LoopHandle<'static, State>,
    loop_signal: LoopSignal,
    /// Why the shell stops, once it does.
    exit: Option<anyhow::Result<()>>,

    pub registry: RegistryState,
    pub output_state: OutputState,
    pub seat_state: SeatState,
    pub compositor: CompositorState,
    pub shm: Shm,
    pub layer_shell: LayerShell,
    /// Popups are `xdg_popup`s; without `xdg_wm_base`, none open.
    pub wm_base: Option<WmBase>,
    pub session_lock_state: SessionLockState,
    pub activation: Option<ActivationState>,
    pub scaling: Scaling,
    pub cursor_shape: Option<WpCursorShapeManagerV1>,

    pub windows: Windows,
    pub model: ShellModel,
    pub actions: Rc<RefCell<VecDeque<ShellAction>>>,
    pub outputs: Vec<OutputShell>,
    pub lock: Lock,
    pub input: Input,
    pub text_input: TextInput,
    pub idle: Idle,
    pub ipc: Ipc,
    pub services: SystemServices,
    pub apps: Apps,
    pub settings: Settings,
    _auth: Option<AuthWorker>,
}

impl State {
    /// Binds the globals, installs the Slint platform, and starts the shell's data sources.
    pub fn new(
        conn: &Connection,
        globals: &GlobalList,
        qh: QueueHandle<State>,
        loop_handle: LoopHandle<'static, State>,
        loop_signal: LoopSignal,
        config_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let compositor = CompositorState::bind(globals, &qh)?;
        let shm = Shm::bind(globals, &qh)?;
        let layer_shell = LayerShell::bind(globals, &qh)?;
        let renderers = Renderers::new(conn, shm.wl_shm().clone(), Preference::from_env());
        let windows = Windows::install(move |surface| renderers.create(surface))?;

        let settings = Settings::load(config_path);
        let actions = Rc::new(RefCell::new(VecDeque::new()));
        let queue = actions.clone();
        let model =
            ShellModel::new(&settings.current, move |action| queue.borrow_mut().push_back(action));
        if let Some(path) = settings.path() {
            model.set_config_path(path);
        }
        let auth = spawn_auth(&loop_handle)?;
        if let Some(auth) = &auth {
            model.on_unlock_attempt(auth.submitter());
        }

        let wm_base = globals
            .bind(&qh, 1..=6, GlobalData)
            .map(WmBase)
            .inspect_err(|err| tracing::warn!("no popups without xdg-shell: {err}"))
            .ok();
        let mut state = Self {
            conn: conn.clone(),
            registry: RegistryState::new(globals),
            output_state: OutputState::new(globals, &qh),
            seat_state: SeatState::new(globals, &qh),
            session_lock_state: SessionLockState::new(globals, &qh),
            activation: ActivationState::bind(globals, &qh).ok(),
            scaling: Scaling {
                viewporter: globals.bind(&qh, 1..=1, ()).ok(),
                fractional: globals.bind(&qh, 1..=1, ()).ok(),
            },
            cursor_shape: globals.bind(&qh, 1..=1, ()).ok(),
            idle: Idle::new(globals.bind(&qh, 1..=1, ()).ok()),
            text_input: TextInput::new(globals.bind(&qh, 1..=1, ()).ok()),
            ipc: Ipc::connect(&loop_handle)?,
            services: SystemServices::spawn(&loop_handle)?,
            apps: Apps::new(&loop_handle)?,
            compositor,
            shm,
            layer_shell,
            wm_base,
            windows,
            model,
            actions,
            outputs: Vec::new(),
            lock: Lock::default(),
            input: Input::default(),
            settings,
            _auth: auth,
            qh,
            loop_handle,
            loop_signal,
            exit: None,
        };
        // `SeatState` binds the seats that already exist without calling `new_seat` for them.
        if let Some(seat) = state.seat_state.seats().next() {
            state.input.seat = Some(seat);
            state.watch_seat();
        }
        state.request(&Request::Subscribe, |_, _| {});
        state.request(&Request::GetState, |state, response| match response {
            Response::State(compositor_state) => {
                state.model.set_compositor_state(&compositor_state)
            }
            other => tracing::warn!("unexpected answer to get-state: {other:?}"),
        });
        state.request(&Request::GetLockState, |state, response| match response {
            Response::LockState { locked, held } => state.lock_state(locked, held),
            other => tracing::warn!("unexpected answer to get-lock-state: {other:?}"),
        });
        state.reload_apps();
        state.settings.watch(&state.loop_handle);
        Ok(state)
    }

    /// Follows idleness and text input on the seat in `input.seat`.
    pub fn watch_seat(&mut self) {
        self.watch_idle();
        self.text_input.set_seat(self.input.seat.as_ref(), &self.qh);
    }

    pub fn running(&self) -> bool {
        self.exit.is_none()
    }

    /// Stops the event loop; `result` becomes the outcome of the process.
    pub fn stop(&mut self, result: anyhow::Result<()>) {
        if self.exit.is_none() {
            self.exit = Some(result);
        }
        self.loop_signal.stop();
    }

    pub fn into_exit(self) -> anyhow::Result<()> {
        self.exit.unwrap_or(Ok(()))
    }

    /// The integer scale of `output`, which surfaces start with.
    pub fn output_scale(&self, output: &WlOutput) -> i32 {
        self.output_state.info(output).map_or(1, |info| info.scale_factor)
    }

    fn surfaces(&self) -> impl Iterator<Item = &SlintSurface> {
        let parts = self.outputs.iter().flat_map(OutputShell::surfaces);
        parts.chain(self.lock.surfaces.iter().map(|l| &l.surface))
    }

    /// The Slint surface on `surface`.
    pub fn surface(&self, surface: &WlSurface) -> Option<&SlintSurface> {
        self.surfaces().find(|s| s.wl_surface() == surface)
    }

    pub fn surface_mut(&mut self, surface: &WlSurface) -> Option<&mut SlintSurface> {
        self.outputs
            .iter_mut()
            .flat_map(OutputShell::surfaces_mut)
            .chain(self.lock.surfaces.iter_mut().map(|l| &mut l.surface))
            .find(|s| s.wl_surface() == surface)
    }

    /// Creates the shell on a new output, and its lock screen while locked.
    pub fn add_output(&mut self, output: &WlOutput) {
        let Some(name) = self.output_state.info(output).and_then(|info| info.name) else {
            tracing::warn!("an output without a name; no shell for it");
            return;
        };
        if self.outputs.iter().any(|o| o.output() == output) {
            return;
        }
        let view = ShellView::new(&self.model, &name);
        self.outputs.push(OutputShell::new(output, name.clone(), view));
        tracing::info!(output = %name, "shell started");
        self.add_lock_surface(output);
    }

    pub fn remove_output(&mut self, output: &WlOutput) {
        self.outputs.retain(|o| o.output() != output);
        self.remove_lock_surface(output);
    }

    /// Runs after every dispatch: Slint timers, the actions they and input queued, and rendering.
    pub fn update(&mut self) {
        slint::platform::update_timers_and_animations();
        self.process_actions();
        let grab = self.input.seat.as_ref().zip(self.input.last_serial);
        for output in &mut self.outputs {
            let scale = self.output_state.info(output.output()).map_or(1, |info| info.scale_factor);
            output.update(&Context {
                qh: &self.qh,
                compositor: &self.compositor,
                layer_shell: &self.layer_shell,
                wm_base: self.wm_base.as_ref(),
                windows: &self.windows,
                scaling: &self.scaling,
                scale,
                grab,
            });
        }
        for lock in &mut self.lock.surfaces {
            lock.surface.render(&self.qh);
            lock.surface.commit();
        }
        // Text fields report their changes while they render.
        self.sync_text_input();
        if let Err(err) = self.conn.flush() {
            self.stop(Err(anyhow!(err).context("lost the Wayland connection")));
        }
    }

    /// How long the event loop may sleep before Slint needs it.
    pub fn next_timeout(&self) -> Option<Duration> {
        let animating = self.surfaces().any(SlintSurface::animates_unpaced);
        let timer = slint::platform::duration_until_next_timer_update();
        match (animating, timer) {
            (true, Some(t)) => Some(t.min(ANIMATION_INTERVAL)),
            (true, None) => Some(ANIMATION_INTERVAL),
            (false, t) => t,
        }
    }
}

/// Runs `on` on the event loop with each value sent through the returned sender, such as from a worker thread.
pub fn forward<T: 'static>(
    handle: &LoopHandle<'static, State>,
    mut on: impl FnMut(&mut State, T) + 'static,
) -> anyhow::Result<Sender<T>> {
    let (sender, receiver) = channel::channel();
    handle
        .insert_source(receiver, move |event, _, state| {
            if let ChannelEvent::Msg(value) = event {
                on(state, value);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    Ok(sender)
}
