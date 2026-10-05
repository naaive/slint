// SPDX-License-Identifier: MIT

//! Starts the compositor headless as a subprocess and drives it with a minimal Wayland client.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_registry::{self, WlRegistry},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

pub const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Compositor {
    child: Child,
    pub dir: tempfile::TempDir,
    pub display: String,
    pub control: PathBuf,
    log: PathBuf,
}

impl Compositor {
    /// Starts `--backend headless --no-shell` with `config` as its configuration file.
    pub fn start(config: &str, extra_env: &[(&str, &str)]) -> Self {
        Self::launch(config, extra_env, false)
    }

    /// Starts `--backend headless` with the shell.
    /// Applications come from an empty data directory and D-Bus is unreachable unless `extra_env` says otherwise.
    pub fn start_with_shell(config: &str, extra_env: &[(&str, &str)]) -> Self {
        Self::launch(config, extra_env, true)
    }

    fn launch(config: &str, extra_env: &[(&str, &str)], shell: bool) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, config).unwrap();
        let log = dir.path().join("compositor.log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_nimbus-compositor"));
        command.args(["--backend", "headless"]);
        if shell {
            let data = dir.path().join("data");
            std::fs::create_dir_all(&data).unwrap();
            let no_bus = format!("unix:path={}", dir.path().join("no-bus").display());
            command
                .env("XDG_DATA_HOME", &data)
                .env("XDG_DATA_DIRS", &data)
                .env("DBUS_SESSION_BUS_ADDRESS", &no_bus)
                .env("DBUS_SYSTEM_BUS_ADDRESS", &no_bus);
        } else {
            command.arg("--no-shell");
        }
        command
            .arg("--config")
            .arg(&config_path)
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
            .stderr(std::fs::File::create(&log).unwrap());
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("start the compositor");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let line = match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(line) => line,
            Err(_) => {
                let _ = child.kill();
                panic!(
                    "no readiness line; log:\n{}",
                    std::fs::read_to_string(&log).unwrap_or_default()
                );
            }
        };
        let mut display = None;
        let mut control = None;
        assert!(line.starts_with("NIMBUS_READY "), "unexpected first line: {line}");
        for part in line.split_whitespace().skip(1) {
            if let Some(v) = part.strip_prefix("WAYLAND_DISPLAY=") {
                display = Some(v.to_owned());
            } else if let Some(v) = part.strip_prefix("NIMBUS_SOCKET=") {
                control = Some(PathBuf::from(v));
            }
        }
        Self { child, dir, display: display.unwrap(), control: control.unwrap(), log }
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("runtime")
    }

    pub fn ipc(&self) -> nimbus_ipc::Client {
        nimbus_ipc::Client::connect_to(&self.control).expect("connect to the control socket")
    }

    pub fn state(&self) -> nimbus_ipc::CompositorState {
        match self.ipc().request(&nimbus_ipc::Request::GetState).expect("get-state") {
            nimbus_ipc::Response::State(state) => state,
            other => panic!("unexpected response {other:?}"),
        }
    }

    pub fn request(&self, request: nimbus_ipc::Request) {
        let response = self.ipc().request(&request).expect("request");
        assert_eq!(response, nimbus_ipc::Response::Ok);
    }

    /// Polls the compositor state until `cond` holds.
    pub fn wait_state(
        &self,
        what: &str,
        cond: impl Fn(&nimbus_ipc::CompositorState) -> bool,
    ) -> nimbus_ipc::CompositorState {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let state = self.state();
            if cond(&state) {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last state {state:#?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Captures `output` through the control socket.
    pub fn screenshot(&self, output: &str) -> image::RgbaImage {
        let path = self.dir.path().join(format!("shot-{}.png", rand_suffix()));
        self.request(nimbus_ipc::Request::Screenshot {
            output: Some(output.into()),
            path: Some(path.clone()),
        });
        let image = image::open(&path).expect("read the screenshot").into_rgba8();
        let _ = std::fs::remove_file(&path);
        image
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_running() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            eprintln!("--- compositor log ---\n{}", self.log());
        }
    }
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

pub struct TestWindow {
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    pub configured: bool,
    /// The last size the compositor asked for; (0, 0) lets the client choose.
    pub requested: (i32, i32),
    /// The size of the attached buffer.
    pub size: (i32, i32),
    pub states: Vec<xdg_toplevel::State>,
    pub close_requested: bool,
    pub destroyed: bool,
    pending: (i32, i32),
    pending_states: Vec<xdg_toplevel::State>,
}

#[derive(Default)]
pub struct App {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    pub windows: Vec<TestWindow>,
    pub default_size: (i32, i32),
}

pub struct TestClient {
    pub conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    pub app: App,
}

impl TestClient {
    pub fn connect(runtime_dir: &Path, display: &str) -> Self {
        let stream =
            UnixStream::connect(runtime_dir.join(display)).expect("connect to the Wayland socket");
        let conn = Connection::from_socket(stream).expect("Wayland connection");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App { default_size: (400, 300), ..App::default() };
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(
            app.compositor.is_some() && app.shm.is_some() && app.wm_base.is_some(),
            "missing globals"
        );
        Self { conn, queue, qh, app }
    }

    /// Creates a toplevel and returns its index once it's mapped with a buffer.
    pub fn create_window(&mut self, app_id: &str, title: &str) -> usize {
        let index = self.app.windows.len();
        let compositor = self.app.compositor.as_ref().unwrap();
        let surface = compositor.create_surface(&self.qh, ());
        let xdg_surface =
            self.app.wm_base.as_ref().unwrap().get_xdg_surface(&surface, &self.qh, index);
        let toplevel = xdg_surface.get_toplevel(&self.qh, index);
        toplevel.set_app_id(app_id.into());
        toplevel.set_title(title.into());
        surface.commit();
        self.app.windows.push(TestWindow {
            surface,
            xdg_surface,
            toplevel,
            configured: false,
            requested: (0, 0),
            size: (0, 0),
            states: Vec::new(),
            close_requested: false,
            destroyed: false,
            pending: (0, 0),
            pending_states: Vec::new(),
        });
        self.dispatch_until("the first configure", |app| app.windows[index].configured);
        index
    }

    pub fn destroy_window(&mut self, index: usize) {
        let window = &mut self.app.windows[index];
        window.toplevel.destroy();
        window.xdg_surface.destroy();
        window.surface.destroy();
        window.destroyed = true;
        self.conn.flush().expect("flush");
    }

    pub fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.app).expect("roundtrip");
        // Event handlers queue requests, such as buffer commits, that a roundtrip doesn't send.
        self.conn.flush().expect("flush");
    }

    pub fn dispatch_until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.roundtrip();
            if cond(&self.app) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn attach_buffer(app: &App, qh: &QueueHandle<App>, surface: &WlSurface, (w, h): (i32, i32)) {
    let shm = app.shm.as_ref().unwrap();
    let stride = w * 4;
    let len = usize::try_from(stride * h).unwrap();
    let mut file = tempfile::tempfile().expect("shm file");
    file.write_all(&vec![0xffu8; len]).unwrap();
    use std::os::fd::AsFd;
    let pool: WlShmPool = shm.create_pool(file.as_fd(), stride * h, qh, ());
    let buffer = pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, w, h);
    surface.commit();
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    app.compositor = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, version.min(5), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
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

impl Dispatch<XdgSurface, usize> for App {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        &index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            let window = &mut app.windows[index];
            if window.destroyed {
                return;
            }
            window.requested = window.pending;
            window.states = window.pending_states.clone();
            let size = if window.pending.0 > 0 && window.pending.1 > 0 {
                window.pending
            } else if window.size.0 > 0 {
                window.size
            } else {
                app.default_size
            };
            window.configured = true;
            let changed = size != window.size;
            window.size = size;
            let surface = window.surface.clone();
            if changed {
                attach_buffer(app, qh, &surface, size);
            } else {
                surface.commit();
            }
        }
    }
}

impl Dispatch<XdgToplevel, usize> for App {
    fn event(
        app: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let window = &mut app.windows[index];
        match event {
            xdg_toplevel::Event::Configure { width, height, states } => {
                window.pending = (width, height);
                window.pending_states = states
                    .chunks_exact(4)
                    .filter_map(|c| {
                        xdg_toplevel::State::try_from(u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                            .ok()
                    })
                    .collect();
            }
            xdg_toplevel::Event::Close => window.close_requested = true,
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, ()> for App {
    fn event(
        _: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
