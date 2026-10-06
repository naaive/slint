// SPDX-License-Identifier: MIT

//! The state behind [`crate::ShellModel`], shared by every view, and the handlers that change it.
//!
//! State lives in one `RefCell`. Handlers finish with it before emitting a [`ShellAction`],
//! because the host may call back into the shell from its action handler.

mod lock;
mod notifications;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

use chrono::{Datelike, Local};
use nimbus_config::{ColorScheme, Config, PanelPosition};
use nimbus_ipc::{CompositorState, Event, OutputInfo, ShellCommand, WindowInfo};
use nimbus_services::{CloseReason, ServiceCommand, ServiceEvent, SystemState};
use nimbus_theme::ThemeSettings;
use nimbus_xdg::{AppIndex, IconResolver};
use slint::{
    ComponentHandle, Image, Model as _, ModelRc, SharedString, Timer, TimerMode, VecModel,
};

use crate::backdrop::Backdrop;
use crate::client_images::ClientImages;
use crate::clock::{self, FALLBACK_CLOCK_FORMAT};
use crate::icons::{ICON_SIZE, IconCache};
use crate::notifications::Notifications;
use crate::persist::ConfigWriter;
use crate::system_scheme::SYSTEM;
use crate::view::View;
use crate::windows::{DockEntry, Windows, dock_entries, readable_app_id};
use crate::{
    AppVisual, CalendarDay, Desktop, DockItem, DockWindow, LockWindow, NotificationAction,
    NotificationItem, Osd, OsdKind, OsdWindow, OverlayWindow, PanelSettings, PanelWindow,
    PopupWindow, ShellAction, SystemStatus, Theme, ToastWindow,
};

/// How long the OSD stays after the last change.
pub const OSD_TIMEOUT: Duration = Duration::from_millis(1500);
/// How often results of background work are checked while any is running.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// The panel heights the layout supports.
const PANEL_HEIGHT: std::ops::RangeInclusive<u32> = 24..=64;
/// Change per volume or brightness key press.
const LEVEL_STEP: f32 = 0.05;

/// Updates `model` to `rows`, touching only the rows that differ.
pub fn sync_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
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

/// The values of the `Desktop` global, which every window holds a copy of.
#[derive(Default)]
struct DesktopData {
    panel: PanelSettings,
    locked: bool,
    active_workspace: i32,
    focused_title: SharedString,
    focused_visual: AppVisual,
    clock_text: SharedString,
    long_time: SharedString,
    weekday: SharedString,
    date: SharedString,
    month_title: SharedString,
    status: SystemStatus,
    volume: f32,
    brightness: f32,
    do_not_disturb: bool,
    has_unread: bool,
    osd_shown: bool,
    osd_kind: OsdKind,
    osd_level: f32,
    osd_muted: bool,
    lock_busy: bool,
    lock_error: SharedString,
    lock_backdrop: Image,
    user_name: SharedString,
}

/// The rows every view shows alike.
#[derive(Default)]
pub struct Models {
    dock: Rc<VecModel<DockItem>>,
    notifications: Rc<VecModel<NotificationItem>>,
    toasts: Rc<VecModel<NotificationItem>>,
    days: Rc<VecModel<CalendarDay>>,
}

