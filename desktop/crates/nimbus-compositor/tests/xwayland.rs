// SPDX-License-Identifier: MIT

//! The X11 display, served on demand by `xwayland-satellite`, which a script stands in for.

mod common;

use nimbus_ipc::Request;
use std::io::Read;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Accepts one connection on the sockets from `-listenfd`, answers it with `X`, and exits,
/// after appending its arguments and the socket of each descriptor to `log`.
const FAKE_SATELLITE: &str = r#"
import os, select, socket, sys
if "--test-listenfd-support" in sys.argv:
    sys.exit(0)
fds = [int(sys.argv[i + 1]) for i, arg in enumerate(sys.argv) if arg == "-listenfd"]
with open(sys.argv[0] + ".log", "a") as log:
    kinds = [os.readlink(f"/proc/self/fd/{fd}").split(":")[0] for fd in fds]
    log.write(" ".join(sys.argv[1:] + kinds) + "\n")
listeners = [socket.socket(fileno=fd) for fd in fds]
readable, _, _ = select.select(listeners, [], [])
connection, _ = readable[0].accept()
connection.sendall(b"X")
"#;

fn python() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join("python3")).find(|p| p.is_file())
}

/// Writes an executable `script` with `interpreter` to `dir`; returns its path.
fn write_script(dir: &Path, interpreter: &Path, script: &str) -> PathBuf {
    let path = dir.join("satellite");
    std::fs::write(&path, format!("#!{}\n{script}", interpreter.display())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn config(satellite: &Path) -> String {
    format!(
        "[appearance]\nanimations = false\n[xwayland]\npath = {:?}\n",
        satellite.to_str().unwrap()
    )
}

fn spawn_log(satellite: &Path) -> Vec<String> {
    let log = std::fs::read_to_string(satellite.with_extension("log")).unwrap_or_default();
    log.lines().map(ToOwned::to_owned).collect()
}

/// Reads the satellite's answer, which proves it accepted on the socket the compositor created.
fn answer(mut stream: UnixStream) -> String {
    stream.set_read_timeout(Some(common::TIMEOUT)).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

#[test]
fn the_first_x11_client_starts_the_satellite_with_the_listening_sockets() {
    let Some(python) = python() else {
        eprintln!("skipping: python3, which stands in for xwayland-satellite, isn't installed");
        return;
    };
    let scripts = tempfile::tempdir().unwrap();
    let satellite = write_script(scripts.path(), &python, FAKE_SATELLITE);
    let mut compositor = common::start(&config(&satellite));
    assert_eq!(compositor.x11_display.as_deref(), Some(":0"), "{}", compositor.log());
    let lock = compositor.dir.path().join(".X0-lock");
    let socket = compositor.dir.path().join(".X11-unix/X0");
    assert!(lock.is_file() && socket.exists());

    let echoed = compositor.dir.path().join("display");
    compositor
        .request(Request::Spawn { command: format!("echo \"$DISPLAY\" > '{}'", echoed.display()) });
    common::wait_for("a child to report DISPLAY", || {
        std::fs::read_to_string(&echoed).ok().filter(|s| !s.is_empty())
    });
    assert_eq!(std::fs::read_to_string(&echoed).unwrap(), ":0\n");
    std::thread::sleep(Duration::from_millis(300));
    assert!(spawn_log(&satellite).is_empty(), "the satellite starts only on demand");

    assert_eq!(answer(UnixStream::connect(&socket).unwrap()), "X");
    let spawns = spawn_log(&satellite);
    assert_eq!(spawns.len(), 1);
    let args: Vec<_> = spawns[0].split(' ').collect();
    assert!(
        matches!(args[..], [":0", "-listenfd", _, "-listenfd", _, "socket", "socket"]),
        "{args:?}"
    );

    // The satellite exited, so the next client, here on the abstract socket, starts another.
    let address = SocketAddr::from_abstract_name(socket.as_os_str().as_bytes()).unwrap();
    assert_eq!(answer(UnixStream::connect_addr(&address).unwrap()), "X");
    assert_eq!(spawn_log(&satellite).len(), 2);

    compositor.request(Request::Quit);
    assert!(compositor.wait_exit(common::TIMEOUT));
    assert!(!lock.exists() && !socket.exists(), "the compositor frees the display");
}

#[test]
fn without_a_usable_satellite_display_stays_unset() {
    let scripts = tempfile::tempdir().unwrap();
    let too_old = write_script(scripts.path(), Path::new("/bin/sh"), "exit 1\n");
    for satellite in [too_old, scripts.path().join("missing")] {
        let compositor = common::start(&config(&satellite));
        assert_eq!(compositor.x11_display, None, "{}", compositor.log());
        assert!(compositor.log().contains("X11 apps are unavailable"));
        assert!(!compositor.dir.path().join(".X11-unix").exists());

        let echoed = compositor.dir.path().join("display");
        compositor.request(Request::Spawn {
            command: format!("echo \"${{DISPLAY-unset}}\" > '{}'", echoed.display()),
        });
        common::wait_for("a child to report DISPLAY", || {
            std::fs::read_to_string(&echoed).ok().filter(|s| !s.is_empty())
        });
        assert_eq!(std::fs::read_to_string(&echoed).unwrap(), "unset\n");
    }
}
