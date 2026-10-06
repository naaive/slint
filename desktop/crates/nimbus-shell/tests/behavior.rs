// SPDX-License-Identifier: MIT

//! Drives the shell on Slint's testing backend with mock time: feeding it compositor and service state,
//! opening its parts, clicking and typing, and checking which parts show, their input regions, keyboard focus, and actions.

mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery, mock_elapsed_time};
use nimbus_config::{ColorScheme, Config, PanelPosition};
use nimbus_ipc::{Event, Request, WindowInfo};
use nimbus_services::{CloseReason, ServiceCommand, ServiceEvent, Urgency};
use nimbus_shell::{
    Align, Desktop, LockView, Osd, Part, PartComponent, Popup, PowerAction, Rect, RectData,
    ShellAction, ShellModel, ShellOutput, ShellView,
};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, Model, SharedString};
use support::desk::Desk;

const SIZE: LogicalSize = LogicalSize::new(1280.0, 800.0);

struct Fixture {
    model: ShellModel,
    desk: Desk,
    /// The panel, which every output shows; its globals hold what the whole output shows.
    panel: PartComponent,
    actions: Rc<RefCell<Vec<ShellAction>>>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::with_config(support::config())
    }

    fn with_config(mut config: Config) -> Self {
        i_slint_backend_testing::init_no_event_loop();
        config.appearance.animations = false;
        let dir = tempfile::tempdir().expect("temporary directory");
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        let model = ShellModel::new(&config, move |action| sink.borrow_mut().push(action));
        model.set_config_path(dir.path().join("config.toml"));
        let (apps, icons) = support::apps(dir.path()).expect("mock apps are written");
        model.set_apps(std::rc::Rc::new(apps), icons);
        model.set_compositor_state(&support::compositor_state());
        model.handle_service_event(&ServiceEvent::State(support::system_state()));
        let desk =
            Desk::new(ShellView::new(&model, support::OUTPUT), SIZE.width, SIZE.height, None);
        let panel = desk.component(Part::Panel).expect("the panel shows");
        Self { model, desk, panel, actions, dir }
    }

    fn view(&self) -> &ShellView {
        &self.desk.view
    }

    /// The parts of another output.
    fn desk_on(&self, output: &str) -> Desk {
        Desk::new(ShellView::new(&self.model, output), SIZE.width, SIZE.height, None)
    }

    fn lock_view(&self) -> LockView {
        let lock = LockView::new(&self.model).expect("the lock screen starts");
        lock.window().set_size(SIZE);
        lock.show().expect("the window shows");
        lock
    }

    fn part(&self, part: Part) -> PartComponent {
        self.desk.component(part).unwrap_or_else(|| panic!("{part:?} isn't shown"))
    }

    fn shows(&self, part: Part) -> bool {
        self.desk.parts().contains(&part)
    }

    fn overlay(&self) -> nimbus_shell::OverlayWindow {
        match self.part(Part::Overlay) {
            PartComponent::Overlay(overlay) => overlay,
            _ => unreachable!(),
        }
    }

    fn desktop(&self) -> Desktop<'_> {
        self.panel.desktop()
    }

    fn output(&self) -> ShellOutput<'_> {
        self.panel.output()
    }

    fn take_actions(&self) -> Vec<ShellAction> {
        std::mem::take(&mut *self.actions.borrow_mut())
    }

    fn button(&self, label: &str) -> ElementHandle {
        self.desk
            .parts()
            .into_iter()
            .filter_map(|part| self.desk.component(part))
            .find_map(|component| {
                query(&component)
                    .match_descendants()
                    .match_accessible_label(label)
                    .match_predicate(|e| {
                        matches!(
                            e.accessible_role(),
                            Some(AccessibleRole::Button | AccessibleRole::Switch)
                        )
                    })
                    .find_first()
            })
            .unwrap_or_else(|| panic!("no button labeled {label:?}"))
    }

    fn click(&self, label: &str) {
        self.button(label).mock_single_click(PointerEventButton::Left);
    }

    /// Clicks a button of `part`, where another part may have a button with the same label.
    fn click_in(&self, part: Part, label: &str) {
        query(&self.part(part))
            .match_descendants()
            .match_accessible_label(label)
            .find_first()
            .unwrap_or_else(|| panic!("no button labeled {label:?} in {part:?}"))
            .mock_single_click(PointerEventButton::Left);
    }

    fn keyboard(&self) -> PartComponent {
        self.desk.keyboard_window().expect("a part takes the keyboard")
    }

    fn type_text(&self, text: &str) {
        type_text(self.keyboard().window(), text);
    }

    fn key(&self, text: &str) {
        key(self.keyboard().window(), text);
    }

    fn full_output(&self) -> Vec<Rect> {
        vec![Rect { x: 0.0, y: 0.0, width: SIZE.width, height: SIZE.height }]
    }
}

