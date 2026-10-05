// SPDX-License-Identifier: MIT

//! Starts the compositor, waits for it to become ready, runs autostart, and restarts it after crashes.

use crate::autostart::Launch;
use crate::env::{ACTIVATION_VARIABLES, SessionEnv, find_in_path};
use anyhow::Context as _;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const SESSION_TARGET: &str = "nimbus-session.target";
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
const CHILD_GRACE: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(50);

/// The compositor's announcement that its sockets accept connections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub wayland_display: String,
    pub socket: PathBuf,
}

/// Parses `NIMBUS_READY WAYLAND_DISPLAY=<name> NIMBUS_SOCKET=<path>`; the path may contain spaces.
pub fn parse_ready_line(line: &str) -> Option<Ready> {
    const DISPLAY_KEY: &str = "WAYLAND_DISPLAY=";
    const SOCKET_KEY: &str = " NIMBUS_SOCKET=";
    let rest = line.trim_end_matches(['\r', '\n']).strip_prefix("NIMBUS_READY ")?;
    let rest = rest.trim_start().strip_prefix(DISPLAY_KEY)?;
    let (display, socket) = rest.split_once(SOCKET_KEY)?;
    let display = display.trim();
    if display.is_empty() || display.contains(char::is_whitespace) || socket.is_empty() {
        return None;
    }
    Some(Ready { wayland_display: display.to_string(), socket: PathBuf::from(socket) })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashDecision {
    Restart,
    GiveUp,
}

/// Allows at most `max_restarts` restarts within any `window`.
#[derive(Clone, Debug)]
pub struct RestartPolicy {
    max_restarts: usize,
    window: Duration,
    restarts: VecDeque<Instant>,
}

impl RestartPolicy {
    pub fn new(max_restarts: usize, window: Duration) -> Self {
        Self { max_restarts, window, restarts: VecDeque::new() }
    }

    /// Records a crash at `now` and decides whether to restart.
    pub fn on_crash(&mut self, now: Instant) -> CrashDecision {
        while self
            .restarts
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= self.window)
        {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= self.max_restarts {
            return CrashDecision::GiveUp;
        }
        self.restarts.push_back(now);
        CrashDecision::Restart
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::new(3, Duration::from_secs(60))
    }
}

/// What the session does once the compositor exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The user logged out or the session received a termination signal.
    Shutdown,
    Crashed,
}

pub fn classify_exit(status: ExitStatus, shutdown_requested: bool) -> Outcome {
    if shutdown_requested || status.success() { Outcome::Shutdown } else { Outcome::Crashed }
}

pub struct CompositorCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

pub struct SessionPlan {
    pub compositor: CompositorCommand,
    /// The environment of the compositor; clients additionally get the compositor's sockets.
    pub env: SessionEnv,
    /// Recomputed after every compositor start, so edits to autostart entries apply after a restart.
    pub autostart: Box<dyn Fn() -> Vec<Launch>>,
    pub ready_timeout: Duration,
    /// The file the compositor keeps while the screen is locked, from [`lock_marker`].
    pub lock_marker: Option<PathBuf>,
}

/// Returns the lock marker in `runtime_dir`, which is `$XDG_RUNTIME_DIR`.
pub fn lock_marker(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("nimbus/locked")
}

#[derive(Default)]
struct Shared {
    compositor_pid: AtomicI32,
    shutdown: AtomicBool,
}

fn compositor_command(plan: &SessionPlan) -> Command {
    let mut command = Command::new(&plan.compositor.program);
    command.args(&plan.compositor.args).stdin(Stdio::null()).stdout(Stdio::piped());
    plan.env.apply(&mut command);
    command
}

