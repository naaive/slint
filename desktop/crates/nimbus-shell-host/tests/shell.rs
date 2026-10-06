// SPDX-License-Identifier: MIT

//! End-to-end tests of `nimbus-shell` against a headless compositor.

mod common;

use common::{
    CONFIG, PrivateBus, Session, TestClient, black, changed_fraction, changed_pixels, distance,
    mean, panel_visible, shell_visible,
};
use nimbus_ipc::Request;
use nimbus_test_support::{FakeBattery, FakeLogind, FakeUPower, InputMethod, WarningLevel};
use nix::sys::signal::Signal;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};

#[test]
fn panel_renders_and_launcher_and_overview_toggle() {
    let session = Session::start(CONFIG, None);
    let idle = session.wait_screenshot("the panel and the dock", shell_visible);
    assert_eq!(idle.dimensions(), (1280, 720));

    // The compositor turns the request into a shell command event, which the shell carries out
    // on a surface over the whole output, which closes with the launcher or overview.
    session.request(Request::ToggleLauncher);
    session.wait_screenshot("the launcher", |shot| changed_fraction(&idle, shot) > 0.3);
    session.wait_opened("Overlay", 1);
    session.request(Request::ToggleLauncher);
    session.wait_closed("Overlay", 1);
    session.wait_screenshot("the launcher to close", |shot| changed_fraction(&idle, shot) < 0.02);
    session.request(Request::ToggleOverview);
    session.wait_screenshot("the overview", |shot| changed_fraction(&idle, shot) > 0.2);
    session.request(Request::ToggleOverview);
    session.wait_closed("Overlay", 2);
    session.wait_screenshot("the overview to close", |shot| changed_fraction(&idle, shot) < 0.02);
}

#[test]
fn alt_tab_shows_the_switcher_until_alt_is_released() {
    const ALT: u32 = 56;
    const TAB: u32 = 15;
    let session = Session::start(CONFIG, None);
    session.wait_screenshot("the panel and the dock", shell_visible);
    let mut client = TestClient::connect(&session.compositor);
    client.create_window("org.example.First", "First");
    client.create_window("org.example.Second", "Second");
    let first = common::wait_for("two windows", || {
        let windows = session.state().windows;
        (windows.len() == 2).then_some(())?;
        windows.iter().find(|w| w.app_id == "org.example.First").map(|w| w.id)
    });
    let focused = || session.state().windows.into_iter().find(|w| w.focused).map(|w| w.id);
    assert_ne!(focused(), Some(first));

    session.request(Request::Key { code: ALT, pressed: true });
    session.press_key(TAB);
    session.wait_opened("Switcher", 1);
    session.request(Request::Key { code: ALT, pressed: false });
    session.wait_closed("Switcher", 1);
    assert_eq!(focused(), Some(first));

    // Choosing a window closes the launcher, which would keep the keyboard from it.
    session.request(Request::ToggleLauncher);
    session.wait_opened("Overlay", 1);
    session.request(Request::Key { code: ALT, pressed: true });
    session.press_key(TAB);
    session.wait_opened("Switcher", 2);
    session.request(Request::Key { code: ALT, pressed: false });
    session.wait_closed("Switcher", 2);
    session.wait_closed("Overlay", 1);
    assert_ne!(focused(), Some(first));
}

/// Maximizes a white window and returns its client and its size once it's drawn.
fn maximized_window(session: &Session) -> (TestClient, (i32, i32)) {
    let mut client = TestClient::connect(&session.compositor);
    let index = client.create_window("org.example.White", "White");
    let id = common::wait_for("the window", || session.state().windows.first().map(|w| w.id));
    session.request(Request::SetMaximized { id, maximized: true });
    client.dispatch_until("the maximized size", |app| app.windows[index].size.1 > 300);
    session.wait_screenshot("the white window", |shot| white(shot, 640, 360));
    let size = client.app.windows[index].size;
    (client, size)
}

fn white(shot: &image::RgbaImage, x: u32, y: u32) -> bool {
    shot.get_pixel(x, y).0[..3].iter().all(|&c| c > 240)
}

#[test]
fn the_panel_and_dock_reserve_their_space() {
    let session = Session::start(CONFIG, None);
    session.wait_screenshot("the panel and the dock", shell_visible);
    // 32 pixels of panel, and the dock with its margins.
    let (_client, size) = maximized_window(&session);
    assert_eq!(size, (1280, 720 - 32 - 84));
    let shot = session.screenshot();
    assert!(mean(&shot, 200, 4, 200, 20)[0] < 128.0, "the window stays below the panel");
    assert!(white(&shot, 200, 33), "the window starts right below the panel");
    assert!(white(&shot, 200, 32 + 603) && !white(&shot, 200, 32 + 605), "and ends above the dock");
    // The dock's surface takes input only on the dock, so the window gets clicks beside it.
    assert!(!white(&shot, 640, 690), "the dock draws in its space");
}