impl DesktopData {
    /// Sets every property; Slint ignores the ones that didn't change.
    fn write(&self, desktop: &Desktop, models: &Models) {
        desktop.set_panel(self.panel.clone());
        desktop.set_locked(self.locked);
        desktop.set_active_workspace(self.active_workspace);
        desktop.set_focused_title(self.focused_title.clone());
        desktop.set_focused_visual(self.focused_visual.clone());
        desktop.set_dock_items(ModelRc::from(models.dock.clone()));
        desktop.set_clock_text(self.clock_text.clone());
        desktop.set_long_time(self.long_time.clone());
        desktop.set_weekday(self.weekday.clone());
        desktop.set_date(self.date.clone());
        desktop.set_month_title(self.month_title.clone());
        desktop.set_days(ModelRc::from(models.days.clone()));
        desktop.set_status(self.status.clone());
        desktop.set_volume(self.volume);
        desktop.set_brightness(self.brightness);
        desktop.set_do_not_disturb(self.do_not_disturb);
        desktop.set_notifications(ModelRc::from(models.notifications.clone()));
        desktop.set_toasts(ModelRc::from(models.toasts.clone()));
        desktop.set_has_unread(self.has_unread);
        desktop.set_osd_shown(self.osd_shown);
        desktop.set_osd_kind(self.osd_kind);
        desktop.set_osd_level(self.osd_level);
        desktop.set_osd_muted(self.osd_muted);
        desktop.set_lock_busy(self.lock_busy);
        desktop.set_lock_error(self.lock_error.clone());
        desktop.set_lock_backdrop(self.lock_backdrop.clone());
        desktop.set_user_name(self.user_name.clone());
    }
}

/// A window that shows the shared state.
pub trait SharedWindow {
    fn show_shared(&self, state: &State, models: &Models, theme: bool);
}

macro_rules! shared_window {
    ($($window:ty),*) => {$(
        impl SharedWindow for $window {
            fn show_shared(&self, state: &State, models: &Models, theme: bool) {
                if theme {
                    nimbus_theme::apply_theme!(self, state.theme);
                }
                state.desktop.write(&self.global::<Desktop>(), models);
            }
        }
    )*};
}
shared_window!(
    PanelWindow,
    DockWindow,
    PopupWindow,
    OverlayWindow,
    ToastWindow,
    OsdWindow,
    LockWindow
);

/// How a window shows in every view, resolved once per refresh.
pub struct WindowRow {
    pub title: SharedString,
    pub app_name: SharedString,
    pub visual: AppVisual,
}

pub struct State {
    pub config: Config,
    theme: ThemeSettings,
    outputs: Vec<OutputInfo>,
    pub windows: Windows,
    pub workspace_count: u32,
    pub active_workspace: u32,
    pub apps: Rc<AppIndex>,
    /// The application name and icon per app id.
    app_names: HashMap<String, (String, Option<String>)>,
    pub icons: IconCache,
    client_images: ClientImages,
    pub dock: Vec<DockEntry>,
    /// One per window in [`Windows::list`].
    pub window_rows: Vec<WindowRow>,
    notifications: Notifications,
    action_models: HashMap<u32, ModelRc<NotificationAction>>,
    system: SystemState,
    /// The month the calendar shows, as `(year, month)`.
    month: (i32, u32),
    pub writer: ConfigWriter,
    backdrop: Backdrop,
    /// The system color scheme query the theme waits for.
    scheme_ticket: Option<u64>,
    desktop: DesktopData,
}

pub struct Model {
    this: Weak<Self>,
    on_action: Box<dyn Fn(ShellAction)>,
    on_unlock: RefCell<Option<Rc<dyn Fn(String)>>>,
    pub state: RefCell<State>,
    models: Models,
    /// The theme the windows show.
    shown_theme: RefCell<Option<ThemeSettings>>,
    views: RefCell<Vec<Weak<View>>>,
    locks: RefCell<Vec<slint::Weak<LockWindow>>>,
    clock_timer: Timer,
    osd_timer: Timer,
    poll_timer: Timer,
    toast_timers: RefCell<HashMap<u32, Timer>>,
}

