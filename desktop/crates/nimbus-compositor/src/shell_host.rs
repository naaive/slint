// SPDX-License-Identifier: MIT

//! Hosts the Slint shell in-process: the Slint platform, one shell per output rendered into memory,
//! input forwarding, and the shell's data sources (compositor state, applications, system services).

use crate::auth::{AuthWorker, PamAuthenticator};
use crate::state::State;
use anyhow::anyhow;
use nimbus_ipc::{CompositorState, Event, Response};
use nimbus_services::{ServiceCommand, ServiceEvent, Services, ServicesConfig, SystemState};
use nimbus_shell::{Exclusive, Osd, Shell, ShellAction};
use nimbus_xdg::{AppIndex, IconResolver};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, LogicalSize, PlatformError, SharedString};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::{ImportMem, Renderer, Texture};
use smithay::output::Output;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::channel::{self, Event as ChannelEvent};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How often to redraw while Slint animations run.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);

/// The Slint platform: windows are software-rendered into buffers the compositor draws.
struct NimbusPlatform {
    created: Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>,
    start: Instant,
}

impl Platform for NimbusPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        self.created.borrow_mut().push(window.clone());
        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

struct ShellInstance {
    output: String,
    shell: Shell,
    window: Rc<MinimalSoftwareWindow>,
    buffer: MemoryRenderBuffer,
    physical: Size<i32, Physical>,
    logical: Size<i32, Logical>,
    scale: f64,
    active: Cell<bool>,
    exclusive: Exclusive,
}

impl ShellInstance {
    fn dispatch(&self, event: WindowEvent) {
        if let Err(err) = self.window.dispatch_event_with_result(event) {
            tracing::warn!(output = %self.output, "the shell rejected an event: {err}");
        }
    }

    /// Updates the window and buffer to the output's current size and scale.
    fn resize(&mut self, output: &Output) {
        let Some((logical, physical, scale)) = output_metrics(output) else {
            return;
        };
        if logical == self.logical && physical == self.physical && scale == self.scale {
            return;
        }
        self.logical = logical;
        self.physical = physical;
        self.scale = scale;
        self.dispatch(WindowEvent::ScaleFactorChanged { scale_factor: scale as f32 });
        self.window.set_size(slint::WindowSize::Logical(LogicalSize::new(
            logical.w as f32,
            logical.h as f32,
        )));
        self.buffer.render().resize(Size::<i32, Buffer>::from((physical.w, physical.h)));
        self.window.request_redraw();
    }
}

/// The output's logical size, physical size, and scale.
fn output_metrics(output: &Output) -> Option<(Size<i32, Logical>, Size<i32, Physical>, f64)> {
    let mode = output.current_mode()?;
    let scale = output.current_scale().fractional_scale();
    let physical = output.current_transform().transform_size(mode.size);
    let logical = physical.to_f64().to_logical(scale).to_i32_round();
    Some((logical, physical, scale))
}

pub struct ShellHost {
    created: Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>,
    instances: Vec<ShellInstance>,
    actions: Rc<RefCell<VecDeque<ShellAction>>>,
    apps: Option<(AppIndex, IconResolver)>,
    services: Option<Services>,
    system_state: Option<SystemState>,
    auth: Option<AuthWorker>,
    config_path: Option<PathBuf>,
}

