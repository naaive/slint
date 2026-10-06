// SPDX-License-Identifier: MIT

//! Runs the compositor headless with `--no-shell` and the `nimbus-shell` binary as its client.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use nimbus_ipc::{CompositorState, Request, Response};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

pub const TIMEOUT: Duration = Duration::from_secs(30);
pub const OUTPUT: &str = "HEADLESS-1";
pub const CONFIG: &str = "favorites = []\n[appearance]\ncolor_scheme = \"dark\"\nanimations = false\n[panel]\nshow_dock = true\n";

/// The compositor binary of this workspace, built before the first use so it's never stale.
///
/// `NIMBUS_COMPOSITOR` overrides it.
/// Cargo only provides `CARGO_BIN_EXE_*` for binaries of the package under test.
fn compositor_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        if let Some(path) = std::env::var_os("NIMBUS_COMPOSITOR") {
            return path.into();
        }
        // Test executables live in `<target>/<profile>/deps`.
        let exe = std::env::current_exe().expect("the test executable's path");
        let profile_dir = exe.parent().and_then(Path::parent).expect("a target directory");
        let target_dir = profile_dir.parent().expect("a target directory");
        let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
            Some("debug") | None => "dev",
            Some(profile) => profile,
        };
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let status = Command::new(&cargo)
            .args(["build", "--package", "nimbus-compositor", "--profile", profile])
            .arg("--manifest-path")
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", target_dir)
            .status()
            .unwrap_or_else(|err| panic!("cannot run {cargo:?} to build nimbus-compositor: {err}"));
        assert!(status.success(), "building nimbus-compositor failed: {status}");
        let binary = profile_dir.join("nimbus-compositor");
        assert!(binary.is_file(), "nimbus-compositor wasn't built at {}", binary.display());
        binary
    })
}

/// A headless compositor without its in-process shell, and the shell process connected to it.
pub struct Session {
    compositor: Child,
    shell: Option<Child>,
    dir: tempfile::TempDir,
    pub display: String,
    pub control: PathBuf,
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
        let dir = tempfile::tempdir().expect("temporary directory");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        let log = std::fs::File::create(dir.path().join("compositor.log")).unwrap();
        let mut compositor = Command::new(compositor_binary())
            .args(["--backend", "headless", "--no-shell", "--config"])
            .arg(dir.path().join("config.toml"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("xdg-config"))
            .env("NIMBUS_HEADLESS_OUTPUTS", "1280x720")
            .env("RUST_LOG", "info")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .env_remove("DISPLAY")
            .env_remove("NIMBUS_SOCKET")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .expect("start the compositor");
        let stdout = compositor.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let Ok(line) = rx.recv_timeout(Duration::from_secs(120)) else {
            let _ = compositor.kill();
            panic!("the compositor didn't report readiness");
        };
        assert!(line.starts_with("NIMBUS_READY "), "unexpected first line: {line}");
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix(name))
                .unwrap_or_else(|| panic!("no {name} in {line}"))
                .to_owned()
        };
        Self {
            display: field("WAYLAND_DISPLAY="),
            control: PathBuf::from(field("NIMBUS_SOCKET=")),
            compositor,
            shell: None,
            dir,
            bus: bus.map(str::to_owned),
        }
    }

    /// Starts the shell; its log goes to `shell.log`, appended to across restarts.
    pub fn start_shell(&mut self) {
        assert!(self.shell.is_none(), "the shell is running");
        let dir = self.dir.path();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("shell.log"))
            .unwrap();
        let no_bus = format!("unix:path={}", dir.join("no-bus").display());
        let shell = Command::new(env!("CARGO_BIN_EXE_nimbus-shell"))
            .arg("--config")
            .arg(dir.join("config.toml"))
            .env("XDG_RUNTIME_DIR", dir.join("runtime"))
            .env("WAYLAND_DISPLAY", &self.display)
            .env("NIMBUS_SOCKET", &self.control)
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
        let _ = self.compositor.kill();
        let _ = self.compositor.wait();
        wait_exit(self.shell.as_mut().expect("the shell is running"))
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("runtime")
    }

    pub fn shell_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("shell.log")).unwrap_or_default()
    }

    pub fn compositor_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("compositor.log")).unwrap_or_default()
    }

    pub fn ipc(&self) -> nimbus_ipc::Client {
        nimbus_ipc::Client::connect_to(&self.control).expect("connect to the control socket")
    }

    pub fn request(&self, request: Request) {
        assert_eq!(self.ipc().request(&request).expect("request"), Response::Ok);
    }

    pub fn state(&self) -> CompositorState {
        match self.ipc().request(&Request::GetState).expect("get-state") {
            Response::State(state) => state,
            other => panic!("unexpected response {other:?}"),
        }
    }

    pub fn locked(&self) -> bool {
        match self.ipc().request(&Request::GetLockState).expect("get-lock-state") {
            Response::LockState { locked } => locked,
            other => panic!("unexpected response {other:?}"),
        }
    }

    pub fn screenshot(&self) -> image::RgbaImage {
        let path = self.dir.path().join("shot.png");
        self.request(Request::Screenshot { output: Some(OUTPUT.into()), path: Some(path.clone()) });
        image::open(&path).expect("read the screenshot").into_rgba8()
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
        let _ = self.compositor.kill();
        let _ = self.compositor.wait();
        if std::thread::panicking() {
            eprintln!("--- compositor log ---\n{}", self.compositor_log());
            eprintln!("--- shell log ---\n{}", self.shell_log());
        }
    }
}

