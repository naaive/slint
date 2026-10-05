// SPDX-License-Identifier: MIT

//! End-to-end tests: the headless compositor, a Wayland client, and the control socket.

mod common;

use common::{Compositor, TIMEOUT, TestClient};
use nimbus_ipc::{Event, LayoutMode, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CONFIG: &str = "[workspaces]\ncount = 4\ngaps = 10\nlayout = \"floating\"\n";

#[test]
fn windows_are_reported_with_metadata_and_focus() {
    let compositor = Compositor::start(CONFIG, &[]);
    let state = compositor.state();
    assert_eq!(state.workspace_count, 4);
    assert_eq!(state.active_workspace, 0);
    assert_eq!(state.layout, LayoutMode::Floating);
    assert_eq!(state.outputs.len(), 1);
    assert_eq!(state.outputs[0].name, "HEADLESS-1");
    assert_eq!((state.outputs[0].width, state.outputs[0].height), (1280, 720));

    let mut client = TestClient::connect(&compositor.runtime_dir(), &compositor.display);
    client.create_window("org.nimbus.First", "First window");
    client.create_window("org.nimbus.Second", "Second window");

    let state = compositor.wait_state("two windows", |s| s.windows.len() == 2);
    let first =
        state.windows.iter().find(|w| w.app_id == "org.nimbus.First").expect("first window");
    let second =
        state.windows.iter().find(|w| w.app_id == "org.nimbus.Second").expect("second window");
    assert_eq!(first.title, "First window");
    assert_eq!(second.title, "Second window");
    assert_eq!((first.workspace, second.workspace), (0, 0));
    assert_eq!(first.output, "HEADLESS-1");
    assert!(!first.focused, "the older window lost focus");
    assert!(second.focused, "the newest window has focus");
    assert!(!first.minimized && !first.maximized && !first.fullscreen);

    // Floating windows keep the size they chose.
    assert_eq!(client.app.windows[0].requested, (0, 0));

    // Activating the first window moves focus to it.
    compositor.request(Request::Activate { id: first.id });
    let state = compositor
        .wait_state("focus change", |s| s.windows.iter().any(|w| w.id == first.id && w.focused));
    assert_eq!(state.windows.iter().filter(|w| w.focused).count(), 1);
    client.dispatch_until("activated state", |app| {
        app.windows[0]
            .states
            .contains(&wayland_protocols::xdg::shell::client::xdg_toplevel::State::Activated)
    });
}

#[test]
fn subscribers_receive_window_events() {
    let compositor = Compositor::start(CONFIG, &[]);
    let (tx, rx) = mpsc::channel();
    let subscriber = compositor.ipc().subscribe().expect("subscribe");
    std::thread::spawn(move || {
        for event in subscriber {
            match event {
                Ok(event) => {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut client = TestClient::connect(&compositor.runtime_dir(), &compositor.display);
    let index = client.create_window("org.nimbus.Events", "Events");

    let next = |pred: &dyn Fn(&Event) -> bool| -> Event {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = rx.recv_timeout(left).expect("event before the timeout");
            if pred(&event) {
                return event;
            }
        }
    };
    let opened = next(&|e| matches!(e, Event::WindowOpened(_)));
    let Event::WindowOpened(info) = opened else { unreachable!() };
    assert_eq!(info.app_id, "org.nimbus.Events");

    client.app.windows[index].toplevel.set_title("Renamed".into());
    client.roundtrip();
    next(&|e| matches!(e, Event::WindowChanged(w) if w.id == info.id && w.title == "Renamed"));

    compositor.request(Request::SwitchWorkspace { workspace: 2 });
    next(&|e| matches!(e, Event::WorkspaceActivated { workspace: 2 }));
    compositor.request(Request::SetLayout { layout: LayoutMode::Tiling });
    next(&|e| matches!(e, Event::LayoutChanged { layout: LayoutMode::Tiling }));

    client.destroy_window(index);
    client.roundtrip();
    next(&|e| matches!(e, Event::WindowClosed { id } if *id == info.id));
}

#[test]
fn workspaces_close_and_tiling_work() {
    let compositor = Compositor::start(CONFIG, &[]);
    let mut client = TestClient::connect(&compositor.runtime_dir(), &compositor.display);
    let a = client.create_window("org.nimbus.A", "A");
    let b = client.create_window("org.nimbus.B", "B");
    let state = compositor.wait_state("two windows", |s| s.windows.len() == 2);
    let id_a = state.windows.iter().find(|w| w.app_id == "org.nimbus.A").unwrap().id;
    let id_b = state.windows.iter().find(|w| w.app_id == "org.nimbus.B").unwrap().id;

    // Tiling: two windows split the 1280x720 output with 10 px gaps.
    compositor.request(Request::SetLayout { layout: LayoutMode::Tiling });
    client.dispatch_until("tiled sizes", |app| {
        app.windows[a].requested == (625, 700) && app.windows[b].requested == (625, 700)
    });
    assert_eq!(compositor.state().layout, LayoutMode::Tiling);

    // Maximize overrides the tile, and restoring re-tiles.
    compositor.request(Request::SetMaximized { id: id_a, maximized: true });
    client.dispatch_until("maximized size", |app| app.windows[a].requested == (1280, 720));
    assert!(compositor.state().windows.iter().any(|w| w.id == id_a && w.maximized));
    // The other window now tiles alone.
    client.dispatch_until("single tile", |app| app.windows[b].requested == (1260, 700));
    compositor.request(Request::SetMaximized { id: id_a, maximized: false });
    client.dispatch_until("re-tiled", |app| {
        app.windows[a].requested == (625, 700) && app.windows[b].requested == (625, 700)
    });

    // Moving a window away leaves the other alone on the workspace.
    compositor.request(Request::MoveToWorkspace { id: id_b, workspace: 1 });
    let state = compositor
        .wait_state("moved window", |s| s.windows.iter().any(|w| w.id == id_b && w.workspace == 1));
    assert_eq!(state.active_workspace, 0);
    assert!(state.windows.iter().any(|w| w.id == id_a && w.focused));
    client.dispatch_until("full tile", |app| app.windows[a].requested == (1260, 700));

    compositor.request(Request::SwitchWorkspace { workspace: 1 });
    let state = compositor.wait_state("switched workspace", |s| s.active_workspace == 1);
    assert!(state.windows.iter().any(|w| w.id == id_b && w.focused));

    // Invalid workspaces and windows are rejected.
    let mut ipc = compositor.ipc();
    assert!(ipc.request(&Request::SwitchWorkspace { workspace: 9 }).is_err());
    assert!(ipc.request(&Request::MoveToWorkspace { id: id_a, workspace: 9 }).is_err());
    assert!(ipc.request(&Request::Close { id: 999 }).is_err());

    // Close asks the client; once it destroys the toplevel, the window is gone.
    compositor.request(Request::Close { id: id_b });
    client.dispatch_until("close request", |app| app.windows[b].close_requested);
    client.destroy_window(b);
    client.roundtrip();
    let state = compositor.wait_state("closed window", |s| s.windows.len() == 1);
    assert_eq!(state.windows[0].id, id_a);
    assert!(!state.windows[0].focused, "the remaining window is on another workspace");
}

#[test]
fn minimize_and_fullscreen_round_trip() {
    let compositor = Compositor::start(CONFIG, &[]);
    let mut client = TestClient::connect(&compositor.runtime_dir(), &compositor.display);
    let a = client.create_window("org.nimbus.Full", "Full");
    let id = compositor.wait_state("window", |s| s.windows.len() == 1).windows[0].id;

    compositor.request(Request::SetFullscreen { id, fullscreen: true });
    client.dispatch_until("fullscreen size", |app| app.windows[a].requested == (1280, 720));
    assert!(compositor.state().windows[0].fullscreen);
    compositor.request(Request::SetFullscreen { id, fullscreen: false });
    let state = compositor.wait_state("left fullscreen", |s| !s.windows[0].fullscreen);
    assert!(state.windows[0].focused);

    compositor.request(Request::SetMinimized { id, minimized: true });
    let state = compositor.wait_state("minimized", |s| s.windows[0].minimized);
    assert!(!state.windows[0].focused);
    compositor.request(Request::Activate { id });
    let state = compositor.wait_state("restored", |s| !s.windows[0].minimized);
    assert!(state.windows[0].focused);
}

#[test]
fn spawn_runs_commands_in_the_session_environment() {
    let compositor = Compositor::start(CONFIG, &[]);
    let out = compositor.dir.path().join("env.txt");
    let command = format!("env > '{0}.tmp' && mv '{0}.tmp' '{0}'", out.display());
    compositor.request(Request::Spawn { command });
    let deadline = Instant::now() + TIMEOUT;
    while !out.exists() {
        assert!(Instant::now() < deadline, "the spawned command didn't run");
        std::thread::sleep(Duration::from_millis(20));
    }
    let env = std::fs::read_to_string(&out).unwrap();
    assert!(env.lines().any(|l| l == format!("WAYLAND_DISPLAY={}", compositor.display)), "{env}");
    assert!(
        env.lines().any(|l| l == format!("NIMBUS_SOCKET={}", compositor.control.display())),
        "{env}"
    );
    assert!(env.lines().any(|l| l == "XDG_CURRENT_DESKTOP=Nimbus"), "{env}");
}

#[test]
fn control_socket_survives_bad_clients() {
    let compositor = Compositor::start(CONFIG, &[]);

    // Malformed JSON gets an error reply, and the connection stays usable.
    let stream = UnixStream::connect(&compositor.control).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    writer.write_all(b"this is not json\n{\"request\":\"get-state\"}\n").unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(matches!(serde_json::from_str::<Response>(&line).unwrap(), Response::Error { .. }));
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert!(matches!(serde_json::from_str::<Response>(&line).unwrap(), Response::State(_)));

    // A subscriber that disconnects, and one that never reads, don't disturb the compositor.
    drop(compositor.ipc().subscribe().unwrap());
    let _silent = compositor.ipc().subscribe().unwrap();
    let mut half = UnixStream::connect(&compositor.control).unwrap();
    half.write_all(b"{\"request\":\"get-st").unwrap();
    drop(half);
    for workspace in [1, 2, 0, 3, 0] {
        compositor.request(Request::SwitchWorkspace { workspace });
    }
    assert_eq!(compositor.state().active_workspace, 0);
}

#[test]
fn config_changes_apply_live() {
    let compositor = Compositor::start(CONFIG, &[]);
    let config_path = compositor.dir.path().join("config.toml");
    std::fs::write(&config_path, "[workspaces]\ncount = 6\ngaps = 0\nlayout = \"tiling\"\n")
        .unwrap();
    let state = compositor.wait_state("reloaded config", |s| {
        s.workspace_count == 6 && s.layout == LayoutMode::Tiling
    });
    assert_eq!(state.active_workspace, 0);
    std::fs::write(&config_path, "[workspaces]\ncount = 2\n").unwrap();
    compositor.request(Request::ReloadConfig);
    assert_eq!(compositor.state().workspace_count, 2);
}

#[test]
fn headless_screenshots_and_quit() {
    let shots = tempfile::tempdir().unwrap();
    let dir = shots.path().to_str().unwrap().to_owned();
    let mut compositor = Compositor::start(
        "[appearance]\nscale = 1.0\n",
        &[("NIMBUS_HEADLESS_SCREENSHOT_DIR", &dir), ("NIMBUS_HEADLESS_SCREENSHOT_FRAMES", "3")],
    );
    let png = shots.path().join("HEADLESS-1.png");
    let deadline = Instant::now() + TIMEOUT;
    while !png.exists() {
        assert!(Instant::now() < deadline, "no screenshot written");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The file appears before it's complete; wait until it decodes.
    let image = loop {
        if let Ok(image) = image::open(&png) {
            break image;
        }
        assert!(Instant::now() < deadline, "screenshot never became readable");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!((image.width(), image.height()), (1280, 720));

    compositor.request(Request::Quit);
    assert!(compositor.wait_exit(TIMEOUT), "the compositor didn't exit");
    assert!(!compositor.control.exists(), "the control socket was removed");
}