impl ShellHost {
    /// Installs the Slint platform and starts loading applications and system services.
    /// Shell-initiated settings changes, such as the dark style toggle, are saved to `config_path`.
    pub fn new(
        handle: &LoopHandle<'static, State>,
        config: &nimbus_config::Config,
        config_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let created = Rc::new(RefCell::new(Vec::new()));
        slint::platform::set_platform(Box::new(NimbusPlatform {
            created: created.clone(),
            start: Instant::now(),
        }))
        .map_err(|e| anyhow!("cannot install the Slint platform: {e}"))?;

        let (apps_tx, apps_rx) = channel::channel::<(AppIndex, IconResolver)>();
        handle
            .insert_source(apps_rx, |event, _, state| {
                if let ChannelEvent::Msg((apps, icons)) = event
                    && let Some(shell) = state.nimbus.shell.as_mut()
                {
                    tracing::info!(count = apps.entries.len(), "applications loaded");
                    shell.set_apps(apps, icons);
                }
            })
            .map_err(|e| anyhow!("cannot receive applications: {e}"))?;
        let icon_theme = config.appearance.icon_theme.clone();
        std::thread::Builder::new()
            .name("nimbus-apps".into())
            .spawn(move || {
                let apps = AppIndex::scan();
                let icons = IconResolver::new(&icon_theme);
                // The receiver only goes away when the compositor exits.
                let _ = apps_tx.send((apps, icons));
            })
            .map_err(|e| anyhow!("cannot start scanning applications: {e}"))?;

        let (services_tx, services_rx) = channel::channel::<ServiceEvent>();
        handle
            .insert_source(services_rx, |event, _, state| {
                if let ChannelEvent::Msg(event) = event {
                    state.handle_service_event(event);
                }
            })
            .map_err(|e| anyhow!("cannot receive service events: {e}"))?;
        let services = Services::spawn(ServicesConfig::default(), move |event| {
            let _ = services_tx.send(event);
        });

        let (auth_tx, auth_rx) = channel::channel::<bool>();
        handle
            .insert_source(auth_rx, |event, _, state| {
                if let ChannelEvent::Msg(ok) = event {
                    state.unlock_result(ok);
                }
            })
            .map_err(|e| anyhow!("cannot receive authentication results: {e}"))?;
        let service = crate::auth::service_name();
        let auth = AuthWorker::spawn(
            Box::new(PamAuthenticator::new(service)),
            crate::auth::current_user(),
            move |ok| {
                let _ = auth_tx.send(ok);
            },
        )
        .map_err(|err| tracing::error!("cannot start lock screen authentication: {err}"))
        .ok();
        tracing::info!(service, "lock screen authentication uses PAM");

        Ok(Self {
            created,
            instances: Vec::new(),
            actions: Rc::new(RefCell::new(VecDeque::new())),
            apps: None,
            services: Some(services),
            system_state: None,
            auth,
            config_path,
        })
    }

    fn instance(&self, output: &str) -> Option<&ShellInstance> {
        self.instances.iter().find(|i| i.output == output)
    }

