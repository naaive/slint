// SPDX-License-Identifier: MIT

//! Hosts the Slint shell in-process: the Slint platform, one shell per output rendered into memory,
//! input forwarding, and the shell's data sources (compositor state, applications, system services).

use crate::auth::{AuthWorker, PamAuthenticator};
use crate::state::State;
use anyhow::anyhow;
use nimbus_ipc::{CompositorState, Event, Response};
use nimbus_services::{
    Notification, ServiceCommand, ServiceEvent, Services, ServicesConfig, SystemState, Urgency,
};
use nimbus_shell::{Exclusive, LockView, Osd, ShellAction, ShellModel, ShellView};
use nimbus_xdg::{AppIndex, IconResolver};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PlatformError, SharedString};
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
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

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

/// The output's size and scale, which every surface on it follows.
#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    physical: Size<i32, Physical>,
    logical: Size<i32, Logical>,
    scale: f64,
}

impl Metrics {
    fn of(output: &Output) -> Option<Self> {
        let mode = output.current_mode()?;
        let physical = output.current_transform().transform_size(mode.size);
        let scale = output.current_scale().fractional_scale();
        let logical = physical.to_f64().to_logical(scale).to_i32_round();
        Some(Self { physical, logical, scale })
    }

    /// The window size in physical pixels, so a window renders exactly as many pixels as its buffer holds.
    fn window_size(&self) -> slint::WindowSize {
        slint::WindowSize::Physical(slint::PhysicalSize::new(
            u32::try_from(self.physical.w).unwrap_or(0),
            u32::try_from(self.physical.h).unwrap_or(0),
        ))
    }
}

/// A Slint window covering an output, software-rendered into a buffer the compositor draws.
struct Surface {
    window: Rc<MinimalSoftwareWindow>,
    buffer: MemoryRenderBuffer,
    active: Cell<bool>,
}

impl Surface {
    fn new(window: Rc<MinimalSoftwareWindow>, metrics: Metrics) -> Self {
        let Metrics { physical, .. } = metrics;
        let surface = Self {
            window,
            buffer: MemoryRenderBuffer::new(
                Fourcc::Abgr8888,
                (physical.w, physical.h),
                1,
                Transform::Normal,
                None,
            ),
            active: Cell::new(false),
        };
        surface.resize(metrics);
        surface
    }

    fn dispatch(&self, event: WindowEvent) {
        if let Err(err) = self.window.dispatch_event_with_result(event) {
            tracing::warn!("the shell rejected an event: {err}");
        }
    }

    fn resize(&self, metrics: Metrics) {
        self.dispatch(WindowEvent::ScaleFactorChanged { scale_factor: metrics.scale as f32 });
        self.window.set_size(metrics.window_size());
        self.window.request_redraw();
    }

    fn set_active(&self, active: bool) {
        if self.active.replace(active) != active {
            self.dispatch(WindowEvent::WindowActiveChanged(active));
        }
    }