/// Runs the session until logout, a termination signal, or too many compositor crashes.
pub fn run(plan: SessionPlan) -> anyhow::Result<ExitCode> {
    let shared = Arc::new(Shared::default());
    forward_signals(shared.clone())?;
    let mut policy = RestartPolicy::default();
    let mut children = Children::default();
    let mut target_started = false;
    let mut client_env = plan.env.clone();
    // The display manager authenticated the user, so a marker left by an earlier session is stale.
    if let Some(marker) = &plan.lock_marker {
        let _ = std::fs::remove_file(marker);
    }

    let code = loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            break ExitCode::SUCCESS;
        }
        let mut compositor = compositor_command(&plan)
            .spawn()
            .with_context(|| format!("cannot start {}", plan.compositor.program.display()))?;
        let pid = i32::try_from(compositor.id()).context("compositor process id out of range")?;
        shared.compositor_pid.store(pid, Ordering::SeqCst);
        if shared.shutdown.load(Ordering::SeqCst) {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }

        let ready = match compositor.stdout.take() {
            Some(stdout) => wait_for_ready(stdout, plan.ready_timeout),
            None => Err(ReadyError::Closed),
        };
        match ready {
            Ok(ready) => {
                tracing::info!(
                    "compositor ready on {} (control socket {})",
                    ready.wayland_display,
                    ready.socket.display()
                );
                client_env = plan.env.clone();
                client_env.set("WAYLAND_DISPLAY", ready.wayland_display);
                client_env.set(nimbus_ipc::SOCKET_ENV, ready.socket.to_string_lossy());
                update_activation_environment(&client_env);
                target_started |= start_systemd_target(&client_env);
                for launch in (plan.autostart)() {
                    children.spawn(&launch, &client_env);
                }
            }
            Err(ReadyError::Timeout) => {
                tracing::error!(
                    "compositor didn't report readiness within {:?}; stopping it",
                    plan.ready_timeout
                );
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
            }
            Err(ReadyError::Closed) => {}
        }

        let status = loop {
            if let Some(status) = compositor.try_wait().context("cannot wait for the compositor")? {
                break status;
            }
            children.reap();
            std::thread::sleep(POLL);
        };
        shared.compositor_pid.store(0, Ordering::SeqCst);
        children.terminate(CHILD_GRACE);

        match classify_exit(status, shared.shutdown.load(Ordering::SeqCst)) {
            Outcome::Shutdown => {
                tracing::info!("compositor exited ({status}); ending the session");
                break ExitCode::SUCCESS;
            }
            Outcome::Crashed if plan.lock_marker.as_deref().is_some_and(Path::exists) => {
                tracing::error!(
                    "compositor exited ({status}) while the screen was locked; ending the session"
                );
                break ExitCode::FAILURE;
            }
            Outcome::Crashed => match policy.on_crash(Instant::now()) {
                CrashDecision::Restart => {
                    tracing::warn!("compositor exited unexpectedly ({status}); restarting it")
                }
                CrashDecision::GiveUp => {
                    tracing::error!("compositor crashed too often ({status}); giving up");
                    break ExitCode::FAILURE;
                }
            },
        }
    };

    if target_started {
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop", SESSION_TARGET]);
        run_helper(stop, &client_env);
    }
    Ok(code)
}

fn forward_signals(shared: Arc<Shared>) -> anyhow::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP])
        .context("cannot install signal handlers")?;
    std::thread::Builder::new().name("nimbus-session-signals".into()).spawn(move || {
        for raw in signals.forever() {
            shared.shutdown.store(true, Ordering::SeqCst);
            let pid = shared.compositor_pid.load(Ordering::SeqCst);
            if pid > 0
                && let Ok(signal) = Signal::try_from(raw)
            {
                tracing::info!("forwarding {signal} to the compositor");
                let _ = kill(Pid::from_raw(pid), signal);
            }
        }
    })?;
    Ok(())
}

enum ReadyError {
    Timeout,
    Closed,
}