    /// Creates the shell for a new output.
    pub fn add_output(
        &mut self,
        output: &Output,
        config: &nimbus_config::Config,
        state: &CompositorState,
    ) {
        let name = output.name();
        let Some((logical, physical, scale)) = output_metrics(output) else {
            tracing::warn!(output = %name, "output without a mode; no shell for it");
            return;
        };
        let actions = self.actions.clone();
        let shell = match Shell::new(config, move |action| actions.borrow_mut().push_back(action)) {
            Ok(shell) => shell,
            Err(err) => {
                tracing::error!(output = %name, "cannot create the shell: {err}");
                return;
            }
        };
        let Some(window) = self.created.borrow_mut().pop() else {
            tracing::error!(output = %name, "the shell created no window");
            return;
        };
        self.created.borrow_mut().clear();
        shell.set_output_name(&name);
        if let Some(path) = &self.config_path {
            shell.set_config_path(path.clone());
        }
        if let Some(auth) = &self.auth {
            shell.on_unlock_attempt(auth.submitter());
        }
        shell.set_compositor_state(state);
        if let Some((apps, icons)) = &self.apps {
            shell.set_apps(apps, icons);
        }
        if let Some(system) = &self.system_state {
            shell.handle_service_event(&ServiceEvent::State(system.clone()));
        }
        let instance = ShellInstance {
            output: name.clone(),
            buffer: MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                (physical.w, physical.h),
                1,
                Transform::Normal,
                None,
            ),
            shell,
            window,
            physical,
            logical,
            scale,
            active: Cell::new(false),
            exclusive: Exclusive::default(),
        };
        instance.dispatch(WindowEvent::ScaleFactorChanged { scale_factor: scale as f32 });
        instance.window.set_size(slint::WindowSize::Logical(LogicalSize::new(
            logical.w as f32,
            logical.h as f32,
        )));
        if let Err(err) = instance.shell.show() {
            tracing::error!(output = %name, "cannot show the shell: {err}");
            return;
        }
        tracing::info!(output = %name, "shell started");
        self.instances.push(instance);
    }

    pub fn remove_output(&mut self, output: &str) {
        self.instances.retain(|i| i.output != output);
    }

    pub fn outputs_changed(&mut self, outputs: &[Output]) {
        for instance in &mut self.instances {
            if let Some(output) = outputs.iter().find(|o| o.name() == instance.output) {
                instance.resize(output);
            }
        }
    }

    fn set_apps(&mut self, apps: AppIndex, icons: IconResolver) {
        for instance in &self.instances {
            instance.shell.set_apps(&apps, &icons);
        }
        self.apps = Some((apps, icons));
    }

    pub fn apps(&self) -> Option<&AppIndex> {
        self.apps.as_ref().map(|(apps, _)| apps)
    }

    pub fn set_config(&self, config: &nimbus_config::Config) {
        for instance in &self.instances {
            instance.shell.set_config(config);
        }
    }

    pub fn handle_compositor_events(&self, events: &[Event]) {
        for instance in &self.instances {
            for event in events {
                instance.shell.handle_compositor_event(event);
            }
        }
    }

    pub fn system_state(&self) -> Option<&SystemState> {
        self.system_state.as_ref()
    }

    pub fn send_service(&self, command: ServiceCommand) {
        match &self.services {
            Some(services) => services.send(command),
            None => tracing::debug!("services aren't running; dropping {command:?}"),
        }
    }

    pub fn show_osd(&self, osd: Osd) {
        for instance in &self.instances {
            instance.shell.show_osd(osd);
        }
    }

    pub fn toggle_launcher(&self, output: &str) {
        if let Some(instance) = self.instance(output).or_else(|| self.instances.first()) {
            instance.shell.toggle_launcher();
        }
    }

    pub fn toggle_overview(&self, output: &str) {
        if let Some(instance) = self.instance(output).or_else(|| self.instances.first()) {
            instance.shell.toggle_overview();
        }
    }

    pub fn set_locked(&self, locked: bool) {
        for instance in &self.instances {
            instance.shell.set_locked(locked);
        }
    }

    /// Tells every lock screen that the last password was wrong.
    pub fn unlock_failed(&self) {
        for instance in &self.instances {
            instance.shell.unlock_failed();
        }
    }

    pub fn is_locked(&self) -> bool {
        self.instances.iter().any(|i| i.shell.is_locked())
    }

    pub fn wants_keyboard(&self) -> bool {
        self.instances.iter().any(|i| i.shell.wants_keyboard())
    }

    pub fn exclusive_zone(&self, output: &str) -> Option<Exclusive> {
        self.instance(output).map(|i| i.exclusive)
    }

    /// Whether the shell on `output` takes the pointer at `local`, in logical output coordinates.
    pub fn accepts_pointer(&self, output: &str, local: Point<f64, Logical>) -> bool {
        self.instance(output).is_some_and(|i| {
            i.shell.input_region().iter().any(|r| r.contains(local.x as f32, local.y as f32))
        })
    }

    pub fn pointer_moved(&self, output: &str, local: Point<f64, Logical>) {
        if let Some(instance) = self.instance(output) {
            instance.dispatch(WindowEvent::PointerMoved { position: position(local) });
        }
    }

    pub fn pointer_button(
        &self,
        output: &str,
        local: Point<f64, Logical>,
        button: PointerEventButton,
        pressed: bool,
    ) {
        if let Some(instance) = self.instance(output) {
            let position = position(local);
            instance.dispatch(if pressed {
                WindowEvent::PointerPressed { position, button }
            } else {
                WindowEvent::PointerReleased { position, button }
            });
        }
    }

    pub fn pointer_scrolled(
        &self,
        output: &str,
        local: Point<f64, Logical>,
        delta_x: f32,
        delta_y: f32,
    ) {
        if let Some(instance) = self.instance(output) {
            instance.dispatch(WindowEvent::PointerScrolled {
                position: position(local),
                delta_x,
                delta_y,
            });
        }
    }

    pub fn pointer_exited(&self, output: &str) {
        if let Some(instance) = self.instance(output) {
            instance.dispatch(WindowEvent::PointerExited);
        }
    }

    /// Sends a key to the shell that holds the keyboard: the locked one, or the one that asked for it.
    pub fn dispatch_key(&self, text: SharedString, pressed: bool, repeat: bool) {
        let target = self
            .instances
            .iter()
            .find(|i| i.shell.is_locked() || i.shell.wants_keyboard())
            .or_else(|| self.instances.first());
        if let Some(instance) = target {
            instance.dispatch(match (pressed, repeat) {
                (true, false) => WindowEvent::KeyPressed { text },
                (true, true) => WindowEvent::KeyPressRepeated { text },
                (false, _) => WindowEvent::KeyReleased { text },
            });
        }
    }

    fn take_actions(&self) -> Vec<ShellAction> {
        self.actions.borrow_mut().drain(..).collect()
    }

    /// Advances timers and animations, tells windows whether they have keyboard focus,
    /// and renders the shells that changed; returns their outputs and whether any exclusive zone changed.
    fn update(&mut self) -> (Vec<String>, bool) {
        slint::platform::update_timers_and_animations();
        let mut damaged = Vec::new();
        let mut exclusive_changed = false;
        for instance in &mut self.instances {
            let wants = instance.shell.is_locked() || instance.shell.wants_keyboard();
            if instance.active.replace(wants) != wants {
                instance.dispatch(WindowEvent::WindowActiveChanged(wants));
            }
            let exclusive = instance.shell.exclusive_zone();
            if exclusive != instance.exclusive {
                instance.exclusive = exclusive;
                exclusive_changed = true;
            }
            let width = usize::try_from(instance.physical.w).unwrap_or(0);
            let expected = width * usize::try_from(instance.physical.h).unwrap_or(0);
            let buffer = &mut instance.buffer;
            let drawn = instance.window.draw_if_needed(|renderer| {
                let mut context = buffer.render();
                let result = context.draw(|memory| {
                    let pixels: &mut [PremultipliedRgbaColor] =
                        bytemuck::try_cast_slice_mut(memory)
                            .map_err(|e| anyhow!("shell buffer: {e}"))?;
                    if pixels.len() < expected || width == 0 {
                        return Err(anyhow!("shell buffer is smaller than the output"));
                    }
                    let region = renderer.render(pixels, width);
                    Ok(region
                        .iter()
                        .map(|(origin, size)| {
                            Rectangle::<i32, Buffer>::new(
                                (origin.x, origin.y).into(),
                                (
                                    i32::try_from(size.width).unwrap_or(0),
                                    i32::try_from(size.height).unwrap_or(0),
                                )
                                    .into(),
                            )
                        })
                        .collect())
                });
                if let Err(err) = result {
                    tracing::warn!("rendering the shell failed: {err:#}");
                }
            });
            if drawn {
                damaged.push(instance.output.clone());
            }
        }
        (damaged, exclusive_changed)
    }

    /// How soon Slint needs the event loop to wake up, for animations and timers.
    pub fn next_wakeup(&self) -> Option<Duration> {
        let animating = self.instances.iter().any(|i| i.window.has_active_animations());
        let timer = slint::platform::duration_until_next_timer_update();
        match (animating, timer) {
            (true, Some(t)) => Some(t.min(ANIMATION_INTERVAL)),
            (true, None) => Some(ANIMATION_INTERVAL),
            (false, t) => t,
        }
    }

    pub fn render_element<R>(
        &self,
        renderer: &mut R,
        output: &str,
        _scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let instance = self.instance(output)?;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((0.0, 0.0)),
            &instance.buffer,
            None,
            None,
            Some(instance.logical),
            Kind::Unspecified,
        )
        .map_err(|err| tracing::debug!("cannot upload the shell: {err:?}"))
        .ok()
    }
}

