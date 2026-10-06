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
            // The surfaces of the shell's parts log when they open and close.
            .env("RUST_LOG", "info,nimbus_shell::output=debug")
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
        self.wait_log_count(what, text, 1);
    }

    /// Waits until the shell logged `text` at least `count` times.
    pub fn wait_log_count(&self, what: &str, text: &str, count: usize) {
        wait_for(what, || (self.shell_log().matches(text).count() >= count).then_some(()));
    }

    /// Waits until the surface of `part`, such as `Overlay` or `Popup(QuickSettings)`, opened `count` times in all.
    pub fn wait_opened(&self, part: &str, count: usize) {
        let text = format!("part opened output={OUTPUT} part={part}\n");
        self.wait_log_count(&format!("{part} to open"), &text, count);
    }

    /// Waits until the surface of `part` closed `count` times in all.
    pub fn wait_closed(&self, part: &str, count: usize) {
        let text = format!("part closed output={OUTPUT} part={part}\n");
        self.wait_log_count(&format!("{part} to close"), &text, count);
    }

    /// How many times the surface of `part` opened so far.
    pub fn opened(&self, part: &str) -> usize {
        self.shell_log().matches(&format!("part opened output={OUTPUT} part={part}\n")).count()
    }

    /// Clicks the left button at a point of the output, in logical pixels.
    pub fn click(&self, x: f64, y: f64) {
        self.request(Request::Click { output: OUTPUT.into(), x, y });
    }

    /// Presses and releases the key with a Linux input event code.
    pub fn press_key(&self, code: u32) {
        self.request(Request::PressKey { code });
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

/// How many pixels of a rectangle differ noticeably between two images of the same size.
pub fn changed_pixels(
    a: &image::RgbaImage,
    b: &image::RgbaImage,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> usize {
    (y..y + h)
        .flat_map(|py| (x..x + w).map(move |px| (px, py)))
        .filter(|&(px, py)| {
            let (p, q) = (a.get_pixel(px, py).0, b.get_pixel(px, py).0);
            p.iter().zip(q).take(3).map(|(x, y)| u32::from(x.abs_diff(y))).sum::<u32>() > 24
        })
        .count()
}

/// Whether any pixel of a rectangle is light, as text and icons are on the dark shell and backdrop.
fn light(shot: &image::RgbaImage, x: u32, y: u32, w: u32, h: u32) -> bool {
    (x..x + w)
        .flat_map(|x| (y..y + h).map(move |y| (x, y)))
        .any(|(x, y)| shot.get_pixel(x, y).0[0] > 180)
}

/// Whether the panel is drawn: the clock in its middle shows light text.
pub fn panel_visible(shot: &image::RgbaImage) -> bool {
    light(shot, 560, 6, 160, 20)
}

/// Whether the panel and the dock are drawn; the dock of the tests shows only the applications button.
pub fn shell_visible(shot: &image::RgbaImage) -> bool {
    panel_visible(shot) && light(shot, 625, 665, 30, 30)
}

/// Whether the whole output is black, as the compositor draws it while locked without a lock surface.
pub fn black(shot: &image::RgbaImage) -> bool {
    shot.pixels().all(|p| p.0[..3].iter().all(|&c| c < 8))
}
