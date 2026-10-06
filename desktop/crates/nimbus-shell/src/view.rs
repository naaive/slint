// SPDX-License-Identifier: MIT

//! [`ShellView`]: the shell on one output, with what only that output shows,
//! such as its open popup, launcher, and the windows on it, in the windows of its [`Part`]s.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use nimbus_ipc::Request;
use nimbus_services::ServiceCommand;
use slint::{ComponentHandle, Model as _, ModelRc, SharedString, VecModel};

use crate::model::{Model, SharedWindow, State, display_title, sync_rows};
use crate::windows::{DockClick, DockEntry, dock_click};
use crate::{
    AppItem, AuthWindow, Desktop, DockMenuWindow, DockWindow, OsdWindow, OverlayWindow,
    PanelWindow, Popup, PopupWindow, PowerAction, Rect, RectData, ShellAction, ShellModel,
    ShellOutput, ToastWindow, WindowItem, WorkspaceItem,
};

/// The most workspace dots the panel shows.
const MAX_WORKSPACES: u32 = 32;
/// The distance between a panel popup and its button.
const PANEL_POPUP_GAP: f32 = 6.0;
/// The distance between the dock menu and its item.
const DOCK_MENU_GAP: f32 = 8.0;

fn index(row: i32) -> Option<usize> {
    usize::try_from(row).ok()
}

/// A part of the shell on one output, which a host shows on a surface of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    /// The panel along the top or bottom edge.
    Panel,
    /// The dock on the bottom edge.
    Dock,
    /// The open popup, next to the part it belongs to; see [`ShellView::popup_placement`].
    Popup(Popup),
    /// The launcher, the overview, or the power dialog, over the whole output and above fullscreen windows.
    Overlay,
    /// The toasts in the top right corner.
    Toasts,
    /// The on-screen display above the bottom edge.
    Osd,
    /// The polkit authentication dialog, over the whole output and above everything else,
    /// on one output while a request is open.
    Auth,
}

/// The output edges a part is attached to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

/// Where a part other than a popup goes on its output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The edges it's attached to; it's centered along an axis with neither edge.
    pub edges: Edges,
    /// The size in logical pixels; 0 along an axis where it's attached to both edges spans the output.
    pub width: f32,
    pub height: f32,
    /// The distance from the edges it's attached to.
    pub margin: f32,
    /// The space along its edge that windows and later parts stay out of,
    /// or `None` to cover the space that other parts keep.
    pub exclusive_zone: Option<f32>,
    /// It shows above fullscreen windows.
    pub above_fullscreen: bool,
    /// It takes the keyboard while it's shown.
    pub keyboard: bool,
}

/// How a popup lines up with the rectangle it opens next to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Center,
    /// Its right edge lines up with the rectangle's.
    End,
}

/// Where the open popup goes: next to a rectangle of the part it belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupPlacement {
    pub parent: Part,
    /// The rectangle it opens next to, in logical pixels of the parent's window.
    pub anchor: Rect,
    /// It opens below the anchor, or else above it.
    pub below: bool,
    pub align: Align,
    /// The distance between the anchor and the popup.
    pub gap: f32,
}

/// The Slint component of a [`PartWindow`].
pub enum PartComponent {
    Panel(PanelWindow),
    Dock(DockWindow),
    Popup(PopupWindow),
    Overlay(OverlayWindow),
    Toasts(ToastWindow),
    Osd(OsdWindow),
    Auth(AuthWindow),
}

/// Runs `$body` with `$ui` bound to the component of whichever part `$component` is.
macro_rules! each_component {
    ($component:expr, $ui:ident => $body:expr) => {
        match $component {
            PartComponent::Panel($ui) => $body,
            PartComponent::Dock($ui) => $body,
            PartComponent::Popup($ui) => $body,
            PartComponent::Overlay($ui) => $body,
            PartComponent::Toasts($ui) => $body,
            PartComponent::Osd($ui) => $body,
            PartComponent::Auth($ui) => $body,
        }
    };
}

impl Clone for PartComponent {
    fn clone(&self) -> Self {
        match self {
            Self::Panel(ui) => Self::Panel(ui.clone_strong()),
            Self::Dock(ui) => Self::Dock(ui.clone_strong()),
            Self::Popup(ui) => Self::Popup(ui.clone_strong()),
            Self::Overlay(ui) => Self::Overlay(ui.clone_strong()),
            Self::Toasts(ui) => Self::Toasts(ui.clone_strong()),
            Self::Osd(ui) => Self::Osd(ui.clone_strong()),
            Self::Auth(ui) => Self::Auth(ui.clone_strong()),
        }
    }
}

