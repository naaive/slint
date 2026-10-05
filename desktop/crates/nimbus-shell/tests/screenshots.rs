// SPDX-License-Identifier: MIT

//! Renders the shell with the software renderer at 1280x800 in its main states, over a wallpaper-like gradient,
//! and checks that each state draws what it should.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/shell-<state>.png`.
//!
//! The Slint platform can be set once per thread, so each test thread renders all states in sequence.

mod support;

use std::path::Path;
use std::rc::Rc;

use nimbus_services::ServiceEvent;
use nimbus_shell::{Osd, Popup, Shell, ShellAction};
use nimbus_theme::headless::{Frame, Headless};
use slint::Rgb8Pixel;
use slint::platform::software_renderer::PremultipliedRgbaColor;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

/// A soft diagonal gradient with two glows, standing in for a wallpaper.
fn backdrop(x: u32, y: u32) -> [f32; 3] {
    let (fx, fy) = (x as f32 / WIDTH as f32, y as f32 / HEIGHT as f32);
    let t = (fx * 0.6 + fy * 0.4).clamp(0.0, 1.0);
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    let mut rgb = [lerp(38.0, 18.0), lerp(52.0, 24.0), lerp(110.0, 58.0)];
    let glow = |cx: f32, cy: f32, radius: f32| {
        let d = ((fx - cx).powi(2) + ((fy - cy) * 0.625).powi(2)).sqrt();
        (1.0 - d / radius).clamp(0.0, 1.0).powi(2)
    };
    let pink = glow(0.78, 0.3, 0.45);
    let teal = glow(0.18, 0.85, 0.5);
    for (channel, (p, q)) in rgb.iter_mut().zip([(180.0, 20.0), (70.0, 140.0), (150.0, 150.0)]) {
        *channel += pink * p * 0.55 + teal * q * 0.45;
    }
    rgb
}

/// Renders the shell over [`backdrop`].
fn render(headless: &Headless) -> Frame {
    let pixels = headless
        .render_pixels::<PremultipliedRgbaColor>()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
            let under = backdrop(x, y);
            let alpha = f32::from(p.alpha) / 255.0;
            let over = [p.red, p.green, p.blue];
            let [r, g, b] = std::array::from_fn(|c| {
                (f32::from(over[c]) + under[c] * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8
            });
            Rgb8Pixel { r, g, b }
        })
        .collect();
    Frame { width: WIDTH, height: HEIGHT, pixels }
}

fn differs_from_backdrop(frame: &Frame, x: u32, y: u32) -> bool {
    let expected = backdrop(x, y);
    let (r, g, b) = frame.pixel(x, y).expect("the pixel is inside the frame");
    [r, g, b].iter().zip(expected).any(|(a, b)| (f32::from(*a) - b).abs() > 6.0)
}

fn save(name: &str, frame: &Frame) {
    let Some(_) = std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS") else {
        return;
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../docs/screenshots/shell-{name}.png"));
    frame.write_png(&path).expect("the screenshot is written");
}

#[test]
fn shell_states_render() {
    let headless = Headless::install(WIDTH, HEIGHT).expect("no platform was set on this thread");

    let dir = tempfile::tempdir().expect("temporary directory");
    let (apps, icons) = support::apps(dir.path()).expect("mock apps are written");
    let mut config = support::config();
    // Snapshots must not catch a transition halfway.
    config.appearance.animations = false;
    let actions = Rc::new(std::cell::RefCell::new(Vec::<ShellAction>::new()));
    let sink = actions.clone();
    let shell =
        Shell::new(&config, move |action| sink.borrow_mut().push(action)).expect("shell starts");
    shell.set_config_path(dir.path().join("config.toml"));
    shell.set_output_name(support::OUTPUT);
    shell.set_apps(&apps, &icons);
    shell.set_compositor_state(&support::compositor_state());
    shell.handle_service_event(&ServiceEvent::State(support::system_state()));
    shell.show().expect("the window shows");

    // Idle: the panel along the top and the dock at the bottom, the rest showing the wallpaper.
    let idle = render(&headless);
    assert!(differs_from_backdrop(&idle, 640, 10), "the panel draws");
    assert!(differs_from_backdrop(&idle, 640, HEIGHT - 30), "the dock draws");
    assert!(!differs_from_backdrop(&idle, 640, 400), "the middle stays clear");
    save("idle", &idle);

    shell.toggle_launcher();
    let launcher = render(&headless);
    assert!(differs_from_backdrop(&launcher, 640, 400), "the launcher covers the output");
    save("launcher", &launcher);
    shell.toggle_launcher();

    shell.component().invoke_popup_requested(Popup::QuickSettings);
    let quick_settings = render(&headless);
    assert!(differs_from_backdrop(&quick_settings, WIDTH - 200, 200), "quick settings draw");
    save("quick-settings", &quick_settings);
    shell.component().invoke_popup_requested(Popup::None);

    shell.toggle_overview();
    let overview = render(&headless);
    assert!(differs_from_backdrop(&overview, 640, 400), "the overview covers the output");
    save("overview", &overview);
    shell.toggle_overview();

    for notification in support::notifications() {
        shell.handle_service_event(&ServiceEvent::Notification(notification));
    }
    shell.show_osd(Osd::Volume { level: 0.62, muted: false });
    let toasts = render(&headless);
    assert!(differs_from_backdrop(&toasts, WIDTH - 200, 80), "toasts draw");
    save("toast-osd", &toasts);

    // Lets the OSD time out; toasts step aside while a popup is open.
    std::thread::sleep(std::time::Duration::from_millis(1600));
    shell.component().invoke_popup_requested(Popup::Calendar);
    let calendar = render(&headless);
    assert!(differs_from_backdrop(&calendar, 640, 300), "the calendar draws");
    save("calendar", &calendar);
    shell.component().invoke_popup_requested(Popup::None);

    shell.set_locked(true);
    let lock = render(&headless);
    assert!(
        (0..WIDTH).step_by(40).all(|x| differs_from_backdrop(&lock, x, HEIGHT / 2)),
        "the lock screen is opaque"
    );
    save("lock", &lock);
    shell.set_locked(false);

    let mut light = config.clone();
    light.appearance.color_scheme = nimbus_config::ColorScheme::Light;
    shell.set_config(&light);
    shell.component().invoke_popup_requested(Popup::QuickSettings);
    let light_settings = render(&headless);
    save("quick-settings-light", &light_settings);

    assert!(actions.borrow().is_empty(), "rendering emits no actions: {:?}", actions.borrow());
}