/// Reads the compositor's standard output on a thread that keeps forwarding it after the ready line.
fn wait_for_ready(
    stdout: impl std::io::Read + Send + 'static,
    timeout: Duration,
) -> Result<Ready, ReadyError> {
    let (tx, rx) = mpsc::channel();
    let spawned =
        std::thread::Builder::new().name("nimbus-compositor-stdout".into()).spawn(move || {
            let mut tx = Some(tx);
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                match parse_ready_line(&line) {
                    Some(ready) if tx.is_some() => {
                        if let Some(tx) = tx.take() {
                            let _ = tx.send(ready);
                        }
                    }
                    _ => {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{line}");
                    }
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!("cannot read the compositor's output: {error}");
        return Err(ReadyError::Closed);
    }
    rx.recv_timeout(timeout).map_err(|e| match e {
        mpsc::RecvTimeoutError::Timeout => ReadyError::Timeout,
        mpsc::RecvTimeoutError::Disconnected => ReadyError::Closed,
    })
}

fn update_activation_environment(env: &SessionEnv) {
    let mut command = Command::new("dbus-update-activation-environment");
    command.arg("--systemd").args(ACTIVATION_VARIABLES);
    run_helper(command, env);
}

/// Starts the session target when a systemd user manager runs; returns whether it was started.
fn start_systemd_target(env: &SessionEnv) -> bool {
    let manager =
        std::env::var_os("XDG_RUNTIME_DIR").map(|dir| Path::new(&dir).join("systemd/private"));
    if !manager.is_some_and(|socket| socket.exists())
        || find_in_path("systemctl", std::env::var_os("PATH")).is_none()
    {
        return false;
    }
    let mut command = Command::new("systemctl");
    command.args(["--user", "start", "--no-block", SESSION_TARGET]);
    run_helper(command, env)
}

/// Runs a best-effort helper with a timeout; returns whether it succeeded.
fn run_helper(mut command: Command, env: &SessionEnv) -> bool {
    env.apply(&mut command);
    command.stdin(Stdio::null()).stdout(Stdio::null());
    let name = command.get_program().to_string_lossy().into_owned();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!("{name} isn't installed; skipping it");
            return false;
        }
        Err(error) => {
            tracing::warn!("cannot run {name}: {error}");
            return false;
        }
    };
    let deadline = Instant::now() + HELPER_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return true,
            Ok(Some(status)) => {
                tracing::warn!("{name} failed ({status})");
                return false;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                tracing::warn!("{name} didn't finish within {HELPER_TIMEOUT:?}; killing it");
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(error) => {
                tracing::warn!("cannot wait for {name}: {error}");
                return false;
            }
        }
    }
}

/// An autostarted process group, led by the process the session started.
struct Group {
    label: String,
    pgid: Pid,
    /// The group leader until it's reaped.
    leader: Option<Child>,
}

/// Autostarted processes, each leading its own process group.
///
/// A group stays tracked until it's empty, so its id can't be reused while the session may still signal it.
#[derive(Default)]
struct Children {
    running: Vec<Group>,
}

impl Children {
    fn spawn(&mut self, launch: &Launch, env: &SessionEnv) {
        let Some((program, args)) = launch.argv.split_first() else { return };
        let mut command = Command::new(program);
        command.args(args).stdin(Stdio::null()).process_group(0);
        if let Some(dir) = launch.working_dir.as_deref().filter(|d| d.is_dir()) {
            command.current_dir(dir);
        }
        env.apply(&mut command);
        match command.spawn() {
            Ok(child) => {
                tracing::info!("autostarted {}", launch.label);
                let Ok(pgid) = i32::try_from(child.id()) else { return };
                self.running.push(Group {
                    label: launch.label.clone(),
                    pgid: Pid::from_raw(pgid),
                    leader: Some(child),
                });
            }
            Err(error) => tracing::warn!("cannot autostart {}: {error}", launch.label),
        }
    }

    /// Reaps exited group leaders and forgets empty groups.
    fn reap(&mut self) {
        self.running.retain_mut(|group| {
            if let Some(leader) = &mut group.leader
                && let Ok(Some(status)) = leader.try_wait()
            {
                tracing::debug!("{} exited ({status})", group.label);
                group.leader = None;
            }
            group.leader.is_some() || killpg(group.pgid, None) != Err(Errno::ESRCH)
        });
    }