impl PartComponent {
    /// The window's copy of what the parts of its output show alike.
    pub fn output(&self) -> ShellOutput<'_> {
        each_component!(self, ui => ui.global::<ShellOutput>())
    }

    pub fn desktop(&self) -> Desktop<'_> {
        each_component!(self, ui => ui.global::<Desktop>())
    }

    pub(crate) fn shared(&self) -> &dyn SharedWindow {
        each_component!(self, ui => ui)
    }

    pub fn window(&self) -> &slint::Window {
        each_component!(self, ui => ui.window())
    }
}

/// The window of one [`Part`], which shows the [`ShellModel`] and the output's [`ShellView`] it was created by.
/// Dropping it hides the window.
pub struct PartWindow {
    part: Part,
    ui: Rc<PartComponent>,
}

impl PartWindow {
    pub fn part(&self) -> Part {
        self.part
    }

    pub fn window(&self) -> &slint::Window {
        self.ui.window()
    }

    /// The Slint component, for tests and for hosts that need direct access to its properties.
    pub fn component(&self) -> &PartComponent {
        &self.ui
    }

    /// The window's copy of what the parts of its output show alike.
    pub fn output(&self) -> ShellOutput<'_> {
        self.ui.output()
    }

    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.window().show()
    }

    /// The logical size the window's surface should have; 0 along an axis that spans the output.
    pub fn size(&self) -> (f32, f32) {
        match &*self.ui {
            PartComponent::Panel(ui) => (0.0, ui.get_surface_height()),
            PartComponent::Dock(ui) => (ui.get_surface_width(), ui.get_surface_height()),
            PartComponent::Popup(ui) => (ui.get_surface_width(), ui.get_surface_height()),
            PartComponent::Overlay(_) | PartComponent::Auth(_) => (0.0, 0.0),
            PartComponent::Toasts(ui) => (ui.get_surface_width(), ui.get_surface_height()),
            PartComponent::Osd(ui) => (ui.get_surface_width(), ui.get_surface_height()),
        }
    }

    /// Where the part goes on its output, or `None` for a popup.
    pub fn placement(&self) -> Option<Placement> {
        let (width, height) = self.size();
        let top = self.ui.desktop().get_panel().top;
        let placement = |edges, margin, exclusive_zone, above_fullscreen| Placement {
            edges,
            width,
            height,
            margin,
            exclusive_zone,
            above_fullscreen,
            keyboard: false,
        };
        let sides = Edges { left: true, right: true, ..Edges::default() };
        let bottom = Edges { bottom: true, ..Edges::default() };
        Some(match &*self.ui {
            PartComponent::Panel(_) => {
                placement(Edges { top, bottom: !top, ..sides }, 0.0, Some(height), false)
            }
            PartComponent::Dock(ui) => placement(bottom, 0.0, Some(ui.get_exclusive_zone()), false),
            PartComponent::Popup(_) => return None,
            PartComponent::Overlay(_) | PartComponent::Auth(_) => Placement {
                keyboard: true,
                ..placement(Edges { top: true, bottom: true, ..sides }, 0.0, None, true)
            },
            PartComponent::Toasts(_) => placement(
                Edges { top: true, right: true, ..Edges::default() },
                0.0,
                Some(0.0),
                true,
            ),
            PartComponent::Osd(ui) => placement(bottom, ui.get_edge_margin(), Some(0.0), true),
        })
    }

    /// The part itself within its window, without the room around it for shadows.
    pub fn geometry(&self) -> Rect {
        match &*self.ui {
            PartComponent::Popup(ui) => ui.get_geometry().into(),
            _ => self.whole(),
        }
    }

    /// Where the window takes pointer input; elsewhere input reaches what's below.
    pub fn input_region(&self) -> Vec<Rect> {
        let rect = match &*self.ui {
            PartComponent::Panel(ui) => ui.get_input_rect().into(),
            PartComponent::Dock(ui) => ui.get_input_rect().into(),
            PartComponent::Popup(ui) => ui.get_geometry().into(),
            PartComponent::Overlay(_) | PartComponent::Auth(_) => self.whole(),
            PartComponent::Toasts(ui) => ui.get_input_rect().into(),
            PartComponent::Osd(_) => Rect::default(),
        };
        if rect.is_empty() { Vec::new() } else { vec![rect] }
    }

    fn whole(&self) -> Rect {
        let window = self.window();
        let size = window.size().to_logical(window.scale_factor());
        Rect { x: 0.0, y: 0.0, width: size.width, height: size.height }
    }
}

