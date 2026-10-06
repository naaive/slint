// SPDX-License-Identifier: MIT

//! Drives the settings window on Slint's testing backend with sample data,
//! checking that UI actions edit and save the configuration.

use nimbus_config::{Action, ColorScheme, Config};
use nimbus_settings::page::Page;
use nimbus_settings::screenshot::sample_app;
use nimbus_settings::view::App;
use nimbus_settings::{Nav, Picker, Prefs, ShortcutsModel};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model, SharedString};

struct Fixture {
    app: App,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(page: Page) -> Self {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().expect("temporary directory");
        let app = sample_app(dir.path(), page, false).expect("the app starts");
        app.window().show().expect("the window shows");
        Self { app, dir }
    }

    fn saved(&self) -> Config {
        self.app.flush();
        Config::load_from(&self.dir.path().join("config.toml")).expect("the saved file parses")
    }

    fn press(&self, text: impl Into<SharedString> + Clone) {
        let window = self.app.window().window();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone().into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    }

    fn press_with_control(&self, text: impl Into<SharedString> + Clone) {
        let window = self.app.window().window();
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        self.press(text);
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
    }
}

#[test]
fn opens_the_requested_page_and_navigates_with_the_keyboard() {
    let f = Fixture::new(Page::Power);
    let nav = f.app.window().global::<Nav>();
    assert_eq!(nav.get_page(), Page::Power.index() as i32);
    f.press_with_control(Key::PageDown);
    assert_eq!(nav.get_page(), Page::Displays.index() as i32);
    f.press_with_control(Key::PageUp);
    f.press_with_control(Key::PageUp);
    assert_eq!(nav.get_page(), Page::Shortcuts.index() as i32);
}

#[test]
fn edits_are_saved() {
    let f = Fixture::new(Page::Appearance);
    let prefs = f.app.window().global::<Prefs>();
    prefs.invoke_set_int("appearance.color-scheme".into(), 0);
    prefs.invoke_set_text("appearance.accent".into(), "#E62D42".into());
    prefs.invoke_set_text("appearance.accent".into(), "not a color".into());
    prefs.invoke_set_float("appearance.scale".into(), 1.5);
    prefs.invoke_set_int("panel.height".into(), 400);
    prefs.invoke_set_bool("panel.show-dock".into(), false);
    prefs.invoke_choose_timeout("power.lock-after".into(), 0);
    let saved = f.saved();
    assert_eq!(saved.appearance.color_scheme, ColorScheme::Light);
    assert_eq!(saved.appearance.accent, "#e62d42");
    assert_eq!(saved.appearance.scale, 1.5);
    assert_eq!(saved.panel.height, 48);
    assert!(!saved.panel.show_dock);
    assert_eq!(saved.power.lock_after_minutes, 0);
    assert_eq!(prefs.get_scale_index(), 2);
    assert_eq!(prefs.get_panel_height(), 48.0, "clamped values are pushed back");
    assert!(!prefs.get_accent_is_custom());
    assert!(prefs.invoke_is_color("#abc".into()));
    assert!(!prefs.invoke_is_color("#abcd".into()));
}

#[test]
fn clock_presets_and_custom_formats() {
    let f = Fixture::new(Page::Panel);
    let prefs = f.app.window().global::<Prefs>();
    assert_eq!(prefs.get_clock_preview(), "Mon 05 Oct  09:41");
    prefs.invoke_choose_clock_preset(0);
    assert_eq!(prefs.get_clock_preview(), "09:41");
    let custom = prefs.get_clock_presets().row_count() as i32 - 1;
    prefs.invoke_choose_clock_preset(custom);
    assert_eq!(prefs.get_clock_preset_index(), custom, "the custom field stays open");
    prefs.set_clock_format("%Q".into());
    prefs.invoke_set_text("panel.clock-format".into(), "%Q".into());
    assert_eq!(f.saved().panel.clock_format, "%H:%M", "invalid formats aren't saved");
    assert_eq!(prefs.get_clock_format(), "%Q", "the typed text stays");
    assert!(!prefs.get_clock_valid());
    prefs.set_clock_format("%H.%M".into());
    prefs.invoke_set_text("panel.clock-format".into(), "%H.%M".into());
    assert_eq!(f.saved().panel.clock_format, "%H.%M");
    assert_eq!(prefs.get_clock_preview(), "09.41");
    assert!(prefs.get_clock_valid());
}

