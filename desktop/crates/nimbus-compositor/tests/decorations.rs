// SPDX-License-Identifier: MIT

//! Server-side decorations: xdg-decoration negotiation, the titlebar's place and pixels, and its buttons.

mod common;

use common::{Compositor, TestClient};
use nimbus_ipc::Request;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode;

const CONFIG: &str = "[workspaces]\ngaps = 10\nlayout = \"floating\"\n";
const OUTPUT: &str = "HEADLESS-1";
/// The focused titlebar's color in the dark scheme, `surface-raised` in `nimbus-theme`.
const DARK_TITLEBAR: [u8; 3] = [0x2f, 0x2f, 0x34];

/// Opens a window asking for `decoration` and maximizes it; returns the client, the window's index, and its id.
fn maximized_window(
    compositor: &Compositor,
    decoration: Option<Mode>,
) -> (TestClient, usize, nimbus_ipc::WindowId) {
    let mut client = TestClient::connect(compositor);
    client.app.decoration = decoration;
    let index = client.create_window("org.nimbus.Decorated", "Decorated window");
    let state = compositor.wait_state("the window", |s| !s.windows.is_empty());
    let id = state.windows.last().unwrap().id;
    compositor.request(Request::SetMaximized { id, maximized: true });
    client.dispatch_until("the maximized size", |app| app.windows[index].requested.0 == 1280);
    (client, index, id)
}

/// The height of the titlebar above a maximized window on the 720 pixel high output.
fn titlebar_height(client: &TestClient, index: usize) -> i32 {
    720 - client.app.windows[index].requested.1
}

/// The center of `button` (0 for close, 1 for maximize, 2 for minimize) on a maximized window's titlebar.
fn button_center(height: i32, button: i32) -> (f64, f64) {
    let inset = (height as f32 * 0.18).round() as i32;
    let side = height - 2 * inset;
    let x = 1280 - inset - side / 2 - button * (side + inset);
    (f64::from(x), f64::from(height / 2))
}

fn rgb(shot: &image::RgbaImage, x: i32, y: i32) -> [u8; 3] {
    let p = shot.get_pixel(x as u32, y as u32).0;
    [p[0], p[1], p[2]]
}

fn click(compositor: &Compositor, (x, y): (f64, f64)) {
    compositor.request(Request::Click { output: OUTPUT.into(), x, y });
}

#[test]
fn decoration_modes_are_negotiated() {
    let compositor = common::start(CONFIG);
    let (server, s, _) = maximized_window(&compositor, Some(Mode::ServerSide));
    assert_eq!(server.app.windows[s].decoration_mode, Some(Mode::ServerSide));
    let height = titlebar_height(&server, s);
    assert!((20..80).contains(&height), "a {height} pixel titlebar");

    let (client, c, _) = maximized_window(&compositor, Some(Mode::ClientSide));
    assert_eq!(client.app.windows[c].decoration_mode, Some(Mode::ClientSide));
    assert_eq!(client.app.windows[c].requested, (1280, 720), "client-side windows are unchanged");

    let (silent, n, _) = maximized_window(&compositor, None);
    assert_eq!(silent.app.windows[n].decoration_mode, None);
    assert_eq!(titlebar_height(&silent, n), height, "clients that don't ask get a titlebar");
}

#[test]
fn tiled_windows_get_a_slim_bar() {
    let compositor = common::start(&CONFIG.replace("floating", "tiling"));
    let mut client = TestClient::connect(&compositor);
    client.app.decoration = Some(Mode::ServerSide);
    let index = client.create_window("org.nimbus.Tiled", "Tiled");
    client.dispatch_until("the tile", |app| app.windows[index].requested.0 == 1260);
    let slim = 700 - client.app.windows[index].requested.1;

    let (full_client, full, _) = maximized_window(&compositor, Some(Mode::ServerSide));
    let full = titlebar_height(&full_client, full);
    assert!(0 < slim && slim < full, "a {slim} pixel bar on tiles and {full} on maximized windows");
}