fn position(local: Point<f64, Logical>) -> LogicalPosition {
    LogicalPosition::new(local.x as f32, local.y as f32)
}

impl State {
    /// Handles queued shell actions, then advances and renders the shells.
    pub fn process_shell(&mut self) {
        // Actions can trigger more actions, for example a request that changes state the shell reacts to.
        for _ in 0..16 {
            let actions = match self.nimbus.shell.as_ref() {
                Some(shell) => shell.take_actions(),
                None => return,
            };
            if actions.is_empty() {
                break;
            }
            for action in actions {
                self.handle_shell_action(action);
            }
        }
        let Some(shell) = self.nimbus.shell.as_mut() else {
            return;
        };
        let (damaged, exclusive_changed) = shell.update();
        for name in damaged {
            if let Some(output) = self.nimbus.output_by_name(&name) {
                self.nimbus.queue_redraw(&output);
            }
        }
        if exclusive_changed {
            self.nimbus.arrange();
        }
    }

    fn handle_shell_action(&mut self, action: ShellAction) {
        match action {
            ShellAction::Compositor(request) => {
                if let Response::Error { message } = self.handle_request(request) {
                    tracing::warn!("shell request failed: {message}");
                }
            }
            ShellAction::Service(command) => {
                if let Some(shell) = self.nimbus.shell.as_ref() {
                    shell.send_service(command);
                }
            }
            ShellAction::Launch(id) => self.launch_app(&id),
            ShellAction::OpenSettings(page) => {
                let mut args = Vec::new();
                if let Some(page) = page.as_deref().filter(|p| is_page_name(p)) {
                    args.extend(["--page", page]);
                }
                if let Err(err) = self.nimbus.children.spawn_args("nimbus-settings", &args) {
                    tracing::warn!("cannot open Settings: {err}");
                }
            }
        }
    }

