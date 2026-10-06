// SPDX-License-Identifier: MIT

//! End-to-end tests of `nimbus-shell` against a headless compositor.

mod common;

use common::{
    CONFIG, PrivateBus, Session, TestClient, black, changed_fraction, distance, mean, panel_visible,
};
use nimbus_ipc::Request;
use nix::sys::signal::Signal;

#[test]
fn panel_renders_and_launcher_and_overview_toggle() {
    let session = Session::start(CONFIG, None);
    // The clock in the center of the panel draws light text on the dark bar.
    let idle = session.wait_screenshot("the panel and its clock", |shot| {
        panel_visible(shot)
            && (560..720)
                .flat_map(|x| (6..26).map(move |y| (x, y)))
                .any(|(x, y)| shot.get_pixel(x, y).0[0] > 180)
    });
    assert_eq!(idle.dimensions(), (1280, 720));

    // The compositor turns the request into a shell command event, which the shell carries out.
    session.request(Request::ToggleLauncher);
    session.wait_screenshot("the launcher", |shot| changed_fraction(&idle, shot) > 0.3);
    session.request(Request::ToggleLauncher);
    session.wait_screenshot("the launcher to close", |shot| changed_fraction(&idle, shot) < 0.02);
    session.request(Request::ToggleOverview);
    session.wait_screenshot("the overview", |shot| changed_fraction(&idle, shot) > 0.2);
    session.request(Request::ToggleOverview);
    session.wait_screenshot("the overview to close", |shot| changed_fraction(&idle, shot) < 0.02);
}

#[test]
fn maximized_windows_stay_below_the_panel() {
    let session = Session::start(CONFIG, None);
    session.wait_screenshot("the panel", panel_visible);
    let mut client = TestClient::connect(&session.compositor);
    let index = client.create_window("org.example.White", "White");
    let id = common::wait_for("the window", || session.state().windows.first().map(|w| w.id));
    session.request(Request::SetMaximized { id, maximized: true });
    client.dispatch_until("the maximized size", |app| app.windows[index].size.1 > 300);
    let (width, height) = client.app.windows[index].size;
    assert_eq!(width, 1280);
    assert!(height < 720 - 30, "the panel and dock reserve space: {height}");
    // The panel is drawn above the window, which starts right below it.
    let shot = session.wait_screenshot("the white window", |shot| {
        shot.get_pixel(640, 360).0[..3].iter().all(|&c| c > 240)
    });
    assert!(mean(&shot, 200, 4, 200, 20)[0] < 128.0, "the window covers the panel");
}

#[test]
fn lock_requests_show_the_lock_screen_on_a_lock_surface() {
    let mut session = Session::start(CONFIG, None);
    let idle = session.wait_screenshot("the panel", panel_visible);
    session.request(Request::Lock);
    assert!(session.locked());
    // Without a lock client, the compositor draws black; anything else is the shell's lock surface.
    let locked = session.wait_screenshot("the lock screen", |shot| {
        !black(shot) && changed_fraction(&idle, shot) > 0.3
    });
    session.wait_log("the shell to hold the lock", "locked");

    // A shell that dies leaves the session locked and black.
    session.stop_shell(Signal::SIGKILL);
    session.wait_screenshot("a black screen", black);
    assert!(session.locked());

    // A restarted shell sees the lock and shows its lock screen again.
    session.start_shell();
    let again = session.wait_screenshot("the lock screen again", |shot| !black(shot));
    assert!(session.locked());
    assert!(changed_fraction(&locked, &again) < 0.05, "the same lock screen comes back");
}

#[test]
fn exits_cleanly_on_sigterm_and_with_failure_without_the_compositor() {
    let mut session = Session::start(CONFIG, None);
    session.wait_screenshot("the panel", panel_visible);
    let status = session.stop_shell(Signal::SIGTERM);
    assert!(status.success(), "SIGTERM: {status}");

    session.start_shell();
    session.wait_log("the restarted shell", "shell started");
    let status = session.stop_compositor();
    assert!(!status.success(), "losing the compositor: {status}");
}

#[test]
fn notifications_show_toasts() {
    let Some(bus) = PrivateBus::start() else { return };
    let session = Session::start(CONFIG, Some(&bus.address));
    let idle = session.wait_screenshot("the panel", panel_visible);
    session.wait_log("the notification server", "Serving org.freedesktop.Notifications");

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let id: u32 = runtime.block_on(async {
        let connection = bus.connect().await;
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
    session.wait_screenshot("the toast", |shot| {
        let region = |s: &image::RgbaImage| mean(s, 900, 50, 360, 90);
        distance(region(&idle), region(shot)) > 20.0
    });
}