impl Model {
    pub fn new(config: &Config, on_action: Box<dyn Fn(ShellAction)>) -> Rc<Self> {
        let today = Local::now().date_naive();
        let model = Rc::new_cyclic(|this| Self {
            this: this.clone(),
            on_action,
            on_unlock: RefCell::new(None),
            state: RefCell::new(State {
                config: config.clone(),
                theme: ThemeSettings::default(),
                outputs: Vec::new(),
                windows: Windows::default(),
                workspace_count: 0,
                active_workspace: 0,
                apps: Rc::default(),
                app_names: HashMap::new(),
                icons: IconCache::default(),
                client_images: ClientImages::new(ICON_SIZE),
                dock: Vec::new(),
                window_rows: Vec::new(),
                notifications: Notifications::default(),
                action_models: HashMap::new(),
                system: SystemState::default(),
                month: (today.year(), today.month()),
                writer: ConfigWriter::new(),
                backdrop: Backdrop::default(),
                scheme_ticket: None,
                desktop: DesktopData {
                    user_name: user_display_name().into(),
                    ..DesktopData::default()
                },
            }),
            models: Models::default(),
            shown_theme: RefCell::new(None),
            views: RefCell::new(Vec::new()),
            locks: RefCell::new(Vec::new()),
            clock_timer: Timer::default(),
            osd_timer: Timer::default(),
            poll_timer: Timer::default(),
            toast_timers: RefCell::new(HashMap::new()),
        });
        model.set_config(config);
        model.schedule_clock();
        model
    }

    pub fn emit(&self, action: ShellAction) {
        tracing::debug!(?action, "shell action");
        (self.on_action)(action);
    }

    // Views.

    /// The live views of the shell, in the order they were added.
    pub fn views(&self) -> Vec<Rc<View>> {
        let mut views = self.views.borrow_mut();
        views.retain(|view| view.strong_count() > 0);
        views.iter().filter_map(Weak::upgrade).collect()
    }

    fn lock_windows(&self) -> Vec<LockWindow> {
        let mut locks = self.locks.borrow_mut();
        locks.retain(|lock| lock.upgrade().is_some());
        locks.iter().filter_map(slint::Weak::upgrade).collect()
    }

    /// Starts showing the shared state on a new view.
    pub fn add_view(&self, view: &Rc<View>) {
        self.views.borrow_mut().push(Rc::downgrade(view));
        view.refresh(&self.state.borrow());
    }

    /// Shows the shared state on a window, with the theme when `theme` is set.
    pub fn show_shared_on(&self, window: &dyn SharedWindow, state: &State, theme: bool) {
        window.show_shared(state, &self.models, theme);
    }

    /// Starts showing the shared state on a new lock screen.
    pub fn add_lock(&self, ui: &LockWindow) {
        self.locks.borrow_mut().push(ui.as_weak());
        ui.show_shared(&self.state.borrow(), &self.models, true);
    }

    /// Shows the current shared state on every window, and the theme if it changed.
    fn publish(&self) {
        let state = self.state.borrow();
        let theme = self.shown_theme.borrow().as_ref() != Some(&state.theme);
        if theme {
            *self.shown_theme.borrow_mut() = Some(state.theme.clone());
        }
        for view in self.views() {
            view.show_shared(&state, theme);
        }
        for lock in self.lock_windows() {
            lock.show_shared(&state, &self.models, theme);
        }
    }

    // Configuration and clock.

    pub fn set_config(&self, config: &Config) {
        let mut scheme_ticket = None;
        let theme = ThemeSettings::from_config_with_system(&config.appearance, || {
            let (ticket, dark) = SYSTEM.query();
            scheme_ticket = Some(ticket);
            dark
        });
        {
            let mut state = self.state.borrow_mut();
            state.desktop.panel = PanelSettings {
                top: config.panel.position == PanelPosition::Top,
                height: config.panel.height.clamp(*PANEL_HEIGHT.start(), *PANEL_HEIGHT.end())
                    as f32,
                show_dock: config.panel.show_dock,
                dock_autohide: config.panel.dock_autohide,
                show_battery_percentage: config.panel.show_battery_percentage,
            };
            state.theme = theme;
            state.config = config.clone();
            state.scheme_ticket = scheme_ticket;
            state.backdrop.request(config.appearance.wallpaper.as_deref());
            update_scale(&mut state);
        }
        self.update_windows();
        self.update_clock();
        self.publish();
        self.schedule_poll();
    }

    pub fn set_config_path(&self, path: Option<PathBuf>) {
        self.state.borrow_mut().writer.set_path(path);
    }