fn query(component: &PartComponent) -> ElementQuery {
    match component {
        PartComponent::Panel(ui) => ElementQuery::from_root(ui),
        PartComponent::Dock(ui) => ElementQuery::from_root(ui),
        PartComponent::Popup(ui) => ElementQuery::from_root(ui),
        PartComponent::Overlay(ui) => ElementQuery::from_root(ui),
        PartComponent::Toasts(ui) => ElementQuery::from_root(ui),
        PartComponent::Osd(ui) => ElementQuery::from_root(ui),
    }
}

fn key(window: &slint::Window, text: &str) {
    let text = SharedString::from(text);
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn type_text(window: &slint::Window, text: &str) {
    for c in text.chars() {
        key(window, &c.to_string());
    }
}

fn escape() -> &'static str {
    "\u{1b}"
}

fn anchor() -> RectData {
    RectData { x: 600.0, y: 3.0, width: 80.0, height: 26.0 }
}

#[test]
fn idle_shell_shows_only_the_panel_and_dock() {
    let f = Fixture::new();
    assert_eq!(f.desk.parts(), [Part::Panel, Part::Dock]);
    assert!(f.desk.keyboard_window().is_none());
    assert_eq!(
        f.desk.input_region(Part::Panel),
        [Rect { x: 0.0, y: 0.0, width: 1280.0, height: 32.0 }]
    );
    let dock = f.desk.input_region(Part::Dock)[0];
    assert!(dock.y + dock.height <= 800.0 && dock.y > 700.0, "{dock:?}");
    assert!((dock.x + dock.width / 2.0 - 640.0).abs() < 1.0, "the dock is centered: {dock:?}");
    for part in [Part::Panel, Part::Dock] {
        assert!(!f.desk.input_region(part).iter().any(|r| r.contains(640.0, 400.0)));
    }
    let placement = |part| f.desk.with(part, |s| s.window.placement().unwrap()).unwrap();
    assert_eq!(placement(Part::Panel).exclusive_zone, Some(32.0));
    // The dock and its margins on both sides.
    assert_eq!(
        placement(Part::Dock).exclusive_zone,
        Some(dock.height + 2.0 * (800.0 - dock.y - dock.height))
    );
    assert!(f.take_actions().is_empty());
}

#[test]
fn exclusive_zones_follow_the_panel_configuration() {
    let mut config = support::config();
    config.panel.position = PanelPosition::Bottom;
    config.panel.height = 40;
    let f = Fixture::with_config(config.clone());
    let placement = |part| f.desk.with(part, |s| s.window.placement().unwrap()).unwrap();
    let panel = placement(Part::Panel);
    assert!(panel.edges.bottom && !panel.edges.top);
    assert_eq!(panel.exclusive_zone, Some(40.0));
    let dock_zone = placement(Part::Dock).exclusive_zone.expect("the dock reserves space");
    assert!(dock_zone > 0.0);
    assert_eq!(
        f.desk.input_region(Part::Panel),
        [Rect { x: 0.0, y: 760.0, width: 1280.0, height: 40.0 }]
    );
    let dock = f.desk.input_region(Part::Dock)[0];
    assert!(dock.y + dock.height < 760.0, "the dock sits above the panel: {dock:?}");

    config.panel.dock_autohide = true;
    f.model.set_config(&config);
    assert_eq!(placement(Part::Dock).exclusive_zone, Some(0.0));
    // An autohidden dock leaves a thin strip at the edge that reveals it.
    let strip = f.desk.input_region(Part::Dock)[0];
    assert_eq!(strip.height, 2.0);
    assert_eq!(strip.y + strip.height, 760.0);

    config.panel.position = PanelPosition::Top;
    config.panel.show_dock = false;
    config.panel.height = 500;
    f.model.set_config(&config);
    assert_eq!(placement(Part::Panel).exclusive_zone, Some(64.0));
    assert_eq!(f.desk.parts(), [Part::Panel], "no dock");
}

#[test]
fn autohidden_dock_reveals_at_the_edge() {
    let mut config = support::config();
    config.panel.dock_autohide = true;
    let f = Fixture::with_config(config);
    let dock = || f.desk.input_region(Part::Dock)[0];
    assert_eq!(dock().height, 2.0);
    f.desk.move_pointer(640.0, 799.0);
    assert_eq!(dock().height, 68.0, "touching the edge reveals the dock");
    f.desk.move_pointer(640.0, 760.0);
    assert!(dock().height > 2.0, "the dock stays while the pointer is on it");
    f.click("Mail");
    assert_eq!(f.take_actions(), [ShellAction::Launch("org.nimbus.Mail".into())]);
    f.desk.move_pointer(640.0, 400.0);
    assert_eq!(dock().height, 2.0, "leaving hides it again");
}

