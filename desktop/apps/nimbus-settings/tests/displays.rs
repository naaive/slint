// SPDX-License-Identifier: MIT

//! The display client against the headless compositor: listing heads and applying a configuration.

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use nimbus_settings::displays::wlr::WlrControl;
use nimbus_settings::displays::{DisplayControl, DisplayEvent, Head, HeadConfig};
use wayland_client::Connection;

const TIMEOUT: Duration = Duration::from_secs(30);

/// The compositor binary of this workspace, built first so it's never stale.
///
/// Cargo only provides `CARGO_BIN_EXE_*` for binaries of the package under test.
fn compositor_binary() -> PathBuf {
    // Test executables live in `<target>/<profile>/deps`.
    let exe = std::env::current_exe().expect("the test executable's path");
    let profile_dir = exe.parent().and_then(Path::parent).expect("a target directory");
    let target_dir = profile_dir.parent().expect("a target directory");
    let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
        Some("debug") | None => "dev",
        Some(profile) => profile,
    };
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(&cargo)
        .args(["build", "--package", "nimbus-compositor", "--profile", profile])
        .arg("--manifest-path")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
        .env("CARGO_TARGET_DIR", target_dir)
        .status()
        .unwrap_or_else(|err| panic!("cannot run {cargo:?} to build nimbus-compositor: {err}"));
    assert!(status.success(), "building nimbus-compositor failed: {status}");
    profile_dir.join("nimbus-compositor")
}

struct Compositor {
    child: Child,
    dir: tempfile::TempDir,
    socket: PathBuf,
    control: PathBuf,
}

impl Compositor {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(dir.path().join("config.toml"), "").unwrap();
        let mut child = Command::new(compositor_binary())
            .args(["--backend", "headless", "--config"])
            .arg(dir.path().join("config.toml"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("xdg-config"))
            .env("NIMBUS_HEADLESS_OUTPUTS", "1280x720,1920x1080")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(dir.path().join("compositor.log")).unwrap())
            .spawn()
            .expect("start the compositor");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let field = |name: &str| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix(name))
                .unwrap_or_else(|| panic!("no {name} in {line:?}"))
                .to_owned()
        };
        Self {
            socket: runtime.join(field("WAYLAND_DISPLAY=")),
            control: field("NIMBUS_SOCKET=").into(),
            child,
            dir,
        }
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if std::thread::panicking() {
            let log = std::fs::read_to_string(self.dir.path().join("compositor.log"));
            eprintln!("--- compositor log ---\n{}", log.unwrap_or_default());
        }
    }
}

fn next_heads(events: &mpsc::Receiver<DisplayEvent>) -> Vec<Head> {
    loop {
        match events.recv_timeout(TIMEOUT).expect("an event") {
            DisplayEvent::Heads(heads) => return heads,
            DisplayEvent::Unavailable(reason) => panic!("unavailable: {reason}"),
            DisplayEvent::Applied(_) => {}
        }
    }
}

#[test]
fn configures_the_compositor() {
    let compositor = Compositor::start();
    let socket = compositor.socket.clone();
    let (sender, events) = mpsc::channel();
    let control = WlrControl::spawn_on(
        move || {
            let stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
            Connection::from_socket(stream).map_err(|e| e.to_string())
        },
        Box::new(move |event| {
            let _ = sender.send(event);
        }),
    )
    .expect("the display thread starts");

    let heads = next_heads(&events);
    let names: Vec<&str> = heads.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"]);
    assert_eq!(heads[1].title(), "Headless");
    assert_eq!(heads[1].position, (1280, 0));
    let mode = heads[1].current_mode.expect("a current mode");
    assert_eq!((mode.width, mode.height, mode.preferred), (1920, 1080, true));

    let mut configuration: Vec<HeadConfig> = heads.iter().map(Head::config).collect();
    configuration[1].scale = 2.0;
    configuration[1].position = (0, 720);
    control.apply(configuration);
    let heads = next_heads(&events);
    assert_eq!((heads[1].scale, heads[1].position), (2.0, (0, 720)));
    match events.recv_timeout(TIMEOUT).expect("the outcome") {
        DisplayEvent::Applied(result) => assert_eq!(result, Ok(())),
        other => panic!("unexpected {other:?}"),
    }

    let mut client = nimbus_ipc::Client::connect_to(&compositor.control).expect("control socket");
    let nimbus_ipc::Response::State(state) =
        client.request(&nimbus_ipc::Request::GetState).expect("state")
    else {
        panic!("unexpected response");
    };
    assert_eq!(state.outputs[1].scale, 2.0);
}
