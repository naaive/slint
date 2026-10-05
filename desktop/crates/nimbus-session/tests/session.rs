// SPDX-License-Identifier: MIT

//! Runs `nimbus-session` against shell scripts that stand in for the compositor.

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
        Self { dir }
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

    fn spawn(&self, compositor: &Path) -> Child {
        self.spawn_with(compositor, &[])
    }

    fn spawn_with(&self, compositor: &Path, args: &[&str]) -> Child {
        Command::new(env!("CARGO_BIN_EXE_nimbus-session"))
            .args(["--backend", "headless", "--ready-timeout", "5", "--compositor"])
            .arg(compositor)
            .args(args)
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
        std::fs::read_to_string(self.path("starts"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
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
fn crash_while_locked_without_the_shell_ends_the_session() {
    let sandbox = Sandbox::new("");
    let compositor = crash_while_locked(&sandbox);
    let mut session = sandbox.spawn_with(&compositor, &["--no-shell"]);
    let status = wait_with_timeout(&mut session, Duration::from_secs(20));
    assert_eq!(status.code(), Some(1));
    assert_eq!(sandbox.starts(), vec!["--backend headless --no-shell"]);
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
    let stat = PathBuf::from(format!("/proc/{}/stat", pid.trim()));
    let deadline = Instant::now() + Duration::from_secs(5);
    // Once killed, the process is gone, or a zombie until its new parent reaps it.
    while std::fs::read_to_string(&stat).is_ok_and(|s| !s.contains(") Z ")) {
        assert!(Instant::now() < deadline, "the autostarted process outlived the session");
        std::thread::sleep(Duration::from_millis(50));
    }
}