#[test]
fn launcher_searches_as_you_type_and_launches() {
    let f = Fixture::new();
    f.view().toggle_launcher();
    assert!(f.shows(Part::Overlay));
    assert!(f.desk.with(Part::Overlay, |s| s.window.placement().unwrap().keyboard).unwrap());
    assert_eq!(f.desk.input_region(Part::Overlay), f.full_output());
    assert_eq!(f.output().get_launcher_apps().row_count(), 16);

    f.type_text("term");
    assert_eq!(f.overlay().get_launcher_query(), "term");
    let first = f.output().get_launcher_apps().row_data(0).expect("a result");
    assert_eq!(first.id, "org.nimbus.Terminal");
    f.key("\n");
    assert_eq!(f.take_actions(), [ShellAction::Launch("org.nimbus.Terminal".into())]);
    assert!(!f.view().launcher_open());
    assert!(!f.shows(Part::Overlay), "the overlay closes with the launcher");
    assert!(f.desk.keyboard_window().is_none());
}

#[test]
fn launcher_keyboard_navigation() {
    let f = Fixture::new();
    f.view().toggle_launcher();
    let overlay = f.overlay();
    assert_eq!(overlay.get_launcher_query(), "", "a reopened launcher starts empty");
    f.key("\u{f703}"); // Right
    f.key("\u{f703}");
    assert_eq!(overlay.get_launcher_selected(), 2);
    f.key("\u{f701}"); // Down: the next row of eight
    assert_eq!(overlay.get_launcher_selected(), 10);
    f.key("\u{f700}"); // Up
    f.key("\u{f702}"); // Left
    assert_eq!(overlay.get_launcher_selected(), 1);
    let expected = f.output().get_launcher_apps().row_data(1).expect("a second app").id.to_string();
    f.key("\n");
    assert_eq!(f.take_actions(), [ShellAction::Launch(expected)]);

    // Escape clears a query first, then closes.
    f.view().toggle_launcher();
    f.type_text("zzz");
    assert_eq!(f.output().get_launcher_apps().row_count(), 0);
    f.key("\n");
    assert!(f.take_actions().is_empty(), "nothing to launch");
    f.key(escape());
    assert_eq!(f.overlay().get_launcher_query(), "");
    assert!(f.view().launcher_open());
    f.key(escape());
    assert!(!f.view().launcher_open());
}

#[test]
fn overview_switches_activates_and_moves_windows() {
    let f = Fixture::new();
    f.view().toggle_overview();
    assert!(f.view().overview_open());
    assert_eq!(f.desk.input_region(Part::Overlay), f.full_output());
    let output = f.part(Part::Overlay);
    let output = output.output();
    assert_eq!(output.get_active_count(), 4);

    output.invoke_window_moved(4, 3);
    output.invoke_workspace_clicked(2);
    output.invoke_window_closed(1);
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Compositor(Request::MoveToWorkspace { id: 5, workspace: 3 }),
            ShellAction::Compositor(Request::SwitchWorkspace { workspace: 2 }),
            ShellAction::Compositor(Request::Close { id: 2 }),
        ]
    );
    assert!(f.view().overview_open(), "switching workspaces keeps the overview open");

    f.click("Workspace 2");
    assert_eq!(
        f.take_actions(),
        [ShellAction::Compositor(Request::SwitchWorkspace { workspace: 1 })]
    );

    output.invoke_window_activated(2);
    assert_eq!(f.take_actions(), [ShellAction::Compositor(Request::Activate { id: 3 })]);
    assert!(!f.view().overview_open());
    assert!(!f.shows(Part::Overlay));

    // Typing in the overview starts a search in the same overlay.
    f.view().toggle_overview();
    let overlay = f.overlay();
    f.type_text("ma");
    assert!(!f.view().overview_open());
    assert!(f.view().launcher_open());
    assert_eq!(f.overlay().get_launcher_query(), "ma");
    assert!(overlay.window().is_visible(), "the overlay stays");
    f.key(escape());
    f.key(escape());
    assert!(!f.view().launcher_open());

    f.view().toggle_overview();
    f.key(escape());
    assert!(!f.view().overview_open());
}

#[test]
fn dock_launches_focuses_and_minimizes() {
    let f = Fixture::new();
    let ids: Vec<String> = f.desktop().get_dock_items().iter().map(|d| d.id.to_string()).collect();
    assert_eq!(
        ids,
        [
            "firefox",
            "org.nimbus.Files",
            "org.nimbus.Terminal",
            "org.nimbus.Mail",
            "org.nimbus.Music",
            "org.nimbus.Photos",
            "org.nimbus.Settings",
            "org.nimbus.TextEditor",
            "org.inkscape.Inkscape",
        ]
    );
    let terminal = f.desktop().get_dock_items().row_data(2).expect("terminal");
    assert_eq!(terminal.windows, 2);
    assert!(f.desktop().get_dock_items().row_data(0).is_some_and(|d| d.focused));

    f.click("Mail");
    f.click("Web Browser");
    f.click("Terminal");
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Launch("org.nimbus.Mail".into()),
            ShellAction::Compositor(Request::SetMinimized { id: 1, minimized: true }),
            ShellAction::Compositor(Request::Activate { id: 2 }),
        ]
    );

    f.click("Show Applications");
    assert!(f.view().launcher_open());
}

