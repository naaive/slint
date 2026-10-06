// SPDX-License-Identifier: MIT

//! The Nimbus Wayland compositor.

mod actions;
mod auth;
mod backend;
mod config;
mod cursor;
mod input;
mod ipc;
mod keybindings;
mod lock;
mod lock_marker;
mod process;
mod render;
mod shell_host;
mod state;
mod wm;

use anyhow::{Context, anyhow};
use backend::Backend;
use clap::{Parser, ValueEnum};
use config::ConfigManager;
use ipc::IpcServer;
use shell_host::ShellHost;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::socket::ListeningSocketSource;
use state::{ClientState, Nimbus, NimbusInit, State};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum BackendKind {
    /// A window inside an existing Wayland or X11 session.
    Winit,
    /// DRM/KMS and libinput on a TTY.
    Udev,
    /// No devices; renders into memory.
    Headless,
}

/// The Nimbus Wayland compositor.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Where to display; defaults to winit inside another session and udev on a TTY.
    #[arg(long, value_enum)]
    backend: Option<BackendKind>,
    /// Name of the Wayland socket in $XDG_RUNTIME_DIR, such as wayland-1; picked automatically by default.
    #[arg(long)]
    socket: Option<String>,
    /// Configuration file; defaults to $XDG_CONFIG_HOME/nimbus/config.toml.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Run without the desktop shell (panel, dock, launcher) and its system services.
    #[arg(long)]
    no_shell: bool,
    /// Start locked; nimbus-session passes this after a crash while locked.
    /// The compositor also starts locked when the lock marker in $XDG_RUNTIME_DIR/nimbus exists.
    #[arg(long)]
    locked: bool,
    /// Headless backend only: write a PNG of every output to this directory after a few frames.
    #[arg(long)]
    screenshot_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(filter)
        .init();

    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Backend resources acquired before the compositor advertises its own environment.
enum PreparedBackend {
    Winit(Box<backend::winit::WinitWindow>),
    Udev(
        smithay::backend::session::libseat::LibSeatSession,
        smithay::backend::session::libseat::LibSeatSessionNotifier,
    ),
    Headless,
}

fn default_backend() -> BackendKind {
    let nested =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    if nested { BackendKind::Winit } else { BackendKind::Udev }
}

