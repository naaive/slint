// SPDX-License-Identifier: MIT

//! Starts the compositor, waits for it to become ready, then starts the shell and runs autostart once.
//! Restarts the shell whenever it exits, and both after the compositor crashes.

use crate::autostart::Launch;
use crate::env::{ACTIVATION_VARIABLES, SessionEnv, find_in_path};
use anyhow::Context as _;
use nimbus_ipc::Ready;
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
/// How long a shell gets to exit after its compositor crashed; it loses its connection and exits on its own.
const CRASH_GRACE: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashDecision {
    Restart,
    GiveUp,
}

/// Allows at most `max_restarts` compositor restarts within any `window`.
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

/// Delays between shell restarts: doubling from `initial` up to `max`,
/// and back to `initial` after the shell ran for `stable`.
#[derive(Clone, Debug)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    stable: Duration,
    next: Duration,
}

impl Backoff {
    pub fn new(initial: Duration, max: Duration, stable: Duration) -> Self {
        Self { initial, max, stable, next: initial }
    }

    /// The delay before restarting a shell that exited after running for `uptime`.
    pub fn delay(&mut self, uptime: Duration) -> Duration {
        if uptime >= self.stable {
            self.next = self.initial;
        }
        let delay = self.next;
        self.next = (delay * 2).min(self.max);
        delay
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(30), Duration::from_secs(30))
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

#[derive(Debug)]
pub struct Program {
    pub path: PathBuf,
    pub args: Vec<OsString>,
}

pub struct SessionPlan {
    /// The compositor; the supervisor adds `--socket`, `--x11-display`, and `--locked`.
    pub compositor: Program,
    /// The Wayland socket name; without one, restarts reuse the name the first compositor picked.
    pub socket: Option<String>,
    /// The shell, started with the client environment once the compositor is ready.
    pub shell: Program,
    /// The environment of the compositor; clients additionally get the compositor's sockets.
    pub env: SessionEnv,
    /// What to start once the first compositor is ready; restarts don't run it again.
    pub autostart: Box<dyn FnOnce() -> Vec<Launch>>,
    pub ready_timeout: Duration,
    /// The file the compositor keeps while the screen is locked, from [`nimbus_ipc::lock_marker_path`].
    pub lock_marker: Option<PathBuf>,
}

/// How to start the compositor this time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Start {
    socket: Option<String>,
    x11_display: Option<String>,
    locked: bool,
}

#[derive(Default)]
struct Shared {
    compositor_pid: AtomicI32,
    shutdown: AtomicBool,
}

fn compositor_command(compositor: &Program, env: &SessionEnv, start: &Start) -> Command {
    let mut command = Command::new(&compositor.path);
    command.args(&compositor.args).stdin(Stdio::null()).stdout(Stdio::piped());
    if let Some(socket) = &start.socket {
        command.arg("--socket").arg(socket);
    }
    if let Some(display) = &start.x11_display {
        command.arg("--x11-display").arg(display);
    }
    if start.locked {
        command.arg("--locked");
    }
    env.apply(&mut command);
    command
}

/// The shell process, restarted with [`Backoff`] whenever it exits.
struct Shell {
    program: Program,
    backoff: Backoff,
    /// The running shell and when it started.
    running: Option<(Child, Instant)>,
    restart_at: Option<Instant>,
}

impl Shell {
    fn new(program: Program, backoff: Backoff) -> Self {
        Self { program, backoff, running: None, restart_at: None }
    }

    fn start(&mut self, env: &SessionEnv) {
        self.restart_at = None;
        let mut command = Command::new(&self.program.path);
        command.args(&self.program.args).stdin(Stdio::null());
        env.apply(&mut command);
        match command.spawn() {
            Ok(child) => {
                tracing::info!("started the shell");
                self.running = Some((child, Instant::now()));
            }
            Err(error) => {
                tracing::error!("cannot start {}: {error}", self.program.path.display());
                self.schedule_restart(Duration::ZERO);
            }
        }
    }

    fn schedule_restart(&mut self, uptime: Duration) {
        let delay = self.backoff.delay(uptime);
        tracing::info!("restarting the shell in {delay:?}");
        self.restart_at = Some(Instant::now() + delay);
    }