#[test]
fn dock_menu_opens_above_its_item_and_pins() {
    let f = Fixture::new();
    let dock = f.part(Part::Dock);
    let item = RectData { x: 200.0, y: 46.0, width: 56.0, height: 56.0 };
    dock.output().invoke_dock_menu_requested(2, item.clone());
    assert_eq!(f.view().popup(), Popup::DockMenu);
    let placement = f.view().popup_placement().expect("the menu has a placement");
    assert_eq!(placement.parent, Part::Dock);
    assert_eq!(placement.anchor, Rect::from(item.clone()));
    assert!(!placement.below);
    assert_eq!(placement.align, Align::Center);
    let menu = f.part(Part::Popup(Popup::DockMenu));
    // The menu opens right above the item, and only the menu itself takes input.
    let item_top = f.desk.with(Part::Dock, |s| s.rect.y).unwrap() + item.y;
    let region = f.desk.input_region(Part::Popup(Popup::DockMenu));
    assert_eq!(region.len(), 1);
    assert!(
        (region[0].y + region[0].height - (item_top - placement.gap)).abs() < 0.5,
        "{region:?}"
    );

    let output = menu.output();
    assert_eq!(output.get_dock_menu_title(), "Terminal");
    let titles: Vec<String> =
        output.get_dock_menu_windows().iter().map(|w| w.title.to_string()).collect();
    assert_eq!(titles, ["~/src/nimbus — cargo test", "htop"]);
    assert!(output.get_dock_menu_favorite());
    output.invoke_dock_menu_window(1);
    assert_eq!(f.take_actions(), [ShellAction::Compositor(Request::Activate { id: 4 })]);
    assert_eq!(f.view().popup(), Popup::None);
    assert!(!f.shows(Part::Popup(Popup::DockMenu)));

    dock.output().invoke_dock_menu_requested(2, item.clone());
    f.part(Part::Popup(Popup::DockMenu)).output().invoke_dock_menu_quit();
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Compositor(Request::Close { id: 2 }),
            ShellAction::Compositor(Request::Close { id: 4 })
        ]
    );

    // Pinning the running Inkscape moves it among the favorites and saves the change.
    dock.output().invoke_dock_menu_requested(8, item);
    let output = f.part(Part::Popup(Popup::DockMenu));
    assert!(!output.output().get_dock_menu_favorite());
    output.output().invoke_dock_menu_toggle_pin();
    assert!(
        f.desktop()
            .get_dock_items()
            .row_data(7)
            .is_some_and(|d| d.favorite && d.id == "org.inkscape.Inkscape")
    );
    let path = f.dir.path().join("config.toml");
    assert!(wait_for(|| Config::load_from(&path).is_ok_and(|c| c
        .favorites
        .last()
        .map(String::as_str)
        == Some("org.inkscape.Inkscape"))));
}

#[test]
fn compositor_events_update_models_in_place() {
    let f = Fixture::new();
    let windows = f.output().get_windows();
    assert_eq!(windows.row_count(), 7);

    let mut changed = support::window(2, "org.nimbus.Terminal", "vim", 0, true);
    changed.fullscreen = true;
    f.model.handle_compositor_event(&Event::WindowChanged(changed));
    assert_eq!(windows.row_count(), 7);
    assert_eq!(windows.row_data(1).map(|w| w.title.to_string()), Some("vim".into()));
    assert!(windows.row_data(0).is_some_and(|w| !w.focused), "the old focus is cleared");
    assert_eq!(f.desktop().get_focused_title(), "vim");
    // A focused fullscreen window hides the panel and the dock, which keep their surfaces.
    assert!(f.desk.input_region(Part::Panel).is_empty());
    assert!(f.desk.input_region(Part::Dock).is_empty());
    assert_eq!(f.desk.parts(), [Part::Panel, Part::Dock]);
    assert!(!f.desktop().get_dock_items().row_data(0).is_some_and(|d| d.focused));

    f.model.handle_compositor_event(&Event::WindowClosed { id: 2 });
    assert_eq!(windows.row_count(), 6);
    assert_eq!(f.desktop().get_focused_title(), "");
    assert_eq!(f.desk.input_region(Part::Panel)[0].y, 0.0, "the panel is back");

    f.model.handle_compositor_event(&Event::WindowOpened(WindowInfo {
        id: 9,
        app_id: "org.nimbus.Mail".into(),
        title: "Inbox".into(),
        workspace: 3,
        output: support::OUTPUT.into(),
        ..Default::default()
    }));
    assert_eq!(windows.row_count(), 7);
    let mail = f
        .desktop()
        .get_dock_items()
        .iter()
        .find(|d| d.id == "org.nimbus.Mail")
        .expect("mail is pinned");
    assert_eq!(mail.windows, 1);
    assert!(f.output().get_workspaces().row_data(3).is_some_and(|w| w.occupied));

    f.model.handle_compositor_event(&Event::WorkspaceActivated { workspace: 3 });
    assert_eq!(f.desktop().get_active_workspace(), 3);
    assert_eq!(f.output().get_active_count(), 1);
    assert!(f.output().get_workspaces().row_data(3).is_some_and(|w| w.active));
    // Windows on other outputs stay out of this output's overview.
    f.model.handle_compositor_event(&Event::WindowOpened(WindowInfo {
        id: 10,
        workspace: 3,
        output: "HDMI-1".into(),
        ..Default::default()
    }));
    assert_eq!(f.output().get_active_count(), 1);
}

