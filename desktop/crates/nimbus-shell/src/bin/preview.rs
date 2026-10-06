// SPDX-License-Identifier: MIT

//! Runs the shell in a 1280x800 window with a mock session: windows that react to the dock and overview,
//! system state that follows quick settings, notifications that arrive over time, and a lock screen
//! that accepts any password except "wrong".
//! The shell's parts render off screen, and the window shows them composited the way the compositor arranges them.
//! Shell actions are logged to standard error; set `RUST_LOG=debug` for more.

#[path = "../../tests/support/mod.rs"]
mod support;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use nimbus_config::Config;
use nimbus_ipc::{CompositorState, Event, Request, WindowInfo};
use nimbus_services::{CloseReason, Notification, ServiceCommand, ServiceEvent, SystemState};
use nimbus_shell::{LockView, ShellAction, ShellModel, ShellView};
use nimbus_xdg::{AppIndex, IconResolver};
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, PlatformError, Rgb8Pixel, SharedPixelBuffer, SharedString,
    Timer, TimerMode,
};
use support::desk::{self, Desk, Software};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

slint::slint! {
    // Shows the composited shell, and passes on the input it receives.
    export component Screen inherits Window {
        in property <image> frame;

        callback pointer-moved(x: length, y: length);
        callback pointer-button(x: length, y: length, button: PointerEventButton, pressed: bool);
        callback scrolled(x: length, y: length, delta-x: length, delta-y: length);
        callback key(text: string, pressed: bool);

        width: 1280px;
        height: 800px;
        title: "Nimbus Shell Preview";

        Image {
            width: 100%;
            height: 100%;
            source: root.frame;
        }

        FocusScope {
            key-pressed(event) => {
                root.key(event.text, true);
                accept
            }
            key-released(event) => {
                root.key(event.text, false);
                accept
            }

            TouchArea {
                changed mouse-x => {
                    root.pointer-moved(self.mouse-x, self.mouse-y);
                }
                changed mouse-y => {
                    root.pointer-moved(self.mouse-x, self.mouse-y);
                }
                pointer-event(event) => {
                    if event.kind == PointerEventKind.down || event.kind == PointerEventKind.up {
                        root.pointer-button(self.mouse-x, self.mouse-y, event.button, event.kind == PointerEventKind.down);
                    }
                }
                scroll-event(event) => {
                    root.scrolled(self.mouse-x, self.mouse-y, event.delta-x, event.delta-y);
                    accept
                }
            }
        }
    }
}

thread_local! {
    /// The next window is the preview's own.
    static PREVIEW_SCREEN: Cell<bool> = const { Cell::new(false) };
}

/// The winit backend for the preview's own window, and off-screen windows for the shell's.
struct PreviewPlatform {
    winit: i_slint_backend_winit::Backend,
    software: Rc<Software>,
}

impl Platform for PreviewPlatform {
    fn bind_context(
        &self,
        context: i_slint_core::SlintContextWeak,
        token: i_slint_core::InternalToken,
    ) {
        self.winit.bind_context(context, token);
    }

    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        if PREVIEW_SCREEN.take() {
            self.winit.create_window_adapter()
        } else {
            Ok(self.software.create_window())
        }
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        self.winit.run_event_loop()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        self.winit.new_event_loop_proxy()
    }
}

/// A lock screen's window, which takes the whole preview while locked.
struct Lock {
    view: LockView,
    window: Rc<MinimalSoftwareWindow>,
    pixels: RefCell<Vec<PremultipliedRgbaColor>>,
}

/// What the mock session reacts to, queued so that it runs outside the shell's callbacks.
enum Input {
    Action(ShellAction),
    Unlock(String),
}

struct Session {
    shell: ShellModel,
    desk: Desk,
    software: Rc<Software>,
    lock: Option<Lock>,
    compositor: CompositorState,
    system: SystemState,
    apps: Rc<AppIndex>,
    next_window: u64,
}

impl Session {
    fn apply(&mut self, input: Input) {
        match input {
            Input::Action(action) => {
                tracing::info!(?action, "shell action");
                match action {
                    ShellAction::Compositor(request) => self.compositor_request(request),
                    ShellAction::Service(command) => self.service_command(command),
                    ShellAction::Launch(id) => self.launch(&id),
                    ShellAction::OpenSettings(page) => tracing::info!(?page, "would open Settings"),
                }
            }
            Input::Unlock(password) => {
                if password == "wrong" {
                    self.shell.unlock_failed();
                } else {
                    self.set_locked(false);
                }
            }
        }
    }