    fn terminate(&mut self, grace: Duration) {
        self.signal_all(Signal::SIGTERM);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            self.reap();
            if self.running.is_empty() {
                return;
            }
            std::thread::sleep(POLL);
        }
        self.signal_all(Signal::SIGKILL);
        for group in self.running.drain(..) {
            tracing::warn!("killed {}, which ignored SIGTERM", group.label);
            if let Some(mut leader) = group.leader {
                let _ = leader.wait();
            }
        }
    }

    fn signal_all(&self, signal: Signal) {
        for group in &self.running {
            let _ = killpg(group.pgid, signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn ready_line_parsing() {
        assert_eq!(
            parse_ready_line(
                "NIMBUS_READY WAYLAND_DISPLAY=wayland-1 NIMBUS_SOCKET=/run/user/1000/nimbus-wayland-1.sock\n"
            ),
            Some(Ready {
                wayland_display: "wayland-1".into(),
                socket: "/run/user/1000/nimbus-wayland-1.sock".into()
            })
        );
        assert_eq!(
            parse_ready_line("NIMBUS_READY WAYLAND_DISPLAY=w NIMBUS_SOCKET=/tmp/a b.sock")
                .map(|r| r.socket),
            Some(PathBuf::from("/tmp/a b.sock"))
        );
        assert_eq!(parse_ready_line("NIMBUS_READY WAYLAND_DISPLAY= NIMBUS_SOCKET=/x"), None);
        assert_eq!(parse_ready_line("NIMBUS_READY WAYLAND_DISPLAY=w NIMBUS_SOCKET="), None);
        assert_eq!(parse_ready_line("NIMBUS_READY NIMBUS_SOCKET=/x"), None);
        assert_eq!(parse_ready_line("starting compositor"), None);
    }

    #[test]
    fn restart_policy_allows_three_restarts_per_minute() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_secs(s);
        let mut policy = RestartPolicy::default();
        assert_eq!(policy.on_crash(at(0)), CrashDecision::Restart);
        assert_eq!(policy.on_crash(at(10)), CrashDecision::Restart);
        assert_eq!(policy.on_crash(at(20)), CrashDecision::Restart);
        assert_eq!(policy.on_crash(at(30)), CrashDecision::GiveUp);

        let mut policy = RestartPolicy::default();
        for s in [0, 10, 20] {
            assert_eq!(policy.on_crash(at(s)), CrashDecision::Restart);
        }
        assert_eq!(
            policy.on_crash(at(60)),
            CrashDecision::Restart,
            "the first crash left the window"
        );
        assert_eq!(policy.on_crash(at(65)), CrashDecision::GiveUp);
        assert_eq!(policy.on_crash(at(200)), CrashDecision::Restart);
    }

    #[test]
    fn exit_classification() {
        let exited = |code: i32| ExitStatus::from_raw(code << 8);
        let signaled = |signal: i32| ExitStatus::from_raw(signal);
        assert_eq!(classify_exit(exited(0), false), Outcome::Shutdown);
        assert_eq!(classify_exit(exited(1), false), Outcome::Crashed);
        assert_eq!(classify_exit(signaled(11), false), Outcome::Crashed);
        assert_eq!(classify_exit(signaled(15), true), Outcome::Shutdown);
    }

    #[test]
    fn ready_is_read_from_a_stream() {
        let output =
            b"log line\nNIMBUS_READY WAYLAND_DISPLAY=wayland-3 NIMBUS_SOCKET=/tmp/n.sock\nafter\n"
                .to_vec();
        let ready = wait_for_ready(std::io::Cursor::new(output), Duration::from_secs(5)).ok();
        assert_eq!(ready.map(|r| r.wayland_display), Some("wayland-3".into()));
        assert!(matches!(
            wait_for_ready(
                std::io::Cursor::new(b"no ready line\n".to_vec()),
                Duration::from_secs(5)
            ),
            Err(ReadyError::Closed)
        ));
    }

    #[test]
    fn children_are_terminated_by_process_group() {
        let mut children = Children::default();
        let env = SessionEnv::default();
        let launch = Launch {
            label: "sleeper".into(),
            argv: vec!["sh".into(), "-c".into(), "sleep 30 & sleep 30".into()],
            working_dir: None,
        };
        children.spawn(&launch, &env);
        assert_eq!(children.running.len(), 1);
        let started = Instant::now();
        children.terminate(Duration::from_secs(5));
        assert!(children.running.is_empty());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(POLL);
        }
        false
    }

    #[test]
    fn exited_children_are_reaped_during_the_session() {
        let mut children = Children::default();
        let env = SessionEnv::default();
        let launch = |label: &str, script: &str| Launch {
            label: label.into(),
            argv: vec!["sh".into(), "-c".into(), script.into()],
            working_dir: None,
        };
        children.spawn(&launch("one-shot", "true"), &env);
        children.spawn(&launch("daemonizes", "sleep 30 &"), &env);
        assert!(wait_until(|| {
            children.reap();
            children.running.len() == 1 && children.running[0].leader.is_none()
        }));
        let group = &children.running[0];
        assert_eq!(group.label, "daemonizes");
        children.terminate(Duration::from_secs(5));
        assert!(children.running.is_empty());
    }

    #[test]
    fn compositor_doesnt_get_the_client_environment() {
        let mut env = SessionEnv::default();
        env.set("XDG_CURRENT_DESKTOP", "Nimbus");
        let plan = SessionPlan {
            compositor: CompositorCommand { program: "nimbus-compositor".into(), args: vec![] },
            env,
            autostart: Box::new(Vec::new),
            ready_timeout: Duration::from_secs(1),
            lock_marker: None,
        };
        let command = compositor_command(&plan);
        let names: Vec<_> = command.get_envs().map(|(name, _)| name.to_owned()).collect();
        assert_eq!(names, [OsString::from("XDG_CURRENT_DESKTOP")]);
    }
}