#[test]
fn toasts_show_in_their_own_part_and_expire_into_the_history() {
    let f = Fixture::new();
    let toasts = f.desktop().get_toasts();
    let mut notifications = support::notifications();
    let critical = {
        let mut n = notifications.remove(0);
        n.urgency = Urgency::Critical;
        n
    };
    assert!(!f.shows(Part::Toasts));
    for n in notifications.iter().chain([&critical]) {
        f.model.handle_service_event(&ServiceEvent::Notification(n.clone()));
    }
    assert_eq!(toasts.row_count(), 3);
    assert!(f.desktop().get_has_unread());
    let region = f.desk.input_region(Part::Toasts);
    assert_eq!(region.len(), 1, "toasts take input: {region:?}");
    assert!(region[0].contains(1100.0, 60.0), "{region:?}");
    let (width, height) = f.desk.with(Part::Toasts, |s| s.window.size()).unwrap();
    assert!(width < 500.0 && height < 600.0, "the toasts' surface fits them: {width}x{height}");

    mock_elapsed_time(Duration::from_millis(5100));
    assert!(toasts.iter().filter(|t| t.closing).count() == 2, "normal toasts fade out");
    mock_elapsed_time(Duration::from_millis(300));
    assert_eq!(toasts.row_count(), 1, "the critical toast stays");
    assert!(toasts.row_data(0).is_some_and(|t| t.critical));
    assert_eq!(f.desktop().get_notifications().row_count(), 3);

    // The panel's clock opens the history, which clears the unread dot, and the toasts step aside.
    f.click("Calendar and notifications");
    assert_eq!(f.view().popup(), Popup::Calendar);
    assert!(!f.desktop().get_has_unread());
    assert!(f.shows(Part::Popup(Popup::Calendar)));
    assert!(!f.shows(Part::Toasts));
    let placement = f.view().popup_placement().expect("the calendar has a placement");
    assert_eq!(placement.parent, Part::Panel);
    assert!(placement.below);
    let calendar = f.desk.input_region(Part::Popup(Popup::Calendar))[0];
    assert!(
        (calendar.x + calendar.width / 2.0 - 640.0).abs() < 2.0,
        "centered on the clock: {calendar:?}"
    );
    assert_eq!(calendar.y, 32.0 + placement.gap);
    f.key(escape());
    assert_eq!(f.view().popup(), Popup::None);
    assert!(!f.shows(Part::Popup(Popup::Calendar)));

    f.model.handle_service_event(&ServiceEvent::NotificationClosed {
        id: critical.id,
        reason: CloseReason::Closed,
    });
    mock_elapsed_time(Duration::from_millis(300));
    assert!(!f.shows(Part::Toasts), "the toasts' part goes with the last toast");
}

#[test]
fn notification_interactions() {
    let f = Fixture::new();
    for n in support::notifications() {
        f.model.handle_service_event(&ServiceEvent::Notification(n));
    }
    let actions: Vec<String> = f
        .desktop()
        .get_toasts()
        .row_data(0)
        .map(|t| t.actions.iter().map(|a| a.label.to_string()).collect())
        .unwrap_or_default();
    assert_eq!(actions, ["Install", "Later"]);
    assert_eq!(
        f.desktop().get_toasts().row_data(0).map(|t| t.body.to_string()),
        Some("3 updates are ready to install, including a security fix.".into())
    );

    let output = f.part(Part::Toasts);
    let output = output.output();
    output.invoke_notification_action(3, "install".into());
    output.invoke_notification_dismissed(2);
    output.invoke_notification_activated(1);
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Service(ServiceCommand::InvokeNotificationAction {
                id: 3,
                action: "install".into()
            }),
            ShellAction::Service(ServiceCommand::CloseNotification {
                id: 2,
                reason: CloseReason::Dismissed
            }),
        ]
    );
    // Without a default action, activating only hides the toast.
    assert_eq!(f.desktop().get_notifications().row_count(), 1);
    mock_elapsed_time(Duration::from_millis(300));
    assert_eq!(f.desktop().get_toasts().row_count(), 0);

    f.model.handle_service_event(&ServiceEvent::NotificationClosed {
        id: 1,
        reason: CloseReason::Closed,
    });
    assert_eq!(f.desktop().get_notifications().row_count(), 0);

    for n in support::notifications() {
        f.model.handle_service_event(&ServiceEvent::Notification(n));
    }
    let output = f.part(Part::Toasts);
    output.output().invoke_notification_activated(3);
    assert_eq!(
        f.take_actions(),
        [ShellAction::Service(ServiceCommand::InvokeNotificationAction {
            id: 3,
            action: "default".into()
        })]
    );
    output.output().invoke_clear_notifications();
    assert_eq!(f.desktop().get_notifications().row_count(), 0);
    assert_eq!(f.take_actions().len(), 2);
}

