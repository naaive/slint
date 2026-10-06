// SPDX-License-Identifier: MIT

//! Drives the settings window on Slint's testing backend with sample data,
//! checking that UI actions edit and save the configuration.

use nimbus_config::{Action, ColorScheme, Config};
use nimbus_settings::page::Page;
use nimbus_settings::screenshot::sample_app;
use nimbus_settings::view::App;
use nimbus_settings::{
    BluetoothModel, DefaultAppsModel, Nav, NetworkModel, Picker, Prefs, ShortcutsModel, SoundModel,
    TimeModel,
};
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

/// The index of the row named `name` in a list model of items with a `name`.
fn index_of<T>(
    model: slint::ModelRc<T>,
    name: &str,
    row_name: impl Fn(&T) -> &SharedString,
) -> i32 {
    model
        .iter()
        .position(|row| row_name(&row) == name)
        .unwrap_or_else(|| panic!("no row named {name}")) as i32
}

#[test]
fn network_page_connects_forgets_and_asks_for_passwords() {
    let f = Fixture::new(Page::Network);
    let window = f.app.window();
    let network = window.global::<NetworkModel>();
    let nav = window.global::<Nav>();
    let row = |name: &str| index_of(network.get_networks(), name, |n| &n.name);
    let item = |name: &str| network.get_networks().row_data(row(name) as usize).expect("a network");
    assert_eq!(network.get_state(), 1);
    assert!(network.get_has_wifi() && network.get_wifi_enabled());
    assert_eq!(network.get_wifi_status(), "Connected to Nimbus HQ");
    assert_eq!(network.get_networks().row_count(), 6);
    assert!(item("Nimbus HQ").active && item("Nimbus HQ").secured);
    assert_eq!(item("Nimbus HQ").status, "Connected · Secured");
    assert_eq!(item("Nimbus HQ").bars, 3);
    assert_eq!(network.get_connection_name(), "Nimbus HQ");
    let details: Vec<SharedString> =
        network.get_connection_details().iter().map(|r| r.label).collect();
    assert!(details.iter().any(|label| label == "IPv4 address"), "{details:?}");
    let wired = network.get_wired().row_data(0).expect("a wired device");
    assert_eq!(
        (wired.title.as_str(), wired.status.as_str()),
        ("Ethernet (enp0s31f6)", "Cable unplugged")
    );

    // A secured network asks for a password first.
    network.invoke_activate(row("Neighbor 5G"));
    assert!(network.get_password_open());
    assert_eq!(network.get_password_network(), "Neighbor 5G");
    assert_eq!(network.get_password_error(), "");
    assert!(!network.invoke_password_acceptable("short".into()), "WPA needs 8 characters");
    assert!(network.invoke_password_acceptable("long enough".into()));
    network.set_password("long enough".into());
    network.invoke_password_submit();
    assert!(!network.get_password_open());
    assert_eq!(network.get_password(), "", "the password doesn't stay in the UI");
    assert!(item("Neighbor 5G").active && item("Neighbor 5G").known);
    assert_eq!(network.get_connection_name(), "Neighbor 5G");

    // A saved network whose password is missing asks for it when NetworkManager fails.
    network.invoke_activate(row("Library"));
    assert!(network.get_password_open());
    assert_eq!(network.get_password_network(), "Library");
    network.invoke_password_cancel();
    assert!(!network.get_password_open());
    assert!(!item("Library").active);

    network.invoke_forget(row("Library"));
    assert!(!item("Library").known);
    network.invoke_disconnect();
    assert_eq!(network.get_connection_details().row_count(), 0);
    assert_eq!(network.get_wifi_status(), "Not connected");

    network.invoke_activate(row("Nimbus Guest"));
    assert!(!network.get_password_open(), "open networks connect right away");
    assert!(item("Nimbus Guest").active);

    network.invoke_activate(row("eduroam"));
    assert!(nav.get_banner().contains("enterprise networks"), "{}", nav.get_banner());

    network.invoke_set_wifi_enabled(false);
    assert!(!network.get_wifi_enabled());
    assert_eq!(network.get_wifi_status(), "Off");
}