#[test]
fn an_autohidden_dock_reserves_no_space() {
    let config = CONFIG.replace("show_dock = true", "show_dock = true\ndock_autohide = true");
    let session = Session::start(&config, None);
    session.wait_screenshot("the panel", panel_visible);
    assert_eq!(maximized_window(&session).1, (1280, 720 - 32));
}

#[test]
fn launcher_opens_on_an_overlay_surface_with_the_keyboard() {
    let session = Session::start(CONFIG, None);
    let idle = session.wait_screenshot("the panel and the dock", shell_visible);
    session.request(Request::ToggleLauncher);
    session.wait_opened("Overlay", 1);
    let open = session.wait_screenshot("the launcher", |shot| changed_fraction(&idle, shot) > 0.3);

    // Keys reach the launcher's search field, since the overlay takes the keyboard.
    for code in [KEY_T, KEY_E, KEY_R, KEY_M] {
        session.press_key(code);
    }
    let typed = session.wait_screenshot("the typed query", |shot| {
        changed_pixels(&open, shot, 380, 40, 520, 100) > 50
    });
    // Escape clears the query, then closes the launcher and its surface.
    session.press_key(KEY_ESC);
    session.wait_screenshot("the cleared query", |shot| {
        changed_pixels(&open, shot, 380, 40, 520, 100) < 10
    });
    assert!(changed_pixels(&open, &typed, 380, 40, 520, 100) > 50);
    session.press_key(KEY_ESC);
    session.wait_closed("Overlay", 1);
    session.wait_screenshot("the launcher to close", |shot| changed_fraction(&idle, shot) < 0.02);
}

#[test]
fn an_input_method_types_chinese_into_the_launcher_search() {
    let session = Session::start(CONFIG, None);
    let mut ime = InputMethod::connect(&session.compositor);
    let idle = session.wait_screenshot("the panel and the dock", shell_visible);
    session.request(Request::ToggleLauncher);
    session.wait_opened("Overlay", 1);
    let open = session.wait_screenshot("the launcher", |shot| changed_fraction(&idle, shot) > 0.3);
    let search = |shot: &image::RgbaImage| changed_pixels(&open, shot, 380, 40, 520, 100);

    // The focused search field enables the text input with its empty text.
    ime.wait("the search field's text input", |text| {
        text.active
            && text.surrounding == Some((String::new(), 0, 0))
            && text.content_type.is_some_and(|(_, purpose)| purpose == ContentPurpose::Normal)
    });

    // A preedit string shows in the field, but isn't part of its text yet.
    ime.preedit("nihao");
    let composing = session.wait_screenshot("the preedit string", |shot| search(shot) > 50);
    assert_eq!(ime.state.current.surrounding, Some((String::new(), 0, 0)));

    // Committed text replaces the preedit string, and the field reports its new text and cursor.
    ime.commit_string("你好");
    ime.wait("the committed text", |text| text.surrounding == Some(("你好".into(), 6, 6)));
    session.wait_screenshot("the committed text", |shot| {
        changed_pixels(&composing, shot, 380, 40, 520, 100) > 50
    });
    ime.delete_surrounding(3, 0);
    ime.wait("the deleted character", |text| text.surrounding == Some(("你".into(), 3, 3)));

    // Closing the launcher takes the field and its text input away.
    session.request(Request::ToggleLauncher);
    session.wait_closed("Overlay", 1);
    ime.wait("the text input to go", |text| !text.active);
}

#[test]
fn the_lock_screen_asks_for_a_password_without_sharing_it() {
    let session = Session::start(CONFIG, None);
    let mut ime = InputMethod::connect(&session.compositor);
    session.wait_screenshot("the panel and the dock", shell_visible);
    session.request(Request::Lock);
    session.wait_log("the shell to hold the lock", "locked");
    ime.wait("the password field's text input", |text| {
        text.active
            && text.content_type
                == Some((
                    ContentHint::SensitiveData | ContentHint::HiddenText,
                    ContentPurpose::Password,
                ))
    });
    assert_eq!(ime.state.current.surrounding, None, "the password field shared its text");
}

#[test]
fn quick_settings_open_in_a_popup_and_close_on_a_click_outside() {
    let session = Session::start(CONFIG, None);
    let idle = session.wait_screenshot("the panel and the dock", shell_visible);
    let popup = |shot: &image::RgbaImage| changed_pixels(&idle, shot, 880, 60, 380, 300);

    // The status icons at the right end of the panel open quick settings below them.
    session.click(1250.0, 16.0);
    session.wait_opened("Popup(QuickSettings)", 1);
    session.wait_screenshot("quick settings", |shot| popup(shot) > 20_000);

    // A click on the desktop, outside the shell's surfaces, dismisses the popup.
    session.click(400.0, 400.0);
    session.wait_closed("Popup(QuickSettings)", 1);
    session.wait_screenshot("quick settings to close", |shot| popup(shot) < 100);

    // A click on the panel button toggles it, without dismissing first.
    session.click(1250.0, 16.0);
    session.wait_opened("Popup(QuickSettings)", 2);
    session.click(1250.0, 16.0);
    session.wait_closed("Popup(QuickSettings)", 2);
    assert_eq!(session.opened("Popup(QuickSettings)"), 2);
}