#[test]
fn do_not_disturb_keeps_notifications_quiet() {
    let f = Fixture::new();
    f.click("System menu");
    assert_eq!(f.view().popup(), Popup::QuickSettings);
    f.click("Do Not Disturb");
    assert_eq!(f.take_actions(), [ShellAction::Service(ServiceCommand::SetDoNotDisturb(true))]);
    assert!(f.desktop().get_do_not_disturb());
    f.key(escape());
    assert_eq!(f.view().popup(), Popup::None);

    for n in support::notifications() {
        f.model.handle_service_event(&ServiceEvent::Notification(n));
    }
    assert_eq!(f.desktop().get_toasts().row_count(), 0);
    assert_eq!(f.desktop().get_notifications().row_count(), 3);
    assert_eq!(f.desk.parts(), [Part::Panel, Part::Dock]);
}

#[test]
fn expired_transient_notifications_leave_no_history() {
    let f = Fixture::new();
    let mut transient = support::notification(40, "Volume", "", "Volume 40%", "", 0);
    transient.transient = true;
    let lasting = support::notification(41, "Mail", "", "New message", "", 0);
    f.model.handle_service_event(&ServiceEvent::Notification(transient));
    f.model.handle_service_event(&ServiceEvent::Notification(lasting));
    assert_eq!(f.desktop().get_notifications().row_count(), 2);
    for id in [40, 41] {
        f.model.handle_service_event(&ServiceEvent::NotificationClosed {
            id,
            reason: CloseReason::Expired,
        });
    }
    let summaries: Vec<_> =
        f.desktop().get_notifications().iter().map(|n| n.summary.to_string()).collect();
    assert_eq!(summaries, ["New message"]);
}

#[test]
fn views_share_toasts_and_history() {
    let f = Fixture::new();
    let other = f.desk_on("HDMI-1");
    // Critical, without a default action: the toast stays until clicked.
    let mut critical = support::notifications().remove(0);
    critical.urgency = Urgency::Critical;
    f.model.handle_service_event(&ServiceEvent::Notification(critical.clone()));
    for desk in [&f.desk, &other] {
        let toasts = desk.component(Part::Toasts).expect("each output shows the toast");
        assert_eq!(toasts.desktop().get_toasts().row_count(), 1);
    }
    let toasts = other.component(Part::Toasts).unwrap();
    toasts.output().invoke_notification_activated(critical.id as i32);
    mock_elapsed_time(Duration::from_millis(300));
    for desk in [&f.desk, &other] {
        let panel = desk.component(Part::Panel).unwrap();
        assert_eq!(
            panel.desktop().get_toasts().row_count(),
            0,
            "a clicked toast closes everywhere"
        );
        assert_eq!(panel.desktop().get_notifications().row_count(), 1);
        assert!(!desk.parts().contains(&Part::Toasts));
    }
    assert!(f.take_actions().is_empty());
}

#[test]
fn views_keep_their_own_parts() {
    let f = Fixture::new();
    let other = f.desk_on("HDMI-1");
    f.view().toggle_launcher();
    assert!(f.shows(Part::Overlay));
    assert!(!other.parts().contains(&Part::Overlay), "the launcher opens on one output");
    let panel = other.component(Part::Panel).unwrap();
    panel.output().invoke_popup_requested(Popup::Calendar, anchor());
    assert!(other.parts().contains(&Part::Popup(Popup::Calendar)));
    assert!(!f.desktop().get_has_unread(), "the unread dot is shared");
    assert_eq!(f.view().popup(), Popup::None);
    // Only windows on the view's own output count toward its workspace.
    assert_eq!(f.output().get_active_count(), 4);
    assert_eq!(panel.output().get_active_count(), 0);

    f.model.set_locked(true);
    assert!(!f.view().launcher_open() && other.view.popup() == Popup::None);
    assert_eq!(f.desk.parts(), [Part::Panel, Part::Dock]);
    assert_eq!(other.parts(), [Part::Panel, Part::Dock]);
}

#[test]
fn closing_the_overlay_closes_its_popup() {
    let f = Fixture::new();
    f.view().toggle_overview();
    // The overlay shows the panel above the overview, so its popups belong to the overlay.
    f.click_in(Part::Overlay, "System menu");
    let placement = f.view().popup_placement().expect("quick settings have a placement");
    assert_eq!(placement.parent, Part::Overlay);
    assert_eq!(placement.align, Align::End);
    assert_eq!(
        f.desk.parts(),
        [Part::Panel, Part::Dock, Part::Overlay, Part::Popup(Popup::QuickSettings)]
    );
    f.view().toggle_overview();
    assert_eq!(f.view().popup(), Popup::None);
    assert_eq!(f.desk.parts(), [Part::Panel, Part::Dock]);
}