    fn set_locked(&mut self, locked: bool) {
        self.shell.set_locked(locked);
        if !locked {
            if let Some(lock) = self.lock.take()
                && let Err(err) = lock.view.window().hide()
            {
                tracing::warn!("Can't hide the lock screen: {err}");
            }
            return;
        }
        if self.lock.is_some() {
            return;
        }
        let lock = LockView::new(&self.shell).and_then(|view| {
            let window = self.software.take_created().ok_or("the lock screen has no window")?;
            view.window().set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
            view.show()?;
            Ok(Lock { view, window, pixels: RefCell::default() })
        });
        match lock {
            Ok(lock) => self.lock = Some(lock),
            Err(err) => tracing::warn!("Can't show the lock screen: {err}"),
        }
    }

    /// The window that takes input: the lock screen while locked, or else the shell's parts.
    fn lock_window(&self) -> Option<&slint::Window> {
        self.lock.as_ref().map(|lock| lock.view.window())
    }

    /// Renders what changed, and returns the new image of the whole preview if anything did.
    fn render(&self, backdrop: &[[f32; 3]]) -> Option<slint::Image> {
        let pixels = match &self.lock {
            Some(lock) => {
                slint::platform::update_timers_and_animations();
                let mut pixels = lock.pixels.borrow_mut();
                if !desk::draw(&lock.window, &mut pixels) {
                    return None;
                }
                pixels.iter().map(|p| [p.red, p.green, p.blue]).collect()
            }
            None => {
                if !self.desk.draw(false) {
                    return None;
                }
                self.desk.composite(|x, y| backdrop[(y * WIDTH + x) as usize])
            }
        };
        let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(WIDTH, HEIGHT);
        for (pixel, [r, g, b]) in buffer.make_mut_slice().iter_mut().zip(pixels) {
            *pixel = Rgb8Pixel { r, g, b };
        }
        Some(slint::Image::from_rgb8(buffer))
    }

    fn window_mut(&mut self, id: u64) -> Option<&mut WindowInfo> {
        self.compositor.windows.iter_mut().find(|w| w.id == id)
    }

    fn changed(&self, id: u64) {
        if let Some(window) = self.compositor.windows.iter().find(|w| w.id == id) {
            self.shell.handle_compositor_event(&Event::WindowChanged(window.clone()));
        }
    }

    fn focus(&mut self, id: Option<u64>) {
        let mut touched = Vec::new();
        for window in &mut self.compositor.windows {
            let focused = Some(window.id) == id;
            if window.focused != focused {
                window.focused = focused;
                touched.push(window.id);
            }
        }
        // The newly focused window last, so the shell sees the final focus.
        touched.sort_by_key(|w| Some(*w) == id);
        for window in touched {
            self.changed(window);
        }
    }

    fn switch_workspace(&mut self, workspace: u32) {
        if workspace < self.compositor.workspace_count {
            self.compositor.active_workspace = workspace;
            self.shell.handle_compositor_event(&Event::WorkspaceActivated { workspace });
        }
    }

    fn compositor_request(&mut self, request: Request) {
        match request {
            Request::Activate { id } => {
                let Some(window) = self.window_mut(id) else {
                    return;
                };
                window.minimized = false;
                let workspace = window.workspace;
                self.switch_workspace(workspace);
                self.focus(Some(id));
            }
            Request::Close { id } => {
                self.compositor.windows.retain(|w| w.id != id);
                self.shell.handle_compositor_event(&Event::WindowClosed { id });
            }
            Request::SetMinimized { id, minimized } => {
                if let Some(window) = self.window_mut(id) {
                    window.minimized = minimized;
                    window.focused &= !minimized;
                    self.changed(id);
                }
            }
            Request::MoveToWorkspace { id, workspace } => {
                if let Some(window) = self.window_mut(id) {
                    window.workspace = workspace;
                    self.changed(id);
                }
            }
            Request::SwitchWorkspace { workspace } => self.switch_workspace(workspace),
            Request::Lock => self.set_locked(true),
            Request::ToggleLauncher => self.desk.view.toggle_launcher(),
            Request::ToggleOverview => self.desk.view.toggle_overview(),
            other => tracing::info!(?other, "not handled by the preview"),
        }
    }