#[test]
fn lock_requests_show_the_lock_screen_on_a_lock_surface() {
    let mut session = Session::start(CONFIG, None);
    let idle = session.wait_screenshot("the panel and the dock", shell_visible);
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
fn toasts_appear_in_their_own_surface_and_expire() {
    let Some(bus) = PrivateBus::start() else { return };
    let session = Session::start(CONFIG, Some(&bus.address));
    let idle = session.wait_screenshot("the panel and the dock", shell_visible);
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
                    2_000i32,
                ),
            )
            .await
            .expect("Notify");
        reply.body().deserialize().expect("notification id")
    });
    assert!(id > 0);
    // The toast appears in the top right corner, on a surface that goes once the toast expires.
    let region = |s: &image::RgbaImage| mean(s, 900, 50, 360, 90);
    session.wait_screenshot("the toast", |shot| distance(region(&idle), region(shot)) > 20.0);
    session.wait_opened("Toasts", 1);
    session.wait_closed("Toasts", 1);
    session.wait_screenshot("the toast to go", |shot| distance(region(&idle), region(shot)) < 2.0);
}

#[test]
fn inserted_media_are_mounted_and_announced_in_a_toast() {
    let Some(bus) = PrivateBus::start() else { return };
    let runtime = fake_runtime();
    let udisks = runtime.block_on(nimbus_test_support::FakeUdisks::start(&bus));
    let session = start_with_system_bus(&bus);
    session.wait_log("the udisks client", "Watching volumes from udisks");

    let device = "/org/freedesktop/UDisks2/block_devices/sdb1";
    let answer = nimbus_test_support::MountAnswer::Mount;
    runtime.block_on(udisks.insert(device, "/dev/sdb1", "STICK", answer));
    session.wait_opened("Toasts", 1);
    common::wait_for("the stick to be mounted", || {
        udisks.calls().contains(&"mount STICK".into()).then_some(())
    });
}

/// A runtime for fakes on a private bus, which answer on its worker thread while the test waits.
fn fake_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap()
}

/// Starts the compositor and the shell with `bus` as the system bus.
fn start_with_system_bus(bus: &PrivateBus) -> Session {
    let mut session = Session::start_compositor(CONFIG, None);
    session.system_bus = Some(bus.address.clone());
    session.start_shell();
    session.wait_screenshot("the panel and the dock", shell_visible);
    session
}

#[test]
fn the_session_locks_before_suspend() {
    let Some(bus) = PrivateBus::start() else { return };
    let runtime = fake_runtime();
    let logind = runtime.block_on(FakeLogind::start(&bus));
    let session = start_with_system_bus(&bus);
    common::wait_for("the sleep inhibitor", || logind.inhibitors().pop());
    let inhibitor = &logind.inhibitors()[0];
    assert_eq!((inhibitor.what.as_str(), inhibitor.mode.as_str()), ("sleep", "delay"));
    assert!(!inhibitor.released);
    assert!(!session.locked());

    // Suspend goes ahead once the shell holds the lock.
    runtime.block_on(logind.prepare_for_sleep(true));
    common::wait_for("the inhibitor's release", || logind.inhibitors()[0].released.then_some(()));
    assert_eq!(session.compositor.lock_state(), (true, true));
    session.wait_log("the shell to hold the lock", "locked");

    // After resuming, the shell delays the next suspend again and stays locked.
    runtime.block_on(logind.prepare_for_sleep(false));
    common::wait_for("a new inhibitor", || (logind.inhibitors().len() == 2).then_some(()));
    assert!(!logind.inhibitors()[1].released);
    assert!(session.locked());
}

#[test]
fn low_battery_shows_a_toast_until_the_battery_charges() {
    let Some(bus) = PrivateBus::start() else { return };
    let runtime = fake_runtime();
    let upower = runtime
        .block_on(FakeUPower::start(&bus, FakeBattery::discharging(50.0, WarningLevel::None)));
    let session = start_with_system_bus(&bus);

    // A critical toast stays until the battery charges.
    runtime.block_on(upower.set(FakeBattery::discharging(4.0, WarningLevel::Critical)));
    session.wait_log("the critical warning", "battery warning level=Critical");
    session.wait_opened("Toasts", 1);
    runtime.block_on(upower.set(FakeBattery::charging(5.0)));
    session.wait_log("the warning to clear", "battery warning cleared");
    session.wait_closed("Toasts", 1);

    // Running low again is a new crossing.
    runtime.block_on(upower.set(FakeBattery::discharging(9.0, WarningLevel::Low)));
    session.wait_log("the low warning", "battery warning level=Low");
    session.wait_opened("Toasts", 2);
}

/// Linux input event codes.
const KEY_ESC: u32 = 1;
const KEY_E: u32 = 18;
const KEY_R: u32 = 19;
const KEY_T: u32 = 20;
const KEY_M: u32 = 50;