#[test]
fn settings_search_finds_pages() {
    let f = Fixture::new(Page::Appearance);
    let nav = f.app.window().global::<Nav>();
    nav.invoke_search_edited("tiling".into());
    let results = nav.get_search_results();
    let first = results.row_data(0).expect("a result");
    assert_eq!(first.page, Page::Workspaces.index() as i32);
    assert_eq!(first.page_title, "Workspaces & Windows");
}

#[test]
fn picker_chooses_fonts_and_layouts() {
    let f = Fixture::new(Page::Appearance);
    let prefs = f.app.window().global::<Prefs>();
    let picker = f.app.window().global::<Picker>();
    prefs.invoke_pick_font();
    assert!(picker.get_open());
    assert_eq!(picker.get_current(), "Inter");
    picker.invoke_query_edited("noto".into());
    let items = picker.get_items();
    assert_eq!(items.row_count(), 1);
    picker.invoke_chosen(items.row_data(0).expect("a font").id);
    assert!(!picker.get_open());
    assert_eq!(f.saved().appearance.font_family, "Noto Sans");

    prefs.invoke_add_source();
    picker.invoke_query_edited("french".into());
    let french = picker.get_items().row_data(0).expect("French is listed");
    assert_eq!(french.id, "fr");
    picker.invoke_chosen(french.id);
    assert_eq!(f.saved().input.keyboard_layout, "us,de,fr");
    assert_eq!(prefs.get_input_sources().row_count(), 3);

    prefs.invoke_move_source_up(2);
    prefs.invoke_remove_source(0);
    let saved = f.saved();
    assert_eq!(saved.input.keyboard_layout, "fr,de");
    assert_eq!(saved.input.keyboard_variant, ",nodeadkeys");

    prefs.invoke_choose_option(1, 1);
    assert_eq!(f.saved().input.keyboard_options, "compose:ralt,ctrl:nocaps");
}

#[test]
fn shortcuts_capture_detects_conflicts() {
    let f = Fixture::new(Page::Shortcuts);
    let model = f.app.window().global::<ShortcutsModel>();
    assert_eq!(model.get_conflict_count(), 0);
    assert!(model.get_categories().row_count() >= 4);

    model.invoke_edit("Super+Q".into());
    assert!(model.get_capture_open());
    assert_eq!(model.get_capture_action(), "Close window");
    model.invoke_key_captured(Key::Meta.into(), false, false, false, true);
    assert!(!model.get_capture_can_apply(), "a modifier alone isn't a shortcut");
    model.invoke_key_captured("l".into(), false, false, false, true);
    assert!(model.get_capture_conflict().contains("Lock the screen"));
    let keys: Vec<SharedString> = model.get_captured_keys().iter().collect();
    assert_eq!(keys, ["Super", "L"]);
    model.invoke_capture_apply();
    assert!(!model.get_capture_open());
    let saved = f.saved();
    assert_eq!(saved.keybindings.0.get("Super+L"), Some(&Action::CloseWindow));
    assert!(!saved.keybindings.0.contains_key("Super+Q"));

    model.invoke_add();
    assert!(model.get_capture_adding());
    model.invoke_capture_argument_edited("foot".into());
    model.invoke_key_captured("\n".into(), true, true, false, false);
    assert!(model.get_capture_can_apply());
    model.invoke_capture_apply();
    assert_eq!(f.saved().keybindings.0.get("Ctrl+Alt+Return"), Some(&Action::Spawn("foot".into())));

    model.set_filter("workspace 3".into());
    model.invoke_filter_edited("workspace 3".into());
    let categories = model.get_categories();
    assert_eq!(categories.row_count(), 1);
    assert_eq!(categories.row_data(0).expect("a category").items.row_count(), 2);

    model.invoke_remove("Ctrl+Alt+Return".into());
    model.invoke_reset();
    assert_eq!(f.saved().keybindings, nimbus_config::Keybindings::default());
}

