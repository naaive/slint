// SPDX-License-Identifier: MIT

//! Runs `nimbus-session` against shell scripts that stand in for the compositor and the shell.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new(config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["config/nimbus", "config/autostart", "runtime", "xdg"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("config/nimbus/config.toml"), config).unwrap();
        let sandbox = Self { dir };
        sandbox.shell("exec sleep 60");
        sandbox
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn compositor(&self, body: &str) -> PathBuf {
        let path = self.path("fake-compositor");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n", self.path("starts").display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Replaces the fake shell, which records `<pid> $WAYLAND_DISPLAY $NIMBUS_SOCKET <time> <args>` per start.
    fn shell(&self, body: &str) {
        let path = self.path("fake-shell");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"$$ $WAYLAND_DISPLAY $NIMBUS_SOCKET $(date +%s.%N) $*\" >> '{}'\n{body}\n",
                self.path("shell-starts").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn spawn(&self, compositor: &Path) -> Child {
        Command::new(env!("CARGO_BIN_EXE_nimbus-session"))
            .args(["--backend", "headless", "--ready-timeout", "5", "--compositor"])
            .arg(compositor)
            .arg("--shell")
            .arg(self.path("fake-shell"))
            .env("DBUS_SESSION_BUS_ADDRESS", "disabled:")
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CONFIG_DIRS", self.path("xdg"))
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("HOME", self.dir.path())
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("QT_QPA_PLATFORM")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn starts(&self) -> Vec<String> {
        self.lines("starts")
    }

    /// Each shell start's fields: pid, `WAYLAND_DISPLAY`, `NIMBUS_SOCKET`, time, and arguments.
    fn shell_starts(&self) -> Vec<Vec<String>> {
        let lines = self.lines("shell-starts");
        lines.iter().map(|line| line.split_whitespace().map(str::to_string).collect()).collect()
    }

    fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.path(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A shell command that waits up to 15 seconds for `name` to have at least `count` lines.
    fn wait_lines(&self, name: &str, count: impl std::fmt::Display) -> String {
        format!(
            "i=0; while [ $(cat '{}' 2>/dev/null | wc -l) -lt {count} ] && [ $i -lt 150 ]; do sleep 0.1; i=$((i+1)); done",
            self.path(name).display()
        )
    }
}

/// Whether the process `pid` is gone, or a zombie until its new parent reaps it.
fn is_gone(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().is_none_or(|s| s.contains(") Z "))
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("nimbus-session didn't exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_file(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.ends_with('\n')
        {
            return text;
        }
        assert!(Instant::now() < deadline, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn clean_exit_runs_autostart_then_stops_it() {
    let sandbox = Sandbox::new("");
    let from_config = sandbox.path("from-config");
    std::fs::write(
        sandbox.path("config/nimbus/config.toml"),
        format!("autostart = [\"echo $XDG_SESSION_TYPE > '{}'\"]\n", from_config.display()),
    )
    .unwrap();
    let marker = sandbox.path("autostart-env");
    std::fs::write(
        sandbox.path("config/autostart/probe.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nExec=sh -c \"echo $WAYLAND_DISPLAY $NIMBUS_SOCKET $XDG_CURRENT_DESKTOP $QT_QPA_PLATFORM > {}; exec sleep 60\"\n",
            marker.display()
        ),
    )
    .unwrap();
    let compositor = sandbox.compositor(
        "echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-test NIMBUS_SOCKET=/tmp/nimbus test.sock'\nsleep 1\nexit 0",
    );

    let started = Instant::now();
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert!(started.elapsed() < Duration::from_secs(10), "the autostarted sleep wasn't terminated");
    assert_eq!(
        wait_for_file(&marker, Duration::from_secs(1)),
        "wayland-test /tmp/nimbus test.sock Nimbus wayland;xcb\n"
    );
    assert_eq!(wait_for_file(&from_config, Duration::from_secs(1)), "wayland\n");
    assert_eq!(sandbox.starts(), vec!["--backend headless"]);
    let shells = sandbox.shell_starts();
    assert_eq!(shells.len(), 1, "{shells:?}");
    assert_eq!(shells[0][1..3], ["wayland-test", "/tmp/nimbus"], "the shell gets the sockets");
}

#[test]
fn crashing_compositor_is_restarted_three_times() {
    let sandbox = Sandbox::new("");
    let compositor = sandbox.compositor("exit 3");
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert_eq!(status.code(), Some(1));
    assert_eq!(sandbox.starts().len(), 4);
}

#[test]
fn sigterm_is_forwarded_and_ends_the_session() {
    let sandbox = Sandbox::new("");
    let compositor = sandbox.compositor(
        "echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-x NIMBUS_SOCKET=/tmp/x.sock'\nexec sleep 60",
    );
    let mut session = sandbox.spawn(&compositor);
    wait_for_file(&sandbox.path("starts"), Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(500));

    let pid = nix::unistd::Pid::from_raw(i32::try_from(session.id()).unwrap());
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).unwrap();
    let status = wait_with_timeout(&mut session, Duration::from_secs(10));
    assert!(status.success(), "{status}");
    assert_eq!(sandbox.starts().len(), 1);
    let shells = sandbox.shell_starts();
    assert_eq!(shells.len(), 1);
    assert!(is_gone(&shells[0][0]), "the shell outlived the session");
}

#[test]
fn restarted_compositor_gets_the_inherited_wayland_display() {
    let sandbox = Sandbox::new("");
    let displays = sandbox.path("displays");
    let compositor = sandbox.compositor(&format!(
        "echo \"${{WAYLAND_DISPLAY-unset}} ${{NIMBUS_SOCKET-unset}}\" >> '{}'\n\
         echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-own NIMBUS_SOCKET=/tmp/own.sock'\n\
         [ $(wc -l < '{}') -lt 2 ] && exit 1\nexit 0",
        displays.display(),
        displays.display(),
    ));
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert_eq!(std::fs::read_to_string(displays).unwrap(), "unset unset\nunset unset\n");
}

#[test]
fn autostart_runs_once_per_session() {
    let sandbox = Sandbox::new("");
    let runs = sandbox.path("runs");
    std::fs::write(
        sandbox.path("config/nimbus/config.toml"),
        format!("autostart = [\"echo run >> '{}'\"]\n", runs.display()),
    )
    .unwrap();
    // Crashes after its first readiness, then logs out after the second.
    let compositor = sandbox.compositor(&format!(
        "echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-once NIMBUS_SOCKET=/tmp/once.sock'\n\
         sleep 1\n[ $(wc -l < '{}') -lt 2 ] && exit 3\nexit 0",
        sandbox.path("starts").display()
    ));
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert_eq!(
        sandbox.starts(),
        vec!["--backend headless", "--backend headless --socket wayland-once"]
    );
    assert_eq!(std::fs::read_to_string(runs).unwrap(), "run\n");
}

/// A fake compositor that creates the lock marker and crashes the first time, then logs out.
fn crash_while_locked(sandbox: &Sandbox) -> PathBuf {
    let marker = sandbox.path("runtime/nimbus/locked");
    sandbox.compositor(&format!(
        "if [ $(wc -l < '{starts}') -lt 2 ]; then mkdir -p '{dir}'; touch '{marker}'; exit 3; fi\nexit 0",
        starts = sandbox.path("starts").display(),
        dir = marker.parent().unwrap().display(),
        marker = marker.display()
    ))
}

#[test]
fn crash_while_locked_restarts_locked() {
    let sandbox = Sandbox::new("");
    let compositor = crash_while_locked(&sandbox);
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert_eq!(sandbox.starts(), vec!["--backend headless", "--backend headless --locked"]);
}

#[test]
fn lock_marker_from_an_earlier_session_is_ignored() {
    let sandbox = Sandbox::new("");
    let marker = sandbox.path("runtime/nimbus/locked");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, "").unwrap();
    let compositor = sandbox.compositor("exit 3");
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert_eq!(status.code(), Some(1));
    assert_eq!(sandbox.starts().len(), 4);
}

#[test]
fn a_failed_restart_still_stops_autostarted_processes() {
    let sandbox = Sandbox::new("");
    let pid_file = sandbox.path("autostart-pid");
    std::fs::write(
        sandbox.path("config/nimbus/config.toml"),
        format!("autostart = [\"echo $$ > '{}'; exec sleep 60\"]\n", pid_file.display()),
    )
    .unwrap();
    // Deletes itself before crashing, so the restart fails to spawn.
    let compositor = sandbox.compositor(
        "echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-gone NIMBUS_SOCKET=/tmp/gone.sock'\nsleep 1\nrm \"$0\"\nexit 3",
    );
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert!(!status.success(), "{status}");

    let pid = wait_for_file(&pid_file, Duration::from_secs(1));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !is_gone(pid.trim()) {
        assert!(Instant::now() < deadline, "the autostarted process outlived the session");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn crashing_shell_is_restarted_with_backoff() {
    let sandbox = Sandbox::new("");
    sandbox.shell("exit 1");
    // Logs out once the shell started three times.
    let compositor = sandbox.compositor(&format!(
        "echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-s NIMBUS_SOCKET=/tmp/s.sock'\n{}\nexit 0",
        sandbox.wait_lines("shell-starts", 3)
    ));
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(30));
    assert!(status.success(), "{status}");
    assert_eq!(sandbox.starts().len(), 1, "a shell crash leaves the compositor alone");
    let times: Vec<f64> = sandbox.shell_starts().iter().map(|s| s[3].parse().unwrap()).collect();
    assert!(times.len() >= 3, "{times:?}");
    let (first, second) = (times[1] - times[0], times[2] - times[1]);
    assert!(first >= 0.4, "restarted after {first}s");
    assert!(second > first, "the delay grows: {first}s, then {second}s");
}

#[test]
fn compositor_crash_restarts_the_compositor_and_the_shell() {
    let sandbox = Sandbox::new("");
    // Crashes once its shell started, then logs out once the second shell started.
    let compositor = sandbox.compositor(&format!(
        "echo 'NIMBUS_READY WAYLAND_DISPLAY=wayland-c NIMBUS_SOCKET=/tmp/c.sock'\n\
         runs=$(wc -l < '{starts}')\n{wait}\n[ \"$runs\" -lt 2 ] && exit 3\nexit 0",
        starts = sandbox.path("starts").display(),
        wait = sandbox.wait_lines("shell-starts", "$runs"),
    ));
    let mut session = sandbox.spawn(&compositor);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert!(status.success(), "{status}");
    assert_eq!(
        sandbox.starts(),
        vec!["--backend headless", "--backend headless --socket wayland-c"]
    );
    let shells = sandbox.shell_starts();
    assert_eq!(shells.len(), 2, "{shells:?}");
    assert!(shells.iter().all(|shell| is_gone(&shell[0])), "every shell was stopped");
}
