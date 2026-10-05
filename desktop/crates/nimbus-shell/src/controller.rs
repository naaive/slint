// SPDX-License-Identifier: MIT

//! The shell's state behind [`crate::Shell`], and the handlers for everything the UI reports.
//!
//! State lives in one `RefCell`. Handlers finish with it before emitting a [`ShellAction`],
//! because the host may call back into the shell from its action handler.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use chrono::{Datelike, Local};
use nimbus_config::{ColorScheme, Config, PanelPosition};
use nimbus_ipc::{CompositorState, Event, OutputInfo, Request, WindowInfo};
use nimbus_services::{CloseReason, Notification, ServiceCommand, ServiceEvent, SystemState};
use nimbus_xdg::{AppIndex, IconResolver};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::backdrop::Backdrop;
use crate::clock::{self, FALLBACK_CLOCK_FORMAT};
use crate::icons::IconCache;
use crate::notifications::{self, DEFAULT_ACTION, Notifications};
use crate::persist::ConfigWriter;
use crate::windows::{DockClick, DockEntry, Windows, dock_click, dock_entries, readable_app_id};
use crate::{
    AppItem, CalendarDay, DockItem, DockMenuWindow, NotificationAction, NotificationItem, Osd,
    OsdKind, PanelSettings, Popup, PowerAction, ShellAction, ShellWindow, Theme, WindowItem,
    WorkspaceItem,
};

/// How long the OSD stays after the last change.
pub const OSD_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long a closing toast fades before it's removed; a little longer than `Theme.duration-normal`.
const TOAST_FADE: Duration = Duration::from_millis(250);
/// The panel heights the layout supports.
const PANEL_HEIGHT: std::ops::RangeInclusive<u32> = 24..=64;
/// The most workspace dots the panel shows.
const MAX_WORKSPACES: u32 = 32;
const UNLOCK_FAILED: &str = "Incorrect password, please try again";
const UNLOCK_UNAVAILABLE: &str = "Unlocking is unavailable";

/// Updates `model` to `rows`, touching only the rows that differ.
fn sync_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let common = model.row_count().min(rows.len());
    for (row, data) in rows.iter().enumerate().take(common) {
        if model.row_data(row).as_ref() != Some(data) {
            model.set_row_data(row, data.clone());
        }
    }
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
    for data in rows.into_iter().skip(common) {
        model.push(data);
    }
}

fn index(row: i32) -> Option<usize> {
    usize::try_from(row).ok()
}

struct Models {
    workspaces: Rc<VecModel<WorkspaceItem>>,
    windows: Rc<VecModel<WindowItem>>,
    dock: Rc<VecModel<DockItem>>,
    dock_menu: Rc<VecModel<DockMenuWindow>>,
    launcher: Rc<VecModel<AppItem>>,
    notifications: Rc<VecModel<NotificationItem>>,
    toasts: Rc<VecModel<NotificationItem>>,
    days: Rc<VecModel<CalendarDay>>,
}

impl Models {
    fn new() -> Self {
        Self {
            workspaces: Rc::new(VecModel::default()),
            windows: Rc::new(VecModel::default()),
            dock: Rc::new(VecModel::default()),
            dock_menu: Rc::new(VecModel::default()),
            launcher: Rc::new(VecModel::default()),
            notifications: Rc::new(VecModel::default()),
            toasts: Rc::new(VecModel::default()),
            days: Rc::new(VecModel::default()),
        }
    }

    fn attach(&self, ui: &ShellWindow) {
        ui.set_workspaces(ModelRc::from(self.workspaces.clone()));
        ui.set_windows(ModelRc::from(self.windows.clone()));
        ui.set_dock_items(ModelRc::from(self.dock.clone()));
        ui.set_dock_menu_windows(ModelRc::from(self.dock_menu.clone()));
        ui.set_launcher_apps(ModelRc::from(self.launcher.clone()));
        ui.set_notifications(ModelRc::from(self.notifications.clone()));
        ui.set_toasts(ModelRc::from(self.toasts.clone()));
        ui.set_days(ModelRc::from(self.days.clone()));
    }
}

struct State {
    config: Config,
    output: String,
    outputs: Vec<OutputInfo>,
    windows: Windows,
    workspace_count: u32,
    active_workspace: u32,
    apps: AppIndex,
    icons: IconCache,
    dock: Vec<DockEntry>,
    /// The dock entry whose menu is open.
    dock_menu: Option<String>,
    launcher_ids: Vec<String>,
    notifications: Notifications,
    action_models: HashMap<u32, ModelRc<NotificationAction>>,
    system: SystemState,
    /// The month the calendar shows, as `(year, month)`.
    month: (i32, u32),
    writer: ConfigWriter,
    backdrop: Backdrop,
}

pub struct Controller {
    window: slint::Weak<ShellWindow>,
    on_action: Box<dyn Fn(ShellAction)>,
    on_unlock: RefCell<Option<Rc<dyn Fn(String)>>>,
    state: RefCell<State>,
    models: Models,
    clock_timer: Timer,
    osd_timer: Timer,
    toast_timers: RefCell<HashMap<u32, Timer>>,
}