    /// Renders the damaged regions; returns whether anything was drawn.
    fn draw(&mut self, physical: Size<i32, Physical>) -> bool {
        let width = usize::try_from(physical.w).unwrap_or(0);
        let height = usize::try_from(physical.h).unwrap_or(0);
        let expected = width * height;
        let window_size = self.window.size();
        let buffer = &mut self.buffer;
        self.window.draw_if_needed(|renderer| {
            let mut context = buffer.render();
            let result = context.draw(|memory| {
                let pixels: &mut [PremultipliedRgbaColor] = bytemuck::try_cast_slice_mut(memory)
                    .map_err(|e| anyhow!("shell buffer: {e}"))?;
                if pixels.len() < expected
                    || width == 0
                    || window_size.width as usize > width
                    || window_size.height as usize > height
                {
                    return Err(anyhow!("shell buffer is smaller than the shell window"));
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
        })
    }
}

/// The shell on one output: its view, and its lock screen while the session is locked.
struct ShellInstance {
    output: String,
    view: ShellView,
    surface: Surface,
    lock: Option<(LockView, Surface)>,
    metrics: Metrics,
    exclusive: Exclusive,
}

impl Drop for ShellInstance {
    fn drop(&mut self) {
        self.unlock();
        // A shown Slint window keeps its component, timers, and models alive until hidden.
        if let Err(err) = self.view.window().hide() {
            tracing::warn!(output = %self.output, "cannot hide the shell: {err}");
        }
    }
}

impl ShellInstance {
    /// The surface on top, which takes input: the lock screen while there is one.
    fn front(&self) -> &Surface {
        self.lock.as_ref().map_or(&self.surface, |(_, surface)| surface)
    }

    fn dispatch(&self, event: WindowEvent) {
        self.front().dispatch(event);
    }

    fn surfaces(&self) -> impl Iterator<Item = &Surface> {
        std::iter::once(&self.surface).chain(self.lock.as_ref().map(|(_, surface)| surface))
    }

    /// Updates the surfaces to the output's current size and scale.
    fn resize(&mut self, output: &Output) {
        let Some(metrics) = Metrics::of(output) else {
            return;
        };
        if metrics == self.metrics {
            return;
        }
        self.metrics = metrics;
        let size = Size::<i32, Buffer>::from((metrics.physical.w, metrics.physical.h));
        for surface in
            std::iter::once(&mut self.surface).chain(self.lock.as_mut().map(|(_, surface)| surface))
        {
            surface.buffer.render().resize(size);
            surface.resize(metrics);
        }
    }

    fn unlock(&mut self) {
        if let Some((lock, _)) = self.lock.take()
            && let Err(err) = lock.window().hide()
        {
            tracing::warn!(output = %self.output, "cannot hide the lock screen: {err}");
        }
    }
}

pub struct ShellHost {
    created: Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>,
    model: ShellModel,
    instances: Vec<ShellInstance>,
    actions: Rc<RefCell<VecDeque<ShellAction>>>,
    apps: Option<(AppIndex, IconResolver)>,
    services: Option<Services>,
    system_state: Option<SystemState>,
    auth: Option<AuthWorker>,
    /// After [`ServiceEvent::LockRequested`], the outputs that haven't presented a locked frame yet;
    /// see [`ServiceCommand::LockPresented`].
    lock_unpresented: Option<HashSet<String>>,
    /// The output whose shell the user last interacted with; only that shell takes keys.
    keyboard_output: Option<String>,
    apps_tx: Option<channel::Sender<(AppIndex, IconResolver)>>,
    /// Ids for the compositor's own toasts, counting down from the top so they stay clear of the
    /// notification server's, which count up.
    next_local_notification: Cell<u32>,
}

impl ShellHost {
    /// Installs the Slint platform and starts loading applications and system services.
    /// Shell-initiated settings changes, such as the dark style toggle, are saved to `config_path`.
    pub fn new(
        handle: &LoopHandle<'static, State>,
        config: &nimbus_config::Config,
        config_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let mut host = Self::with_platform(config, config_path)?;

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
        host.apps_tx = Some(apps_tx);

        let (services_tx, services_rx) = channel::channel::<ServiceEvent>();
        handle
            .insert_source(services_rx, |event, _, state| {
                if let ChannelEvent::Msg(event) = event {
                    state.handle_service_event(event);
                }
            })
            .map_err(|e| anyhow!("cannot receive service events: {e}"))?;
        host.services = Some(Services::spawn(ServicesConfig::default(), move |event| {
            let _ = services_tx.send(event);
        }));

        let (auth_tx, auth_rx) = channel::channel::<bool>();
        handle
            .insert_source(auth_rx, |event, _, state| {
                if let ChannelEvent::Msg(ok) = event {
                    state.unlock_result(ok);
                }
            })
            .map_err(|e| anyhow!("cannot receive authentication results: {e}"))?;
        let service = crate::auth::service_name();
        host.auth = AuthWorker::spawn(
            Box::new(PamAuthenticator::new(service)),
            crate::auth::current_user(),
            move |ok| {
                let _ = auth_tx.send(ok);
            },
        )
        .map_err(|err| tracing::error!("cannot start lock screen authentication: {err}"))
        .ok();
        if let Some(auth) = &host.auth {
            host.model.on_unlock_attempt(auth.submitter());
        }
        tracing::info!(service, "lock screen authentication uses PAM");

        host.reload_apps(config.appearance.icon_theme.clone());
        Ok(host)
    }

    /// A host without services, authentication, or applications, on a new Slint platform for this thread.
    fn with_platform(
        config: &nimbus_config::Config,
        config_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let created = Rc::new(RefCell::new(Vec::new()));
        slint::platform::set_platform(Box::new(NimbusPlatform {
            created: created.clone(),
            start: Instant::now(),
        }))
        .map_err(|e| anyhow!("cannot install the Slint platform: {e}"))?;
        let actions = Rc::new(RefCell::new(VecDeque::new()));
        let queue = actions.clone();
        let model = ShellModel::new(config, move |action| queue.borrow_mut().push_back(action));
        if let Some(path) = config_path {
            model.set_config_path(path);
        }
        Ok(Self {
            created,
            model,
            instances: Vec::new(),
            actions,
            apps: None,
            services: None,
            system_state: None,
            auth: None,
            lock_unpresented: None,
            keyboard_output: None,
            apps_tx: None,
            next_local_notification: Cell::new(u32::MAX),
        })
    }

    /// Scans applications and loads the icon theme in the background, then hands them to the shell.
    pub fn reload_apps(&self, icon_theme: String) {
        let Some(apps_tx) = self.apps_tx.clone() else {
            return;
        };
        let spawned = std::thread::Builder::new().name("nimbus-apps".into()).spawn(move || {
            let apps = AppIndex::scan();
            let icons = IconResolver::new(&icon_theme);
            // The receiver only goes away when the compositor exits.
            let _ = apps_tx.send((apps, icons));
        });
        if let Err(err) = spawned {
            tracing::error!("cannot start scanning applications: {err}");
        }
    }

    fn instance(&self, output: &str) -> Option<&ShellInstance> {
        self.instances.iter().find(|i| i.output == output)
    }

    /// Creates a Slint component with `create` and returns it with the window it got.
    fn create<T>(
        &self,
        create: impl FnOnce() -> Result<T, PlatformError>,
    ) -> Result<(T, Rc<MinimalSoftwareWindow>), PlatformError> {
        let component = create()?;
        let window = self.created.borrow_mut().pop();
        self.created.borrow_mut().clear();
        let window = window.ok_or_else(|| PlatformError::from("no window was created"))?;
        Ok((component, window))
    }

    /// Creates the shell for a new output.
    pub fn add_output(&mut self, output: &Output, state: &CompositorState) {
        let name = output.name();
        let Some(metrics) = Metrics::of(output) else {
            tracing::warn!(output = %name, "output without a mode; no shell for it");
            return;
        };
        self.model.set_compositor_state(state);
        let (view, window) = match self.create(|| ShellView::new(&self.model, &name)) {
            Ok(created) => created,
            Err(err) => {
                tracing::error!(output = %name, "cannot create the shell: {err}");
                return;
            }
        };
        let mut instance = ShellInstance {
            output: name.clone(),
            surface: Surface::new(window, metrics),
            view,
            lock: None,
            metrics,
            exclusive: Exclusive::default(),
        };
        if let Err(err) = instance.view.show() {
            tracing::error!(output = %name, "cannot show the shell: {err}");
            return;
        }
        if self.model.is_locked() {
            instance.lock = self.lock_screen(&instance);
        }
        tracing::info!(output = %name, "shell started");
        self.instances.push(instance);
    }

    /// Creates a lock screen for the output of `instance`.
    fn lock_screen(&self, instance: &ShellInstance) -> Option<(LockView, Surface)> {
        let created = self.create(|| LockView::new(&self.model)).and_then(|(lock, window)| {
            lock.show()?;
            Ok((lock, window))
        });
        match created {
            Ok((lock, window)) => Some((lock, Surface::new(window, instance.metrics))),
            Err(err) => {
                tracing::error!(output = %instance.output, "cannot show the lock screen: {err}");
                None
            }
        }
    }

    pub fn remove_output(&mut self, output: &str) {
        self.instances.retain(|i| i.output != output);
        if self.keyboard_output.as_deref() == Some(output) {
            self.keyboard_output = None;
        }
    }

    pub fn outputs_changed(&mut self, outputs: &[Output]) {
        for instance in &mut self.instances {
            if let Some(output) = outputs.iter().find(|o| o.name() == instance.output) {
                instance.resize(output);
            }
        }
    }

    fn set_apps(&mut self, apps: AppIndex, icons: IconResolver) {
        self.model.set_apps(&apps, &icons);
        self.apps = Some((apps, icons));
    }

    pub fn apps(&self) -> Option<&AppIndex> {
        self.apps.as_ref().map(|(apps, _)| apps)
    }

    pub fn set_config(&self, config: &nimbus_config::Config) {
        self.model.set_config(config);
    }

    pub fn handle_compositor_events(&self, events: &[Event]) {
        for event in events {
            self.model.handle_compositor_event(event);
        }
    }

    /// The last system state from the services, which the volume and brightness keys update ahead of them.
    pub fn system_state_mut(&mut self) -> Option<&mut SystemState> {
        self.system_state.as_mut()
    }

    pub fn send_service(&self, command: ServiceCommand) {
        match &self.services {
            Some(services) => services.send(command),
            None => tracing::debug!("services aren't running; dropping {command:?}"),
        }
    }

    /// Shows a toast, for errors the user would otherwise never see.
    pub fn show_error(&self, summary: String, body: String) {
        let id = self.next_local_notification.get();
        self.next_local_notification.set(id.wrapping_sub(1));
        self.model.handle_service_event(&ServiceEvent::Notification(Notification {
            id,
            app_name: "Nimbus".into(),
            app_icon: "dialog-error".into(),
            summary,
            body,
            actions: Vec::new(),
            urgency: Urgency::Normal,
            expire_timeout: None,
            received: SystemTime::now(),
            transient: true,
            resident: false,
        }));
    }

    pub fn show_osd(&self, osd: Osd) {
        self.model.show_osd(osd);
    }

    pub fn toggle_launcher(&mut self, output: &str) {
        self.toggle_on(output, ShellView::toggle_launcher);
    }

    pub fn toggle_overview(&mut self, output: &str) {
        self.toggle_on(output, ShellView::toggle_overview);
    }

    /// Toggles a full-output surface on `output`, closing the launcher and overview on the others,
    /// and gives that shell the keyboard.
    fn toggle_on(&mut self, output: &str, toggle: fn(&ShellView)) {
        let Some(name) =
            self.instance(output).or_else(|| self.instances.first()).map(|i| i.output.clone())
        else {
            return;
        };
        for instance in self.instances.iter().filter(|i| i.output != name) {
            let ui = instance.view.component();
            if ui.get_launcher_open() {
                instance.view.toggle_launcher();
            }
            if ui.get_overview_open() {
                instance.view.toggle_overview();
            }
        }
        if let Some(instance) = self.instance(&name) {
            toggle(&instance.view);
        }
        self.keyboard_output = Some(name);
    }

    /// Gives the keyboard to the shell on `output`, or with `None`, back to the windows.
    pub fn focus_output(&mut self, output: Option<&str>) {
        self.keyboard_output = output.map(str::to_owned);
    }

    /// Locks or unlocks the shell: every output gets a lock screen while locked.
    pub fn set_locked(&mut self, locked: bool) {
        self.model.set_locked(locked);
        for index in 0..self.instances.len() {
            if !locked {
                self.instances[index].unlock();
            } else if self.instances[index].lock.is_none() {
                self.instances[index].lock = self.lock_screen(&self.instances[index]);
            }
        }
    }

    /// Tells every lock screen that the last password was wrong.
    pub fn unlock_failed(&self) {
        self.model.unlock_failed();
    }

    /// Sends [`ServiceCommand::LockPresented`] once each of `outputs` presented a locked frame.
    pub fn await_lock_presented(&mut self, outputs: HashSet<String>) {
        self.lock_unpresented = Some(outputs);
    }

    pub fn awaits_lock_presented(&self) -> bool {
        self.lock_unpresented.is_some()
    }

    /// Records that the `rendered` outputs presented a frame, out of the `live` ones;
    /// returns whether this sent [`ServiceCommand::LockPresented`].
    pub fn frames_presented(&mut self, rendered: &[&str], live: &HashSet<String>) -> bool {
        if !self.model.is_locked() {
            self.lock_unpresented = None;
            return false;
        }
        let Some(unpresented) = self.lock_unpresented.as_mut() else {
            return false;
        };
        unpresented.retain(|name| live.contains(name) && !rendered.contains(&name.as_str()));
        if !unpresented.is_empty() {
            return false;
        }
        self.lock_unpresented = None;
        self.send_service(ServiceCommand::LockPresented);
        true
    }

    /// The shell that takes keys: while locked, the one the user last used or the first one;
    /// otherwise the one the user last used, if it wants the keyboard.
    fn keyboard_instance(&self) -> Option<&ShellInstance> {
        let focused = self.keyboard_output.as_deref().and_then(|o| self.instance(o));
        if self.model.is_locked() {
            focused.or_else(|| self.instances.first())
        } else {
            focused.filter(|i| i.view.wants_keyboard())
        }
    }

    pub fn wants_keyboard(&self) -> bool {
        self.keyboard_instance().is_some()
    }

    /// Whether the shell on `output` covers it, for example with the launcher.
    pub fn wants_keyboard_on(&self, output: &str) -> bool {
        self.instance(output).is_some_and(|i| i.view.wants_keyboard())
    }

    pub fn exclusive_zone(&self, output: &str) -> Option<Exclusive> {
        self.instance(output).map(|i| i.exclusive)
    }

    /// Whether the shell on `output` takes the pointer at `local`, in logical output coordinates.
    pub fn accepts_pointer(&self, output: &str, local: Point<f64, Logical>) -> bool {
        self.instance(output).is_some_and(|i| {
            i.view.input_region().iter().any(|r| r.contains(local.x as f32, local.y as f32))
        })
    }

    pub fn pointer_moved(&self, output: &str, local: Point<f64, Logical>) {
        if let Some(instance) = self.instance(output) {
            instance.dispatch(WindowEvent::PointerMoved { position: position(local) });
        }
    }

    pub fn pointer_button(
        &mut self,
        output: &str,
        local: Point<f64, Logical>,
        button: PointerEventButton,
        pressed: bool,
    ) {
        if pressed {
            self.keyboard_output = Some(output.to_owned());
        }
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

    /// Sends a key to the shell that holds the keyboard; see [`ShellHost::keyboard_instance`].
    pub fn dispatch_key(&self, text: SharedString, pressed: bool, repeat: bool) {
        let target =
            self.keyboard_instance().or_else(|| if repeat { None } else { self.instances.first() });
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
        let keyboard = self.keyboard_instance().map(|i| i.output.clone());
        for instance in &mut self.instances {
            let wants = keyboard.as_deref() == Some(instance.output.as_str());
            let locked = instance.lock.is_some();
            instance.surface.set_active(wants && !locked);
            if let Some((_, lock)) = &instance.lock {
                lock.set_active(wants);
            }
            let exclusive = instance.view.exclusive_zone();
            if exclusive != instance.exclusive {
                instance.exclusive = exclusive;
                exclusive_changed = true;
            }
            let physical = instance.metrics.physical;
            let mut drawn = instance.surface.draw(physical);
            if let Some((_, lock)) = &mut instance.lock {
                drawn |= lock.draw(physical);
            }
            if drawn {
                damaged.push(instance.output.clone());
            }
        }
        (damaged, exclusive_changed)
    }

    /// How soon Slint needs the event loop to wake up, for animations and timers.
    pub fn next_wakeup(&self) -> Option<Duration> {
        let animating = self
            .instances
            .iter()
            .flat_map(ShellInstance::surfaces)
            .any(|s| s.window.has_active_animations());
        let timer = slint::platform::duration_until_next_timer_update();
        match (animating, timer) {
            (true, Some(t)) => Some(t.min(ANIMATION_INTERVAL)),
            (true, None) => Some(ANIMATION_INTERVAL),
            (false, t) => t,
        }
    }

    /// The shell's element for `output`: its lock screen while locked, otherwise its view.
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
        let Metrics { physical, logical, .. } = instance.metrics;
        shell_element(renderer, &instance.front().buffer, physical, logical)
            .map_err(|err| tracing::debug!("cannot upload the shell: {err:?}"))
            .ok()
    }
}

/// Draws the whole `physical`-sized shell buffer over the output's `logical` area.
fn shell_element<R>(
    renderer: &mut R,
    buffer: &MemoryRenderBuffer,
    physical: Size<i32, Physical>,
    logical: Size<i32, Logical>,
) -> Result<MemoryRenderBufferRenderElement<R>, R::Error>
where
    R: Renderer + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    // Smithay converts `src` to buffer coordinates at the buffer's scale of 1, so it takes physical numbers.
    let src =
        Rectangle::<f64, Logical>::from_size((f64::from(physical.w), f64::from(physical.h)).into());
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        Point::<f64, Physical>::from((0.0, 0.0)),
        buffer,
        None,
        Some(src),
        Some(logical),
        Kind::Unspecified,
    )
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
        if self.nimbus.is_locked() && !allowed_while_locked(&action) {
            tracing::warn!("ignoring a shell action while locked: {action:?}");
            return;
        }
        match action {
            ShellAction::Compositor(request) => {
                if let Response::Error { message } = self.handle_request(request) {
                    tracing::warn!("shell request failed: {message}");
                }
            }
            ShellAction::Service(command) => {
                let command = with_activation_token(command, || {
                    let (token, _) = self.nimbus.xdg_activation_state.create_external_token(None);
                    token.as_str().to_owned()
                });
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
                    self.show_error("Couldn't open Settings".into(), err.to_string());
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
            self.show_error(
                format!("Couldn't start {id}"),
                "The application isn't installed.".into(),
            );
            return;
        };
        let (token, _) = self.nimbus.xdg_activation_state.create_external_token(None);
        let token = token.as_str().to_owned();
        if let Err(err) = nimbus_xdg::launch(&entry, &[], Some(&token)) {
            tracing::warn!("cannot launch '{id}': {err}");
            self.show_error(format!("Couldn't start {}", entry.name), err.to_string());
        }
    }

    fn show_error(&self, summary: String, body: String) {
        if let Some(shell) = self.nimbus.shell.as_ref() {
            shell.show_error(summary, body);
        }
    }

    /// Applies the outcome of a lock screen password check.
    pub fn unlock_result(&mut self, ok: bool) {
        if ok {
            self.unlock_from_shell();
        } else if let Some(shell) = self.nimbus.shell.as_ref() {
            shell.unlock_failed();
            self.nimbus.queue_redraw_all();
        }
    }

    /// Unlocks the session for the shell's lock screen, unless an ext-session-lock client holds the lock.
    fn unlock_from_shell(&mut self) {
        if self.nimbus.shell_draws_lock() {
            self.unlock_session();
        }
    }

    pub fn handle_service_event(&mut self, event: ServiceEvent) {
        if event == ServiceEvent::LogoutRequested {
            // Exit status 0 tells nimbus-session that the user logged out.
            tracing::info!("logging out");
            self.nimbus.stop();
            return;
        }
        if event == ServiceEvent::LockRequested {
            self.lock_session();
            let outputs = self.nimbus.outputs().map(|o| o.name()).collect();
            if let Some(shell) = self.nimbus.shell.as_mut() {
                shell.await_lock_presented(outputs);
            }
            return;
        }
        if event == ServiceEvent::UnlockRequested {
            self.unlock_from_shell();
            return;
        }
        let Some(shell) = self.nimbus.shell.as_mut() else {
            return;
        };
        match &event {
            ServiceEvent::State(system) => shell.system_state = Some(system.clone()),
            ServiceEvent::LockRequested
            | ServiceEvent::UnlockRequested
            | ServiceEvent::LogoutRequested
            | ServiceEvent::Notification(_)
            | ServiceEvent::NotificationClosed { .. } => {}
        }
        shell.model.handle_service_event(&event);
    }
}

/// The lock screen only locks again and closes notifications; everything else waits for an unlock.
fn allowed_while_locked(action: &ShellAction) -> bool {
    matches!(
        action,
        ShellAction::Compositor(nimbus_ipc::Request::Lock)
            | ShellAction::Service(
                ServiceCommand::LockSession | ServiceCommand::CloseNotification { .. }
            )
    )
}

/// Gives a notification action an xdg-activation token from `mint`, so the client can raise its window.
fn with_activation_token(command: ServiceCommand, mint: impl FnOnce() -> String) -> ServiceCommand {
    match command {
        ServiceCommand::InvokeNotificationAction { id, action } => {
            ServiceCommand::InvokeNotificationActionWithToken {
                id,
                action,
                activation_token: mint(),
            }
        }
        command => command,
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
    use nimbus_services::CloseReason;
    use smithay::backend::renderer::element::Element;
    use smithay::backend::renderer::pixman::PixmanRenderer;
    use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};
    use smithay::utils::Scale as UtilScale;

    impl ShellHost {
        fn for_tests() -> Self {
            Self::with_platform(&nimbus_config::Config::default(), None)
                .expect("no platform was set on this thread")
        }

        fn add_test_output(&mut self, output: &Output) {
            self.add_output(output, &CompositorState::default());
        }
    }

    fn output(name: &str, (w, h): (i32, i32), scale: f64) -> Output {
        let output = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Test".into(),
                model: "Test".into(),
            },
        );
        let mode = Mode { size: (w, h).into(), refresh: 60_000 };
        output.change_current_state(Some(mode), None, Some(Scale::Fractional(scale)), None);
        output
    }

    fn locked_instances(host: &ShellHost) -> Vec<bool> {
        host.instances.iter().map(|i| i.lock.is_some()).collect()
    }

    #[test]
    fn outputs_added_while_locked_are_locked() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.set_locked(true);
        host.add_test_output(&output("B", (800, 600), 1.0));
        assert_eq!(locked_instances(&host), [true, true]);
    }

    #[test]
    fn losing_every_output_keeps_the_session_locked() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.set_locked(true);
        host.remove_output("A");
        assert!(host.model.is_locked());
        host.add_test_output(&output("A", (800, 600), 1.0));
        assert!(host.model.is_locked());
        assert_eq!(locked_instances(&host), [true]);
    }

    #[test]
    fn unlocking_removes_every_lock_screen() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.add_test_output(&output("B", (800, 600), 1.0));
        host.set_locked(true);
        let locks: Vec<_> = host
            .instances
            .iter()
            .filter_map(|i| i.lock.as_ref())
            .map(|(lock, _)| slint::ComponentHandle::as_weak(lock.component()))
            .collect();
        assert_eq!(locks.len(), 2);
        host.set_locked(false);
        assert_eq!(locked_instances(&host), [false, false]);
        assert!(!host.model.is_locked());
        assert!(locks.iter().all(|lock| lock.upgrade().is_none()), "lock screens are freed");
    }

    #[test]
    fn removed_outputs_free_their_shell() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.set_locked(true);
        let instance = &host.instances[0];
        let view = slint::ComponentHandle::as_weak(instance.view.component());
        let lock = instance
            .lock
            .as_ref()
            .map(|(lock, _)| slint::ComponentHandle::as_weak(lock.component()));
        host.remove_output("A");
        assert!(view.upgrade().is_none());
        assert!(lock.is_some_and(|lock| lock.upgrade().is_none()));
    }

    #[test]
    fn keys_reach_the_lock_screen_instead_of_the_shell() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.set_locked(true);
        host.update();
        for pressed in [true, false] {
            host.dispatch_key("x".into(), pressed, false);
        }
        let (lock, _) = host.instances[0].lock.as_ref().expect("a lock screen");
        assert_eq!(lock.component().get_password(), "x");
    }

    #[test]
    fn odd_sizes_at_fractional_scales_render() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (1001, 601), 2.0));
        host.add_test_output(&output("B", (1367, 769), 1.25));
        host.update();
        host.set_locked(true);
        for instance in &host.instances {
            let physical = instance.metrics.physical;
            for surface in instance.surfaces() {
                let size = surface.window.size();
                assert_eq!((size.width as i32, size.height as i32), (physical.w, physical.h));
            }
        }
    }

    #[test]
    fn keys_go_to_the_lock_screen_the_user_pressed_on() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.add_test_output(&output("B", (800, 600), 1.0));
        host.set_locked(true);
        host.pointer_button("B", (10.0, 10.0).into(), PointerEventButton::Left, true);
        assert_eq!(host.keyboard_instance().map(|i| i.output.as_str()), Some("B"));
    }

    #[test]
    fn one_launcher_at_a_time_and_clicks_elsewhere_release_the_keyboard() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.add_test_output(&output("B", (800, 600), 1.0));
        host.toggle_launcher("A");
        assert!(host.wants_keyboard());
        host.toggle_launcher("B");
        let open: Vec<bool> =
            host.instances.iter().map(|i| i.view.component().get_launcher_open()).collect();
        assert_eq!(open, [false, true]);
        assert_eq!(host.keyboard_instance().map(|i| i.output.as_str()), Some("B"));
        host.focus_output(None);
        assert!(!host.wants_keyboard());
        assert!(host.wants_keyboard_on("B"), "the launcher still covers B");
    }

    #[test]
    fn the_shell_element_covers_the_whole_buffer() {
        let mut renderer = PixmanRenderer::new().expect("pixman");
        let physical = Size::<i32, Physical>::from((2560, 1440));
        let logical = Size::<i32, Logical>::from((1280, 720));
        let buffer =
            MemoryRenderBuffer::new(Fourcc::Abgr8888, (2560, 1440), 1, Transform::Normal, None);
        let element = shell_element(&mut renderer, &buffer, physical, logical).expect("element");
        assert_eq!(element.src(), Rectangle::<f64, Buffer>::from_size((2560.0, 1440.0).into()));
        assert_eq!(element.geometry(UtilScale::from(2.0)).size, physical);
    }

    #[test]
    fn the_lock_screen_blocks_desktop_actions() {
        assert!(!allowed_while_locked(&ShellAction::Launch("org.example.App".into())));
        assert!(!allowed_while_locked(&ShellAction::OpenSettings(None)));
        assert!(!allowed_while_locked(&ShellAction::Service(ServiceCommand::SetWifiEnabled(
            false
        ))));
        assert!(!allowed_while_locked(&ShellAction::Compositor(nimbus_ipc::Request::Spawn {
            command: "sh".into()
        })));
        assert!(allowed_while_locked(&ShellAction::Compositor(nimbus_ipc::Request::Lock)));
    }

    #[test]
    fn lock_presented_waits_for_every_output() {
        let mut host = ShellHost::for_tests();
        host.set_locked(true);
        let live: HashSet<String> = ["A".to_owned(), "B".to_owned(), "C".to_owned()].into();
        host.await_lock_presented(live.clone());
        assert!(!host.frames_presented(&["A"], &live));
        assert!(host.awaits_lock_presented());
        let live: HashSet<String> = ["A".to_owned(), "B".to_owned()].into();
        assert!(host.frames_presented(&["B"], &live), "C was unplugged");
        assert!(!host.awaits_lock_presented());
        assert!(!host.frames_presented(&["A", "B"], &live), "sent only once");
    }

    #[test]
    fn unlocking_cancels_lock_presented() {
        let mut host = ShellHost::for_tests();
        host.set_locked(true);
        let live: HashSet<String> = ["A".to_owned()].into();
        host.await_lock_presented(live.clone());
        host.set_locked(false);
        assert!(!host.frames_presented(&["A"], &live));
        assert!(!host.awaits_lock_presented());
    }

    #[test]
    fn notification_actions_carry_an_activation_token() {
        let invoke = ServiceCommand::InvokeNotificationAction { id: 4, action: "default".into() };
        assert_eq!(
            with_activation_token(invoke, || "token".into()),
            ServiceCommand::InvokeNotificationActionWithToken {
                id: 4,
                action: "default".into(),
                activation_token: "token".into()
            }
        );
        let other = ServiceCommand::CloseNotification { id: 4, reason: CloseReason::Dismissed };
        assert_eq!(
            with_activation_token(other.clone(), || panic!("no token for other commands")),
            other
        );
    }

    #[test]
    fn errors_show_on_every_output() {
        let mut host = ShellHost::for_tests();
        host.add_test_output(&output("A", (800, 600), 1.0));
        host.add_test_output(&output("B", (800, 600), 1.0));
        host.show_error("Summary".into(), "Body".into());
        for instance in &host.instances {
            let desktop =
                slint::ComponentHandle::global::<nimbus_shell::Desktop>(instance.view.component());
            assert_eq!(slint::Model::row_count(&desktop.get_toasts()), 1);
        }
    }

    #[test]
    fn page_names_are_validated() {
        assert!(is_page_name("appearance"));
        assert!(is_page_name("night-light_2"));
        assert!(!is_page_name(""));
        assert!(!is_page_name("--evil"));
        assert!(!is_page_name("a b"));
    }
}