#[test]
fn bluetooth_page_discovers_pairs_and_manages_devices() {
    let f = Fixture::new(Page::Bluetooth);
    let window = f.app.window();
    let bluetooth = window.global::<BluetoothModel>();
    let nav = window.global::<Nav>();
    let row = |name: &str| index_of(bluetooth.get_devices(), name, |d| &d.name);
    let device =
        |name: &str| bluetooth.get_devices().row_data(row(name) as usize).expect("a device");
    assert_eq!(bluetooth.get_state(), 1);
    assert!(bluetooth.get_powered());
    assert!(bluetooth.get_discovering(), "the open page looks for devices");
    assert_eq!(bluetooth.get_devices().row_count(), 4);
    let headphones = device("WH-1000XM5");
    assert_eq!((headphones.kind, headphones.status.as_str()), (1, "Connected · Battery 70%"));

    bluetooth.invoke_pair(row("Pixel 8"));
    assert!(bluetooth.get_pairing_open());
    assert_eq!(bluetooth.get_pairing_kind(), 0);
    assert_eq!(bluetooth.get_pairing_title(), "Pair with Pixel 8?");
    assert_eq!(bluetooth.get_pairing_code(), "482913");
    assert!(device("Pixel 8").busy);
    bluetooth.invoke_pairing_accept();
    assert!(!bluetooth.get_pairing_open());
    let phone = device("Pixel 8");
    assert!(phone.paired && phone.connected && !phone.busy);

    bluetooth.invoke_pair(row("MX Master 3S"));
    assert!(bluetooth.get_pairing_open());
    bluetooth.invoke_pairing_cancel();
    assert!(!bluetooth.get_pairing_open());
    assert!(!device("MX Master 3S").paired);
    assert!(nav.get_banner().contains("MX Master 3S"), "{}", nav.get_banner());

    bluetooth.invoke_disconnect(row("WH-1000XM5"));
    assert!(!device("WH-1000XM5").connected);
    bluetooth.invoke_connect(row("MX Keys"));
    assert!(device("MX Keys").connected);
    bluetooth.invoke_remove(row("MX Keys"));
    assert_eq!(bluetooth.get_devices().row_count(), 3);

    bluetooth.invoke_set_discoverable(true);
    assert!(bluetooth.get_discoverable());
    assert_eq!(bluetooth.get_status(), "Visible as “workstation”");

    nav.set_page(Page::Appearance.index() as i32);
    slint::platform::update_timers_and_animations();
    assert!(!bluetooth.get_discovering(), "leaving the page stops looking");

    bluetooth.invoke_set_powered(false);
    assert!(!bluetooth.get_powered());
    nav.set_page(Page::Bluetooth.index() as i32);
    slint::platform::update_timers_and_animations();
    assert!(!bluetooth.get_discovering(), "a powered-off adapter doesn't look");
}

#[test]
fn sound_page_chooses_devices_and_changes_volume() {
    let f = Fixture::new(Page::Sound);
    let sound = f.app.window().global::<SoundModel>();
    let output = |row: usize| sound.get_outputs().row_data(row).expect("an output");
    assert_eq!(sound.get_state(), 1);
    assert_eq!(sound.get_outputs().row_count(), 3);
    assert_eq!(sound.get_inputs().row_count(), 2);
    let names: Vec<SharedString> = sound.get_output_names().iter().collect();
    assert_eq!(names, ["Speakers", "WH-1000XM5", "DELL U2720Q (HDMI)"]);
    assert_eq!(sound.get_output_index(), 0);
    let speakers = output(0);
    assert_eq!((speakers.volume, speakers.muted, speakers.level), (62.0, false, 2));

    sound.invoke_choose_default(false, 1);
    assert_eq!(sound.get_output_index(), 1);

    // The slider writes its row before it reports the change, as a drag does.
    let mut headphones = output(1);
    headphones.volume = 85.0;
    sound.get_outputs().set_row_data(1, headphones);
    sound.invoke_set_volume(false, 1, 85.0);
    assert_eq!((output(1).volume, output(1).level), (85.0, 3));

    sound.invoke_set_muted(false, 1, true);
    assert!(output(1).muted);
    assert_eq!(output(1).level, 0, "a muted device shows the muted icon");
    assert_eq!(output(0).volume, 62.0, "other devices keep their volume");

    sound.invoke_choose_default(true, 1);
    assert_eq!(sound.get_input_index(), 1);
    sound.invoke_set_muted(true, 0, true);
    assert!(sound.get_inputs().row_data(0).expect("an input").muted);
}

