// SPDX-License-Identifier: MIT

//! [`ShellView`]: the overlay on one output, with what only that output shows,
//! such as its open popup, launcher, and the windows on it.

use std::cell::RefCell;
use std::rc::Rc;

use nimbus_ipc::Request;
use nimbus_services::ServiceCommand;
use slint::{ComponentHandle, Model as _, ModelRc, VecModel};

use crate::model::{Model, State, app_name_and_icon, display_title, sync_rows};
use crate::windows::{DockClick, DockEntry, dock_click};
use crate::{
    AppItem, Desktop, DockMenuWindow, Exclusive, Popup, PowerAction, Rect, ShellAction, ShellModel,
    ShellWindow, WindowItem, WorkspaceItem,
};

/// The most workspace dots the panel shows.
const MAX_WORKSPACES: u32 = 32;

fn index(row: i32) -> Option<usize> {
    usize::try_from(row).ok()
}

/// The shell's overlay on one output: panel, dock, launcher, overview, popups, toasts, and OSD.
///
/// It shows the [`ShellModel`] it was created with, and keeps only what's particular to its output.
pub struct ShellView(Rc<View>);

impl ShellView {
    /// Creates the view for the output named `output`; the current Slint platform supplies its window.
    pub fn new(model: &ShellModel, output: &str) -> Result<Self, slint::PlatformError> {
        let view = Rc::new(View {
            model: model.0.clone(),
            ui: ShellWindow::new()?,
            output: output.to_owned(),
            state: RefCell::default(),
            models: Models::default(),
        });
        view.models.attach(&view.ui);
        View::connect(&view);
        model.0.add_view(&view);
        Ok(Self(view))
    }

    pub fn output(&self) -> &str {
        &self.0.output
    }

    pub fn window(&self) -> &slint::Window {
        self.0.ui.window()
    }

    /// The Slint component, for tests and for hosts that need direct access to its properties.
    pub fn component(&self) -> &ShellWindow {
        &self.0.ui
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.0.ui.show()
    }

    pub fn toggle_launcher(&self) {
        if !self.0.model.is_locked() {
            self.0.set_launcher(!self.0.ui.get_launcher_open(), "");
        }
    }

    pub fn toggle_overview(&self) {
        if !self.0.model.is_locked() {
            self.0.set_overview(!self.0.ui.get_overview_open());
        }
    }

    /// Returns where the view takes pointer input; everywhere else belongs to the windows below.
    /// While a popup, the launcher, or the overview is open, this covers the whole output.
    ///
    /// Toasts count from the frame that first draws them, so query this after rendering.
    pub fn input_region(&self) -> Vec<Rect> {
        let ui = &self.0.ui;
        if self.covers_output() {
            let window = self.window();
            let size = window.size().to_logical(window.scale_factor());
            return vec![Rect { x: 0.0, y: 0.0, width: size.width, height: size.height }];
        }
        [ui.get_panel_rect(), ui.get_dock_rect(), ui.get_toast_rect()]
            .into_iter()
            .map(Rect::from)
            .filter(|rect| !rect.is_empty())
            .collect()
    }

    /// Returns whether the view currently wants keyboard focus.
    pub fn wants_keyboard(&self) -> bool {
        self.covers_output()
    }

    pub fn exclusive_zone(&self) -> Exclusive {
        let ui = &self.0.ui;
        let panel = ui.global::<Desktop>().get_panel();
        let dock =
            if panel.show_dock && !panel.dock_autohide { ui.get_dock_exclusive() } else { 0.0 };
        if panel.top {
            Exclusive { top: panel.height, bottom: dock, ..Exclusive::default() }
        } else {
            Exclusive { bottom: panel.height + dock, ..Exclusive::default() }
        }
    }

    fn covers_output(&self) -> bool {
        let ui = &self.0.ui;
        ui.get_launcher_open()
            || ui.get_overview_open()
            || ui.get_popup() != Popup::None
            || ui.get_power_action() != PowerAction::None
    }
}

#[derive(Default)]
struct ViewState {
    /// The dock entry whose menu is open.
    dock_menu: Option<String>,
    launcher_ids: Vec<String>,
}

#[derive(Default)]
struct Models {
    workspaces: Rc<VecModel<WorkspaceItem>>,
    windows: Rc<VecModel<WindowItem>>,
    dock_menu: Rc<VecModel<DockMenuWindow>>,
    launcher: Rc<VecModel<AppItem>>,
}