    /// Polls background work until all of it is done.
    fn schedule_poll(&self) {
        if self.poll_timer.running() {
            return;
        }
        let weak = self.this.clone();
        self.poll_timer.start(TimerMode::Repeated, POLL_INTERVAL, move || {
            if let Some(this) = weak.upgrade()
                && !this.poll()
            {
                this.poll_timer.stop();
            }
        });
    }

    /// Applies the results of finished background work; returns whether any is still running.
    fn poll(&self) -> bool {
        let (changed, images) = {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let mut changed = false;
            if let Some(image) = state.backdrop.take_update() {
                state.desktop.lock_backdrop = image.unwrap_or_default();
                changed = true;
            }
            let dark = state.scheme_ticket.and_then(|ticket| SYSTEM.answer(ticket));
            if let Some(dark) = dark {
                state.scheme_ticket = None;
                if state.config.appearance.color_scheme == ColorScheme::System {
                    state.theme =
                        ThemeSettings::from_config_with_system(&state.config.appearance, || dark);
                    changed = true;
                }
            }
            (changed, state.client_images.poll())
        };
        if images {
            self.refresh_notifications();
        }
        if changed {
            self.publish();
        }
        let state = self.state.borrow();
        state.backdrop.is_pending()
            || state.scheme_ticket.is_some()
            || state.client_images.is_pending()
    }

    fn schedule_clock(&self) {
        let weak = self.this.clone();
        self.clock_timer.start(
            TimerMode::SingleShot,
            clock::until_next_minute(&Local::now()),
            move || {
                if let Some(this) = weak.upgrade() {
                    this.refresh_clock();
                    this.schedule_clock();
                }
            },
        );
    }

    /// Updates everything that shows the time: the clock, the lock screen, the calendar, and notification ages.
    pub fn refresh_clock(&self) {
        self.update_clock();
        self.publish();
    }