    /// Notices an exited shell, and starts it again once its restart delay passed.
    fn poll(&mut self, env: &SessionEnv) {
        if let Some((child, started)) = &mut self.running
            && let Ok(Some(status)) = child.try_wait()
        {
            let uptime = started.elapsed();
            self.running = None;
            tracing::warn!("the shell exited ({status}) after {uptime:.1?}");
            self.schedule_restart(uptime);
        }
        if self.restart_at.is_some_and(|at| Instant::now() >= at) {
            self.start(env);
        }
    }
}

/// Processes that [`terminate`] stops.
trait Processes {
    fn signal(&mut self, signal: Signal);
    /// Reaps exited processes; returns whether any still run.
    fn reap(&mut self) -> bool;
    /// Kills and reaps the processes that still run.
    fn kill(&mut self);
}

/// Sends SIGTERM to every process of `targets`, and SIGKILL to those still running after `grace`.
fn terminate(targets: &mut [&mut dyn Processes], grace: Duration) {
    for target in targets.iter_mut() {
        target.signal(Signal::SIGTERM);
    }
    let deadline = Instant::now() + grace;
    loop {
        let mut running = false;
        for target in targets.iter_mut() {
            running |= target.reap();
        }
        if !running {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(POLL);
    }
    for target in targets {
        target.kill();
    }
}

impl Processes for Shell {
    fn signal(&mut self, signal: Signal) {
        self.restart_at = None;
        if let Some((child, _)) = &self.running
            && let Ok(pid) = i32::try_from(child.id())
        {
            let _ = kill(Pid::from_raw(pid), signal);
        }
    }

    fn reap(&mut self) -> bool {
        if let Some((child, _)) = &mut self.running
            && !matches!(child.try_wait(), Ok(None))
        {
            self.running = None;
        }
        self.running.is_some()
    }

    fn kill(&mut self) {
        if let Some((mut child, _)) = self.running.take() {
            tracing::warn!("killing the shell, which ignored SIGTERM");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Runs the session until logout, a termination signal, or too many compositor crashes.
pub fn run(plan: SessionPlan) -> anyhow::Result<ExitCode> {
    let shared = Arc::new(Shared::default());
    forward_signals(shared.clone())?;
    let mut policy = RestartPolicy::default();
    let mut shell = Shell::new(plan.shell, Backoff::default());
    let mut children = Children::default();
    let mut target_started = false;
    let mut client_env = plan.env.clone();
    let mut start = Start { socket: plan.socket.clone(), x11_display: None, locked: false };
    let mut autostart = Some(plan.autostart);
    // The display manager authenticated the user, so a marker left by an earlier session is stale.
    if let Some(marker) = &plan.lock_marker {
        let _ = std::fs::remove_file(marker);
    }

    let result = (|| -> anyhow::Result<ExitCode> {
        Ok(loop {
            if shared.shutdown.load(Ordering::SeqCst) {
                break ExitCode::SUCCESS;
            }
            let mut compositor = compositor_command(&plan.compositor, &plan.env, &start)
                .spawn()
                .with_context(|| format!("cannot start {}", plan.compositor.path.display()))?;
            let pid =
                i32::try_from(compositor.id()).context("compositor process id out of range")?;
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
                    // Autostarted clients keep their environment, so restarts keep the socket name.
                    start.socket.get_or_insert_with(|| ready.wayland_display.clone());
                    if start.x11_display.is_none() {
                        start.x11_display.clone_from(&ready.x11_display);
                    }
                    client_env = plan.env.clone();
                    client_env.set("WAYLAND_DISPLAY", ready.wayland_display);
                    match ready.x11_display {
                        Some(display) => client_env.set("DISPLAY", display),
                        None => client_env.unset("DISPLAY"),
                    }
                    client_env.set(nimbus_ipc::SOCKET_ENV, ready.socket.to_string_lossy());
                    shell.start(&client_env);
                    update_activation_environment(&client_env);
                    target_started |= start_systemd_target(&client_env);
                    if let Some(autostart) = autostart.take() {
                        for launch in autostart() {
                            children.spawn(&launch, &client_env);
                        }
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
                if let Some(status) =
                    compositor.try_wait().context("cannot wait for the compositor")?
                {
                    break status;
                }
                // A shell that lost the compositor during shutdown exits too, and stays stopped.
                if !shared.shutdown.load(Ordering::SeqCst) {
                    shell.poll(&client_env);
                }
                children.reap();
                std::thread::sleep(POLL);
            };
            shared.compositor_pid.store(0, Ordering::SeqCst);

            if classify_exit(status, shared.shutdown.load(Ordering::SeqCst)) == Outcome::Shutdown {
                tracing::info!("compositor exited ({status}); ending the session");
                break ExitCode::SUCCESS;
            }
            terminate(&mut [&mut shell], CRASH_GRACE);
            match policy.on_crash(Instant::now()) {
                CrashDecision::Restart => {
                    start.locked = plan.lock_marker.as_deref().is_some_and(Path::exists);
                    tracing::warn!(
                        locked = start.locked,
                        "compositor exited unexpectedly ({status}); restarting it and the shell"
                    );
                }
                CrashDecision::GiveUp => {
                    tracing::error!("compositor crashed too often ({status}); giving up");
                    break ExitCode::FAILURE;
                }
            }
        })
    })();
    terminate(&mut [&mut shell, &mut children], CHILD_GRACE);

    if target_started {
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop", SESSION_TARGET]);
        run_helper(stop, &client_env);
    }
    result
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
                match Ready::parse(&line) {
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
}

impl Processes for Children {
    fn signal(&mut self, signal: Signal) {
        for group in &self.running {
            let _ = killpg(group.pgid, signal);
        }
    }

    /// Reaps exited group leaders and forgets empty groups.
    fn reap(&mut self) -> bool {
        self.running.retain_mut(|group| {
            if let Some(leader) = &mut group.leader
                && let Ok(Some(status)) = leader.try_wait()
            {
                tracing::debug!("{} exited ({status})", group.label);
                group.leader = None;
            }
            group.leader.is_some() || killpg(group.pgid, None) != Err(Errno::ESRCH)
        });
        !self.running.is_empty()
    }

    fn kill(&mut self) {
        self.signal(Signal::SIGKILL);
        for group in self.running.drain(..) {
            tracing::warn!("killed {}, which ignored SIGTERM", group.label);
            if let Some(mut leader) = group.leader {
                let _ = leader.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

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
        terminate(&mut [&mut children], Duration::from_secs(5));
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
        terminate(&mut [&mut children], Duration::from_secs(5));
        assert!(children.running.is_empty());
    }

    #[test]
    fn compositor_doesnt_get_the_client_environment() {
        let mut env = SessionEnv::default();
        env.set("XDG_CURRENT_DESKTOP", "Nimbus");
        let compositor = Program { path: "nimbus-compositor".into(), args: vec![] };
        let command = compositor_command(&compositor, &env, &Start::default());
        let names: Vec<_> = command.get_envs().map(|(name, _)| name.to_owned()).collect();
        assert_eq!(names, [OsString::from("XDG_CURRENT_DESKTOP")]);
        assert_eq!(command.get_args().count(), 0);

        let restart = Start {
            socket: Some("wayland-2".into()),
            x11_display: Some(":1".into()),
            locked: true,
        };
        let args: Vec<_> = compositor_command(&compositor, &env, &restart)
            .get_args()
            .map(ToOwned::to_owned)
            .collect();
        assert_eq!(args, ["--socket", "wayland-2", "--x11-display", ":1", "--locked"]);
    }

    #[test]
    fn shell_restarts_back_off_until_it_runs_stably() {
        let ms = Duration::from_millis;
        let mut backoff = Backoff::new(ms(500), ms(3000), Duration::from_secs(30));
        let delays: Vec<_> = (0..5).map(|_| backoff.delay(ms(10))).collect();
        assert_eq!(delays, [ms(500), ms(1000), ms(2000), ms(3000), ms(3000)]);
        assert_eq!(backoff.delay(Duration::from_secs(30)), ms(500), "a stable run resets it");
        assert_eq!(backoff.delay(ms(10)), ms(1000));
    }
}