impl Models {
    fn attach(&self, ui: &ShellWindow) {
        ui.set_workspaces(ModelRc::from(self.workspaces.clone()));
        ui.set_windows(ModelRc::from(self.windows.clone()));
        ui.set_dock_menu_windows(ModelRc::from(self.dock_menu.clone()));
        ui.set_launcher_apps(ModelRc::from(self.launcher.clone()));
    }
}

pub struct View {
    model: Rc<Model>,
    ui: ShellWindow,
    output: String,
    state: RefCell<ViewState>,
    models: Models,
}

impl View {
    pub fn ui(&self) -> &ShellWindow {
        &self.ui
    }

    /// Whether `output` is a window's output as seen from this view; an empty name matches any.
    fn shows(&self, output: &str) -> bool {
        self.output.is_empty() || output.is_empty() || output == self.output
    }

    /// Brings the windows, workspaces, and dock menu of this output up to date with `state`.
    pub fn refresh(&self, state: &mut State) {
        let ui = &self.ui;
        let slots = state.windows.slots(&self.output);
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
        sync_rows(&self.models.windows, rows);
        ui.set_active_count(active_count as i32);

        let count = if state.workspace_count > 0 {
            state.workspace_count
        } else {
            state.config.workspaces.count
        };
        let count = count
            .clamp(1, MAX_WORKSPACES)
            .max(state.active_workspace.saturating_add(1).min(MAX_WORKSPACES));
        let workspaces = (0..count)
            .map(|index| WorkspaceItem {
                index: index as i32,
                active: index == state.active_workspace,
                occupied: state
                    .windows
                    .list()
                    .iter()
                    .any(|w| w.workspace == index && self.shows(&w.output)),
            })
            .collect();
        sync_rows(&self.models.workspaces, workspaces);

        ui.set_fullscreen_active(state.windows.focused().is_some_and(|window| {
            window.fullscreen
                && self.shows(&window.output)
                && window.workspace == state.active_workspace
        }));
        self.fill_dock_menu(state);
    }

    pub fn window_removed(&self, row: usize) {
        if row < self.models.windows.row_count() {
            self.models.windows.remove(row);
        }
    }