impl Controller {
    pub fn new(ui: &ShellWindow, config: &Config, on_action: Box<dyn Fn(ShellAction)>) -> Rc<Self> {
        let today = Local::now().date_naive();
        let controller = Rc::new(Self {
            window: ui.as_weak(),
            on_action,
            on_unlock: RefCell::new(None),
            state: RefCell::new(State {
                config: config.clone(),
                output: String::new(),
                outputs: Vec::new(),
                windows: Windows::default(),
                workspace_count: 0,
                active_workspace: 0,
                apps: AppIndex::default(),
                icons: IconCache::default(),
                dock: Vec::new(),
                dock_menu: None,
                launcher_ids: Vec::new(),
                notifications: Notifications::default(),
                action_models: HashMap::new(),
                system: SystemState::default(),
                month: (today.year(), today.month()),
                writer: ConfigWriter::new(),
                backdrop: Backdrop::default(),
            }),
            models: Models::new(),
            clock_timer: Timer::default(),
            osd_timer: Timer::default(),
            toast_timers: RefCell::new(HashMap::new()),
        });
        controller.models.attach(ui);
        ui.set_user_name(user_display_name().into());
        Self::connect(&controller, ui);
        controller.set_config(ui, config);
        Self::schedule_clock(&controller);
        controller
    }

    fn emit(&self, action: ShellAction) {
        tracing::debug!(?action, "shell action");
        (self.on_action)(action);
    }

    fn ui(&self) -> Option<ShellWindow> {
        self.window.upgrade()
    }

    // Configuration and clock.

    pub fn set_config(&self, ui: &ShellWindow, config: &Config) {
        nimbus_theme::apply_theme!(
            ui,
            nimbus_theme::ThemeSettings::from_config(&config.appearance)
        );
        ui.set_panel(PanelSettings {
            top: config.panel.position == PanelPosition::Top,
            height: config.panel.height.clamp(*PANEL_HEIGHT.start(), *PANEL_HEIGHT.end()) as f32,
            show_dock: config.panel.show_dock,
            dock_autohide: config.panel.dock_autohide,
            show_battery_percentage: config.panel.show_battery_percentage,
        });
        {
            let mut state = self.state.borrow_mut();
            state.config = config.clone();
            state.backdrop.request(config.appearance.wallpaper.as_deref());
            let scale = output_scale(&state);
            state.icons.set_scale(scale);
        }
        self.refresh_clock(ui);
        self.refresh_windows(ui);
    }

    pub fn set_config_path(&self, path: Option<PathBuf>) {
        self.state.borrow_mut().writer.set_path(path);
    }

    pub fn set_output_name(&self, ui: &ShellWindow, name: &str) {
        {
            let mut state = self.state.borrow_mut();
            state.output = name.to_owned();
            let scale = output_scale(&state);
            state.icons.set_scale(scale);
        }
        self.refresh_windows(ui);
    }

    fn schedule_clock(this: &Rc<Self>) {
        let weak = Rc::downgrade(this);
        this.clock_timer.start(
            TimerMode::SingleShot,
            clock::until_next_minute(&Local::now()),
            move || {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                if let Some(ui) = this.ui() {
                    this.refresh_clock(&ui);
                }
                Self::schedule_clock(&this);
            },
        );
    }

    /// Updates everything that shows the time: the clock, the lock screen, the calendar, and notification ages.
    pub fn refresh_clock(&self, ui: &ShellWindow) {
        let now = Local::now();
        let format = self.state.borrow().config.panel.clock_format.clone();
        ui.set_clock_text(clock::format_time(&now, &format, FALLBACK_CLOCK_FORMAT).into());
        let twelve_hour = ["%I", "%l", "%p", "%r"].iter().any(|s| format.contains(s));
        let long_time = if twelve_hour { "%-I:%M" } else { "%H:%M" };
        ui.set_long_time(clock::format_time(&now, long_time, "%H:%M").into());
        ui.set_weekday(clock::format_time(&now, "%A", "%A").into());
        ui.set_date(clock::format_time(&now, "%B %-d %Y", "%B %-d %Y").into());
        self.refresh_calendar(ui);
        self.refresh_notifications();
        let backdrop = self.state.borrow_mut().backdrop.take_update();
        if let Some(image) = backdrop {
            ui.set_lock_backdrop(image.unwrap_or_default());
        }
    }

    fn refresh_calendar(&self, ui: &ShellWindow) {
        let today = Local::now().date_naive();
        let (year, month) = self.state.borrow().month;
        let days = clock::month_grid(year, month, today)
            .into_iter()
            .map(|d| CalendarDay {
                day: d.date.day() as i32,
                in_month: d.in_month,
                today: d.today,
                weekend: d.weekend,
            })
            .collect();
        sync_rows(&self.models.days, days);
        ui.set_month_title(clock::month_title(year, month).into());
    }

    fn change_month(&self, ui: &ShellWindow, delta: i32) {
        {
            let mut state = self.state.borrow_mut();
            state.month = if delta == 0 {
                let today = Local::now().date_naive();
                (today.year(), today.month())
            } else {
                clock::add_months(state.month.0, state.month.1, delta)
            };
        }
        self.refresh_calendar(ui);
    }

    // Windows, workspaces, and the dock.