impl Drop for PartWindow {
    fn drop(&mut self) {
        // A shown Slint window keeps its component alive until hidden.
        if let Err(err) = self.window().hide() {
            tracing::warn!("cannot hide a shell window: {err}");
        }
    }
}

/// The shell on one output: the panel, the dock, popups, the launcher and overview, toasts, and the OSD.
///
/// It shows the [`ShellModel`] it was created with, and keeps only what's particular to its output.
/// It has no windows of its own: a host shows each of its [`ShellView::parts`] in a window from [`ShellView::create`].
pub struct ShellView(Rc<View>);

impl ShellView {
    /// Creates the view for the output named `output`.
    pub fn new(model: &ShellModel, output: &str) -> Self {
        let view = Rc::new_cyclic(|this| View {
            this: this.clone(),
            model: model.0.clone(),
            output: output.to_owned(),
            state: RefCell::default(),
            models: Models::default(),
            parts: RefCell::default(),
        });
        model.0.add_view(&view);
        Self(view)
    }

    pub fn output(&self) -> &str {
        &self.0.output
    }

    /// The parts to show now, in the order to create their surfaces, bottom to top.
    pub fn parts(&self) -> Vec<Part> {
        self.0.parts()
    }

    /// Creates the window of `part`; the current Slint platform supplies it.
    pub fn create(&self, part: Part) -> Result<PartWindow, slint::PlatformError> {
        self.0.create(part)
    }

    /// Where the open popup goes.
    pub fn popup_placement(&self) -> Option<PopupPlacement> {
        self.0.state.borrow().placement
    }

    pub fn popup(&self) -> Popup {
        self.0.state.borrow().data.popup
    }

    pub fn launcher_open(&self) -> bool {
        self.0.state.borrow().data.launcher_open
    }

    pub fn overview_open(&self) -> bool {
        self.0.state.borrow().data.overview_open
    }

    pub fn power_action(&self) -> PowerAction {
        self.0.state.borrow().data.power_action
    }

    /// Closes the open popup, as when the host's compositor dismissed it.
    pub fn close_popup(&self) {
        self.0.set_popup(Popup::None, None);
    }

    pub fn toggle_launcher(&self) {
        if !self.0.model.is_locked() {
            self.0.set_launcher(!self.launcher_open(), "");
        }
    }

    pub fn toggle_overview(&self) {
        if !self.0.model.is_locked() {
            self.0.set_overview(!self.overview_open());
        }
    }
}

/// The values of the `ShellOutput` global, which every part of the output holds a copy of.
#[derive(Clone, Default)]
struct OutputData {
    active_count: i32,
    fullscreen_active: bool,
    popup: Popup,
    launcher_open: bool,
    overview_open: bool,
    power_action: PowerAction,
    dock_menu_index: i32,
    dock_menu_title: SharedString,
    dock_menu_favorite: bool,
    dock_menu_running: bool,
}

impl OutputData {
    /// Whether the overlay shows the launcher, the overview, or the power dialog.
    fn overlay_open(&self) -> bool {
        self.launcher_open || self.overview_open || self.power_action != PowerAction::None
    }

    /// Sets every property; Slint ignores the ones that didn't change.
    fn write(&self, output: &ShellOutput, models: &Models) {
        output.set_workspaces(ModelRc::from(models.workspaces.clone()));
        output.set_windows(ModelRc::from(models.windows.clone()));
        output.set_active_count(self.active_count);
        output.set_fullscreen_active(self.fullscreen_active);
        output.set_popup(self.popup);
        output.set_launcher_open(self.launcher_open);
        output.set_launcher_apps(ModelRc::from(models.launcher.clone()));
        output.set_overview_open(self.overview_open);
        output.set_power_action(self.power_action);
        output.set_dock_menu_index(self.dock_menu_index);
        output.set_dock_menu_title(self.dock_menu_title.clone());
        output.set_dock_menu_windows(ModelRc::from(models.dock_menu.clone()));
        output.set_dock_menu_favorite(self.dock_menu_favorite);
        output.set_dock_menu_running(self.dock_menu_running);
    }
}