    fn launch_app(&mut self, id: &str) {
        let Some(entry) = self
            .nimbus
            .shell
            .as_ref()
            .and_then(|s| s.apps())
            .and_then(|apps| apps.get(id))
            .cloned()
        else {
            tracing::warn!("cannot launch '{id}': no such application");
            return;
        };
        let (token, _) = self.nimbus.xdg_activation_state.create_external_token(None);
        let token = token.as_str().to_owned();
        if let Err(err) = nimbus_xdg::launch(&entry, &[], Some(&token)) {
            tracing::warn!("cannot launch '{id}': {err}");
        }
    }

    /// Applies the outcome of a lock screen password check.
    pub fn unlock_result(&mut self, ok: bool) {
        let Some(shell) = self.nimbus.shell.as_ref() else {
            return;
        };
        if ok {
            tracing::info!("unlocked");
            shell.set_locked(false);
            self.nimbus.last_activity = Instant::now();
        } else {
            shell.unlock_failed();
        }
        self.nimbus.queue_redraw_all();
    }

    pub fn handle_service_event(&mut self, event: ServiceEvent) {
        if event == ServiceEvent::LogoutRequested {
            // Exit status 0 tells nimbus-session that the user logged out.
            tracing::info!("logging out");
            self.nimbus.stop();
            return;
        }
        let Some(shell) = self.nimbus.shell.as_mut() else {
            return;
        };
        match &event {
            ServiceEvent::State(system) => shell.system_state = Some(system.clone()),
            ServiceEvent::LockRequested => shell.set_locked(true),
            ServiceEvent::UnlockRequested => shell.set_locked(false),
            ServiceEvent::LogoutRequested
            | ServiceEvent::Notification(_)
            | ServiceEvent::NotificationClosed { .. } => {}
        }
        for instance in &shell.instances {
            instance.shell.handle_service_event(&event);
        }
    }
}

/// Settings page names are plain identifiers such as `appearance`.
fn is_page_name(page: &str) -> bool {
    !page.is_empty()
        && !page.starts_with('-')
        && page.len() <= 64
        && page.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_names_are_validated() {
        assert!(is_page_name("appearance"));
        assert!(is_page_name("night-light_2"));
        assert!(!is_page_name(""));
        assert!(!is_page_name("--evil"));
        assert!(!is_page_name("a b"));
    }
}
