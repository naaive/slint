// SPDX-License-Identifier: MIT

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use nimbus_ipc::{CompositorState, Event, Ready, Request, Response};

use crate::TIMEOUT;

/// The compositor binary of this workspace, built before the first use so it's never stale.
///
/// `NIMBUS_COMPOSITOR` overrides it.
/// Cargo only provides `CARGO_BIN_EXE_*` for binaries of the package under test.
pub fn compositor_binary() -> &'static Path {
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

/// Options for [`Compositor::builder`].
pub struct CompositorBuilder {
    binary: Option<PathBuf>,
    config: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    prepare: Option<Box<dyn FnOnce(&Path)>>,
}

impl CompositorBuilder {
    /// Runs `binary` instead of [`compositor_binary`].
    pub fn binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = Some(binary.into());
        self
    }

    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// The sizes of the headless outputs, such as `1280x720,1920x1080`; the default is one 1280x720 output.
    pub fn outputs(self, sizes: &str) -> Self {
        self.env("NIMBUS_HEADLESS_OUTPUTS", sizes)
    }

    /// Runs `prepare` on the runtime directory before the compositor starts.
    pub fn prepare(mut self, prepare: impl FnOnce(&Path) + 'static) -> Self {
        self.prepare = Some(Box::new(prepare));
        self
    }

    /// Starts the compositor and waits until it reports readiness.
    pub fn start(self) -> Compositor {
        let dir = tempfile::tempdir().expect("temporary directory");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        if let Some(prepare) = self.prepare {
            prepare(&runtime);
        }
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, &self.config).unwrap();
        let log = dir.path().join("compositor.log");
        let binary = self.binary.as_deref().unwrap_or_else(|| compositor_binary());
        let mut child = Command::new(binary)
            .args(["--backend", "headless"])
            .args(&self.args)
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
            .env_remove(nimbus_ipc::SOCKET_ENV)
            .envs(self.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .expect("start the compositor");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let line = rx.recv_timeout(Duration::from_secs(120));
        let Some(ready) = line.as_deref().ok().and_then(Ready::parse) else {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "no readiness line ({line:?}); log:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        };
        Compositor { child, dir, display: ready.wayland_display, control: ready.socket, log }
    }
}

/// A headless compositor process in a temporary directory, killed on drop.
///
/// The directory holds `config.toml`, `compositor.log`, and the `runtime` directory.
pub struct Compositor {
    child: Child,
    pub dir: tempfile::TempDir,
    pub display: String,
    pub control: PathBuf,
    log: PathBuf,
}

impl Compositor {
    /// Prepares `--backend headless` with `config` as its configuration file.
    pub fn builder(config: &str) -> CompositorBuilder {
        CompositorBuilder {
            binary: None,
            config: config.into(),
            args: Vec::new(),
            env: Vec::new(),
            prepare: None,
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.path().join("config.toml")
    }

    pub fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("runtime")
    }

    /// The path of the Wayland socket.
    pub fn wayland_socket(&self) -> PathBuf {
        self.runtime_dir().join(&self.display)
    }

    pub fn ipc(&self) -> nimbus_ipc::Client {
        nimbus_ipc::Client::connect_to(&self.control).expect("connect to the control socket")
    }

    pub fn state(&self) -> CompositorState {
        match self.ipc().request(&Request::GetState).expect("get-state") {
            Response::State(state) => state,
            other => panic!("unexpected response {other:?}"),
        }
    }

    /// Subscribes to events, which a thread reads so waits can time out.
    pub fn subscribe(&self) -> Events {
        let (tx, rx) = mpsc::channel();
        let events = self.ipc().subscribe().expect("subscribe");
        std::thread::spawn(move || {
            for event in events.map_while(Result::ok) {
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        Events(rx)
    }

    pub fn locked(&self) -> bool {
        match self.ipc().request(&Request::GetLockState).expect("get-lock-state") {
            Response::LockState { locked, .. } => locked,
            other => panic!("unexpected response {other:?}"),
        }
    }

    /// Sends `request` and asserts that the compositor accepted it.
    pub fn request(&self, request: Request) {
        let response = self.ipc().request(&request).expect("request");
        assert_eq!(response, Response::Ok);
    }

    /// Polls the compositor state until `cond` holds.
    pub fn wait_state(
        &self,
        what: &str,
        cond: impl Fn(&CompositorState) -> bool,
    ) -> CompositorState {
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
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let shot = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = self.dir.path().join(format!("shot-{shot}.png"));
        self.request(Request::Screenshot { output: Some(output.into()), path: Some(path.clone()) });
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

    /// Waits up to `timeout` for the compositor to exit; returns whether it did.
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

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        self.kill();
        if std::thread::panicking() {
            eprintln!("--- compositor log ---\n{}", self.log());
        }
    }
}

/// Events of a [`Compositor::subscribe`] subscription.
pub struct Events(mpsc::Receiver<Event>);

impl Events {
    /// Waits for the first event that `pick` maps to a value.
    pub fn wait<T>(&self, what: &str, pick: impl Fn(Event) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self.0.recv_timeout(left).unwrap_or_else(|_| panic!("no {what}"));
            if let Some(value) = pick(event) {
                return value;
            }
        }
    }
}