    pub fn set_compositor_state(&self, ui: &ShellWindow, compositor: &CompositorState) {
        {
            let mut state = self.state.borrow_mut();
            state.windows.replace(&compositor.windows);
            state.workspace_count = compositor.workspace_count;
            state.active_workspace = compositor.active_workspace;
            state.outputs = compositor.outputs.clone();
            let scale = output_scale(&state);
            state.icons.set_scale(scale);
        }
        self.refresh_windows(ui);
    }

    pub fn handle_compositor_event(&self, ui: &ShellWindow, event: &Event) {
        {
            let mut state = self.state.borrow_mut();
            match event {
                Event::WindowOpened(info) | Event::WindowChanged(info) => {
                    state.windows.upsert(info);
                }
                Event::WindowClosed { id } => {
                    if let Some(row) = state.windows.remove(*id)
                        && row < self.models.windows.row_count()
                    {
                        self.models.windows.remove(row);
                    }
                }
                Event::WorkspaceActivated { workspace } => state.active_workspace = *workspace,
                Event::OutputsChanged { outputs } => {
                    state.outputs = outputs.clone();
                    let scale = output_scale(&state);
                    state.icons.set_scale(scale);
                }
                Event::LayoutChanged { .. } => return,
            }
        }
        self.refresh_windows(ui);
    }

    /// Brings every window-derived model and property up to date with the state.
    fn refresh_windows(&self, ui: &ShellWindow) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let slots = state.windows.slots(&state.output);
        let mut rows = Vec::with_capacity(slots.len());
        for (window, (slot, count, on_output)) in state.windows.list().iter().zip(slots) {
            let (name, icon) = app_name_and_icon(&state.apps, window);
            rows.push(WindowItem {
                title: display_title(window, &name).into(),
                app_name: name.as_str().into(),
                visual: state.icons.visual(&name, icon.as_deref()),
                workspace: window.workspace as i32,
                focused: window.focused,
                minimized: window.minimized,
                slot: slot as i32,
                slot_count: count as i32,
                on_output,
            });
        }
        let active = state.active_workspace as i32;
        let active_count = rows.iter().filter(|w| w.on_output && w.workspace == active).count();
        let focused = state.windows.focused().cloned();
        sync_rows(&self.models.windows, rows);

        let count = if state.workspace_count > 0 {
            state.workspace_count
        } else {
            state.config.workspaces.count
        };
        let count = count
            .clamp(1, MAX_WORKSPACES)
            .max(state.active_workspace.saturating_add(1).min(MAX_WORKSPACES));
        let output = state.output.clone();
        let workspaces = (0..count)
            .map(|index| WorkspaceItem {
                index: index as i32,
                active: index == state.active_workspace,
                occupied: state.windows.list().iter().any(|w| {
                    w.workspace == index
                        && (output.is_empty() || w.output.is_empty() || w.output == output)
                }),
            })
            .collect();
        sync_rows(&self.models.workspaces, workspaces);

        state.dock = dock_entries(&state.config.favorites, &state.windows, &state.apps);
        let dock = state
            .dock
            .iter()
            .map(|entry| DockItem {
                id: entry.id.as_str().into(),
                name: entry.name.as_str().into(),
                visual: state.icons.visual(&entry.name, entry.icon.as_deref()),
                favorite: entry.favorite,
                windows: entry.windows.len() as i32,
                focused: entry.focused,
            })
            .collect();
        sync_rows(&self.models.dock, dock);
        Self::fill_dock_menu(state, &self.models.dock_menu, ui);