fn run(args: Args) -> anyhow::Result<()> {
    let kind = args.backend.unwrap_or_else(default_backend);
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .context("XDG_RUNTIME_DIR must name an existing directory")?;
    let lock_marker = lock_marker::LockMarker::new(&runtime_dir);
    let start_locked = args.locked || lock_marker.is_present();
    if start_locked {
        tracing::info!("starting locked");
    }
    let mut config = ConfigManager::load(args.config.clone());

    let mut event_loop: EventLoop<'static, State> =
        EventLoop::try_new().context("cannot create the event loop")?;
    let handle = event_loop.handle();
    let display: Display<State> = Display::new().context("cannot create the Wayland display")?;
    let display_handle = display.handle();

    // The nested window connects to the parent session, so it must open before our variables replace its.
    let (prepared, seat_name) = match kind {
        BackendKind::Winit => {
            (PreparedBackend::Winit(Box::new(backend::winit::open_window()?)), "winit".to_owned())
        }
        BackendKind::Udev => {
            use smithay::backend::session::Session;
            let (session, notifier) = backend::udev::open_session()?;
            let seat = session.seat();
            (PreparedBackend::Udev(session, notifier), seat)
        }
        BackendKind::Headless => (PreparedBackend::Headless, "headless".to_owned()),
    };

    let socket = match &args.socket {
        Some(name) => ListeningSocketSource::with_name(name),
        None => ListeningSocketSource::new_auto(),
    }
    .map_err(|e| anyhow!("cannot create the Wayland socket: {e}"))?;
    let socket_name = socket.socket_name().to_string_lossy().into_owned();
    handle
        .insert_source(socket, |stream, _, state| {
            let data = Arc::new(ClientState::default());
            if let Err(err) = state.nimbus.display_handle.insert_client(stream, data) {
                tracing::warn!("cannot accept a Wayland client: {err}");
            }
        })
        .map_err(|e| anyhow!("cannot watch the Wayland socket: {e}"))?;
    handle
        .insert_source(Generic::new(display, Interest::READ, Mode::Level), |_, display, state| {
            // SAFETY: the display is never dropped while the source is registered.
            unsafe { display.get_mut() }.dispatch_clients(state).map_err(std::io::Error::other)?;
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow!("cannot watch the Wayland display: {e}"))?;

    let ipc_path = nimbus_ipc::socket_path_for(&runtime_dir, &socket_name);
    let ipc = IpcServer::bind(ipc_path.clone(), &handle)?;

    // SAFETY: no other threads exist yet; services, scanners, and watchers start below.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
        std::env::set_var(nimbus_ipc::SOCKET_ENV, &ipc_path);
        std::env::set_var("XDG_CURRENT_DESKTOP", nimbus_xdg::DESKTOP_NAME);
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        std::env::remove_var("WAYLAND_SOCKET");
    }

    let shell = if args.no_shell {
        None
    } else {
        let mut shell =
            ShellHost::new(&handle, config.current(), config.path().map(Path::to_path_buf))?;
        // Before any output exists, so every shell starts with the lock screen and no frame shows the desktop.
        shell.set_locked(start_locked);
        Some(shell)
    };
    config.watch(&handle);
    let mut nimbus = Nimbus::new(NimbusInit {
        display_handle,
        loop_handle: handle.clone(),
        loop_signal: event_loop.get_signal(),
        seat_name,
        config,
        ipc,
        lock_marker,
        locked: start_locked,
    });
    nimbus.shell = shell;

    let backend = match prepared {
        PreparedBackend::Winit(window) => Backend::Winit(Box::new(
            backend::winit::WinitBackend::new(*window, &mut nimbus, &handle)?,
        )),
        PreparedBackend::Udev(session, notifier) => Backend::Udev(Box::new(
            backend::udev::UdevBackend::new(session, notifier, &mut nimbus, &handle)?,
        )),
        PreparedBackend::Headless => {
            let sizes = backend::headless::parse_outputs(
                std::env::var(backend::headless::OUTPUTS_ENV).ok().as_deref(),
            );
            let screenshot =
                backend::headless::ScreenshotRequest::from_env(args.screenshot_dir.clone());
            Backend::Headless(backend::headless::HeadlessBackend::new(
                &mut nimbus,
                &sizes,
                screenshot,
            )?)
        }
    };
    let mut state = State { backend, nimbus };
    start_housekeeping(&mut state)?;

    state.nimbus.sync_lock_marker();
    state.nimbus.arrange();

    tracing::info!(backend = ?kind, socket = %socket_name, control = %ipc_path.display(), "Nimbus is ready");
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "NIMBUS_READY WAYLAND_DISPLAY={socket_name} NIMBUS_SOCKET={}",
        ipc_path.display()
    )
    .and_then(|()| stdout.flush())
    .context("cannot report readiness")?;
    drop(stdout);

    while state.nimbus.running {
        let timeout = state.next_timeout();
        event_loop.dispatch(timeout, &mut state).context("the event loop failed")?;
        state.post_dispatch();
    }
    tracing::info!("shutting down");
    state.nimbus.ipc.flush_all();
    Ok(())
}

/// Reaps children and locks the session after the configured idle time.
fn start_housekeeping(state: &mut State) -> anyhow::Result<()> {
    let handle = state.nimbus.loop_handle.clone();
    handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, state| {
            state.nimbus.children.reap();
            // Visibility changes, such as minimizing or switching workspaces, can end an inhibition.
            state.nimbus.refresh_idle_inhibit();
            if let Some(timeout) = state.lock_timeout()
                && state.nimbus.last_activity.elapsed() >= timeout
                && !state.nimbus.idle_inhibited()
                && !state.nimbus.is_locked()
                && state.nimbus.shell.is_some()
            {
                tracing::info!("locking after {timeout:?} of inactivity");
                state.lock_session();
            }
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow!("cannot start housekeeping: {e}"))?;
    Ok(())
}

impl State {
    /// How long the event loop may sleep before Slint or the backend's frame clock needs it.
    fn next_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let backend =
            self.backend.next_deadline(&self.nimbus).map(|t| t.saturating_duration_since(now));
        let shell = self.nimbus.shell.as_ref().and_then(ShellHost::next_wakeup);
        match (backend, shell) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl Nimbus {
    pub fn stop(&mut self) {
        self.running = false;
        self.loop_signal.stop();
    }
}