#[test]
fn background_data_is_shown() {
    let f = Fixture::new(Page::About);
    let window = f.app.window();
    let prefs = window.global::<Prefs>();
    assert!(!prefs.get_wallpapers_loading());
    let wallpapers = prefs.get_wallpapers();
    assert_eq!(wallpapers.row_count(), 8);
    assert!(wallpapers.iter().all(|w| w.thumbnail.size().width > 0), "thumbnails arrived");
    prefs.invoke_set_text(
        "appearance.wallpaper".into(),
        wallpapers.row_data(2).expect("a wallpaper").path,
    );
    assert!(f.saved().appearance.wallpaper.is_some_and(|p| p.ends_with("fjord.jpg")));

    let about = window.global::<nimbus_settings::AboutModel>();
    assert_eq!(about.get_os_name(), "Nimbus OS 1.0");
    assert_eq!(about.get_hardware().row_count(), 5);
    let displays = window.global::<nimbus_settings::DisplaysModel>();
    assert_eq!(displays.get_state(), 1);
    assert_eq!(displays.get_items().row_count(), 2);
}

#[test]
fn displays_are_edited_applied_and_reverted() {
    let f = Fixture::new(Page::Displays);
    let displays = f.app.window().global::<nimbus_settings::DisplaysModel>();
    let item = |index: usize| displays.get_items().row_data(index).expect("a display");
    assert!(displays.get_editable());
    assert!(!displays.get_changed());
    assert_eq!(item(0).title, "Built-in Display");
    assert_eq!(item(1).title, "DELL U2720Q");
    assert_eq!(item(1).scale_index, 2);

    displays.invoke_choose_scale(1, 0);
    assert!(displays.get_changed());
    displays.invoke_reset();
    assert!(!displays.get_changed());
    assert_eq!(item(1).scale_index, 2);

    // At 100%, the monitor is wider than at 150%.
    let width_before = item(1).frac_width;
    displays.invoke_choose_scale(1, 0);
    displays.invoke_apply();
    assert!(!displays.get_changed());
    assert_eq!(displays.get_confirm_seconds(), 15);
    assert_eq!(item(1).scale_index, 0);
    assert!(item(1).frac_width > width_before);
    displays.invoke_revert();
    assert_eq!(displays.get_confirm_seconds(), 0);
    assert_eq!(item(1).scale_index, 2, "back at 150%");

    // Rotating the laptop and keeping it.
    displays.invoke_choose_rotation(0, 1);
    displays.invoke_apply();
    displays.invoke_keep();
    assert_eq!(displays.get_confirm_seconds(), 0);
    assert_eq!(item(0).rotation_index, 1);

    // Dropping the monitor above the laptop.
    displays.invoke_moved(1, 0.0, -0.9);
    assert!(displays.get_changed());
    assert!(item(1).frac_y < item(0).frac_y);

    // Turning the monitor off hides it from the arrangement.
    displays.invoke_set_enabled(1, false);
    assert!(!item(1).enabled);
    assert_eq!(item(1).frac_width, 0.0);
    assert_eq!(item(0).frac_width, 1.0);

    // The resolution list goes largest first, and changing it picks the fastest rate.
    displays.invoke_reset();
    assert_eq!(item(1).resolutions.row_data(1).as_deref(), Some("2560 × 1440"));
    displays.invoke_choose_resolution(1, 1);
    assert_eq!(item(1).refresh_rates.row_data(0).as_deref(), Some("59.95 Hz"));
    assert_eq!(f.saved().outputs, [], "the compositor saves displays, not Settings");
}