        ui.set_active_workspace(active);
        ui.set_active_count(active_count as i32);
        match focused {
            Some(window) => {
                let (name, icon) = app_name_and_icon(&state.apps, &window);
                ui.set_focused_title(display_title(&window, &name).into());
                ui.set_focused_visual(state.icons.visual(&name, icon.as_deref()));
                let here = output.is_empty() || window.output.is_empty() || window.output == output;
                ui.set_fullscreen_active(
                    window.fullscreen && here && window.workspace == state.active_workspace,
                );
            }
            None => {
                ui.set_focused_title(SharedString::new());
                ui.set_fullscreen_active(false);
            }
        }
    }

    fn fill_dock_menu(state: &State, model: &VecModel<DockMenuWindow>, ui: &ShellWindow) {
        let Some(id) = &state.dock_menu else {
            return;
        };
        let Some((index, entry)) = state.dock.iter().enumerate().find(|(_, e)| &e.id == id) else {
            sync_rows(model, Vec::new());
            ui.set_dock_menu_running(false);
            return;
        };
        let windows = entry
            .windows
            .iter()
            .filter_map(|id| state.windows.row_of(*id).and_then(|row| state.windows.get(row)))
            .map(|w| DockMenuWindow {
                title: display_title(w, &entry.name).into(),
                focused: w.focused,
            })
            .collect();
        sync_rows(model, windows);
        ui.set_dock_menu_index(index as i32);
        ui.set_dock_menu_title(entry.name.as_str().into());
        ui.set_dock_menu_favorite(entry.favorite);
        ui.set_dock_menu_running(!entry.windows.is_empty());
    }

    pub fn set_apps(&self, ui: &ShellWindow, apps: &AppIndex, icons: &IconResolver) {
        {
            let mut state = self.state.borrow_mut();
            state.apps = apps.clone();
            state.icons.set_resolver(icons.clone());
            let scale = output_scale(&state);
            state.icons.set_scale(scale);
        }
        self.refresh_windows(ui);
        if ui.get_launcher_open() {
            self.search(ui, &ui.get_launcher_query());
        }
    }

    fn dock_clicked(&self, ui: &ShellWindow, row: usize) {
        let click = {
            let state = self.state.borrow();
            state.dock.get(row).and_then(|entry| dock_click(entry, &state.windows))
        };
        let Some(click) = click else {
            return;
        };
        self.set_overview(ui, false);
        self.emit(match click {
            DockClick::Launch(id) => ShellAction::Launch(id),
            DockClick::Compositor(request) => ShellAction::Compositor(request),
        });
    }

    fn open_dock_menu(&self, ui: &ShellWindow, row: usize, center_x: f32) {
        {
            let mut state = self.state.borrow_mut();
            let Some(id) = state.dock.get(row).map(|e| e.id.clone()) else {
                return;
            };
            state.dock_menu = Some(id);
            Self::fill_dock_menu(&state, &self.models.dock_menu, ui);
        }
        ui.set_dock_menu_x(center_x);
        self.set_popup(ui, Popup::DockMenu);
    }

    fn dock_menu_entry(&self) -> Option<DockEntry> {
        let state = self.state.borrow();
        let id = state.dock_menu.as_ref()?;
        state.dock.iter().find(|e| &e.id == id).cloned()
    }

    fn dock_menu_window(&self, ui: &ShellWindow, row: usize) {
        let id = self.dock_menu_entry().and_then(|entry| entry.windows.get(row).copied());
        self.set_popup(ui, Popup::None);
        self.set_overview(ui, false);
        if let Some(id) = id {
            self.emit(ShellAction::Compositor(Request::Activate { id }));
        }
    }

    fn dock_menu_new_window(&self, ui: &ShellWindow) {
        let entry = self.dock_menu_entry();
        self.set_popup(ui, Popup::None);
        if let Some(entry) = entry.filter(|e| e.launchable) {
            self.set_overview(ui, false);
            self.emit(ShellAction::Launch(entry.id));
        }
    }

    fn dock_menu_toggle_pin(&self, ui: &ShellWindow) {
        let Some(entry) = self.dock_menu_entry() else {
            self.set_popup(ui, Popup::None);
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            let pin = !entry.favorite;
            let id = entry.id.clone();
            let edit = move |favorites: &mut Vec<String>| {
                favorites.retain(|f| *f != id);
                if pin {
                    favorites.push(id.clone());
                }
            };
            edit(&mut state.config.favorites);
            state.writer.edit(move |config| edit(&mut config.favorites));
        }
        self.set_popup(ui, Popup::None);
        self.refresh_windows(ui);
    }

    fn dock_menu_quit(&self, ui: &ShellWindow) {
        let entry = self.dock_menu_entry();
        self.set_popup(ui, Popup::None);
        for id in entry.map(|e| e.windows).unwrap_or_default() {
            self.emit(ShellAction::Compositor(Request::Close { id }));
        }
    }

    fn window_id(&self, row: i32) -> Option<nimbus_ipc::WindowId> {
        let state = self.state.borrow();
        state.windows.get(index(row)?).map(|w| w.id)
    }

    // Launcher, overview, and popups.

    pub fn set_launcher(&self, ui: &ShellWindow, open: bool, query: &str) {
        if open {
            self.set_overview(ui, false);
            self.set_popup(ui, Popup::None);
            ui.set_launcher_query(query.into());
            self.search(ui, query);
            ui.set_launcher_open(true);
            ui.invoke_focus_launcher_at(query.len().try_into().unwrap_or(i32::MAX));
        } else {
            ui.set_launcher_open(false);
        }
    }

    fn search(&self, ui: &ShellWindow, query: &str) {
        let apps = {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let results: Vec<_> = state.apps.search(query).into_iter().cloned().collect();
            state.launcher_ids = results.iter().map(|e| e.id.clone()).collect();
            results
                .iter()
                .map(|entry| AppItem {
                    id: entry.id.as_str().into(),
                    name: entry.name.as_str().into(),
                    description: entry
                        .comment
                        .as_deref()
                        .or(entry.generic_name.as_deref())
                        .unwrap_or_default()
                        .into(),
                    visual: state.icons.visual(&entry.name, entry.icon.as_deref()),
                })
                .collect::<Vec<_>>()
        };
        self.models.launcher.set_vec(apps);
        ui.set_launcher_selected(0);
    }

    fn launch_result(&self, ui: &ShellWindow, row: i32) {
        let id = index(row).and_then(|row| self.state.borrow().launcher_ids.get(row).cloned());
        if let Some(id) = id {
            self.set_launcher(ui, false, "");
            self.emit(ShellAction::Launch(id));
        }
    }

    pub fn set_overview(&self, ui: &ShellWindow, open: bool) {
        if open == ui.get_overview_open() {
            return;
        }
        if open {
            ui.set_launcher_open(false);
            self.set_popup(ui, Popup::None);
            ui.set_overview_open(true);
            ui.invoke_focus_overview();
        } else {
            ui.set_overview_open(false);
        }
    }

    pub fn set_popup(&self, ui: &ShellWindow, popup: Popup) {
        if popup != Popup::DockMenu {
            self.state.borrow_mut().dock_menu = None;
        }
        ui.set_power_menu_open(false);
        ui.set_popup(popup);
        match popup {
            Popup::None => {}
            Popup::Calendar => {
                ui.set_has_unread(false);
                self.change_month(ui, 0);
                ui.invoke_focus_popup();
            }
            Popup::QuickSettings | Popup::DockMenu => ui.invoke_focus_popup(),
        }
    }

    fn close_everything(&self, ui: &ShellWindow) {
        self.set_popup(ui, Popup::None);
        ui.set_launcher_open(false);
        ui.set_overview_open(false);
        ui.set_power_action(PowerAction::None);
    }

    // System state and quick settings.

    pub fn handle_service_event(self: &Rc<Self>, ui: &ShellWindow, event: &ServiceEvent) {
        match event {
            ServiceEvent::State(system) => {
                self.state.borrow_mut().system = system.clone();
                self.refresh_status(ui);
            }
            ServiceEvent::Notification(notification) => self.add_notification(ui, notification),
            ServiceEvent::NotificationClosed { id, reason } => {
                let transient = self
                    .state
                    .borrow()
                    .notifications
                    .history()
                    .iter()
                    .any(|n| n.id == *id && n.transient);
                if *reason == CloseReason::Expired && !transient {
                    self.close_toast(*id);
                } else {
                    self.remove_notification(*id);
                }
            }
            ServiceEvent::LockRequested => self.set_locked(ui, true),
            ServiceEvent::UnlockRequested => self.set_locked(ui, false),
            ServiceEvent::LogoutRequested => {}
        }
    }

    fn refresh_status(&self, ui: &ShellWindow) {
        let state = self.state.borrow();
        let status = crate::status::status(&state.system);
        ui.set_status(status.status);
        ui.set_volume(status.volume_percent);
        ui.set_brightness(status.brightness_percent);
        ui.set_do_not_disturb(state.system.do_not_disturb);
    }

    fn update_system(
        &self,
        ui: &ShellWindow,
        change: impl FnOnce(&mut SystemState) -> Option<ServiceCommand>,
    ) {
        let command = change(&mut self.state.borrow_mut().system);
        self.refresh_status(ui);
        if let Some(command) = command {
            self.emit(ShellAction::Service(command));
        }
    }

    fn toggle_dark_style(&self, ui: &ShellWindow) {
        let theme = ui.global::<Theme>();
        let dark = !theme.get_dark();
        theme.set_dark(dark);
        let scheme = if dark { ColorScheme::Dark } else { ColorScheme::Light };
        let mut state = self.state.borrow_mut();
        state.config.appearance.color_scheme = scheme;
        state.writer.edit(move |config| config.appearance.color_scheme = scheme);
    }

    pub fn show_osd(this: &Rc<Self>, ui: &ShellWindow, osd: Osd) {
        let sanitize = |level: f32| if level.is_finite() { level.clamp(0.0, 1.5) } else { 0.0 };
        match osd {
            Osd::Volume { level, muted } => {
                ui.set_osd_kind(OsdKind::Volume);
                ui.set_osd_level(sanitize(level));
                ui.set_osd_muted(muted);
                let mut state = this.state.borrow_mut();
                if let Some(audio) = &mut state.system.audio {
                    audio.volume = sanitize(level);
                    audio.muted = muted;
                }
            }
            Osd::Brightness { level } => {
                ui.set_osd_kind(OsdKind::Brightness);
                ui.set_osd_level(sanitize(level).min(1.0));
                ui.set_osd_muted(false);
                let mut state = this.state.borrow_mut();
                if state.system.brightness.is_some() {
                    state.system.brightness = Some(sanitize(level).min(1.0));
                }
            }
        }
        this.refresh_status(ui);
        ui.set_osd_shown(true);
        let weak = Rc::downgrade(this);
        this.osd_timer.start(TimerMode::SingleShot, OSD_TIMEOUT, move || {
            if let Some(ui) = weak.upgrade().and_then(|this| this.ui()) {
                ui.set_osd_shown(false);
            }
        });
    }

    fn power_confirmed(&self, ui: &ShellWindow, action: PowerAction) {
        ui.set_power_action(PowerAction::None);
        let command = match action {
            PowerAction::Restart => ServiceCommand::Reboot,
            PowerAction::PowerOff => ServiceCommand::PowerOff,
            PowerAction::LogOut => ServiceCommand::Logout,
            PowerAction::None => return,
        };
        self.emit(ShellAction::Service(command));
    }

    // Notifications.

    fn add_notification(self: &Rc<Self>, ui: &ShellWindow, notification: &Notification) {
        let id = notification.id;
        let evicted = {
            let mut state = self.state.borrow_mut();
            let toast = !state.system.do_not_disturb && !ui.get_locked();
            state.action_models.remove(&id);
            state.notifications.add(notification.clone(), toast)
        };
        if ui.get_popup() != Popup::Calendar {
            ui.set_has_unread(true);
        }
        let mut timers = self.toast_timers.borrow_mut();
        for evicted in evicted {
            timers.remove(&evicted);
        }
        timers.remove(&id);
        let showing = self.state.borrow().notifications.toasts().iter().any(|t| t.id == id);
        if let Some(timeout) = notifications::toast_timeout(notification).filter(|_| showing) {
            let timer = Timer::default();
            let controller = Rc::downgrade(self);
            timer.start(TimerMode::SingleShot, timeout, move || {
                if let Some(this) = controller.upgrade() {
                    this.close_toast(id);
                }
            });
            timers.insert(id, timer);
        }
        drop(timers);
        self.refresh_notifications();
    }

    /// Fades the toast out, keeping the notification in the history.
    fn close_toast(self: &Rc<Self>, id: u32) {
        if !self.state.borrow_mut().notifications.close_toast(id) {
            return;
        }
        let timer = Timer::default();
        let controller = Rc::downgrade(self);
        timer.start(TimerMode::SingleShot, TOAST_FADE, move || {
            if let Some(this) = controller.upgrade() {
                this.state.borrow_mut().notifications.drop_toast(id);
                this.toast_timers.borrow_mut().remove(&id);
                this.refresh_notifications();
            }
        });
        self.toast_timers.borrow_mut().insert(id, timer);
        self.refresh_notifications();
    }

    fn remove_notification(&self, id: u32) -> bool {
        let removed = {
            let mut state = self.state.borrow_mut();
            state.action_models.remove(&id);
            state.notifications.remove(id)
        };
        self.toast_timers.borrow_mut().remove(&id);
        self.refresh_notifications();
        removed
    }

    fn notification_activated(self: &Rc<Self>, ui: &ShellWindow, id: u32) {
        let has_default = self
            .state
            .borrow()
            .notifications
            .get(id)
            .is_some_and(|n| n.actions.iter().any(|(key, _)| key == DEFAULT_ACTION));
        if has_default {
            self.remove_notification(id);
            self.set_popup(ui, Popup::None);
            self.emit(ShellAction::Service(ServiceCommand::InvokeNotificationAction {
                id,
                action: DEFAULT_ACTION.into(),
            }));
        } else {
            self.close_toast(id);
        }
    }

    fn notification_action(&self, id: u32, key: String) {
        if self.remove_notification(id) {
            self.emit(ShellAction::Service(ServiceCommand::InvokeNotificationAction {
                id,
                action: key,
            }));
        }
    }

    fn notification_dismissed(&self, id: u32) {
        if self.remove_notification(id) {
            self.emit(ShellAction::Service(ServiceCommand::CloseNotification {
                id,
                reason: CloseReason::Dismissed,
            }));
        }
    }

    fn clear_notifications(&self) {
        let ids = {
            let mut state = self.state.borrow_mut();
            state.action_models.clear();
            state.notifications.clear()
        };
        self.toast_timers.borrow_mut().clear();
        self.refresh_notifications();
        for id in ids {
            self.emit(ShellAction::Service(ServiceCommand::CloseNotification {
                id,
                reason: CloseReason::Dismissed,
            }));
        }
    }

    fn refresh_notifications(&self) {
        let now = SystemTime::now();
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let item = |n: &Notification,
                    closing: bool,
                    icons: &mut IconCache,
                    actions: &mut HashMap<u32, ModelRc<NotificationAction>>| {
            let actions = actions
                .entry(n.id)
                .or_insert_with(|| {
                    let buttons: Vec<_> = notifications::button_actions(n)
                        .map(|(key, label)| NotificationAction {
                            key: key.as_str().into(),
                            label: label.as_str().into(),
                        })
                        .collect();
                    ModelRc::new(VecModel::from(buttons))
                })
                .clone();
            let icon = n.app_icon.strip_prefix("file://").unwrap_or(&n.app_icon);
            let name =
                if n.app_name.trim().is_empty() { "Notification" } else { n.app_name.as_str() };
            NotificationItem {
                id: n.id as i32,
                app_name: name.into(),
                summary: notifications::plain_text(&n.summary).into(),
                body: notifications::plain_text(&n.body).into(),
                visual: icons.visual(name, Some(icon)),
                time: clock::relative_time(n.received, now).into(),
                critical: n.urgency == nimbus_services::Urgency::Critical,
                actions,
                closing,
            }
        };
        let history: Vec<_> = state
            .notifications
            .history()
            .iter()
            .map(|n| item(n, false, &mut state.icons, &mut state.action_models))
            .collect();
        let toasts: Vec<_> = state
            .notifications
            .toasts()
            .iter()
            .filter_map(|t| {
                let n = state.notifications.get(t.id)?;
                Some(item(n, t.closing, &mut state.icons, &mut state.action_models))
            })
            .collect();
        sync_rows(&self.models.notifications, history);
        sync_rows(&self.models.toasts, toasts);
    }

    // Lock screen.

    pub fn set_locked(&self, ui: &ShellWindow, locked: bool) {
        if locked {
            self.close_everything(ui);
            ui.set_lock_password(SharedString::new());
            ui.set_lock_error(SharedString::new());
            ui.set_lock_busy(false);
            self.refresh_clock(ui);
            ui.set_locked(true);
            ui.invoke_focus_lock();
        } else {
            ui.set_locked(false);
            ui.set_lock_password(SharedString::new());
            ui.set_lock_error(SharedString::new());
            ui.set_lock_busy(false);
        }
    }

    pub fn set_unlock_handler(&self, handler: Rc<dyn Fn(String)>) {
        *self.on_unlock.borrow_mut() = Some(handler);
    }

    fn unlock_requested(&self, ui: &ShellWindow, password: String) {
        if !ui.get_locked() || ui.get_lock_busy() {
            return;
        }
        ui.set_lock_password(SharedString::new());
        let handler = self.on_unlock.borrow().clone();
        match handler {
            Some(handler) => {
                ui.set_lock_error(SharedString::new());
                ui.set_lock_busy(true);
                handler(password);
            }
            None => {
                tracing::warn!("No unlock handler is registered; the session stays locked");
                ui.set_lock_error(UNLOCK_UNAVAILABLE.into());
            }
        }
    }

    pub fn unlock_failed(&self, ui: &ShellWindow) {
        ui.set_lock_busy(false);
        ui.set_lock_password(SharedString::new());
        if ui.get_locked() {
            ui.set_lock_error(UNLOCK_FAILED.into());
            ui.invoke_focus_lock();
        }
    }

    // Wiring.

    fn connect(this: &Rc<Self>, ui: &ShellWindow) {
        macro_rules! on {
            ($setter:ident, |$c:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(this);
                ui.$setter(move |$($arg),*| {
                    if let Some($c) = weak.upgrade() {
                        if let Some($ui) = $c.ui() {
                            return $body;
                        }
                    }
                    Default::default()
                });
            }};
        }
        let service = |command: ServiceCommand| ShellAction::Service(command);

        on!(on_overview_requested, |c, ui, open| c.set_overview(&ui, open));
        on!(on_launcher_requested, |c, ui, open| c.set_launcher(&ui, open, ""));
        on!(on_popup_requested, |c, ui, popup| c.set_popup(&ui, popup));
        on!(on_workspace_clicked, |c, ui, workspace| {
            let _ = &ui;
            if let Ok(workspace) = u32::try_from(workspace) {
                c.emit(ShellAction::Compositor(Request::SwitchWorkspace { workspace }));
            }
        });
        on!(on_dock_clicked, |c, ui, row| if let Some(row) = index(row) {
            c.dock_clicked(&ui, row);
        });
        on!(on_dock_menu_requested, |c, ui, row, x| if let Some(row) = index(row) {
            c.open_dock_menu(&ui, row, x);
        });
        on!(on_dock_menu_window, |c, ui, row| if let Some(row) = index(row) {
            c.dock_menu_window(&ui, row);
        });
        on!(on_dock_menu_new_window, |c, ui| c.dock_menu_new_window(&ui));
        on!(on_dock_menu_toggle_pin, |c, ui| c.dock_menu_toggle_pin(&ui));
        on!(on_dock_menu_quit, |c, ui| c.dock_menu_quit(&ui));
        on!(on_launcher_query_edited, |c, ui, text| c.search(&ui, &text));
        on!(on_launcher_activated, |c, ui, row| c.launch_result(&ui, row));
        on!(on_window_activated, |c, ui, row| if let Some(id) = c.window_id(row) {
            c.set_overview(&ui, false);
            c.emit(ShellAction::Compositor(Request::Activate { id }));
        });
        on!(on_window_closed, |c, ui, row| {
            let _ = &ui;
            if let Some(id) = c.window_id(row) {
                c.emit(ShellAction::Compositor(Request::Close { id }));
            }
        });
        on!(on_window_moved, |c, ui, row, workspace| {
            let _ = &ui;
            if let (Some(id), Ok(workspace)) = (c.window_id(row), u32::try_from(workspace)) {
                c.emit(ShellAction::Compositor(Request::MoveToWorkspace { id, workspace }));
            }
        });
        on!(on_overview_key_typed, |c, ui, text| {
            if is_search_text(&text) {
                c.set_launcher(&ui, true, &text);
                true
            } else {
                false
            }
        });
        on!(on_volume_changed, |c, ui, percent| c.update_system(&ui, |system| {
            let volume = (percent / 100.0).clamp(0.0, 1.5);
            if let Some(audio) = &mut system.audio {
                audio.volume = volume;
            }
            Some(ServiceCommand::SetVolume(volume))
        }));
        on!(on_mute_toggled, |c, ui| c.update_system(&ui, |system| {
            if let Some(audio) = &mut system.audio {
                audio.muted = !audio.muted;
            }
            Some(ServiceCommand::ToggleMute)
        }));
        on!(on_brightness_changed, |c, ui, percent| c.update_system(&ui, |system| {
            let level = (percent / 100.0).clamp(0.01, 1.0);
            system.brightness = system.brightness.map(|_| level);
            Some(ServiceCommand::SetBrightness(level))
        }));
        on!(on_wifi_toggled, |c, ui| c.update_system(&ui, |system| {
            system.network.wifi_enabled = !system.network.wifi_enabled;
            Some(ServiceCommand::SetWifiEnabled(system.network.wifi_enabled))
        }));
        on!(on_bluetooth_toggled, |c, ui| c.update_system(&ui, |system| {
            let bluetooth = system.bluetooth.as_mut()?;
            bluetooth.powered = !bluetooth.powered;
            Some(ServiceCommand::SetBluetoothPowered(bluetooth.powered))
        }));
        on!(on_do_not_disturb_toggled, |c, ui, on| c.update_system(&ui, |system| {
            system.do_not_disturb = on;
            Some(ServiceCommand::SetDoNotDisturb(on))
        }));
        on!(on_dark_style_toggled, |c, ui| c.toggle_dark_style(&ui));
        on!(on_settings_requested, |c, ui, page| {
            c.set_popup(&ui, Popup::None);
            let page = (!page.is_empty()).then(|| page.to_string());
            c.emit(ShellAction::OpenSettings(page));
        });
        on!(on_media_previous, |c, ui| {
            let _ = &ui;
            c.emit(service(ServiceCommand::MediaPrevious));
        });
        on!(on_media_play_pause, |c, ui| {
            let _ = &ui;
            c.emit(service(ServiceCommand::MediaPlayPause));
        });
        on!(on_media_next, |c, ui| {
            let _ = &ui;
            c.emit(service(ServiceCommand::MediaNext));
        });
        on!(on_lock_requested, |c, ui| {
            c.set_popup(&ui, Popup::None);
            c.emit(ShellAction::Compositor(Request::Lock));
        });
        on!(on_suspend_requested, |c, ui| {
            c.set_popup(&ui, Popup::None);
            c.emit(service(ServiceCommand::Suspend));
        });
        on!(on_power_requested, |c, ui, action| {
            c.set_popup(&ui, Popup::None);
            ui.set_power_action(action);
            ui.invoke_focus_power_dialog();
        });
        on!(on_power_confirmed, |c, ui, action| c.power_confirmed(&ui, action));
        on!(on_month_changed, |c, ui, delta| c.change_month(&ui, delta));
        on!(on_notification_activated, |c, ui, id| c.notification_activated(&ui, id as u32));
        on!(on_notification_dismissed, |c, ui, id| {
            let _ = &ui;
            c.notification_dismissed(id as u32);
        });
        on!(on_notification_action, |c, ui, id, key| {
            let _ = &ui;
            c.notification_action(id as u32, key.to_string());
        });
        on!(on_clear_notifications, |c, ui| {
            let _ = &ui;
            c.clear_notifications();
        });
        on!(on_unlock_requested, |c, ui, password| c.unlock_requested(&ui, password.to_string()));
    }
}