    fn update_clock(&self) {
        let now = Local::now();
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let format = &state.config.panel.clock_format;
            let desktop = &mut state.desktop;
            desktop.clock_text = clock::format_time(&now, format, FALLBACK_CLOCK_FORMAT).into();
            let twelve_hour = ["%I", "%l", "%p", "%r"].iter().any(|s| format.contains(s));
            let long_time = if twelve_hour { "%-I:%M" } else { "%H:%M" };
            desktop.long_time = clock::format_time(&now, long_time, "%H:%M").into();
            desktop.weekday = clock::format_time(&now, "%A", "%A").into();
            desktop.date = clock::format_time(&now, "%B %-d %Y", "%B %-d %Y").into();
            if let Some(image) = state.backdrop.take_update() {
                desktop.lock_backdrop = image.unwrap_or_default();
            }
        }
        self.refresh_notifications();
        self.update_calendar();
    }

    fn update_calendar(&self) {
        let today = Local::now().date_naive();
        let mut state = self.state.borrow_mut();
        let (year, month) = state.month;
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
        state.desktop.month_title = clock::month_title(year, month).into();
    }

    /// Shows the month `delta` months after the shown one, or the current month for 0.
    pub fn change_month(&self, delta: i32) {
        {
            let mut state = self.state.borrow_mut();
            state.month = if delta == 0 {
                let today = Local::now().date_naive();
                (today.year(), today.month())
            } else {
                clock::add_months(state.month.0, state.month.1, delta)
            };
        }
        self.update_calendar();
        self.publish();
    }

    // Windows, workspaces, and applications.

    pub fn set_compositor_state(&self, compositor: &CompositorState) {
        {
            let mut state = self.state.borrow_mut();
            state.windows.replace(&compositor.windows);
            state.workspace_count = compositor.workspace_count;
            state.active_workspace = compositor.active_workspace;
            state.outputs = compositor.outputs.clone();
            update_scale(&mut state);
        }
        self.refresh_windows();
    }

    /// Applies compositor events in order, then refreshes the views once.
    pub fn handle_compositor_events(&self, events: &[Event]) {
        let mut changed = false;
        {
            let mut state = self.state.borrow_mut();
            for event in events {
                match event {
                    Event::WindowOpened(info) | Event::WindowChanged(info) => {
                        state.windows.upsert(info);
                    }
                    Event::WindowClosed { id } => {
                        if let Some(row) = state.windows.remove(*id) {
                            if row < state.window_rows.len() {
                                state.window_rows.remove(row);
                            }
                            for view in self.views() {
                                view.window_removed(row);
                            }
                        }
                    }
                    Event::WorkspaceActivated { workspace } => state.active_workspace = *workspace,
                    Event::OutputsChanged { outputs } => {
                        state.outputs = outputs.clone();
                        update_scale(&mut state);
                    }
                    Event::LayoutChanged { .. }
                    | Event::ShellCommand { .. }
                    | Event::LockState { .. } => continue,
                }
                changed = true;
            }
        }
        if changed {
            self.refresh_windows();
        }
    }

    /// Brings the dock, the focused window, and every view's windows up to date with the state.
    pub fn refresh_windows(&self) {
        self.update_windows();
        self.publish();
    }

    fn update_windows(&self) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
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

        let rows = state
            .windows
            .list()
            .iter()
            .map(|window| {
                let (name, icon) = app_name_and_icon(&state.apps, &mut state.app_names, window);
                WindowRow {
                    title: display_title(window, &name).into(),
                    visual: state.icons.visual(&name, icon.as_deref()),
                    app_name: name.into(),
                }
            })
            .collect();
        state.window_rows = rows;

        let focused = state.windows.list().iter().position(|w| w.focused);
        if let Some(row) = focused.and_then(|row| state.window_rows.get(row)) {
            state.desktop.focused_visual = row.visual.clone();
            state.desktop.focused_title = row.title.clone();
        } else {
            state.desktop.focused_title = SharedString::new();
        }
        state.desktop.active_workspace = state.active_workspace as i32;
        for view in self.views() {
            view.refresh(state);
        }
    }

    pub fn set_apps(&self, apps: Rc<AppIndex>, icons: IconResolver) {
        {
            let mut state = self.state.borrow_mut();
            state.apps = apps;
            state.app_names.clear();
            state.icons.set_resolver(icons);
            update_scale(&mut state);
        }
        self.refresh_windows();
        for view in self.views() {
            view.refresh_search();
        }
    }

    // System state and quick settings.

    pub fn handle_service_event(&self, event: &ServiceEvent) {
        match event {
            ServiceEvent::State(system) => {
                self.state.borrow_mut().system = system.clone();
                self.update_status();
                self.publish();
            }
            ServiceEvent::Notification(notification) => self.add_notification(notification),
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
            ServiceEvent::LockRequested => self.set_locked(true),
            ServiceEvent::UnlockRequested => self.set_locked(false),
            ServiceEvent::LogoutRequested => {}
        }
    }

    fn update_status(&self) {
        let mut state = self.state.borrow_mut();
        let status = crate::status::status(&state.system);
        state.desktop.do_not_disturb = state.system.do_not_disturb;
        state.desktop.status = status.status;
        state.desktop.volume = status.volume_percent;
        state.desktop.brightness = status.brightness_percent;
    }

    /// Applies a change from quick settings right away, and asks the services to make it.
    pub fn update_system(&self, change: impl FnOnce(&mut SystemState) -> Option<ServiceCommand>) {
        let command = change(&mut self.state.borrow_mut().system);
        self.update_status();
        self.publish();
        if let Some(command) = command {
            self.emit(ShellAction::Service(command));
        }
    }

    pub fn toggle_dark_style(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.theme.dark = !state.theme.dark;
            let scheme = if state.theme.dark { ColorScheme::Dark } else { ColorScheme::Light };
            state.config.appearance.color_scheme = scheme;
            state.writer.edit(move |config| config.appearance.color_scheme = scheme);
        }
        self.publish();
    }

    pub fn show_osd(&self, osd: Osd) {
        let sanitize = |level: f32| if level.is_finite() { level.clamp(0.0, 1.5) } else { 0.0 };
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let (kind, level, muted) = match osd {
                Osd::Volume { level, muted } => {
                    if let Some(audio) = &mut state.system.audio {
                        audio.volume = sanitize(level);
                        audio.muted = muted;
                    }
                    (OsdKind::Volume, sanitize(level), muted)
                }
                Osd::Brightness { level } => {
                    let level = sanitize(level).min(1.0);
                    if state.system.brightness.is_some() {
                        state.system.brightness = Some(level);
                    }
                    (OsdKind::Brightness, level, false)
                }
            };
            let desktop = &mut state.desktop;
            (desktop.osd_kind, desktop.osd_level, desktop.osd_muted) = (kind, level, muted);
            desktop.osd_shown = true;
        }
        self.update_status();
        self.publish();
        let weak = self.this.clone();
        self.osd_timer.start(TimerMode::SingleShot, OSD_TIMEOUT, move || {
            if let Some(this) = weak.upgrade() {
                this.state.borrow_mut().desktop.osd_shown = false;
                this.publish();
            }
        });
    }

    /// Applies a volume or brightness key to the shown level, shows the OSD,
    /// and returns the command for the services.
    pub fn step_level(&self, command: ShellCommand) -> Option<ServiceCommand> {
        let (command, osd) = level_step(&mut self.state.borrow_mut().system, command)?;
        if let Some(osd) = osd {
            self.show_osd(osd);
        }
        Some(command)
    }

    pub fn shows_dock(&self) -> bool {
        self.state.borrow().desktop.panel.show_dock
    }

    pub fn has_toasts(&self) -> bool {
        self.models.toasts.row_count() > 0
    }

    pub fn osd_shown(&self) -> bool {
        self.state.borrow().desktop.osd_shown
    }

    /// Clears the unread dot once the user opened the notification history.
    pub fn mark_read(&self) {
        self.state.borrow_mut().desktop.has_unread = false;
        self.publish();
    }
}