#[test]
fn date_time_page_picks_time_zones_and_switches_the_clock() {
    let f = Fixture::new(Page::DateTime);
    let window = f.app.window();
    let time = window.global::<TimeModel>();
    let picker = window.global::<Picker>();
    let prefs = window.global::<Prefs>();
    assert_eq!(time.get_state(), 1);
    assert_eq!(
        (time.get_timezone().as_str(), time.get_timezone_city().as_str()),
        ("Europe/Berlin", "Berlin")
    );
    assert!(time.get_can_pick_timezone() && time.get_can_sync() && time.get_sync());
    assert_eq!(time.get_sync_status(), "Synchronized with network time servers");
    assert_eq!(time.get_now(), "Monday, October 5, 09:41");

    time.invoke_set_sync(false);
    assert!(!time.get_sync());
    assert_eq!(time.get_sync_status(), "Set by hand");

    time.invoke_pick_timezone();
    assert!(picker.get_open());
    assert_eq!(picker.get_title(), "Time Zone");
    let current =
        picker.get_items().row_data(picker.get_current_index() as usize).expect("the current zone");
    assert_eq!(current.id, "Europe/Berlin");
    picker.invoke_query_edited("buenos".into());
    let found = picker.get_items().row_data(0).expect("a match");
    assert_eq!(
        (found.title.as_str(), found.id.as_str()),
        ("Buenos Aires", "America/Argentina/Buenos_Aires")
    );
    picker.invoke_chosen(found.id);
    assert!(!picker.get_open());
    assert_eq!(time.get_timezone(), "America/Argentina/Buenos_Aires");
    assert_eq!(time.get_timezone_city(), "Buenos Aires");

    // The 24-hour switch changes the panel's clock format.
    assert!(prefs.get_clock_24_hour());
    prefs.invoke_set_clock_24_hour(false);
    assert_eq!(f.saved().panel.clock_format, "%a %d %b  %I:%M %p");
    assert!(!prefs.get_clock_24_hour());
    assert_eq!(prefs.get_clock_preset_index(), 4);
    assert_eq!(time.get_now(), "Monday, October 5, 9:41 AM");
    prefs.invoke_set_clock_24_hour(true);
    assert_eq!(f.saved().panel.clock_format, "%a %d %b  %H:%M");
}

#[test]
fn default_apps_page_chooses_applications() {
    let f = Fixture::new(Page::DefaultApps);
    let model = f.app.window().global::<DefaultAppsModel>();
    let item = |row: usize| model.get_items().row_data(row).expect("a category");
    assert!(!model.get_loading());
    assert_eq!(model.get_items().row_count(), 8);
    let web = item(0);
    assert_eq!(web.title, "Web browser");
    let choices: Vec<SharedString> = web.choices.iter().collect();
    assert_eq!(choices, ["Chromium", "Firefox"]);
    assert_eq!(web.current, 1);

    model.invoke_chosen(0, 0);
    assert_eq!(item(0).current, 0);
    model.invoke_chosen(3, 0);
    assert_eq!((item(3).title.as_str(), item(3).current), ("Terminal", 0));

    let music = item(7);
    assert!(!music.available);
    assert_eq!(music.choices.row_data(0).as_deref(), Some("None installed"));
}