#[derive(Default)]
struct ViewState {
    data: OutputData,
    placement: Option<PopupPlacement>,
    /// The dock entry whose menu is open.
    dock_menu: Option<String>,
    launcher_query: String,
    launcher_ids: Vec<String>,
}

#[derive(Default)]
struct Models {
    workspaces: Rc<VecModel<WorkspaceItem>>,
    windows: Rc<VecModel<WindowItem>>,
    dock_menu: Rc<VecModel<DockMenuWindow>>,
    launcher: Rc<VecModel<AppItem>>,
}

pub struct View {
    this: Weak<Self>,
    model: Rc<Model>,
    output: String,
    state: RefCell<ViewState>,
    models: Models,
    parts: RefCell<Vec<(Part, Weak<PartComponent>)>>,
}

impl View {
    /// Whether `output` is a window's output as seen from this view; an empty name matches any.
    fn shows(&self, output: &str) -> bool {
        self.output.is_empty() || output.is_empty() || output == self.output
    }

    pub fn popup(&self) -> Popup {
        self.state.borrow().data.popup
    }

    pub fn output(&self) -> &str {
        &self.output
    }

    /// The live windows of this view's parts.
    fn windows(&self) -> Vec<(Part, Rc<PartComponent>)> {
        let mut parts = self.parts.borrow_mut();
        parts.retain(|(_, ui)| ui.strong_count() > 0);
        parts.iter().filter_map(|(part, ui)| Some((*part, ui.upgrade()?))).collect()
    }

    fn overlay(&self) -> Option<OverlayWindow> {
        self.windows().into_iter().find_map(|(_, ui)| match &*ui {
            PartComponent::Overlay(overlay) => Some(overlay.clone_strong()),
            _ => None,
        })
    }

    pub fn auth_window(&self) -> Option<AuthWindow> {
        self.windows().into_iter().find_map(|(_, ui)| match &*ui {
            PartComponent::Auth(auth) => Some(auth.clone_strong()),
            _ => None,
        })
    }

    /// Shows the shared state on every window of this view.
    pub fn show_shared(&self, state: &State, theme: bool) {
        for (_, ui) in self.windows() {
            self.model.show_shared_on(ui.shared(), state, theme);
        }
    }

    fn parts(&self) -> Vec<Part> {
        self.close_orphaned_popup();
        let state = self.state.borrow();
        let data = &state.data;
        let model = &self.model;
        let locked = model.is_locked();
        let auth = model.shows_auth(&self.output);
        let mut parts: Vec<Part> = [Part::Panel, Part::Dock, Part::Overlay]
            .into_iter()
            .filter(|&p| self.shown(p))
            .collect();
        if data.popup != Popup::None {
            parts.push(Part::Popup(data.popup));
        }
        if !locked
            && data.popup == Popup::None
            && data.power_action == PowerAction::None
            && !auth
            && model.has_toasts()
        {
            parts.push(Part::Toasts);
        }
        let osd_fading = self.windows().iter().any(|(_, ui)| match &**ui {
            PartComponent::Osd(osd) => osd.get_showing(),
            _ => false,
        });
        if (model.osd_shown() && !locked) || osd_fading {
            parts.push(Part::Osd);
        }
        if auth {
            parts.push(Part::Auth);
        }
        parts
    }

    fn create(&self, part: Part) -> Result<PartWindow, slint::PlatformError> {
        let ui = Rc::new(match part {
            Part::Panel => PartComponent::Panel(PanelWindow::new()?),
            Part::Dock => PartComponent::Dock(DockWindow::new()?),
            Part::Popup(kind) => {
                let popup = PopupWindow::new()?;
                popup.set_kind(kind);
                PartComponent::Popup(popup)
            }
            Part::Overlay => {
                let overlay = OverlayWindow::new()?;
                overlay.set_launcher_query(self.state.borrow().launcher_query.as_str().into());
                PartComponent::Overlay(overlay)
            }
            Part::Toasts => PartComponent::Toasts(ToastWindow::new()?),
            Part::Osd => PartComponent::Osd(OsdWindow::new()?),
            Part::Auth => PartComponent::Auth(AuthWindow::new()?),
        });
        self.model.show_shared_on(ui.shared(), &self.model.state.borrow(), true);
        let output = ui.output();
        self.state.borrow().data.write(&output, &self.models);
        self.connect(part, &output);
        self.parts.borrow_mut().push((part, Rc::downgrade(&ui)));
        match &*ui {
            PartComponent::Overlay(overlay) => self.focus_overlay(overlay),
            PartComponent::Auth(auth) => auth.invoke_focus_response(),
            _ => {}
        }
        Ok(PartWindow { part, ui })
    }