fn update_scale(state: &mut State) {
    state.icons.set_scale(icon_scale(state));
    state.client_images.set_size(ICON_SIZE * state.icons.scale());
}

/// The scale to load icons at: that of the densest output, from the compositor or else the configuration.
fn icon_scale(state: &State) -> f64 {
    state.outputs.iter().map(|o| o.scale).reduce(f64::max).unwrap_or(state.config.appearance.scale)
}

/// Changes the level a key acts on, so the next press builds on it before the services report back.
fn level_step(
    system: &mut SystemState,
    command: ShellCommand,
) -> Option<(ServiceCommand, Option<Osd>)> {
    let volume = |system: &mut SystemState, delta: f32| {
        let audio = system.audio.as_mut()?;
        audio.volume = (audio.volume + delta).clamp(0.0, 1.0);
        let muted = audio.muted && delta <= 0.0;
        Some((
            ServiceCommand::SetVolume(audio.volume),
            Some(Osd::Volume { level: audio.volume, muted }),
        ))
    };
    let brightness = |system: &mut SystemState, delta: f32| {
        let level = system.brightness.as_mut()?;
        // Never fully dark: a black screen looks like a hang.
        *level = (*level + delta).clamp(0.01, 1.0);
        Some((ServiceCommand::SetBrightness(*level), Some(Osd::Brightness { level: *level })))
    };
    match command {
        ShellCommand::VolumeUp => volume(system, LEVEL_STEP),
        ShellCommand::VolumeDown => volume(system, -LEVEL_STEP),
        ShellCommand::ToggleMute => {
            let osd = system.audio.as_mut().map(|audio| {
                audio.muted = !audio.muted;
                Osd::Volume { level: audio.volume, muted: audio.muted }
            });
            Some((ServiceCommand::ToggleMute, osd))
        }
        ShellCommand::BrightnessUp => brightness(system, LEVEL_STEP),
        ShellCommand::BrightnessDown => brightness(system, -LEVEL_STEP),
        ShellCommand::ToggleLauncher | ShellCommand::ToggleOverview => None,
    }
}