    fn fill_dock_menu(&self, state: &State) {
        let ui = &self.ui;
        let Some(id) = self.state.borrow().dock_menu.clone() else {
            return;
        };
        let Some((index, entry)) = state.dock.iter().enumerate().find(|(_, e)| e.id == id) else {
            sync_rows(&self.models.dock_menu, Vec::new());
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
        sync_rows(&self.models.dock_menu, windows);
        ui.set_dock_menu_index(index as i32);
        ui.set_dock_menu_title(entry.name.as_str().into());
        ui.set_dock_menu_favorite(entry.favorite);
        ui.set_dock_menu_running(!entry.windows.is_empty());
    }

    // The dock.

    fn dock_clicked(&self, row: usize) {
        let click = {
            let state = self.model.state.borrow();
            state.dock.get(row).and_then(|entry| dock_click(entry, &state.windows))
        };
        let Some(click) = click else {
            return;
        };
        self.set_overview(false);
        self.model.emit(match click {
            DockClick::Launch(id) => ShellAction::Launch(id),
            DockClick::Compositor(request) => ShellAction::Compositor(request),
        });
    }

    fn open_dock_menu(&self, row: usize, center_x: f32) {
        {
            let state = self.model.state.borrow();
            let Some(entry) = state.dock.get(row) else {
                return;
            };
            self.state.borrow_mut().dock_menu = Some(entry.id.clone());
            self.fill_dock_menu(&state);
        }
        self.ui.set_dock_menu_x(center_x);
        self.set_popup(Popup::DockMenu);
    }

    fn dock_menu_entry(&self) -> Option<DockEntry> {
        let id = self.state.borrow().dock_menu.clone()?;
        self.model.state.borrow().dock.iter().find(|e| e.id == id).cloned()
    }

    fn dock_menu_window(&self, row: usize) {
        let id = self.dock_menu_entry().and_then(|entry| entry.windows.get(row).copied());
        self.set_popup(Popup::None);
        self.set_overview(false);
        if let Some(id) = id {
            self.model.emit(ShellAction::Compositor(Request::Activate { id }));
        }
    }

    fn dock_menu_new_window(&self) {
        let entry = self.dock_menu_entry();
        self.set_popup(Popup::None);
        if let Some(entry) = entry.filter(|e| e.launchable) {
            self.set_overview(false);
            self.model.emit(ShellAction::Launch(entry.id));
        }
    }

    fn dock_menu_toggle_pin(&self) {
        let Some(entry) = self.dock_menu_entry() else {
            self.set_popup(Popup::None);
            return;
        };
        {
            let mut state = self.model.state.borrow_mut();
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
        self.set_popup(Popup::None);
        self.model.refresh_windows();
    }

    fn dock_menu_quit(&self) {
        let entry = self.dock_menu_entry();
        self.set_popup(Popup::None);
        for id in entry.map(|e| e.windows).unwrap_or_default() {
            self.model.emit(ShellAction::Compositor(Request::Close { id }));
        }
    }

    fn window_id(&self, row: i32) -> Option<nimbus_ipc::WindowId> {
        self.model.state.borrow().windows.get(index(row)?).map(|w| w.id)
    }

    // Launcher, overview, and popups.

    fn set_launcher(&self, open: bool, query: &str) {
        let ui = &self.ui;
        if open {
            self.set_overview(false);
            self.set_popup(Popup::None);
            ui.set_launcher_query(query.into());
            self.search(query);
            ui.set_launcher_open(true);
            ui.invoke_focus_launcher_at(query.len().try_into().unwrap_or(i32::MAX));
        } else {
            ui.set_launcher_open(false);
        }
    }

    /// Searches again for the launcher's query, for example after the applications changed.
    pub fn refresh_search(&self) {
        if self.ui.get_launcher_open() {
            self.search(&self.ui.get_launcher_query());
        }
    }

    fn search(&self, query: &str) {
        let apps = {
            let mut state = self.model.state.borrow_mut();
            let state = &mut *state;
            let results: Vec<_> = state.apps.search(query).into_iter().cloned().collect();
            self.state.borrow_mut().launcher_ids = results.iter().map(|e| e.id.clone()).collect();
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
        self.ui.set_launcher_selected(0);
    }

    fn launch_result(&self, row: i32) {
        let id = index(row).and_then(|row| self.state.borrow().launcher_ids.get(row).cloned());
        if let Some(id) = id {
            self.set_launcher(false, "");
            self.model.emit(ShellAction::Launch(id));
        }
    }

    fn set_overview(&self, open: bool) {
        let ui = &self.ui;
        if open == ui.get_overview_open() {
            return;
        }
        if open {
            ui.set_launcher_open(false);
            self.set_popup(Popup::None);
            ui.set_overview_open(true);
            ui.invoke_focus_overview();
        } else {
            ui.set_overview_open(false);
        }
    }

    fn set_popup(&self, popup: Popup) {
        let ui = &self.ui;
        if popup != Popup::DockMenu {
            self.state.borrow_mut().dock_menu = None;
        }
        ui.set_power_menu_open(false);
        ui.set_popup(popup);
        match popup {
            Popup::None => {}
            Popup::Calendar => {
                self.model.mark_read();
                self.model.change_month(0);
                ui.invoke_focus_popup();
            }
            Popup::QuickSettings | Popup::DockMenu => ui.invoke_focus_popup(),
        }
    }

    pub fn close_everything(&self) {
        self.set_popup(Popup::None);
        self.ui.set_launcher_open(false);
        self.ui.set_overview_open(false);
        self.ui.set_power_action(PowerAction::None);
    }

    fn power_confirmed(&self, action: PowerAction) {
        self.ui.set_power_action(PowerAction::None);
        let command = match action {
            PowerAction::Restart => ServiceCommand::Reboot,
            PowerAction::PowerOff => ServiceCommand::PowerOff,
            PowerAction::LogOut => ServiceCommand::Logout,
            PowerAction::None => return,
        };
        self.model.emit(ShellAction::Service(command));
    }

    // Wiring.

    fn connect(this: &Rc<Self>) {
        macro_rules! on {
            ($setter:ident, |$v:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(this);
                this.ui.$setter(move |$($arg),*| {
                    if let Some($v) = weak.upgrade() {
                        return $body;
                    }
                    Default::default()
                });
            }};
        }
        let service = |command: ServiceCommand| ShellAction::Service(command);

        on!(on_overview_requested, |v, open| v.set_overview(open));
        on!(on_launcher_requested, |v, open| v.set_launcher(open, ""));
        on!(on_popup_requested, |v, popup| v.set_popup(popup));
        on!(on_workspace_clicked, |v, workspace| {
            if let Ok(workspace) = u32::try_from(workspace) {
                v.model.emit(ShellAction::Compositor(Request::SwitchWorkspace { workspace }));
            }
        });
        on!(on_dock_clicked, |v, row| if let Some(row) = index(row) {
            v.dock_clicked(row);
        });
        on!(on_dock_menu_requested, |v, row, x| if let Some(row) = index(row) {
            v.open_dock_menu(row, x);
        });
        on!(on_dock_menu_window, |v, row| if let Some(row) = index(row) {
            v.dock_menu_window(row);
        });
        on!(on_dock_menu_new_window, |v| v.dock_menu_new_window());
        on!(on_dock_menu_toggle_pin, |v| v.dock_menu_toggle_pin());
        on!(on_dock_menu_quit, |v| v.dock_menu_quit());
        on!(on_launcher_query_edited, |v, text| v.search(&text));
        on!(on_launcher_activated, |v, row| v.launch_result(row));
        on!(on_window_activated, |v, row| if let Some(id) = v.window_id(row) {
            v.set_overview(false);
            v.model.emit(ShellAction::Compositor(Request::Activate { id }));
        });
        on!(on_window_closed, |v, row| if let Some(id) = v.window_id(row) {
            v.model.emit(ShellAction::Compositor(Request::Close { id }));
        });
        on!(on_window_moved, |v, row, workspace| {
            if let (Some(id), Ok(workspace)) = (v.window_id(row), u32::try_from(workspace)) {
                v.model.emit(ShellAction::Compositor(Request::MoveToWorkspace { id, workspace }));
            }
        });
        on!(on_overview_key_typed, |v, text| {
            if is_search_text(&text) {
                v.set_launcher(true, &text);
                true
            } else {
                false
            }
        });
        on!(on_volume_changed, |v, percent| v.model.update_system(|system| {
            let volume = (percent / 100.0).clamp(0.0, 1.5);
            if let Some(audio) = &mut system.audio {
                audio.volume = volume;
            }
            Some(ServiceCommand::SetVolume(volume))
        }));
        on!(on_mute_toggled, |v| v.model.update_system(|system| {
            if let Some(audio) = &mut system.audio {
                audio.muted = !audio.muted;
            }
            Some(ServiceCommand::ToggleMute)
        }));
        on!(on_brightness_changed, |v, percent| v.model.update_system(|system| {
            let level = (percent / 100.0).clamp(0.01, 1.0);
            system.brightness = system.brightness.map(|_| level);
            Some(ServiceCommand::SetBrightness(level))
        }));
        on!(on_wifi_toggled, |v| v.model.update_system(|system| {
            system.network.wifi_enabled = !system.network.wifi_enabled;
            Some(ServiceCommand::SetWifiEnabled(system.network.wifi_enabled))
        }));
        on!(on_bluetooth_toggled, |v| v.model.update_system(|system| {
            let bluetooth = system.bluetooth.as_mut()?;
            bluetooth.powered = !bluetooth.powered;
            Some(ServiceCommand::SetBluetoothPowered(bluetooth.powered))
        }));
        on!(on_do_not_disturb_toggled, |v, on| v.model.update_system(|system| {
            system.do_not_disturb = on;
            Some(ServiceCommand::SetDoNotDisturb(on))
        }));
        on!(on_dark_style_toggled, |v| v.model.toggle_dark_style());
        on!(on_settings_requested, |v, page| {
            v.set_popup(Popup::None);
            let page = (!page.is_empty()).then(|| page.to_string());
            v.model.emit(ShellAction::OpenSettings(page));
        });
        on!(on_media_previous, |v| v.model.emit(service(ServiceCommand::MediaPrevious)));
        on!(on_media_play_pause, |v| v.model.emit(service(ServiceCommand::MediaPlayPause)));
        on!(on_media_next, |v| v.model.emit(service(ServiceCommand::MediaNext)));
        on!(on_lock_requested, |v| {
            v.set_popup(Popup::None);
            v.model.emit(ShellAction::Compositor(Request::Lock));
        });
        on!(on_suspend_requested, |v| {
            v.set_popup(Popup::None);
            v.model.emit(service(ServiceCommand::Suspend));
        });
        on!(on_power_requested, |v, action| {
            v.set_popup(Popup::None);
            v.ui.set_power_action(action);
            v.ui.invoke_focus_power_dialog();
        });
        on!(on_power_confirmed, |v, action| v.power_confirmed(action));
        on!(on_month_changed, |v, delta| v.model.change_month(delta));
        on!(on_notification_activated, |v, id| if v.model.activate_notification(id as u32) {
            v.set_popup(Popup::None);
        });
        on!(on_notification_dismissed, |v, id| v.model.dismiss_notification(id as u32));
        on!(on_notification_action, |v, id, key| v
            .model
            .invoke_notification_action(id as u32, key.to_string()));
        on!(on_clear_notifications, |v| v.model.clear_notifications());
    }
}

/// Whether a key typed in the overview should start a search: printable text, not a control or function key.
fn is_search_text(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            !c.is_control() && !c.is_whitespace() && !('\u{f700}'..='\u{f8ff}').contains(&c)
        })
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
}