    fn service_command(&mut self, command: ServiceCommand) {
        let system = &mut self.system;
        match command {
            ServiceCommand::SetVolume(volume) => {
                if let Some(audio) = &mut system.audio {
                    audio.volume = volume;
                }
            }
            ServiceCommand::ToggleMute => {
                if let Some(audio) = &mut system.audio {
                    audio.muted = !audio.muted;
                }
            }
            ServiceCommand::SetBrightness(level) => system.brightness = Some(level),
            ServiceCommand::SetWifiEnabled(enabled) => {
                system.network.wifi_enabled = enabled;
                system.network.kind = if enabled {
                    nimbus_services::ConnectionKind::Wifi
                } else {
                    nimbus_services::ConnectionKind::None
                };
            }
            ServiceCommand::SetBluetoothPowered(powered) => {
                if let Some(bluetooth) = &mut system.bluetooth {
                    bluetooth.powered = powered;
                }
            }
            ServiceCommand::SetDoNotDisturb(on) => system.do_not_disturb = on,
            ServiceCommand::MediaPlayPause => {
                if let Some(media) = &mut system.media {
                    media.playing = !media.playing;
                }
            }
            ServiceCommand::MediaNext | ServiceCommand::MediaPrevious => {
                if let Some(media) = &mut system.media {
                    media.title = if media.title == "Midnight City" {
                        "Outro".into()
                    } else {
                        "Midnight City".into()
                    };
                }
            }
            ServiceCommand::InvokeNotificationAction { id, .. }
            | ServiceCommand::CloseNotification { id, .. } => {
                self.shell.handle_service_event(&ServiceEvent::NotificationClosed {
                    id,
                    reason: CloseReason::Dismissed,
                });
                return;
            }
            ServiceCommand::LockSession => {
                self.set_locked(true);
                return;
            }
            other => {
                tracing::info!(?other, "the preview doesn't power off or log out");
                return;
            }
        }
        self.shell.handle_service_event(&ServiceEvent::State(self.system.clone()));
    }

    fn launch(&mut self, id: &str) {
        let title = self.apps.get(id).map_or_else(|| id.to_owned(), |entry| entry.name.clone());
        let window = WindowInfo {
            id: self.next_window,
            app_id: id.to_owned(),
            title,
            workspace: self.compositor.active_workspace,
            output: support::OUTPUT.into(),
            ..Default::default()
        };
        self.next_window += 1;
        self.compositor.windows.push(window.clone());
        self.shell.handle_compositor_event(&Event::WindowOpened(window.clone()));
        self.focus(Some(window.id));
    }
}

/// A diagonal gradient from indigo through violet, behind the shell.
fn wallpaper() -> Vec<[f32; 3]> {
    let stops: [(f32, [u8; 3]); 3] =
        [(0.0, [0x26, 0x34, 0x6e]), (0.55, [0x5a, 0x2d, 0x7a]), (1.0, [0x12, 0x18, 0x3a])];
    (0..WIDTH * HEIGHT)
        .map(|i| {
            let (x, y) = (i % WIDTH, i / WIDTH);
            let t =
                (x as f32 / WIDTH as f32 * 0.6 + y as f32 / HEIGHT as f32 * 0.4).clamp(0.0, 1.0);
            let segment = stops.windows(2).find(|w| t <= w[1].0).unwrap_or(&stops[1..3]);
            let local = (t - segment[0].0) / (segment[1].0 - segment[0].0);
            std::array::from_fn(|c| {
                let (a, b) = (f32::from(segment[0].1[c]), f32::from(segment[1].1[c]));
                a + (b - a) * local
            })
        })
        .collect()
}

fn apps(config: &Config, mock_dir: &std::path::Path) -> (AppIndex, IconResolver) {
    let apps = AppIndex::scan();
    if !apps.entries.is_empty() {
        return (apps, IconResolver::new(&config.appearance.icon_theme));
    }
    match support::apps(mock_dir) {
        Ok(mock) => mock,
        Err(err) => {
            tracing::warn!("Can't write the mock applications: {err}");
            (AppIndex::default(), IconResolver::new(&config.appearance.icon_theme))
        }
    }
}