/// Whether a key typed in the overview should start a search: printable text, not a control or function key.
fn is_search_text(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            !c.is_control() && !c.is_whitespace() && !('\u{f700}'..='\u{f8ff}').contains(&c)
        })
}

/// The scale of this shell's output, from the compositor or else the configuration.
fn output_scale(state: &State) -> f64 {
    state
        .outputs
        .iter()
        .find(|o| o.name == state.output)
        .or_else(|| if state.output.is_empty() { state.outputs.first() } else { None })
        .map_or(state.config.appearance.scale, |o| o.scale)
}

/// The application name and icon for a window, from its desktop entry when there is one.
fn app_name_and_icon(apps: &AppIndex, window: &WindowInfo) -> (String, Option<String>) {
    match apps.find_by_app_id(&window.app_id) {
        Some(entry) => (entry.name.clone(), entry.icon.clone()),
        None if !window.app_id.trim().is_empty() => {
            (readable_app_id(&window.app_id), Some(window.app_id.clone()))
        }
        None => (window.title.clone(), None),
    }
}

fn display_title(window: &WindowInfo, app_name: &str) -> String {
    if window.title.trim().is_empty() { app_name.to_owned() } else { window.title.clone() }
}

/// The user's full name from the password database, or the login name.
fn user_display_name() -> String {
    let login = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
    let full_name = std::fs::read_to_string("/etc/passwd").ok().and_then(|passwd| {
        passwd.lines().find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == login).then_some(())?;
            let gecos = fields.nth(3)?.split(',').next()?.trim();
            (!gecos.is_empty()).then(|| gecos.to_owned())
        })
    });
    match full_name {
        Some(name) => name,
        None if !login.is_empty() => login,
        None => "User".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_text_excludes_special_keys() {
        assert!(is_search_text("f"));
        assert!(is_search_text("é"));
        assert!(!is_search_text(""));
        assert!(!is_search_text(" "));
        assert!(!is_search_text("\t"));
        assert!(!is_search_text("\u{1b}"));
        assert!(!is_search_text("\u{f700}"));
    }

    #[test]
    fn rows_sync_in_place() {
        let model = VecModel::from(vec![1, 2, 3]);
        sync_rows(&model, vec![1, 5]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5]);
        sync_rows(&model, vec![1, 5, 6, 7]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5, 6, 7]);
        sync_rows(&model, Vec::new());
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn titles_fall_back_to_the_app_name() {
        let window = WindowInfo { title: "  ".into(), ..Default::default() };
        assert_eq!(display_title(&window, "Files"), "Files");
        let window = WindowInfo { title: "Doc".into(), ..Default::default() };
        assert_eq!(display_title(&window, "Files"), "Doc");
    }

    #[test]
    fn user_name_is_never_empty() {
        assert!(!user_display_name().is_empty());
    }
}
