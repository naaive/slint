// SPDX-License-Identifier: MIT

//! Renders the shell with the software renderer at 1280x800 in its main states, over a wallpaper-like gradient,
//! and checks that each state draws what it should.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/shell-<state>.png`.
//!
//! The Slint platform can be set once per thread, so each test thread renders all states in sequence.

mod support;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use nimbus_services::ServiceEvent;
use nimbus_shell::{Osd, Popup, Shell, ShellAction};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{PhysicalSize, PlatformError};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

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

fn render(window: &MinimalSoftwareWindow) -> Vec<[u8; 3]> {
    slint::platform::update_timers_and_animations();
    let mut buffer = vec![PremultipliedRgbaColor::default(); (WIDTH * HEIGHT) as usize];
    window.request_redraw();
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH as usize);
    });
    assert!(drawn, "the window didn't redraw");
    buffer
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
            let under = backdrop(x, y);
            let alpha = f32::from(p.alpha) / 255.0;
            let over = [p.red, p.green, p.blue];
            std::array::from_fn(|c| {
                (f32::from(over[c]) + under[c] * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8
            })
        })
        .collect()
}

fn pixel(buffer: &[[u8; 3]], x: u32, y: u32) -> [u8; 3] {
    buffer[(y * WIDTH + x) as usize]
}

fn differs_from_backdrop(buffer: &[[u8; 3]], x: u32, y: u32) -> bool {
    let expected = backdrop(x, y);
    pixel(buffer, x, y).iter().zip(expected).any(|(a, b)| (f32::from(*a) - b).abs() > 6.0)
}

fn save(name: &str, buffer: &[[u8; 3]]) {
    let Some(_) = std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS") else {
        return;
    };
    let dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots");
    std::fs::create_dir_all(&dir).expect("screenshot directory can be created");
    let bytes: Vec<u8> = buffer.iter().flatten().copied().collect();
    let image = image::RgbImage::from_raw(WIDTH, HEIGHT, bytes).expect("buffer matches the size");
    image.save(dir.join(format!("shell-{name}.png"))).expect("PNG encodes");
}

#[test]
fn shell_states_render() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform { window: window.clone() }))
        .expect("no platform was set on this thread");
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));

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
    let idle = render(&window);
    assert!(differs_from_backdrop(&idle, 640, 10), "the panel draws");
    assert!(differs_from_backdrop(&idle, 640, HEIGHT - 30), "the dock draws");
    assert!(!differs_from_backdrop(&idle, 640, 400), "the middle stays clear");
    save("idle", &idle);

    shell.toggle_launcher();
    let launcher = render(&window);
    assert!(differs_from_backdrop(&launcher, 640, 400), "the launcher covers the output");
    save("launcher", &launcher);
    shell.toggle_launcher();

    shell.component().invoke_popup_requested(Popup::QuickSettings);
    let quick_settings = render(&window);
    assert!(differs_from_backdrop(&quick_settings, WIDTH - 200, 200), "quick settings draw");
    save("quick-settings", &quick_settings);
    shell.component().invoke_popup_requested(Popup::None);

    shell.toggle_overview();
    let overview = render(&window);
    assert!(differs_from_backdrop(&overview, 640, 400), "the overview covers the output");
    save("overview", &overview);
    shell.toggle_overview();

    for notification in support::notifications() {
        shell.handle_service_event(&ServiceEvent::Notification(notification));
    }
    shell.show_osd(Osd::Volume { level: 0.62, muted: false });
    let toasts = render(&window);
    assert!(differs_from_backdrop(&toasts, WIDTH - 200, 80), "toasts draw");
    save("toast-osd", &toasts);

    // Lets the OSD time out; toasts step aside while a popup is open.
    std::thread::sleep(std::time::Duration::from_millis(1600));
    shell.component().invoke_popup_requested(Popup::Calendar);
    let calendar = render(&window);
    assert!(differs_from_backdrop(&calendar, 640, 300), "the calendar draws");
    save("calendar", &calendar);
    shell.component().invoke_popup_requested(Popup::None);

    shell.set_locked(true);
    let lock = render(&window);
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
    let light_settings = render(&window);
    save("quick-settings-light", &light_settings);

    assert!(actions.borrow().is_empty(), "rendering emits no actions: {:?}", actions.borrow());
}
