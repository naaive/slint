// SPDX-License-Identifier: MIT

//! Renders the shell's parts with the software renderer, composited at 1280x800 in its main states over a wallpaper-like gradient,
//! and checks that each state draws what it should.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/shell-<state>.png`.
//!
//! The Slint platform can be set once per thread, so the test renders all states in sequence.

mod support;

use std::path::Path;
use std::rc::Rc;

use i_slint_backend_testing::ElementQuery;
use nimbus_services::{AuthenticationEvent, AuthenticationRequest, ServiceEvent};
use nimbus_shell::{
    LockView, Osd, Part, PartComponent, Popup, RectData, ShellAction, ShellModel, ShellView,
};
use nimbus_theme::headless::Frame;
use slint::Rgb8Pixel;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use support::desk::{Desk, Software};

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

/// Renders the shown parts over [`backdrop`].
fn render(desk: &Desk) -> Frame {
    let pixels = desk.render(backdrop).into_iter().map(|[r, g, b]| Rgb8Pixel { r, g, b }).collect();
    Frame { width: WIDTH, height: HEIGHT, pixels }
}

/// Renders `window`, such as a lock screen's, over the whole output.
fn render_window(window: &MinimalSoftwareWindow) -> Frame {
    window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    slint::platform::update_timers_and_animations();
    let mut pixels = vec![Rgb8Pixel::default(); (WIDTH * HEIGHT) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, WIDTH as usize);
    });
    Frame { width: WIDTH, height: HEIGHT, pixels }
}

/// Opens `popup` next to the panel button labeled `label`, as clicking it does.
fn open_popup(desk: &Desk, popup: Popup, label: &str) {
    let panel = desk.component(Part::Panel).expect("the panel shows");
    let PartComponent::Panel(ui) = &panel else { unreachable!() };
    let button = ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_label(label)
        .find_first()
        .unwrap_or_else(|| panic!("no button labeled {label:?}"));
    let (position, size) = (button.absolute_position(), button.size());
    let anchor = RectData { x: position.x, y: position.y, width: size.width, height: size.height };
    panel.output().invoke_popup_requested(popup, anchor);
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
    let software = Software::install().expect("no platform was set on this thread");

    let dir = tempfile::tempdir().expect("temporary directory");
    let (apps, icons) = support::apps(dir.path()).expect("mock apps are written");
    let mut config = support::config();
    // Snapshots must not catch a transition halfway.
    config.appearance.animations = false;
    let actions = Rc::new(std::cell::RefCell::new(Vec::<ShellAction>::new()));
    let sink = actions.clone();
    let model = ShellModel::new(&config, move |action| sink.borrow_mut().push(action));
    model.set_config_path(dir.path().join("config.toml"));
    model.set_apps(std::rc::Rc::new(apps), icons);
    model.set_compositor_state(&support::compositor_state());
    model.handle_service_event(&ServiceEvent::State(support::system_state()));
    let view = ShellView::new(&model, support::OUTPUT);
    let desk = Desk::new(view, WIDTH as f32, HEIGHT as f32, Some(software.clone()));

    // Idle: the panel along the top and the dock at the bottom, the rest showing the wallpaper.
    let idle = render(&desk);
    assert!(differs_from_backdrop(&idle, 640, 10), "the panel draws");
    assert!(differs_from_backdrop(&idle, 640, HEIGHT - 30), "the dock draws");
    assert!(!differs_from_backdrop(&idle, 640, 400), "the middle stays clear");
    save("idle", &idle);

    desk.view.toggle_launcher();
    let launcher = render(&desk);
    assert!(differs_from_backdrop(&launcher, 640, 400), "the launcher covers the output");
    save("launcher", &launcher);
    desk.view.toggle_launcher();

    open_popup(&desk, Popup::QuickSettings, "System menu");
    let quick_settings = render(&desk);
    assert!(differs_from_backdrop(&quick_settings, WIDTH - 200, 200), "quick settings draw");
    save("quick-settings", &quick_settings);
    desk.view.close_popup();

    desk.view.toggle_overview();
    let overview = render(&desk);
    assert!(differs_from_backdrop(&overview, 640, 400), "the overview covers the output");
    save("overview", &overview);
    desk.view.toggle_overview();

    desk.view.open_switcher(vec![1, 2, 3, 4, 7], 2);
    let switcher = render(&desk);
    assert!(differs_from_backdrop(&switcher, 640, 400), "the switcher draws in the middle");
    assert!(!differs_from_backdrop(&switcher, 100, 400), "and leaves the sides clear");
    save("switcher", &switcher);
    desk.view.close_switcher();

    for notification in support::notifications() {
        model.handle_service_event(&ServiceEvent::Notification(notification));
    }
    model.show_osd(Osd::Volume { level: 0.62, muted: false });
    let toasts = render(&desk);
    assert!(differs_from_backdrop(&toasts, WIDTH - 200, 80), "toasts draw");
    assert!(differs_from_backdrop(&toasts, 640, HEIGHT - 150), "the OSD draws");
    save("toast-osd", &toasts);

    // Lets the OSD time out; toasts step aside while a popup is open.
    std::thread::sleep(std::time::Duration::from_millis(1600));
    open_popup(&desk, Popup::Calendar, "Calendar and notifications");
    let calendar = render(&desk);
    assert!(differs_from_backdrop(&calendar, 640, 300), "the calendar draws");
    save("calendar", &calendar);
    desk.view.close_popup();

    // A polkit request with several identities, as the helper asks for a password.
    let started = AuthenticationEvent::Started(AuthenticationRequest {
        id: 1,
        action_id: "org.freedesktop.systemd1.manage-units".into(),
        message: "Authentication is required to restart the network service.".into(),
        icon_name: String::new(),
        identities: vec!["alice".into(), "root".into()],
        selected: 0,
    });
    let prompt = AuthenticationEvent::Prompt {
        id: 1,
        identity: 0,
        prompt: "Password: ".into(),
        echo: false,
    };
    for event in [started, prompt] {
        model.handle_service_event(&ServiceEvent::Authentication(event));
    }
    let auth = render(&desk);
    assert!(differs_from_backdrop(&auth, 640, 400), "the dialog draws");
    assert!(differs_from_backdrop(&auth, 20, 400), "the dialog dims the output");
    save("auth", &auth);
    model.handle_service_event(&ServiceEvent::Authentication(AuthenticationEvent::Ended { id: 1 }));

    // The lock screen is a window of its own, which a host shows instead of the parts.
    model.set_locked(true);
    let lock = LockView::new(&model).expect("the lock screen starts");
    let lock_window = software.take_created().expect("the lock screen has a window");
    lock.show().expect("the window shows");
    let frame = render_window(&lock_window);
    assert!(
        (0..WIDTH).step_by(40).all(|x| differs_from_backdrop(&frame, x, HEIGHT / 2)),
        "the lock screen is opaque"
    );
    save("lock", &frame);
    drop(lock);
    model.set_locked(false);

    let mut light = config.clone();
    light.appearance.color_scheme = nimbus_config::ColorScheme::Light;
    model.set_config(&light);
    open_popup(&desk, Popup::QuickSettings, "System menu");
    let light_settings = render(&desk);
    save("quick-settings-light", &light_settings);

    assert!(actions.borrow().is_empty(), "rendering emits no actions: {:?}", actions.borrow());
}
