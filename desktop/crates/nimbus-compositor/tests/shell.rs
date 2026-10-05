// SPDX-License-Identifier: MIT

//! End-to-end tests of the compositor with the in-process shell: rendering, IPC toggles, and window stacking.

mod common;

use common::{Compositor, TIMEOUT, TestClient};
use nimbus_ipc::Request;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT: &str = "HEADLESS-1";
const CONFIG: &str = "favorites = []\n[appearance]\ncolor_scheme = \"dark\"\nanimations = false\n[panel]\nshow_dock = true\n";

/// Mean color of a rectangle.
fn mean(image: &image::RgbaImage, x: u32, y: u32, w: u32, h: u32) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for py in y..y + h {
        for px in x..x + w {
            let p = image.get_pixel(px, py).0;
            for c in 0..3 {
                sum[c] += f64::from(p[c]);
            }
        }
    }
    sum.map(|s| s / f64::from(w * h))
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum()
}

/// Share of pixels that differ noticeably between two images of the same size.
fn changed_fraction(a: &image::RgbaImage, b: &image::RgbaImage) -> f64 {
    let changed = a
        .pixels()
        .zip(b.pixels())
        .filter(|(p, q)| {
            p.0.iter().zip(q.0).take(3).map(|(x, y)| x.abs_diff(y) as u32).sum::<u32>() > 24
        })
        .count();
    changed as f64 / f64::from(a.width() * a.height())
}

/// Takes screenshots until `cond` holds for one.
fn wait_screenshot(
    compositor: &Compositor,
    what: &str,
    cond: impl Fn(&image::RgbaImage) -> bool,
) -> image::RgbaImage {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let shot = compositor.screenshot(OUTPUT);
        if cond(&shot) {
            return shot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether the panel is drawn: its strip differs from the backdrop just below it.
fn panel_visible(shot: &image::RgbaImage) -> bool {
    let panel = mean(shot, 200, 4, 200, 20);
    let below = mean(shot, 200, 60, 200, 20);
    distance(panel, below) > 12.0
}

#[test]
fn shell_renders_toggles_and_stays_above_windows() {
    let compositor = Compositor::start_with_shell(CONFIG, &[]);
    assert!(compositor.log().contains("shell started"), "{}", compositor.log());

    let idle = wait_screenshot(&compositor, "the panel", panel_visible);
    assert_eq!(idle.dimensions(), (1280, 720));
    // The clock in the center of the panel draws light text on the dark bar.
    let brightest = (560..720)
        .flat_map(|x| (6..26).map(move |y| (x, y)))
        .map(|(x, y)| idle.get_pixel(x, y).0[0])
        .max();
    assert!(brightest.is_some_and(|b| b > 180), "no clock text in the panel");

    // The launcher covers the desktop and goes away again.
    compositor.request(Request::ToggleLauncher);
    let open =
        wait_screenshot(&compositor, "the launcher", |shot| changed_fraction(&idle, shot) > 0.3);
    assert!(changed_fraction(&idle, &open) > 0.3);
    compositor.request(Request::ToggleLauncher);
    wait_screenshot(&compositor, "the launcher to close", |shot| {
        changed_fraction(&idle, shot) < 0.02
    });

    // A maximized client fills the area below the panel, and the panel stays on top.
    let mut client = TestClient::connect(&compositor.runtime_dir(), &compositor.display);
    client.create_window("org.example.White", "White");
    let state = compositor.wait_state("the window", |s| s.windows.len() == 1);
    let id = state.windows[0].id;
    compositor.request(Request::SetMaximized { id, maximized: true });
    client.dispatch_until("the maximized size", |app| app.windows[0].size.1 > 300);
    let (width, height) = client.app.windows[0].size;
    assert_eq!(width, 1280);
    assert!(height < 720, "the panel's exclusive zone is respected: {height}");
    let shot =
        wait_screenshot(&compositor, "the white window", |shot| client_visible(shot, 640, 360));
    assert!(mean(&shot, 200, 4, 200, 20)[0] < 128.0, "the window covers the panel");
    assert!(
        client_visible(&shot, 640, 720 - height as u32 / 2),
        "the window isn't below the panel"
    );

    // Toggling the overview through IPC keeps the client alive and listed.
    compositor.request(Request::ToggleOverview);
    wait_screenshot(&compositor, "the overview", |s| changed_fraction(&shot, s) > 0.2);
    compositor.request(Request::ToggleOverview);
    wait_screenshot(&compositor, "the overview to close", |s| client_visible(s, 640, 360));
    client.roundtrip();
    assert_eq!(compositor.state().windows.len(), 1);
}

fn client_visible(shot: &image::RgbaImage, x: u32, y: u32) -> bool {
    let p = shot.get_pixel(x, y.min(shot.height() - 1)).0;
    p[0] > 240 && p[1] > 240 && p[2] > 240
}

/// A private session bus, or `None` when `dbus-daemon` can't run here.
struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut line = String::new();
        let stdout = child.stdout.take()?;
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line).ok()?;
        let address = line.trim().to_owned();
        if address.is_empty() {
            let _ = child.kill();
            return None;
        }
        Some(Self { child, address })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn notifications_show_toasts() {
    let Some(bus) = Bus::start() else {
        eprintln!("skipping: dbus-daemon is unavailable");
        return;
    };
    let compositor =
        Compositor::start_with_shell(CONFIG, &[("DBUS_SESSION_BUS_ADDRESS", &bus.address)]);
    let idle = wait_screenshot(&compositor, "the panel", panel_visible);
    let deadline = Instant::now() + TIMEOUT;
    while !compositor.log().contains("Serving org.freedesktop.Notifications") {
        assert!(
            Instant::now() < deadline,
            "the notification server didn't start:\n{}",
            compositor.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let id: u32 = runtime.block_on(async {
        let connection = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .expect("connect to the private bus");
        let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
        let reply = connection
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &(
                    "Tests",
                    0u32,
                    "",
                    "Build finished",
                    "All 42 tests passed",
                    Vec::<&str>::new(),
                    hints,
                    10_000i32,
                ),
            )
            .await
            .expect("Notify");
        reply.body().deserialize().expect("notification id")
    });
    assert!(id > 0);
    // The toast appears in the top right corner.
    wait_screenshot(&compositor, "the toast", |shot| {
        let region = |s: &image::RgbaImage| mean(s, 900, 50, 360, 90);
        distance(region(&idle), region(shot)) > 20.0
    });
}