#[test]
fn notification_images_load_in_the_background() {
    let f = Fixture::new();
    // The default allowed directories include `/tmp`.
    let dir = tempfile::tempdir_in("/tmp").expect("temporary directory");
    let photo = dir.path().join("photo.png");
    image::RgbImage::new(16, 16).save(&photo).expect("fixture saves");
    let oversized = dir.path().join("huge.png");
    std::fs::File::create(&oversized)
        .and_then(|file| file.set_len(64 * 1024 * 1024))
        .expect("fixture writes");
    let icon = |id: u32| {
        f.desktop()
            .get_notifications()
            .iter()
            .find(|n| n.id == id as i32)
            .is_some_and(|n| n.visual.has_icon)
    };

    let with_photo =
        support::notification(50, "Chat", &format!("file://{}", photo.display()), "Hi", "", 0);
    let with_huge =
        support::notification(51, "Chat", &oversized.display().to_string(), "Yo", "", 0);
    f.model.handle_service_event(&ServiceEvent::Notification(with_photo));
    f.model.handle_service_event(&ServiceEvent::Notification(with_huge));
    assert!(!icon(50) && !icon(51), "the UI thread doesn't read client files");
    assert!(
        wait_for(|| {
            mock_elapsed_time(Duration::from_millis(50));
            icon(50)
        }),
        "the image arrives"
    );
    assert!(!icon(51), "an oversized file shows the initial");
}

#[test]
fn quick_settings_controls() {
    let f = Fixture::new();
    f.click("System menu");
    assert!(f.shows(Part::Popup(Popup::QuickSettings)));
    let placement = f.view().popup_placement().expect("quick settings have a placement");
    assert_eq!((placement.parent, placement.align), (Part::Panel, Align::End));
    let region = f.desk.input_region(Part::Popup(Popup::QuickSettings))[0];
    let button = f.button("System menu");
    let button_right = button.absolute_position().x + button.size().width;
    assert!(
        (region.x + region.width - button_right).abs() < 0.5,
        "lined up at the right: {region:?}"
    );
    f.click("Wi-Fi");
    f.click("Bluetooth");
    f.click("Wi-Fi settings");
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Service(ServiceCommand::SetWifiEnabled(false)),
            ShellAction::Service(ServiceCommand::SetBluetoothPowered(false)),
            ShellAction::OpenSettings(Some("network".into())),
        ]
    );
    assert_eq!(f.view().popup(), Popup::None, "opening settings closes the menu");
    assert!(!f.desktop().get_status().wifi_enabled);

    f.click("System menu");
    let output = f.part(Part::Popup(Popup::QuickSettings));
    f.click("Pause");
    f.click("Next track");
    f.click("Mute");
    output.output().invoke_volume_changed(40.0);
    output.output().invoke_brightness_changed(0.0);
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Service(ServiceCommand::MediaPlayPause),
            ShellAction::Service(ServiceCommand::MediaNext),
            ShellAction::Service(ServiceCommand::ToggleMute),
            ShellAction::Service(ServiceCommand::SetVolume(0.4)),
            ShellAction::Service(ServiceCommand::SetBrightness(0.01)),
        ]
    );
    f.click("System Settings");
    assert_eq!(f.take_actions(), [ShellAction::OpenSettings(None)]);

    f.click("System menu");
    f.click("Lock Screen");
    assert_eq!(f.take_actions(), [ShellAction::Compositor(Request::Lock)]);
}

#[test]
fn dark_style_toggles_locally_and_saves() {
    let f = Fixture::new();
    let PartComponent::Panel(panel) = &f.panel else { unreachable!() };
    let theme = panel.global::<nimbus_shell::Theme>();
    assert!(theme.get_dark());
    panel.global::<ShellOutput>().invoke_dark_style_toggled();
    assert!(!theme.get_dark());
    let path = f.dir.path().join("config.toml");
    assert!(wait_for(
        || Config::load_from(&path).is_ok_and(|c| c.appearance.color_scheme == ColorScheme::Light)
    ));
    assert!(f.take_actions().is_empty());
}

#[test]
fn power_actions_ask_first() {
    let f = Fixture::new();
    f.click("System menu");
    f.click("Power Off or Log Out");
    let PartComponent::Popup(popup) = f.part(Part::Popup(Popup::QuickSettings)) else {
        unreachable!()
    };
    assert!(popup.get_power_menu_open());
    f.click("Power Off…");
    assert_eq!(f.view().popup(), Popup::None);
    assert_eq!(f.view().power_action(), PowerAction::PowerOff);
    assert!(f.shows(Part::Overlay));
    assert_eq!(f.desk.input_region(Part::Overlay), f.full_output());
    assert!(f.take_actions().is_empty(), "nothing happens before confirming");
    f.click("Power Off");
    assert_eq!(f.take_actions(), [ShellAction::Service(ServiceCommand::PowerOff)]);
    assert_eq!(f.view().power_action(), PowerAction::None);
    assert!(!f.shows(Part::Overlay));

    f.output().invoke_power_requested(PowerAction::LogOut);
    f.key(escape());
    assert_eq!(f.view().power_action(), PowerAction::None);
    f.output().invoke_power_requested(PowerAction::Restart);
    f.key("\n");
    f.output().invoke_suspend_requested();
    assert_eq!(
        f.take_actions(),
        [
            ShellAction::Service(ServiceCommand::Reboot),
            ShellAction::Service(ServiceCommand::Suspend)
        ]
    );
}