    /// Changes what the parts of this output show alike, then shows it on each.
    fn update(&self, change: impl FnOnce(&mut OutputData)) {
        let data = {
            let mut state = self.state.borrow_mut();
            change(&mut state.data);
            state.data.clone()
        };
        for (_, ui) in self.windows() {
            data.write(&ui.output(), &self.models);
        }
    }

    /// Brings the windows, workspaces, and dock menu of this output up to date with `state`.
    pub fn refresh(&self, state: &State) {
        let slots = state.windows.slots(&self.output);
        let mut rows = Vec::with_capacity(slots.len());
        let windows = state.windows.list().iter().zip(&state.window_rows);
        for ((window, row), (slot, count, on_output)) in windows.zip(slots) {
            rows.push(WindowItem {
                title: row.title.clone(),
                app_name: row.app_name.clone(),
                visual: row.visual.clone(),
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

        let fullscreen_active = state.windows.focused().is_some_and(|window| {
            window.fullscreen
                && self.shows(&window.output)
                && window.workspace == state.active_workspace
        });
        self.update(|data| {
            data.active_count = active_count as i32;
            data.fullscreen_active = fullscreen_active;
        });
        self.fill_dock_menu(state);
    }

    pub fn window_removed(&self, row: usize) {
        if row < self.models.windows.row_count() {
            self.models.windows.remove(row);
        }
    }

    fn fill_dock_menu(&self, state: &State) {
        let Some(id) = self.state.borrow().dock_menu.clone() else {
            return;
        };
        let Some((index, entry)) = state.dock.iter().enumerate().find(|(_, e)| e.id == id) else {
            sync_rows(&self.models.dock_menu, Vec::new());
            self.update(|data| data.dock_menu_running = false);
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
        self.update(|data| {
            data.dock_menu_index = index as i32;
            data.dock_menu_title = entry.name.as_str().into();
            data.dock_menu_favorite = entry.favorite;
            data.dock_menu_running = !entry.windows.is_empty();
        });
    }

    // The dock.

    fn dock_clicked(&self, row: usize) {
        let click = {
            let state = self.model.state.borrow();
            state.dock.get(row).and_then(|entry| dock_click(entry, &state.windows))
        };
        self.set_popup(Popup::None, None);
        let Some(click) = click else {
            return;
        };
        self.set_overview(false);
        self.model.emit(match click {
            DockClick::Launch(id) => ShellAction::Launch(id),
            DockClick::Compositor(request) => ShellAction::Compositor(request),
        });
    }

    fn open_dock_menu(&self, row: usize, parent: Part, anchor: Rect) {
        {
            let state = self.model.state.borrow();
            let Some(entry) = state.dock.get(row) else {
                return;
            };
            self.state.borrow_mut().dock_menu = Some(entry.id.clone());
            self.fill_dock_menu(&state);
        }
        let placement = PopupPlacement {
            parent,
            anchor,
            below: false,
            align: Align::Center,
            gap: DOCK_MENU_GAP,
        };
        self.set_popup(Popup::DockMenu, Some(placement));
    }

    fn dock_menu_entry(&self) -> Option<DockEntry> {
        let id = self.state.borrow().dock_menu.clone()?;
        self.model.state.borrow().dock.iter().find(|e| e.id == id).cloned()
    }

    fn dock_menu_window(&self, row: usize) {
        let id = self.dock_menu_entry().and_then(|entry| entry.windows.get(row).copied());
        self.set_popup(Popup::None, None);
        self.set_overview(false);
        if let Some(id) = id {
            self.model.emit(ShellAction::Compositor(Request::Activate { id }));
        }
    }

    fn dock_menu_new_window(&self) {
        let entry = self.dock_menu_entry();
        self.set_popup(Popup::None, None);
        if let Some(entry) = entry.filter(|e| e.launchable) {
            self.set_overview(false);
            self.model.emit(ShellAction::Launch(entry.id));
        }
    }

    fn dock_menu_toggle_pin(&self) {
        let Some(entry) = self.dock_menu_entry() else {
            self.set_popup(Popup::None, None);
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
        self.set_popup(Popup::None, None);
        self.model.refresh_windows();
    }

    fn dock_menu_quit(&self) {
        let entry = self.dock_menu_entry();
        self.set_popup(Popup::None, None);
        for id in entry.map(|e| e.windows).unwrap_or_default() {
            self.model.emit(ShellAction::Compositor(Request::Close { id }));
        }
    }

    fn window_id(&self, row: i32) -> Option<nimbus_ipc::WindowId> {
        self.model.state.borrow().windows.get(index(row)?).map(|w| w.id)
    }

    // Launcher, overview, and popups.

    /// Focuses what the overlay shows: the power dialog, the launcher, or the overview.
    fn focus_overlay(&self, overlay: &OverlayWindow) {
        let (data, query_len) = {
            let state = self.state.borrow();
            (state.data.clone(), state.launcher_query.len())
        };
        if data.power_action != PowerAction::None {
            overlay.invoke_focus_power_dialog();
        } else if data.launcher_open {
            overlay.invoke_focus_launcher_at(query_len.try_into().unwrap_or(i32::MAX));
        } else if data.overview_open {
            overlay.invoke_focus_overview();
        }
    }

    fn set_launcher(&self, open: bool, query: &str) {
        if open {
            self.set_overview(false);
            self.set_popup(Popup::None, None);
            self.state.borrow_mut().launcher_query = query.to_owned();
            self.search(query);
            self.update(|data| data.launcher_open = true);
            if let Some(overlay) = self.overlay() {
                overlay.set_launcher_query(query.into());
                self.focus_overlay(&overlay);
            }
        } else {
            self.update(|data| data.launcher_open = false);
            self.close_orphaned_popup();
        }
    }

    /// Searches again for the launcher's query, for example after the applications changed.
    pub fn refresh_search(&self) {
        if self.state.borrow().data.launcher_open {
            let query = self.state.borrow().launcher_query.clone();
            self.search(&query);
        }
    }

    fn search(&self, query: &str) {
        let apps = {
            let mut state = self.model.state.borrow_mut();
            let state = &mut *state;
            let results: Vec<_> = state.apps.search(query).into_iter().cloned().collect();
            let mut view = self.state.borrow_mut();
            view.launcher_query = query.to_owned();
            view.launcher_ids = results.iter().map(|e| e.id.clone()).collect();
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
        if let Some(overlay) = self.overlay() {
            overlay.set_launcher_selected(0);
        }
    }

    fn launch_result(&self, row: i32) {
        let id = index(row).and_then(|row| self.state.borrow().launcher_ids.get(row).cloned());
        if let Some(id) = id {
            self.set_launcher(false, "");
            self.model.emit(ShellAction::Launch(id));
        }
    }

    fn set_overview(&self, open: bool) {
        if open == self.state.borrow().data.overview_open {
            return;
        }
        if open {
            self.set_popup(Popup::None, None);
            self.update(|data| {
                data.launcher_open = false;
                data.overview_open = true;
            });
            if let Some(overlay) = self.overlay() {
                self.focus_overlay(&overlay);
            }
        } else {
            self.update(|data| data.overview_open = false);
            self.close_orphaned_popup();
        }
    }

    /// Whether `part`, one a popup can open from, is shown.
    fn shown(&self, part: Part) -> bool {
        match part {
            Part::Panel => true,
            Part::Dock => self.model.shows_dock(),
            Part::Overlay => self.state.borrow().data.overlay_open(),
            _ => false,
        }
    }

    /// Closes the popup once the part it opened from is no longer shown.
    fn close_orphaned_popup(&self) {
        let orphaned = self.state.borrow().placement.is_some_and(|p| !self.shown(p.parent));
        if orphaned {
            self.set_popup(Popup::None, None);
        }
    }

    fn set_popup(&self, popup: Popup, placement: Option<PopupPlacement>) {
        {
            let mut state = self.state.borrow_mut();
            if popup != Popup::DockMenu {
                state.dock_menu = None;
            }
            state.placement = placement.filter(|_| popup != Popup::None);
        }
        self.update(|data| data.popup = popup);
        if popup == Popup::Calendar {
            self.model.mark_read();
            self.model.change_month(0);
        }
    }

    /// Opens or closes a popup of the panel, which `parent` shows, next to `anchor`.
    fn request_popup(&self, popup: Popup, parent: Part, anchor: Rect) {
        let placement = PopupPlacement {
            parent,
            anchor,
            below: self.model.state.borrow().config.panel.position
                == nimbus_config::PanelPosition::Top,
            align: if popup == Popup::QuickSettings { Align::End } else { Align::Center },
            gap: PANEL_POPUP_GAP,
        };
        self.set_popup(popup, Some(placement));
    }

    pub fn close_everything(&self) {
        self.set_popup(Popup::None, None);
        self.update(|data| {
            data.launcher_open = false;
            data.overview_open = false;
            data.power_action = PowerAction::None;
        });
    }

    fn power_requested(&self, action: PowerAction) {
        self.set_popup(Popup::None, None);
        self.update(|data| data.power_action = action);
        if let Some(overlay) = self.overlay() {
            self.focus_overlay(&overlay);
        }
    }

    fn power_confirmed(&self, action: PowerAction) {
        self.update(|data| data.power_action = PowerAction::None);
        self.close_orphaned_popup();
        let command = match action {
            PowerAction::Restart => ServiceCommand::Reboot,
            PowerAction::PowerOff => ServiceCommand::PowerOff,
            PowerAction::LogOut => ServiceCommand::Logout,
            PowerAction::None => return,
        };
        self.model.emit(ShellAction::Service(command));
    }

    // Wiring.

    /// Handles the callbacks of the `ShellOutput` global of a window of `part`.
    fn connect(&self, part: Part, output: &ShellOutput) {
        macro_rules! on {
            ($setter:ident, |$v:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = self.this.clone();
                output.$setter(move |$($arg),*| {
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
        on!(on_popup_requested, |v, popup, anchor| v.request_popup(
            popup,
            part,
            Rect::from(anchor)
        ));
        on!(on_workspace_clicked, |v, workspace| {
            if let Ok(workspace) = u32::try_from(workspace) {
                v.model.emit(ShellAction::Compositor(Request::SwitchWorkspace { workspace }));
            }
        });
        on!(on_dock_clicked, |v, row| if let Some(row) = index(row) {
            v.dock_clicked(row);
        });
        on!(on_dock_menu_requested, |v, row, anchor| if let Some(row) = index(row) {
            v.open_dock_menu(row, part, Rect::from(anchor));
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
            v.set_popup(Popup::None, None);
            let page = (!page.is_empty()).then(|| page.to_string());
            v.model.emit(ShellAction::OpenSettings(page));
        });
        on!(on_media_previous, |v| v.model.emit(service(ServiceCommand::MediaPrevious)));
        on!(on_media_play_pause, |v| v.model.emit(service(ServiceCommand::MediaPlayPause)));
        on!(on_media_next, |v| v.model.emit(service(ServiceCommand::MediaNext)));
        on!(on_lock_requested, |v| {
            v.set_popup(Popup::None, None);
            v.model.emit(ShellAction::Compositor(Request::Lock));
        });
        on!(on_suspend_requested, |v| {
            v.set_popup(Popup::None, None);
            v.model.emit(service(ServiceCommand::Suspend));
        });
        on!(on_power_requested, |v, action| v.power_requested(action));
        on!(on_power_confirmed, |v, action| v.power_confirmed(action));
        on!(on_month_changed, |v, delta| v.model.change_month(delta));
        on!(on_notification_activated, |v, id| if v.model.activate_notification(id as u32) {
            v.set_popup(Popup::None, None);
        });
        on!(on_notification_dismissed, |v, id| v.model.dismiss_notification(id as u32));
        on!(on_notification_action, |v, id, key| v
            .model
            .invoke_notification_action(id as u32, key.to_string()));
        on!(on_clear_notifications, |v| v.model.clear_notifications());
        on!(on_auth_responded, |v, response| v.model.auth_responded(response.into()));
        on!(on_auth_cancelled, |v| v.model.auth_cancelled());
        on!(on_auth_identity_selected, |v, identity| if let Some(identity) = index(identity) {
            v.model.auth_identity_selected(identity);
        });
    }
}

impl From<RectData> for Rect {
    fn from(rect: RectData) -> Self {
        Self { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
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