fn wait_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("wait for the process") {
            return status;
        }
        assert!(Instant::now() < deadline, "the process didn't exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Polls `poll` until it returns a value.
pub fn wait_for<T>(what: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
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

/// A Wayland client with one white toplevel.
pub struct Window {
    conn: Connection,
    queue: EventQueue<WindowState>,
    state: WindowState,
}

#[derive(Default)]
pub struct WindowState {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    surface: Option<WlSurface>,
    pending: (i32, i32),
    size: (i32, i32),
}

impl Window {
    pub fn open(session: &Session) -> Self {
        let stream = UnixStream::connect(session.runtime_dir().join(&session.display))
            .expect("connect to the Wayland socket");
        let conn = Connection::from_socket(stream).expect("Wayland connection");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = WindowState::default();
        queue.roundtrip(&mut state).expect("roundtrip");
        let surface = state.compositor.as_ref().expect("wl_compositor").create_surface(&qh, ());
        let xdg = state.wm_base.as_ref().expect("xdg_wm_base").get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("org.example.White".into());
        surface.commit();
        state.surface = Some(surface);
        let mut window = Self { conn, queue, state };
        window.dispatch_until("the first buffer", |(width, _)| width > 0);
        window
    }

    /// The size of the attached buffer.
    pub fn size(&self) -> (i32, i32) {
        self.state.size
    }

    /// Dispatches until `cond` holds for the size of the attached buffer.
    pub fn dispatch_until(&mut self, what: &str, cond: impl Fn((i32, i32)) -> bool) {
        wait_for(what, || {
            self.queue.roundtrip(&mut self.state).expect("roundtrip");
            // Event handlers queue requests, such as commits, that a roundtrip doesn't send.
            self.conn.flush().expect("flush");
            cond(self.state.size).then_some(())
        });
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for WindowState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => state.wm_base = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for WindowState {
    fn event(
        _: &mut Self,
        base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for WindowState {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure { width, height, .. } = event {
            state.pending = (width, height);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for WindowState {
    fn event(
        state: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let xdg_surface::Event::Configure { serial } = event else {
            return;
        };
        xdg.ack_configure(serial);
        let size =
            if state.pending.0 > 0 && state.pending.1 > 0 { state.pending } else { (400, 300) };
        let surface = state.surface.as_ref().unwrap();
        if size != state.size {
            state.size = size;
            let (w, h) = size;
            let mut file = tempfile::tempfile().expect("shm file");
            file.write_all(&vec![0xff; usize::try_from(w * h * 4).unwrap()]).unwrap();
            let pool = state.shm.as_ref().unwrap().create_pool(
                std::os::fd::AsFd::as_fd(&file),
                w * h * 4,
                qh,
                (),
            );
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
            pool.destroy();
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, w, h);
        }
        surface.commit();
    }
}

delegate_noop!(WindowState: ignore WlCompositor);
delegate_noop!(WindowState: ignore WlSurface);
delegate_noop!(WindowState: ignore WlShm);
delegate_noop!(WindowState: ignore WlShmPool);
delegate_noop!(WindowState: ignore WlBuffer);