#[test]
fn lock_screen_authenticates_through_the_host() {
    let f = Fixture::new();
    let attempts = Rc::new(RefCell::new(Vec::new()));
    let sink = attempts.clone();
    f.model.on_unlock_attempt(move |password| sink.borrow_mut().push(password));

    f.view().toggle_launcher();
    f.model.handle_service_event(&ServiceEvent::LockRequested);
    assert!(f.model.is_locked());
    assert!(!f.view().launcher_open(), "locking closes everything");
    assert!(f.desk.keyboard_window().is_none(), "the lock screen takes the keys");
    f.view().toggle_launcher();
    f.view().toggle_overview();
    assert!(
        !f.view().launcher_open() && !f.view().overview_open(),
        "nothing opens over the lock screen"
    );

    let lock = f.lock_view();
    let other = f.lock_view();
    type_text(lock.window(), "hunter2");
    key(lock.window(), "\n");
    assert_eq!(*attempts.borrow(), ["hunter2"]);
    for screen in [&lock, &other] {
        assert!(screen.component().global::<Desktop>().get_lock_busy(), "every lock screen waits");
    }
    assert_eq!(lock.component().get_password(), "", "the field doesn't keep the password");
    key(lock.window(), "\n");
    type_text(other.window(), "x\n");
    assert_eq!(attempts.borrow().len(), 1, "no second attempt while one is pending");

    f.model.unlock_failed();
    assert!(!f.desktop().get_lock_busy());
    assert!(!lock.component().global::<Desktop>().get_lock_error().is_empty());
    assert!(f.model.is_locked());

    type_text(lock.window(), "correct horse");
    key(lock.window(), "\n");
    assert_eq!(attempts.borrow().last().map(String::as_str), Some("correct horse"));
    f.model.set_locked(false);
    assert!(!f.model.is_locked());
    assert!(f.desktop().get_lock_error().is_empty());
    assert!(f.take_actions().is_empty());
}

#[test]
fn lock_screen_without_a_handler_stays_locked() {
    let f = Fixture::new();
    f.model.set_locked(true);
    let lock = f.lock_view();
    type_text(lock.window(), "secret\n");
    assert!(f.model.is_locked());
    assert!(!f.desktop().get_lock_busy());
    assert!(!f.desktop().get_lock_error().is_empty());
    f.model.handle_service_event(&ServiceEvent::UnlockRequested);
    assert!(!f.model.is_locked());
}

#[test]
fn osd_shows_in_its_own_part_and_takes_no_input() {
    let f = Fixture::new();
    f.model.show_osd(Osd::Volume { level: 0.3, muted: false });
    assert!(f.desktop().get_osd_shown());
    assert!(f.shows(Part::Osd));
    assert!(f.desk.input_region(Part::Osd).is_empty());
    assert!(f.desk.keyboard_window().is_none());
    // The OSD keeps 48 pixels above the dock's reserved space.
    let pill_bottom = f.desk.with(Part::Osd, |s| s.rect.y + s.rect.height).unwrap() - 38.0;
    let dock_zone =
        f.desk.with(Part::Dock, |s| s.window.placement().unwrap().exclusive_zone).unwrap();
    assert_eq!(pill_bottom, 800.0 - dock_zone.unwrap() - 48.0);
    assert!((f.desktop().get_volume() - 30.0).abs() < 0.01, "the panel follows the key");
    mock_elapsed_time(Duration::from_millis(1000));
    f.model.show_osd(Osd::Brightness { level: f32::NAN });
    assert_eq!(f.desktop().get_osd_level(), 0.0);
    mock_elapsed_time(Duration::from_millis(1000));
    assert!(f.desktop().get_osd_shown(), "a new level restarts the timeout");
    mock_elapsed_time(Duration::from_millis(600));
    assert!(!f.desktop().get_osd_shown());
    assert!(!f.shows(Part::Osd), "the OSD's part goes once it's hidden");
}

#[test]
fn clock_and_calendar() {
    let mut config = support::config();
    config.panel.clock_format = "%H:%M".into();
    let f = Fixture::with_config(config.clone());
    let clock = f.desktop().get_clock_text();
    assert_eq!(clock.len(), 5, "{clock}");
    assert_eq!(clock.as_bytes()[2], b':');

    config.panel.clock_format = "%Q broken %".into();
    f.model.set_config(&config);
    assert!(!f.desktop().get_clock_text().contains("broken"), "an invalid format falls back");

    let title = f.desktop().get_month_title();
    f.output().invoke_month_changed(1);
    assert_ne!(f.desktop().get_month_title(), title);
    assert_eq!(f.desktop().get_days().row_count(), 42);
    f.output().invoke_month_changed(0);
    assert_eq!(f.desktop().get_month_title(), title);
    assert_eq!(f.desktop().get_days().iter().filter(|d| d.today).count(), 1);
}

fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}
