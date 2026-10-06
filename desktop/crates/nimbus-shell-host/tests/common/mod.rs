// SPDX-License-Identifier: MIT

//! Runs the compositor headless and the `nimbus-shell` binary as its client.

#![allow(dead_code)]

use std::process::{Child, Command, ExitStatus, Stdio};

use nimbus_ipc::{CompositorState, Request};
pub use nimbus_test_support::{Compositor, PrivateBus, TestClient, wait_for};

pub const OUTPUT: &str = "HEADLESS-1";
pub const CONFIG: &str = "favorites = []\n[appearance]\ncolor_scheme = \"dark\"\nanimations = false\n[panel]\nshow_dock = true\n";

/// A headless compositor and the shell process connected to it.
pub struct Session {
    pub compositor: Compositor,
    shell: Option<Child>,
    bus: Option<String>,
}

impl Session {
    /// Starts the compositor and the shell with `config`; D-Bus is unreachable unless `bus` names a private one.
    pub fn start(config: &str, bus: Option<&str>) -> Self {
        let mut session = Self::start_compositor(config, bus);
        session.start_shell();
        session
    }

    pub fn start_compositor(config: &str, bus: Option<&str>) -> Self {
        let compositor = Compositor::builder(config).start();
        std::fs::create_dir_all(compositor.dir.path().join("data")).unwrap();
        Self { compositor, shell: None, bus: bus.map(str::to_owned) }
    }

    /// Starts the shell; its log goes to `shell.log`, appended to across restarts.
    pub fn start_shell(&mut self) {
        assert!(self.shell.is_none(), "the shell is running");
        let compositor = &self.compositor;
        let dir = compositor.dir.path();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("shell.log"))
            .unwrap();
        let no_bus = format!("unix:path={}", dir.join("no-bus").display());
        let shell = Command::new(env!("CARGO_BIN_EXE_nimbus-shell"))
            .arg("--config")
            .arg(compositor.config_path())
            .env("XDG_RUNTIME_DIR", compositor.runtime_dir())
            .env("WAYLAND_DISPLAY", &compositor.display)
            .env(nimbus_ipc::SOCKET_ENV, &compositor.control)
            // Software rendering keeps screenshots independent of the GPU, unless the test run asks for another renderer.
            .env(
                "NIMBUS_SHELL_RENDERER",
                std::env::var("NIMBUS_SHELL_RENDERER").as_deref().unwrap_or("software"),
            )
            .env("HOME", dir)
            .env("XDG_CONFIG_HOME", dir.join("xdg-config"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_DATA_DIRS", dir.join("data"))
            .env("DBUS_SESSION_BUS_ADDRESS", self.bus.as_deref().unwrap_or(&no_bus))
            .env("DBUS_SYSTEM_BUS_ADDRESS", &no_bus)
            .env("RUST_LOG", "info")
            .env_remove("WAYLAND_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("start nimbus-shell");
        self.shell = Some(shell);
    }

    /// Sends `signal` to the shell and waits for it to exit.
    pub fn stop_shell(&mut self, signal: nix::sys::signal::Signal) -> ExitStatus {
        let mut shell = self.shell.take().expect("the shell is running");
        let pid = nix::unistd::Pid::from_raw(i32::try_from(shell.id()).unwrap());
        nix::sys::signal::kill(pid, signal).expect("signal the shell");
        wait_exit(&mut shell)
    }

    /// Stops the compositor and waits for the shell to notice.
    pub fn stop_compositor(&mut self) -> ExitStatus {
        self.compositor.kill();
        wait_exit(self.shell.as_mut().expect("the shell is running"))
    }

    pub fn shell_log(&self) -> String {
        std::fs::read_to_string(self.compositor.dir.path().join("shell.log")).unwrap_or_default()
    }

    pub fn request(&self, request: Request) {
        self.compositor.request(request);
    }

    pub fn state(&self) -> CompositorState {
        self.compositor.state()
    }

    pub fn locked(&self) -> bool {
        self.compositor.locked()
    }

    pub fn screenshot(&self) -> image::RgbaImage {
        self.compositor.screenshot(OUTPUT)
    }

    /// Takes screenshots until `cond` holds for one.
    pub fn wait_screenshot(
        &self,
        what: &str,
        cond: impl Fn(&image::RgbaImage) -> bool,
    ) -> image::RgbaImage {
        wait_for(what, || Some(self.screenshot()).filter(&cond))
    }

    pub fn wait_log(&self, what: &str, text: &str) {
        wait_for(what, || self.shell_log().contains(text).then_some(()));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(shell) = &mut self.shell {
            let _ = shell.kill();
            let _ = shell.wait();
        }
        if std::thread::panicking() {
            eprintln!("--- shell log ---\n{}", self.shell_log());
        }
    }
}

fn wait_exit(child: &mut Child) -> ExitStatus {
    wait_for("the process to exit", || child.try_wait().expect("wait for the process"))
}

/// Mean color of a rectangle.
pub fn mean(image: &image::RgbaImage, x: u32, y: u32, w: u32, h: u32) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for py in y..y + h {
        for px in x..x + w {
            let p = image.get_pixel(px, py).0;
            for (s, c) in sum.iter_mut().zip(p) {
                *s += f64::from(c);
            }
        }
    }
    sum.map(|s| s / f64::from(w * h))
}

pub fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum()
}

/// Share of pixels that differ noticeably between two images of the same size.
pub fn changed_fraction(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    let changed = a
        .pixels()
        .zip(b.pixels())
        .filter(|(p, q)| {
            p.0.iter().zip(q.0).take(3).map(|(x, y)| u32::from(x.abs_diff(y))).sum::<u32>() > 24
        })
        .count();
    changed as f64 / f64::from(a.width() * a.height())
}

/// Whether the panel is drawn: its strip differs from the backdrop just below it.
pub fn panel_visible(shot: &image::RgbaImage) -> bool {
    distance(mean(shot, 200, 4, 200, 20), mean(shot, 200, 60, 200, 20)) > 12.0
}

/// Whether the whole output is black, as the compositor draws it while locked without a lock surface.
pub fn black(shot: &image::RgbaImage) -> bool {
    shot.pixels().all(|p| p.0[..3].iter().all(|&c| c < 8))
}
