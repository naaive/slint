// SPDX-License-Identifier: MIT

//! The window's behavior on Slint's testing backend: tabs, shortcuts, search, and preferences.
//!
//! Slint's platform can be set once per process, so this binary has a single test.
//! Events from worker threads are handed to the controller here instead of by an event loop.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_channel::mpsc::UnboundedReceiver;
use nimbus_config::{Appearance, ColorScheme};
use nimbus_terminal::app::{self, AppEvent, Controller, Options};
use nimbus_terminal::fonts::FontSet;
use nimbus_terminal::keys::Modifiers;
use nimbus_terminal::prefs::Prefs;
use nimbus_terminal::{AppWindow, Preferences};
use slint::{ComponentHandle, Model};

/// Hands events to the controller until `done` holds or a few seconds pass.
fn pump_until(
    controller: &Rc<Controller>,
    events: &mut UnboundedReceiver<AppEvent>,
    done: impl Fn() -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        while let Ok(event) = events.try_recv() {
            controller.handle(event);
        }
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn tab_titles(window: &AppWindow) -> Vec<String> {
    let tabs = window.get_tabs();
    (0..tabs.row_count()).filter_map(|i| tabs.row_data(i)).map(|t| t.title.to_string()).collect()
}

#[test]
fn tabs_shortcuts_search_and_preferences() {
    i_slint_backend_testing::init_no_event_loop();
    let window = AppWindow::new().expect("the window is created");
    let (sender, mut events) = app::channel();
    let appearance =
        Appearance { color_scheme: ColorScheme::Dark, animations: false, ..Appearance::default() };
    let controller = Controller::new(
        &window,
        sender,
        Options { prefs: Prefs::default(), prefs_path: None, appearance, title: None },
    );
    let fonts = FontSet::discover("").ok().map(Arc::new);
    if let Some(fonts) = &fonts {
        controller.set_fonts(fonts.clone());
    }
    window.show().expect("the window shows");

    controller.open_detached_tab("first", b"needle in the scrollback\r\n");
    controller.open_detached_tab("second", b"hello\r\n");
    assert_eq!(tab_titles(&window), ["first", "second"]);
    assert_eq!(window.get_current_tab(), 1);
    assert_eq!(window.get_window_title(), "second");

    // Tab switching shortcuts.
    assert!(controller.key("\u{F72C}", Modifiers::CTRL));
    assert_eq!(window.get_current_tab(), 0);
    assert!(controller.key("2", Modifiers::ALT));
    assert_eq!(window.get_current_tab(), 1);

    // Ctrl+Shift+T opens a shell in a new tab, after finding the current directory on another thread.
    assert!(controller.key("T", Modifiers::CTRL_SHIFT));
    assert!(
        pump_until(&controller, &mut events, || controller.tab_count() == 3),
        "a third tab opens"
    );
    assert_eq!(window.get_current_tab(), 2);

    // Ctrl+Shift+W closes it right away, since only the shell runs in it.
    assert!(controller.key("W", Modifiers::CTRL_SHIFT));
    assert!(pump_until(&controller, &mut events, || controller.tab_count() == 2), "the tab closes");

    // Search in the first tab.
    controller.select_tab(0);
    assert!(controller.key("F", Modifiers::CTRL_SHIFT));
    assert!(window.get_search_open());
    window.invoke_search_edited("needle".into());
    assert_eq!(window.get_search_status(), "");
    window.invoke_search_edited("missing".into());
    assert_eq!(window.get_search_status(), "No matches");
    window.invoke_search_closed();
    assert!(!window.get_search_open());

    // Plain keys aren't taken by the window on a tab without a program.
    assert!(!controller.key("\u{10}", Modifiers::SHIFT));

    // Choosing a scheme recolors the terminal.
    let preferences = window.global::<Preferences>();
    assert!(preferences.get_schemes().row_count() >= 8);
    let dracula = (0..preferences.get_schemes().row_count())
        .find(|i| preferences.get_schemes().row_data(*i).is_some_and(|s| s.name == "Dracula"))
        .expect("Dracula is offered");
    preferences.invoke_scheme_selected(dracula as i32);
    assert_eq!(preferences.get_scheme_index(), dracula as i32);
    if fonts.is_some() {
        controller.render_now();
        assert_eq!(window.get_term_background(), slint::Color::from_rgb_u8(0x28, 0x2a, 0x36));
        assert!(window.get_screen_width() > 0.0);
    }

    // Zooming changes the font size, and the reset restores it.
    let before = window.get_screen_height();
    assert!(controller.key("+", Modifiers::CTRL));
    controller.render_now();
    if fonts.is_some() {
        assert!(window.get_screen_height() != before || before == 0.0);
    }
    assert!(controller.key("0", Modifiers::CTRL));

    window.hide().expect("the window hides");
    controller.shutdown();
}