/// The application name and icon for a window, from its desktop entry when there is one.
fn app_name_and_icon(
    apps: &AppIndex,
    cache: &mut HashMap<String, (String, Option<String>)>,
    window: &WindowInfo,
) -> (String, Option<String>) {
    if window.app_id.trim().is_empty() {
        return (window.title.clone(), None);
    }
    if let Some(found) = cache.get(&window.app_id) {
        return found.clone();
    }
    let found = match apps.find_by_app_id(&window.app_id) {
        Some(entry) => (entry.name.clone(), entry.icon.clone()),
        None => (readable_app_id(&window.app_id), Some(window.app_id.clone())),
    };
    cache.insert(window.app_id.clone(), found.clone());
    found
}

pub fn display_title(window: &WindowInfo, app_name: &str) -> String {
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
    fn icons_load_for_the_densest_output() {
        i_slint_backend_testing::init_no_event_loop();
        let model = Model::new(&Config::default(), Box::new(|_| {}));
        let output =
            |name: &str, scale| OutputInfo { name: name.into(), scale, ..Default::default() };
        model.set_compositor_state(&CompositorState {
            outputs: vec![output("A", 1.0), output("B", 1.5)],
            ..Default::default()
        });
        assert_eq!(model.state.borrow().icons.scale(), 2);
    }

    #[test]
    fn the_lock_backdrop_arrives_without_a_clock_tick() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("wall.png");
        image::RgbImage::new(200, 100).save(&path).expect("fixture saves");
        let mut config = Config::default();
        config.appearance.color_scheme = ColorScheme::Dark;
        config.appearance.wallpaper = Some(path);
        let model = Model::new(&config, Box::new(|_| {}));
        let lock = LockWindow::new().expect("the window opens");
        model.add_lock(&lock);
        let backdrop = || lock.global::<Desktop>().get_lock_backdrop().size().width;

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !model.state.borrow().backdrop.is_decoded() {
            assert!(std::time::Instant::now() < deadline, "the wallpaper never decoded");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(backdrop(), 0);
        i_slint_backend_testing::mock_elapsed_time(POLL_INTERVAL);
        assert!(backdrop() > 0);
        assert!(!model.poll_timer.running(), "polling stops once nothing is pending");
    }

    #[test]
    fn rapid_level_keys_build_on_each_other() {
        let mut system = SystemState {
            audio: Some(nimbus_services::Audio { volume: 0.5, muted: false }),
            brightness: Some(0.5),
            ..Default::default()
        };
        let commands: Vec<ServiceCommand> = (0..2)
            .filter_map(|_| level_step(&mut system, ShellCommand::VolumeUp).map(|(c, _)| c))
            .collect();
        assert!(matches!(
            commands[..],
            [ServiceCommand::SetVolume(a), ServiceCommand::SetVolume(b)]
                if (a - 0.55).abs() < 1e-6 && (b - 0.60).abs() < 1e-6
        ));
        let mutes: Vec<Option<Osd>> = (0..2)
            .filter_map(|_| level_step(&mut system, ShellCommand::ToggleMute).map(|(_, osd)| osd))
            .collect();
        assert!(matches!(
            mutes[..],
            [Some(Osd::Volume { muted: true, .. }), Some(Osd::Volume { muted: false, .. })]
        ));
        level_step(&mut system, ShellCommand::BrightnessDown);
        level_step(&mut system, ShellCommand::BrightnessDown);
        assert!(system.brightness.is_some_and(|b| (b - 0.40).abs() < 1e-6));
        assert!(level_step(&mut system, ShellCommand::ToggleLauncher).is_none());
    }

    #[test]
    fn user_name_is_never_empty() {
        assert!(!user_display_name().is_empty());
    }
}
