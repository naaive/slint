// SPDX-License-Identifier: MIT

//! Runs the shell in a 1280x800 window with a mock session: windows that react to the dock and overview,
//! system state that follows quick settings, notifications that arrive over time, and a lock screen
//! in a second window that accepts any password except "wrong".
//! Shell actions are logged to standard error; set `RUST_LOG=debug` for more.

#[path = "../../tests/support/mod.rs"]
mod support;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use nimbus_config::Config;
use nimbus_ipc::{CompositorState, Event, Request, WindowInfo};
use nimbus_services::{CloseReason, Notification, ServiceCommand, ServiceEvent, SystemState};
use nimbus_shell::{LockView, ShellAction, ShellModel, ShellView};
use nimbus_xdg::{AppIndex, IconResolver};
use slint::{LogicalSize, Rgb8Pixel, SharedPixelBuffer, Timer, TimerMode};

/// What the mock session reacts to, queued so that it runs outside the shell's callbacks.
enum Input {
    Action(ShellAction),
    Unlock(String),
}

struct Session {
    shell: ShellModel,
    view: ShellView,
    lock: Option<LockView>,
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
                && let Err(err) = lock.window().hide()
            {
                tracing::warn!("Can't hide the lock screen: {err}");
            }
            return;
        }
        if self.lock.is_some() {
            return;
        }
        let lock = LockView::new(&self.shell).and_then(|lock| {
            lock.window().set_size(LogicalSize::new(1280.0, 800.0));
            lock.show()?;
            Ok(lock)
        });
        match lock {
            Ok(lock) => self.lock = Some(lock),
            Err(err) => tracing::warn!("Can't show the lock screen: {err}"),
        }
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
            Request::ToggleLauncher => self.view.toggle_launcher(),
            Request::ToggleOverview => self.view.toggle_overview(),
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

/// A diagonal gradient from indigo through violet, small enough to generate instantly; the shell scales it smoothly.
fn wallpaper() -> slint::Image {
    let (width, height) = (320u32, 200u32);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(width, height);
    let stops: [(f32, [u8; 3]); 3] =
        [(0.0, [0x26, 0x34, 0x6e]), (0.55, [0x5a, 0x2d, 0x7a]), (1.0, [0x12, 0x18, 0x3a])];
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        let t = (x as f32 / width as f32 * 0.6 + y as f32 / height as f32 * 0.4).clamp(0.0, 1.0);
        let segment = stops.windows(2).find(|w| t <= w[1].0).unwrap_or(&stops[1..3]);
        let local = (t - segment[0].0) / (segment[1].0 - segment[0].0);
        let channel = |c: usize| {
            let (a, b) = (f32::from(segment[0].1[c]), f32::from(segment[1].1[c]));
            (a + (b - a) * local).round() as u8
        };
        *pixel = Rgb8Pixel { r: channel(0), g: channel(1), b: channel(2) };
    }
    slint::Image::from_rgb8(pixels)
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

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
    let view = ShellView::new(&shell, support::OUTPUT)?;
    view.component().set_wallpaper(wallpaper());
    view.window().set_size(LogicalSize::new(1280.0, 800.0));
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
        view,
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

    session.borrow().view.show()?;
    tracing::info!("Nimbus shell preview: click the panel, the dock, or the activities button");
    slint::run_event_loop()?;
    drop((pump, notifiers, battery));
    if let Err(err) = std::fs::remove_dir_all(&scratch) {
        tracing::debug!("Can't remove {}: {err}", scratch.display());
    }
    Ok(())
}