#[test]
fn the_titlebar_is_drawn_above_the_content() {
    let compositor = common::start(CONFIG);
    let (client, index, _) = maximized_window(&compositor, None);
    let height = titlebar_height(&client, index);
    let shot = compositor.screenshot(OUTPUT);

    assert_eq!(rgb(&shot, 4, height / 2), DARK_TITLEBAR, "the titlebar's background");
    assert_eq!(rgb(&shot, 640, height), [255; 3], "the window's content starts below it");
    assert_eq!(rgb(&shot, 640, 719), [255; 3]);
    let (cx, cy) = button_center(height, 0);
    assert_ne!(rgb(&shot, cx as i32, cy as i32), DARK_TITLEBAR, "the close button's symbol");

    let installed =
        std::process::Command::new("fc-match").output().is_ok_and(|o| o.status.success());
    if installed {
        let title_drawn = (0..height)
            .flat_map(|y| (12..1000).map(move |x| (x, y)))
            .any(|(x, y)| rgb(&shot, x, y)[0] > 0x80);
        assert!(title_drawn, "the title shows");
    } else {
        eprintln!("skipped the title check: fontconfig isn't installed");
    }

    // The theme follows the configuration.
    let config = format!("{CONFIG}[appearance]\ncolor_scheme = \"light\"\n");
    std::fs::write(compositor.config_path(), config).unwrap();
    let deadline = std::time::Instant::now() + common::TIMEOUT;
    loop {
        let shot = compositor.screenshot(OUTPUT);
        if rgb(&shot, 4, height / 2) == [255; 3] {
            assert_ne!(rgb(&shot, 4, height - 1), [255; 3], "a separator below the light titlebar");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the titlebar never turned light");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn fullscreen_windows_have_no_titlebar() {
    let compositor = common::start(CONFIG);
    let (mut client, index, id) = maximized_window(&compositor, Some(Mode::ServerSide));
    compositor.request(Request::SetFullscreen { id, fullscreen: true });
    client.dispatch_until("the fullscreen size", |app| app.windows[index].requested == (1280, 720));
    let shot = compositor.screenshot(OUTPUT);
    assert_eq!(rgb(&shot, 4, 4), [255; 3]);
}

#[test]
fn titlebar_buttons_and_double_clicks_work() {
    let compositor = common::start(CONFIG);
    let (mut client, index, id) = maximized_window(&compositor, Some(Mode::ServerSide));
    let height = titlebar_height(&client, index);
    let window = |compositor: &Compositor| {
        compositor.state().windows.into_iter().find(|w| w.id == id).expect("the window")
    };

    click(&compositor, button_center(height, 1));
    compositor.wait_state("restored by the maximize button", |s| {
        s.windows.iter().any(|w| w.id == id && !w.maximized)
    });

    compositor.request(Request::SetMaximized { id, maximized: true });
    compositor
        .wait_state("maximized again", |s| s.windows.iter().any(|w| w.id == id && w.maximized));
    click(&compositor, (300.0, f64::from(height / 2)));
    click(&compositor, (300.0, f64::from(height / 2)));
    compositor.wait_state("restored by a double click", |s| {
        s.windows.iter().any(|w| w.id == id && !w.maximized)
    });

    compositor.request(Request::SetMaximized { id, maximized: true });
    compositor
        .wait_state("maximized again", |s| s.windows.iter().any(|w| w.id == id && w.maximized));
    click(&compositor, button_center(height, 2));
    compositor.wait_state("minimized", |s| s.windows.iter().any(|w| w.id == id && w.minimized));

    compositor.request(Request::Activate { id });
    compositor.wait_state("shown again", |s| s.windows.iter().any(|w| w.id == id && !w.minimized));
    assert!(window(&compositor).maximized);
    click(&compositor, button_center(height, 0));
    client.dispatch_until("the close request", |app| app.windows[index].close_requested);
}