/// Opens the preview's own window, which passes its input on to the lock screen or the shell's parts.
fn show_screen(session: &Rc<RefCell<Session>>) -> Result<Screen, PlatformError> {
    PREVIEW_SCREEN.set(true);
    let screen = Screen::new()?;

    let weak = Rc::downgrade(session);
    screen.on_pointer_moved(move |x, y| {
        let Some(session) = weak.upgrade() else { return };
        let session = session.borrow();
        let position = LogicalPosition::new(x, y);
        match session.lock_window() {
            Some(window) => window.dispatch_event(WindowEvent::PointerMoved { position }),
            None => session.desk.move_pointer(x, y),
        }
    });
    let weak = Rc::downgrade(session);
    screen.on_pointer_button(move |x, y, button, pressed| {
        let Some(session) = weak.upgrade() else { return };
        let session = session.borrow();
        let position = LogicalPosition::new(x, y);
        match session.lock_window() {
            Some(window) => window.dispatch_event(if pressed {
                WindowEvent::PointerPressed { position, button }
            } else {
                WindowEvent::PointerReleased { position, button }
            }),
            None => session.desk.button(x, y, button, pressed),
        }
    });
    let weak = Rc::downgrade(session);
    screen.on_scrolled(move |x, y, delta_x, delta_y| {
        let Some(session) = weak.upgrade() else { return };
        let session = session.borrow();
        let event = |position| WindowEvent::PointerScrolled { position, delta_x, delta_y };
        match session.lock_window() {
            Some(window) => window.dispatch_event(event(LogicalPosition::new(x, y))),
            None => {
                session.desk.pointer(x, y, event);
            }
        }
    });
    let weak = Rc::downgrade(session);
    screen.on_key(move |text: SharedString, pressed| {
        let Some(session) = weak.upgrade() else { return };
        let session = session.borrow();
        match session.lock_window() {
            Some(window) => window.dispatch_event(if pressed {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            }),
            None => session.desk.key(text, pressed),
        }
    });
    screen.show()?;
    Ok(screen)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let software = Software::new();
    let winit = i_slint_backend_winit::Backend::new()?;
    let platform = PreviewPlatform { winit, software: software.clone() };
    slint::platform::set_platform(Box::new(platform))
        .map_err(|err| format!("cannot set the platform: {err}"))?;

    // The preview never touches the user's configuration file.
    let scratch = std::env::temp_dir().join(format!("nimbus-shell-preview-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let mut config = Config::load().unwrap_or_else(|err| {
        tracing::warn!("Using the default configuration: {err}");
        Config::default()
    });
    if config.favorites.is_empty() {
        config.favorites = support::config().favorites;
    }
    let (apps, icons) = apps(&config, &scratch.join("apps"));
    let apps = Rc::new(apps);

    let queue = Rc::new(RefCell::new(VecDeque::new()));
    let sink = queue.clone();
    let shell =
        ShellModel::new(&config, move |action| sink.borrow_mut().push_back(Input::Action(action)));
    shell.set_config_path(scratch.join("config.toml"));
    shell.set_apps(apps.clone(), icons);
    let view = ShellView::new(&shell, support::OUTPUT);
    let desk = Desk::new(view, WIDTH as f32, HEIGHT as f32, Some(software.clone()));
    let unlocks = queue.clone();
    shell
        .on_unlock_attempt(move |password| unlocks.borrow_mut().push_back(Input::Unlock(password)));

    let mut compositor = support::compositor_state();
    // With real applications, the mock windows belong to some of them.
    if apps.get("firefox").is_none() {
        for (window, entry) in compositor.windows.iter_mut().zip(apps.entries.iter().cycle()) {
            window.app_id = entry.id.clone();
        }
    }
    shell.set_compositor_state(&compositor);
    let system = support::system_state();
    shell.handle_service_event(&ServiceEvent::State(system.clone()));
    let next_window = compositor.windows.iter().map(|w| w.id).max().unwrap_or(0) + 1;
    let session = Rc::new(RefCell::new(Session {
        shell,
        desk,
        software,
        lock: None,
        compositor,
        system,
        apps,
        next_window,
    }));

    // Applies queued actions with a short delay, like a compositor answering on its next loop iteration.
    let pump = Timer::default();
    let pump_session = session.clone();
    pump.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        loop {
            let Some(input) = queue.borrow_mut().pop_front() else {
                break;
            };
            pump_session.borrow_mut().apply(input);
        }
    });

    let arrivals: Vec<(u64, Notification)> = support::notifications()
        .into_iter()
        .zip([2, 7, 14])
        .map(|(mut n, delay)| {
            n.received = std::time::SystemTime::now() + Duration::from_secs(delay);
            (delay, n)
        })
        .collect();
    let notifiers: Vec<Timer> = arrivals
        .into_iter()
        .map(|(delay, notification)| {
            let timer = Timer::default();
            let session = session.clone();
            timer.start(TimerMode::SingleShot, Duration::from_secs(delay), move || {
                let mut notification = notification.clone();
                notification.received = std::time::SystemTime::now();
                session
                    .borrow()
                    .shell
                    .handle_service_event(&ServiceEvent::Notification(notification));
            });
            timer
        })
        .collect();

    let battery = Timer::default();
    let battery_session = session.clone();
    battery.start(TimerMode::Repeated, Duration::from_secs(30), move || {
        let mut session = battery_session.borrow_mut();
        if let Some(battery) = &mut session.system.battery {
            battery.level = (battery.level - 0.01).max(0.05);
        }
        let state = session.system.clone();
        session.shell.handle_service_event(&ServiceEvent::State(state));
    });

    let screen = show_screen(&session)?;
    let backdrop = wallpaper();
    let frames = Timer::default();
    let weak = screen.as_weak();
    let frame_session = session.clone();
    frames.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        if let (Some(screen), Some(frame)) =
            (weak.upgrade(), frame_session.borrow().render(&backdrop))
        {
            screen.set_frame(frame);
        }
    });
    tracing::info!("Nimbus shell preview: click the panel, the dock, or the activities button");
    slint::run_event_loop()?;
    drop((pump, notifiers, battery, frames));
    if let Err(err) = std::fs::remove_dir_all(&scratch) {
        tracing::debug!("Can't remove {}: {err}", scratch.display());
    }
    Ok(())
}
