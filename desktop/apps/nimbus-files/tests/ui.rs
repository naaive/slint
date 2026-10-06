// SPDX-License-Identifier: MIT

//! Drives the window through its callbacks on Slint's testing backend, against temporary folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use nimbus_files::app::{Controller, Env};
use nimbus_files::{AppWindow, DialogKind, ViewKind};
use nimbus_services::BusAddress;
use slint::platform::Key;
use slint::{ComponentHandle as _, Model as _};

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    ui: AppWindow,
    controller: Rc<Controller>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_live_updates(false)
    }

    fn with_live_updates(live_updates: bool) -> Self {
        Self::with_options(live_updates, BusAddress::Disabled)
    }

    fn with_options(live_updates: bool, system_bus: BusAddress) -> Self {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path().join("home");
        for folder in ["Documents", "Music", "Projects/rust/src"] {
            fs::create_dir_all(home.join(folder)).expect("mkdir");
        }
        fs::write(home.join("notes.txt"), "hello").expect("write");
        fs::write(home.join("report.pdf"), vec![0u8; 2000]).expect("write");
        fs::write(home.join(".hidden"), "").expect("write");
        fs::write(home.join("Projects/rust/src/report-draft.md"), "").expect("write");
        fs::write(dir.path().join("mountinfo"), "22 1 259:2 / / rw - ext4 /dev/sda1 rw\n")
            .expect("write");
        let env = Env {
            home: home.clone(),
            config_dir: home.join(".config"),
            data_dir: home.join(".local/share"),
            cache_dir: dir.path().join("cache"),
            prefs_path: None,
            mountinfo: dir.path().join("mountinfo"),
            live_updates,
            system_bus,
        };
        let ui = AppWindow::new().expect("window");
        let controller = Controller::new(ui.clone_strong(), env, Some(home.clone()));
        let fixture = Self { _dir: dir, home, ui, controller };
        fixture.wait_until("listing", |f| !f.ui.get_loading());
        fixture
    }

    /// Handles background results until `condition` holds.
    fn wait_until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.controller.pump();
            if condition(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn names(&self) -> Vec<String> {
        let files = self.ui.get_files();
        (0..files.row_count())
            .filter_map(|i| files.row_data(i))
            .map(|f| f.name.to_string())
            .collect()
    }

    fn selected(&self) -> Vec<String> {
        let files = self.ui.get_files();
        (0..files.row_count())
            .filter_map(|i| files.row_data(i))
            .filter(|f| f.selected)
            .map(|f| f.name.to_string())
            .collect()
    }

    fn index_of(&self, name: &str) -> i32 {
        self.names().iter().position(|n| n == name).unwrap_or_else(|| panic!("{name} is listed"))
            as i32
    }

    fn key(&self, key: Key) -> bool {
        self.ui.invoke_view_key(char::from(key).to_string().into(), false, false, false)
    }

    fn shortcut(&self, text: &str, ctrl: bool, shift: bool) -> bool {
        self.ui.invoke_window_key(text.into(), ctrl, shift, false)
            || self.ui.invoke_view_key(text.into(), ctrl, shift, false)
    }

    fn crumbs(&self) -> Vec<String> {
        let crumbs = self.ui.get_crumbs();
        (0..crumbs.row_count())
            .filter_map(|i| crumbs.row_data(i))
            .map(|c| c.label.to_string())
            .collect()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn select_only(&self, name: &str) {
        self.ui.invoke_item_pressed(self.index_of(name), false, false);
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[test]
fn lists_sorts_and_selects() {
    let f = Fixture::new();
    assert_eq!(f.names(), ["Documents", "Music", "Projects", "notes.txt", "report.pdf"]);
    assert_eq!(f.crumbs(), ["Home"]);
    assert_eq!(f.ui.get_status_text(), "5 items");

    f.ui.invoke_item_pressed(1, false, false);
    f.ui.invoke_item_pressed(3, false, true);
    assert_eq!(f.selected(), ["Music", "Projects", "notes.txt"]);
    f.ui.invoke_item_pressed(2, true, false);
    assert_eq!(f.selected(), ["Music", "notes.txt"]);
    assert_eq!(f.ui.get_status_text(), "2 items selected (5 bytes)");

    // Sorting by size puts folders first, then files by size.
    f.ui.invoke_set_sort_key(1);
    f.ui.invoke_set_sort_descending(true);
    assert_eq!(f.names()[3..], ["report.pdf", "notes.txt"]);
    assert_eq!(f.selected(), ["Music", "notes.txt"], "the selection follows the items");

    // Hidden files appear with Ctrl+H.
    assert!(f.shortcut("h", true, false));
    assert!(f.ui.get_show_hidden());
    assert!(f.names().contains(&".hidden".to_string()));

    // Rubber band in a list: rows 0 and 1.
    f.ui.invoke_set_list_mode(true);
    f.ui.invoke_background_pressed(false);
    f.ui.invoke_band(10.0, 5.0, 50.0, 40.0, 1, 600.0, 36.0, 0.0);
    assert_eq!(f.selected().len(), 2);
}

#[test]
fn keyboard_navigation() {
    let f = Fixture::new();
    f.ui.invoke_set_list_mode(true);
    assert!(f.key(Key::DownArrow));
    assert_eq!(f.selected(), ["Documents"]);
    assert!(f.ui.get_keyboard_cursor());
    assert!(f.key(Key::End));
    assert_eq!(f.selected(), ["report.pdf"]);
    assert!(f.ui.invoke_view_key(char::from(Key::UpArrow).to_string().into(), false, true, false));
    assert_eq!(f.selected(), ["notes.txt", "report.pdf"]);
    assert!(f.shortcut("a", true, false));
    assert_eq!(f.selected().len(), 5);

    // Type-ahead jumps to a matching name.
    assert!(f.key(Key::Escape));
    assert!(f.ui.invoke_view_key("p".into(), false, false, false));
    assert_eq!(f.selected(), ["Projects"]);

    // Enter opens the folder; Backspace goes back up and selects it.
    assert!(f.key(Key::Return));
    f.wait_until("Projects", |f| f.names() == ["rust"]);
    assert_eq!(f.crumbs(), ["Home", "Projects"]);
    assert!(f.ui.get_can_go_back());
    assert!(f.key(Key::Backspace));
    f.wait_until("home", |f| f.names().len() == 5);
    assert_eq!(f.selected(), ["Projects"]);
    assert!(f.ui.invoke_window_key(
        char::from(Key::LeftArrow).to_string().into(),
        false,
        false,
        true
    ));
    f.wait_until("back", |f| f.names() == ["rust"]);
    assert!(f.ui.invoke_window_key(
        char::from(Key::RightArrow).to_string().into(),
        false,
        false,
        true
    ));
    f.wait_until("forward", |f| f.names().len() == 5);

    // Ctrl+L edits the location; a typed path navigates.
    assert!(f.shortcut("l", true, false));
    assert!(f.ui.get_editing_location());
    f.ui.invoke_location_accepted("~/Projects/rust".into());
    f.wait_until("typed path", |f| f.names() == ["src"]);
    assert_eq!(f.crumbs(), ["Home", "Projects", "rust"]);
    f.ui.invoke_crumb_clicked(1);
    f.wait_until("crumb", |f| f.names() == ["rust"]);
    assert_eq!(f.selected(), ["rust"]);
}

#[test]
fn search_filters_and_recurses() {
    let f = Fixture::new();
    assert!(f.shortcut("f", true, false));
    assert!(f.ui.get_search_active());
    f.ui.set_search_text("rep".into());
    f.ui.invoke_search_edited("rep".into());
    assert_eq!(f.names(), ["report.pdf"]);

    f.ui.invoke_toggle_recursive();
    assert!(f.ui.get_search_recursive());
    f.wait_until("recursive results", |f| f.names().len() == 2 && !f.ui.get_searching());
    assert_eq!(f.names(), ["report-draft.md", "report.pdf"]);
    let files = f.ui.get_files();
    let draft = files.row_data(0).expect("row");
    assert_eq!(draft.location_label, "Projects/rust/src");

    f.ui.invoke_search_edited("nothing-matches".into());
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    slint::platform::update_timers_and_animations();
    f.wait_until("no results", |f| f.names().is_empty() && !f.ui.get_searching());
    assert_eq!(f.ui.get_empty_title(), "No Results");

    assert!(f.key(Key::Escape));
    assert!(!f.ui.get_search_active());
    assert_eq!(f.names().len(), 5);
}

#[test]
fn folder_rename_copy_trash_and_undo() {
    let f = Fixture::new();

    // New folder.
    assert!(f.shortcut("N", true, true));
    assert_eq!(f.ui.get_dialog(), DialogKind::Name);
    assert_eq!(f.ui.get_dialog_initial_text(), "New Folder");
    f.ui.invoke_name_edited("Music".into());
    assert!(f.ui.get_dialog_error().contains("already exists"));
    f.ui.invoke_name_edited("a/b".into());
    assert!(f.ui.get_dialog_error().contains("/"));
    f.ui.invoke_name_accepted("Work".into());
    assert_eq!(f.ui.get_dialog(), DialogKind::None);
    f.wait_until("new folder", |f| f.names().contains(&"Work".to_string()));
    assert!(f.path("Work").is_dir());
    assert_eq!(f.selected(), ["Work"]);

    // Rename with F2 preselects the stem.
    f.select_only("notes.txt");
    assert!(f.key(Key::F2));
    assert_eq!(f.ui.get_dialog(), DialogKind::Name);
    assert_eq!(f.ui.get_dialog_select_length(), 5);
    f.ui.invoke_name_accepted("todo.txt".into());
    f.wait_until("rename", |f| f.names().contains(&"todo.txt".to_string()));
    assert_eq!(f.selected(), ["todo.txt"]);

    // Copy and paste into the same folder makes a duplicate.
    assert!(f.shortcut("c", true, false));
    assert!(f.shortcut("v", true, false));
    f.wait_until("paste", |f| f.names().contains(&"todo (copy).txt".to_string()));
    assert_eq!(fs::read_to_string(f.path("todo (copy).txt")).expect("read"), "hello");

    // Cut and paste into a folder through the context menu's "Paste Into Folder".
    f.select_only("report.pdf");
    assert!(f.shortcut("x", true, false));
    let files = f.ui.get_files();
    assert!(files.row_data(f.index_of("report.pdf") as usize).is_some_and(|r| r.cut));
    f.ui.invoke_item_context(f.index_of("Work"), 10.0, 10.0);
    let rows = f.ui.get_menu_rows();
    let paste = (0..rows.row_count())
        .position(|i| rows.row_data(i).is_some_and(|r| r.label == "Paste Into Folder"))
        .expect("paste into folder");
    f.ui.invoke_menu_activated(paste as i32);
    f.wait_until("move", |f| !f.names().contains(&"report.pdf".to_string()));
    assert!(f.path("Work/report.pdf").exists());

    // Delete moves to the trash, with an undo.
    f.select_only("todo (copy).txt");
    assert!(f.key(Key::Delete));
    f.wait_until("trash", |f| !f.names().contains(&"todo (copy).txt".to_string()));
    assert!(f.ui.get_toast_visible());
    assert_eq!(f.ui.get_toast_action(), "Undo");
    assert!(f.ui.get_toast_text().contains("moved to the Trash"));
    f.ui.invoke_toast_action_clicked();
    f.wait_until("undo", |f| f.names().contains(&"todo (copy).txt".to_string()));
    assert!(f.path("todo (copy).txt").exists());

    // Shift+Delete asks first, then deletes permanently.
    f.select_only("todo (copy).txt");
    assert!(f.ui.invoke_view_key(char::from(Key::Delete).to_string().into(), false, true, false));
    assert_eq!(f.ui.get_dialog(), DialogKind::Confirm);
    f.ui.invoke_confirm_accepted();
    f.wait_until("delete", |f| !f.names().contains(&"todo (copy).txt".to_string()));
    assert!(!exists(&f.path("todo (copy).txt")));
}

#[test]
fn conflicts_ask_before_replacing() {
    let f = Fixture::new();
    fs::write(f.path("Documents/todo.txt"), "old").expect("write");
    fs::write(f.path("todo.txt"), "new").expect("write");
    assert!(f.shortcut("r", true, false));
    f.wait_until("reload", |f| f.names().contains(&"todo.txt".to_string()));
    f.select_only("todo.txt");
    assert!(f.shortcut("c", true, false));
    f.ui.invoke_item_context(f.index_of("Documents"), 0.0, 0.0);
    let rows = f.ui.get_menu_rows();
    let paste = (0..rows.row_count())
        .position(|i| rows.row_data(i).is_some_and(|r| r.label == "Paste Into Folder"))
        .expect("paste into folder");
    f.ui.invoke_menu_activated(paste as i32);
    f.wait_until("conflict", |f| f.ui.get_dialog() == DialogKind::Conflict);
    assert_eq!(f.ui.get_dialog_title(), "Replace “todo.txt”?");
    assert!(!f.ui.get_conflict_merge());
    assert!(f.ui.get_conflict_existing().contains("bytes"));
    f.ui.invoke_conflict_resolved(2, false);
    let copy = f.path("Documents/todo (2).txt");
    f.wait_until("keep both", |_| copy.exists());
    assert_eq!(fs::read_to_string(&copy).expect("read"), "new");
    assert_eq!(fs::read_to_string(f.path("Documents/todo.txt")).expect("read"), "old");
}

#[test]
fn trash_view_restores_and_empties() {
    let f = Fixture::new();
    f.select_only("notes.txt");
    assert!(f.key(Key::Delete));
    f.wait_until("trashed", |f| !f.names().contains(&"notes.txt".to_string()));
    f.select_only("report.pdf");
    assert!(f.key(Key::Delete));
    f.wait_until("trashed", |f| !f.names().contains(&"report.pdf".to_string()));

    let places = f.ui.get_places();
    let trash = (0..places.row_count())
        .position(|i| places.row_data(i).is_some_and(|p| p.label == "Trash"))
        .expect("Trash place");
    f.ui.invoke_place_clicked(trash as i32);
    f.wait_until("trash listing", |f| f.names().len() == 2);
    assert_eq!(f.ui.get_view_kind(), ViewKind::Trash);
    assert_eq!(f.ui.get_current_place(), trash as i32);
    let files = f.ui.get_files();
    assert_eq!(files.row_data(0).map(|r| r.location_label.to_string()).as_deref(), Some("~"));

    f.select_only("notes.txt");
    f.ui.invoke_restore_clicked();
    f.wait_until("restore", |f| f.names() == ["report.pdf"]);
    assert!(f.path("notes.txt").exists());

    f.ui.invoke_empty_trash_clicked();
    assert_eq!(f.ui.get_dialog(), DialogKind::Confirm);
    f.ui.invoke_confirm_accepted();
    f.wait_until("empty", |f| f.names().is_empty() && !f.ui.get_loading());
    assert_eq!(f.ui.get_empty_title(), "Trash Is Empty");
    assert!(f.ui.get_trash_empty());
}

#[test]
fn properties_and_menus() {
    let f = Fixture::new();
    f.select_only("Projects");
    assert!(f.ui.invoke_window_key(char::from(Key::Return).to_string().into(), false, false, true));
    assert_eq!(f.ui.get_dialog(), DialogKind::Properties);
    assert_eq!(f.ui.get_props_name(), "Projects");
    f.wait_until("folder size", |f| !f.ui.get_props_computing());
    let rows = f.ui.get_props_rows();
    let values: Vec<(String, String)> = (0..rows.row_count())
        .filter_map(|i| rows.row_data(i))
        .map(|r| (r.label.to_string(), r.value.to_string()))
        .collect();
    assert!(values.iter().any(|(k, v)| k == "Size" && v.contains("1 file")), "{values:?}");
    assert!(values.iter().any(|(k, _)| k == "Permissions"));
    f.ui.invoke_dialog_cancelled();
    assert_eq!(f.ui.get_dialog(), DialogKind::None);

    f.ui.invoke_background_context(5.0, 5.0);
    assert!(f.selected().is_empty());
    let rows = f.ui.get_menu_rows();
    assert_eq!(rows.row_data(0).map(|r| r.label.to_string()).as_deref(), Some("New Folder…"));
    let first = f.ui.invoke_menu_step(-1, 1);
    assert_eq!(first, 0);
    assert_eq!(
        f.ui.get_menu_item_count() + f.ui.get_menu_separator_count(),
        rows.row_count() as i32
    );
}

#[test]
fn live_updates_refresh_the_view() {
    let f = Fixture::with_live_updates(true);
    f.select_only("notes.txt");
    fs::write(f.path("added.txt"), "new").expect("write");
    fs::remove_file(f.path("report.pdf")).expect("remove");
    // Changes are coalesced by a short timer before the folder is listed again.
    f.wait_until("refresh", |f| {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(100));
        slint::platform::update_timers_and_animations();
        f.names().contains(&"added.txt".to_string())
            && !f.names().contains(&"report.pdf".to_string())
    });
    assert_eq!(f.selected(), ["notes.txt"], "the selection survives the refresh");
}

#[test]
fn keyboard_menu_bookmarks_and_window_menu() {
    let f = Fixture::new();
    f.select_only("Projects");
    assert!(f.ui.invoke_view_key(char::from(Key::F10).to_string().into(), false, true, false));
    let rows = f.ui.get_menu_rows();
    let labels: Vec<String> = (0..rows.row_count())
        .filter_map(|i| rows.row_data(i))
        .map(|r| r.label.to_string())
        .collect();
    assert_eq!(labels[0], "Open");
    let bookmark = labels.iter().position(|l| l == "Add to Bookmarks").expect("bookmark entry");
    f.ui.invoke_menu_activated(bookmark as i32);
    let has_bookmark = |f: &Fixture| {
        let places = f.ui.get_places();
        (0..places.row_count())
            .filter_map(|i| places.row_data(i))
            .any(|p| p.label == "Projects" && p.heading == "Bookmarks")
    };
    f.wait_until("bookmark", has_bookmark);
    let text = fs::read_to_string(f.path(".config/gtk-3.0/bookmarks")).expect("bookmarks file");
    assert!(text.trim_end().ends_with("/home/Projects"));

    // Ctrl+D on the selected folder's menu target toggles it back off.
    f.ui.invoke_item_context(f.index_of("Projects"), 0.0, 0.0);
    let rows = f.ui.get_menu_rows();
    let remove = (0..rows.row_count())
        .position(|i| rows.row_data(i).is_some_and(|r| r.label == "Remove from Bookmarks"))
        .expect("remove entry");
    f.ui.invoke_menu_activated(remove as i32);
    f.wait_until("bookmark removed", |f| !has_bookmark(f));

    f.ui.invoke_main_menu_requested(500.0, 40.0);
    let rows = f.ui.get_menu_rows();
    assert_eq!(rows.row_data(0).map(|r| r.label.to_string()).as_deref(), Some("New Window"));
    let hidden = (0..rows.row_count())
        .position(|i| rows.row_data(i).is_some_and(|r| r.checkable))
        .expect("hidden files toggle");
    f.ui.invoke_menu_activated(hidden as i32);
    assert!(f.ui.get_show_hidden());
}

#[test]
fn volumes_mount_when_opened_and_power_off_from_the_sidebar() {
    use nimbus_test_support::{FakeUdisks, MountAnswer, PrivateBus};
    let Some(bus) = PrivateBus::start() else { return };
    // The fake answers on the runtime's worker thread while the test waits.
    let runtime =
        tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let udisks = runtime.block_on(async {
        let udisks = FakeUdisks::start(&bus).await;
        let device = "/org/freedesktop/UDisks2/block_devices/sdb1";
        udisks.insert(device, "/dev/sdb1", "STICK", MountAnswer::Mount).await;
        udisks
    });
    let f = Fixture::with_options(false, BusAddress::Address(bus.address.clone()));
    let stick = |f: &Fixture| {
        let places = f.ui.get_places();
        (0..places.row_count()).find(|&i| places.row_data(i).is_some_and(|p| p.label == "STICK"))
    };
    f.wait_until("the stick in the sidebar", |f| stick(f).is_some());
    let index = stick(&f).unwrap();
    let place = f.ui.get_places().row_data(index).unwrap();
    assert_eq!(place.heading, "");
    assert_eq!(place.detail, "/dev/sdb1");
    assert!(place.can_eject, "a drive that powers off can be ejected");

    f.ui.invoke_place_clicked(index as i32);
    f.wait_until("the mounted stick", |f| f.ui.get_location_text() == "/run/media/ada/STICK");
    assert_eq!(udisks.calls(), ["mount STICK"]);
    f.wait_until("the stick's place to follow its mount", |f| {
        stick(f).is_some_and(|i| f.ui.get_current_place() == i as i32)
    });

    f.ui.invoke_place_context(stick(&f).unwrap() as i32, 10.0, 10.0);
    let rows = f.ui.get_menu_rows();
    let labels: Vec<String> = (0..rows.row_count())
        .filter_map(|i| rows.row_data(i))
        .map(|r| r.label.to_string())
        .collect();
    assert_eq!(labels, ["Open", "", "Unmount", "Safely Remove Drive"]);
    f.ui.invoke_menu_activated(3);
    // Leaving the stick's folder comes first.
    assert_eq!(f.ui.get_location_text(), f.home.to_string_lossy().as_ref());
    f.wait_until("the stick to power off", |_| udisks.calls().len() == 3);
    assert_eq!(udisks.calls(), ["mount STICK", "unmount STICK", "power off"]);
}

#[test]
fn a_dismissed_mount_doesnt_open_the_volume_later() {
    use nimbus_test_support::{FakeUdisks, MountAnswer, PrivateBus};
    let Some(bus) = PrivateBus::start() else { return };
    let runtime =
        tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let device = "/org/freedesktop/UDisks2/block_devices/sdb1";
    let udisks = runtime.block_on(async {
        let udisks = FakeUdisks::start(&bus).await;
        udisks.insert(device, "/dev/sdb1", "STICK", MountAnswer::Dismiss).await;
        udisks
    });
    let f = Fixture::with_options(false, BusAddress::Address(bus.address.clone()));
    let stick = |f: &Fixture| {
        let places = f.ui.get_places();
        (0..places.row_count()).find(|&i| places.row_data(i).is_some_and(|p| p.label == "STICK"))
    };
    f.wait_until("the stick in the sidebar", |f| stick(f).is_some());
    let location = f.ui.get_location_text();

    // Opening the stick asks for authorization, which the user dismisses.
    f.ui.invoke_place_clicked(stick(&f).unwrap() as i32);
    f.wait_until("the dismissed mount", |_| udisks.calls() == ["mount STICK"]);
    runtime.block_on(udisks.set_answer(device, MountAnswer::Mount));

    // Mounting it from its menu later leaves the folder alone.
    f.ui.invoke_place_context(stick(&f).unwrap() as i32, 10.0, 10.0);
    let rows = f.ui.get_menu_rows();
    let mount = (0..rows.row_count())
        .position(|i| rows.row_data(i).is_some_and(|r| r.label == "Mount"))
        .expect("a Mount entry");
    f.ui.invoke_menu_activated(mount as i32);
    f.wait_until("the stick to mount", |_| udisks.calls() == ["mount STICK", "mount STICK"]);
    let settled = Instant::now() + Duration::from_millis(300);
    f.wait_until("the events to settle", |_| Instant::now() > settled);
    assert_eq!(f.ui.get_location_text(), location);
}
