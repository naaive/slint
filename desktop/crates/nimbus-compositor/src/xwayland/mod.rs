// SPDX-License-Identifier: MIT

//! X11 apps through `xwayland-satellite`, which starts when the first X11 client connects.
//!
//! The compositor holds the X11 display's listening sockets and hands them to the satellite with `-listenfd`,
//! so clients can connect before it runs and while it restarts.

mod display;

pub use display::parse_display;

use crate::state::State;
use display::X11Display;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use std::cell::RefCell;
use std::io;
use std::os::fd::{AsFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Overrides `/tmp` as the directory of the X11 sockets and lock files, for tests.
pub const DIR_ENV: &str = "NIMBUS_X11_DIR";

const SATELLITE: &str = "xwayland-satellite";

/// How often a running satellite is checked for having exited.
const REAP_INTERVAL: Duration = Duration::from_secs(1);

/// The X11 display while the compositor runs; dropping it stops the satellite and frees the display.
pub struct Xwayland {
    satellite: Rc<RefCell<Satellite>>,
    display_name: String,
}

struct Satellite {
    program: PathBuf,
    /// `None` once the satellite failed to start, which closes the sockets.
    display: Option<X11Display>,
    handle: LoopHandle<'static, State>,
    /// The sources watching the listening sockets, enabled while no satellite runs.
    listening: Vec<RegistrationToken>,
    running: Option<(Child, RegistrationToken)>,
}

impl Xwayland {
    /// Takes an X11 display, `preferred` when it's free, unless `config` disables it or `xwayland-satellite` can't serve it.
    pub fn start(
        config: &nimbus_config::Xwayland,
        preferred: Option<u32>,
        handle: &LoopHandle<'static, State>,
    ) -> Option<Self> {
        if !config.enabled {
            tracing::info!("XWayland is disabled; X11 apps are unavailable");
            return None;
        }
        let program = config.path.clone().unwrap_or_else(|| SATELLITE.into());
        match supports_listenfd(&program) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    "{} doesn't support -listenfd, which needs version 0.6 or later; X11 apps are unavailable",
                    program.display()
                );
                return None;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                tracing::info!("{} isn't installed; X11 apps are unavailable", program.display());
                return None;
            }
            Err(err) => {
                tracing::warn!("cannot run {}: {err}; X11 apps are unavailable", program.display());
                return None;
            }
        }
        let dir = std::env::var_os(DIR_ENV).map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let display = match X11Display::allocate(&dir, preferred) {
            Ok(display) => display,
            Err(err) => {
                tracing::warn!("cannot create an X11 display: {err}; X11 apps are unavailable");
                return None;
            }
        };
        let display_name = display.name();
        let satellite = Rc::new(RefCell::new(Satellite {
            program,
            display: None,
            handle: handle.clone(),
            listening: Vec::new(),
            running: None,
        }));
        for listener in display.listeners() {
            let source =
                listener.try_clone().map(|fd| Generic::new(fd, Interest::READ, Mode::Level));
            let token = source.map_err(|err| err.to_string()).and_then(|source| {
                let satellite = satellite.clone();
                handle
                    .insert_source(source, move |_, _, _| {
                        Satellite::spawn(&satellite);
                        Ok(PostAction::Continue)
                    })
                    .map_err(|err| err.to_string())
            });
            match token {
                Ok(token) => satellite.borrow_mut().listening.push(token),
                Err(err) => {
                    tracing::warn!("cannot watch the X11 sockets: {err}; X11 apps are unavailable");
                    satellite.borrow_mut().stop();
                    return None;
                }
            }
        }
        tracing::info!("X11 apps connect to DISPLAY={display_name}");
        satellite.borrow_mut().display = Some(display);
        Some(Self { satellite, display_name })
    }

    /// The name for `DISPLAY`, such as `:1`.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

impl Drop for Xwayland {
    fn drop(&mut self) {
        self.satellite.borrow_mut().stop();
    }
}

impl Satellite {
    /// Starts the satellite on the listening sockets, and stops watching them until it exits.
    fn spawn(this: &Rc<RefCell<Self>>) {
        let mut satellite = this.borrow_mut();
        let Some(display) = &satellite.display else {
            return;
        };
        let (name, fds) = (display.name(), display.raw_fds());
        let child = match spawn_with_fds(&satellite.program, &name, fds) {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(
                    "cannot start {}: {err}; X11 apps are unavailable",
                    satellite.program.display()
                );
                satellite.stop();
                return;
            }
        };
        tracing::info!(pid = child.id(), "started {} for {name}", satellite.program.display());
        for token in &satellite.listening {
            if let Err(err) = satellite.handle.disable(token) {
                tracing::warn!("cannot pause the X11 sockets: {err}");
            }
        }
        let reaper = this.clone();
        let timer =
            satellite.handle.insert_source(Timer::from_duration(REAP_INTERVAL), move |_, _, _| {
                if reaper.borrow_mut().reap() {
                    TimeoutAction::Drop
                } else {
                    TimeoutAction::ToDuration(REAP_INTERVAL)
                }
            });
        match timer {
            Ok(timer) => satellite.running = Some((child, timer)),
            Err(err) => {
                tracing::warn!("cannot watch {}: {err}", satellite.program.display());
                drop(child);
                satellite.stop();
            }
        }
    }

    /// Returns whether the satellite exited, in which case the next X11 client starts it again.
    fn reap(&mut self) -> bool {
        let Some((child, _)) = &mut self.running else {
            return true;
        };
        let status = match child.try_wait() {
            Ok(None) => return false,
            Ok(Some(status)) => status.to_string(),
            Err(err) => err.to_string(),
        };
        tracing::info!("{} exited: {status}", self.program.display());
        self.running = None;
        for token in &self.listening {
            if let Err(err) = self.handle.enable(token) {
                tracing::warn!("cannot watch the X11 sockets: {err}");
            }
        }
        true
    }

    /// Stops the satellite and closes the X11 display for good.
    fn stop(&mut self) {
        if let Some((child, timer)) = self.running.take() {
            self.handle.remove(timer);
            terminate(child);
        }
        for token in self.listening.drain(..) {
            self.handle.remove(token);
        }
        self.display = None;
    }
}

/// Starts `program` for display `name`, with the listening sockets `fds` open in it.
fn spawn_with_fds(program: &Path, name: &str, fds: [RawFd; 2]) -> io::Result<Child> {
    let mut command = Command::new(program);
    command.arg(name);
    for fd in fds {
        command.arg("-listenfd").arg(fd.to_string());
    }
    // Standard output carries the compositor's ready line.
    let stdout =
        io::stderr().as_fd().try_clone_to_owned().map_or_else(|_| Stdio::null(), Stdio::from);
    command.stdin(Stdio::null()).stdout(stdout);
    // SAFETY: `fcntl` is async-signal-safe, and the closure doesn't allocate.
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Asks `program` whether it takes listening sockets, which `xwayland-satellite` 0.6 and later do.
///
/// Without a Wayland display, older versions exit with an error instead of starting an X server.
fn supports_listenfd(program: &Path) -> io::Result<bool> {
    let mut child = Command::new(program)
        .args([":0", "--test-listenfd-support"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= deadline {
            terminate(child);
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn terminate(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}
