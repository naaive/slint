// SPDX-License-Identifier: MIT

//! Window management: workspaces, stacking, focus, floating placement, tiling, and window states.
//!
//! Workspaces are global and every output shows the active one.
//! Each window belongs to one workspace and one output.
//! The [`Space`] only holds the windows that are currently visible, in stacking order.

pub mod floating;
pub mod focus;
pub mod grabs;
pub mod layout;
pub mod tiling;
pub mod workspace;

use focus::FocusStack;
use layout::{Layout, Rect};
use nimbus_ipc::{Direction, Event, LayoutMode, WindowId, WindowInfo, WorkspaceId};
use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use tiling::MasterStack;
use workspace::Workspaces;

/// One output's place in the global coordinate space and the part windows may cover.
#[derive(Clone, Debug)]
pub struct OutputArea {
    pub name: String,
    pub geometry: Rect,
    /// The geometry minus the shell's and layer-shell surfaces' exclusive zones.
    pub usable: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Normal,
    Maximized,
    Fullscreen,
}

/// An interactive resize in progress; see [`grabs::ResizeGrab`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeState {
    pub edges: xdg_toplevel::ResizeEdge,
    pub initial: Rect,
    /// Set when the grab ended and the window's last resize commit is still outstanding.
    pub finishing: bool,
}

pub struct ManagedWindow {
    pub id: WindowId,
    pub window: Window,
    pub workspace: WorkspaceId,
    pub output: Option<String>,
    /// The client made its initial commit, so it may be configured.
    pub initial_commit: bool,
    /// The client attached a buffer; only mapped windows are shown and reported.
    pub mapped: bool,
    pub minimized: bool,
    pub mode: WindowMode,
    /// Where the window sits while floating, also restored after maximize, fullscreen, and tiling.
    pub floating: Option<Rect>,
    /// The user dragged the window out of the tiling layout.
    pub user_floating: bool,
    /// A size to request once when the window returns to floating; `Some(None)` lets the client choose.
    restore_size: Option<Option<Size<i32, Logical>>>,
    pub resize: Option<ResizeState>,
    pub app_id: String,
    pub title: String,
    pub foreign: Option<ForeignToplevelHandle>,
}

impl ManagedWindow {
    pub fn toplevel(&self) -> Option<&ToplevelSurface> {
        self.window.toplevel()
    }

    pub fn wl_surface(&self) -> Option<WlSurface> {
        self.window.wl_surface().map(|s| s.into_owned())
    }

    /// Dialogs and fixed-size windows never tile.
    fn prefers_floating(&self) -> bool {
        let Some(toplevel) = self.toplevel() else {
            return true;
        };
        if toplevel.parent().is_some() {
            return true;
        }
        with_states(toplevel.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            current.min_size.w > 0 && current.min_size == current.max_size
        })
    }

    pub fn floats(&self, layout: LayoutMode) -> bool {
        layout == LayoutMode::Floating || self.user_floating || self.prefers_floating()
    }

    fn info(&self, focused: bool) -> WindowInfo {
        WindowInfo {
            id: self.id,
            app_id: self.app_id.clone(),
            title: self.title.clone(),
            workspace: self.workspace,
            output: self.output.clone().unwrap_or_default(),
            focused,
            minimized: self.minimized,
            maximized: self.mode == WindowMode::Maximized,
            fullscreen: self.mode == WindowMode::Fullscreen,
        }
    }
}

/// Reads the toplevel's `(app_id, title)`.
pub fn toplevel_metadata(toplevel: &ToplevelSurface) -> (String, String) {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .map(|data| {
                (data.app_id.clone().unwrap_or_default(), data.title.clone().unwrap_or_default())
            })
            .unwrap_or_default()
    })
}

/// Whether the client still has to acknowledge a configure we sent.
pub fn configure_pending(toplevel: &ToplevelSurface) -> bool {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok())
            .is_some_and(|data| !data.pending_configures().is_empty())
    })
}

/// The window manager's state.
pub struct Wm {
    pub space: Space<Window>,
    /// Creation order, which is also the tiling order.
    windows: Vec<ManagedWindow>,
    /// Bottom to top.
    stack: Vec<WindowId>,
    next_id: WindowId,
    focus_stack: FocusStack,
    focused: Option<WindowId>,
    workspaces: Workspaces,
    layout: LayoutMode,
    tiling: MasterStack,
    gaps: i32,
    events: Vec<Event>,
}

impl Wm {
    pub fn new(config: &nimbus_config::Workspaces) -> Self {
        Self {
            space: Space::default(),
            windows: Vec::new(),
            stack: Vec::new(),
            next_id: 1,
            focus_stack: FocusStack::default(),
            focused: None,
            workspaces: Workspaces::new(config.count),
            layout: layout_from_config(config.layout),
            tiling: MasterStack::default(),
            gaps: clamp_gaps(config.gaps),
            events: Vec::new(),
        }
    }

    pub fn workspaces(&self) -> &Workspaces {
        &self.workspaces
    }

    pub fn layout(&self) -> LayoutMode {
        self.layout
    }

    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }

    pub fn focused_window(&self) -> Option<&ManagedWindow> {
        self.focused.and_then(|id| self.get(id))
    }

    /// Events this module produces directly; window changes are found by diffing [`Wm::infos`].
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn get(&self, id: WindowId) -> Option<&ManagedWindow> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut ManagedWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    pub fn find_surface(&self, surface: &WlSurface) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .map(|w| w.id)
    }

    pub fn find_window(&self, window: &Window) -> Option<WindowId> {
        self.windows.iter().find(|w| &w.window == window).map(|w| w.id)
    }

    /// Whether the window is shown: mapped, not minimized, and on the active workspace.
    pub fn is_visible(&self, id: WindowId) -> bool {
        self.get(id).is_some_and(|w| self.visible(w))
    }

    fn visible(&self, w: &ManagedWindow) -> bool {
        w.mapped && !w.minimized && w.workspace == self.workspaces.active()
    }

    /// Visible windows, bottom to top.
    pub fn visible_stack(&self) -> impl DoubleEndedIterator<Item = &ManagedWindow> {
        self.stack.iter().filter_map(|&id| self.get(id)).filter(|w| self.visible(w))
    }

    /// The fullscreen window shown on `output`, if any.
    pub fn fullscreen_on(&self, output: &str) -> Option<&ManagedWindow> {
        self.visible_stack()
            .rev()
            .find(|w| w.mode == WindowMode::Fullscreen && w.output.as_deref() == Some(output))
    }

    pub fn infos(&self) -> Vec<WindowInfo> {
        self.windows
            .iter()
            .filter(|w| w.mapped)
            .map(|w| w.info(self.focused == Some(w.id)))
            .collect()
    }

    /// Starts managing a new toplevel; it's shown once it commits a buffer.
    pub fn add(&mut self, window: Window) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;
        let (app_id, title) = window.toplevel().map(toplevel_metadata).unwrap_or_default();
        self.windows.push(ManagedWindow {
            id,
            window,
            workspace: self.workspaces.active(),
            output: None,
            initial_commit: false,
            mapped: false,
            minimized: false,
            mode: WindowMode::Normal,
            floating: None,
            user_floating: false,
            restore_size: None,
            resize: None,
            app_id,
            title,
            foreign: None,
        });
        self.stack.push(id);
        id
    }

    /// Stops managing a window and moves focus to the previous window if it was focused.
    pub fn remove(&mut self, id: WindowId) -> Option<ManagedWindow> {
        let index = self.windows.iter().position(|w| w.id == id)?;
        let removed = self.windows.remove(index);
        self.space.unmap_elem(&removed.window);
        self.stack.retain(|&w| w != id);
        self.focus_stack.remove(id);
        if self.focused == Some(id) {
            self.focused = None;
            self.focus_fallback();
        }
        Some(removed)
    }

    /// Records the client's initial commit on `output`, so the next arrange configures it.
    pub fn initial_commit(&mut self, id: WindowId, output: Option<String>) {
        let active = self.workspaces.active();
        if let Some(w) = self.get_mut(id) {
            w.initial_commit = true;
            w.workspace = active;
            if w.output.is_none() {
                w.output = output;
            }
        }
    }

    /// Shows a window after its first buffer: places it when floating and focuses it.
    pub fn map(&mut self, id: WindowId, areas: &[OutputArea]) {
        let layout = self.layout;
        let Some(index) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        let parent_rect = self.windows[index]
            .toplevel()
            .and_then(|t| t.parent())
            .and_then(|parent| self.find_surface(&parent))
            .and_then(|pid| self.get(pid))
            .filter(|p| self.visible(p))
            .and_then(|p| self.space.element_geometry(&p.window));
        let output = self.windows[index].output.clone();
        let Some(area) = area_for(areas, output.as_deref()) else {
            self.windows[index].mapped = true;
            return;
        };
        let occupied: Vec<Point<i32, Logical>> = self
            .windows
            .iter()
            .filter(|o| {
                o.id != id && self.visible(o) && o.output.as_deref() == Some(area.name.as_str())
            })
            .filter_map(|o| self.space.element_location(&o.window))
            .collect();

        let w = &mut self.windows[index];
        w.mapped = true;
        w.output = Some(area.name.clone());
        if w.floating.is_none() && w.floats(layout) {
            let size = w.window.geometry().size;
            let clamped = floating::clamp_size(size, area.usable);
            let location = match parent_rect {
                Some(parent) => floating::clamp_location(
                    Rect::new(
                        (
                            parent.loc.x + (parent.size.w - clamped.w) / 2,
                            parent.loc.y + (parent.size.h - clamped.h) / 2,
                        )
                            .into(),
                        clamped,
                    ),
                    area.usable,
                ),
                None => floating::place(clamped, area.usable, &occupied),
            };
            if clamped != size && size.w > 0 && size.h > 0 {
                w.restore_size = Some(Some(clamped));
            }
            w.floating = Some(Rect::new(location, clamped));
        }
        if self.windows[index].workspace == self.workspaces.active() {
            self.focus(Some(id));
        }
    }

    /// Hides a window whose client attached a null buffer; it maps again with its next buffer.
    pub fn unmap(&mut self, id: WindowId) {
        let Some(w) = self.get_mut(id) else {
            return;
        };
        w.mapped = false;
        w.initial_commit = false;
        let window = w.window.clone();
        self.space.unmap_elem(&window);
        if self.focused == Some(id) {
            self.focused = None;
            self.focus_fallback();
        }
    }

    /// Records a floating window's own size changes, once it acknowledged all our configures.
    pub fn track_floating_size(&mut self, id: WindowId) {
        let layout = self.layout;
        if let Some(w) = self.get_mut(id) {
            if w.mode != WindowMode::Normal || !w.floats(layout) || w.resize.is_some() {
                return;
            }
            let pending = w.toplevel().is_some_and(configure_pending);
            let size = w.window.geometry().size;
            if let Some(rect) = w.floating.as_mut().filter(|_| !pending && size.w > 0 && size.h > 0)
            {
                rect.size = size;
            }
        }
    }

    /// Sets keyboard focus to `id`, or clears it, raising the window.
    pub fn focus(&mut self, id: Option<WindowId>) {
        self.focus_with(id, true);
    }

    pub fn focus_with(&mut self, id: Option<WindowId>, raise: bool) {
        match id {
            Some(id) if self.is_visible(id) => {
                self.focused = Some(id);
                self.focus_stack.touch(id);
                if raise {
                    self.raise(id);
                }
            }
            Some(_) => {}
            None => self.focused = None,
        }
    }

    /// Focuses the most recently focused visible window.
    pub fn focus_fallback(&mut self) {
        let next = self.focus_stack.recent().find(|&id| self.is_visible(id));
        let next = next.or_else(|| self.visible_stack().last().map(|w| w.id));
        self.focused = None;
        if let Some(id) = next {
            self.focus(Some(id));
        }
    }

    pub fn raise(&mut self, id: WindowId) {
        if self.stack.last() != Some(&id) && self.get(id).is_some() {
            self.stack.retain(|&w| w != id);
            self.stack.push(id);
        }
        if let Some(window) = self.get(id).map(|w| w.window.clone()) {
            self.space.raise_element(&window, false);
        }
    }

    /// Unminimizes, switches to its workspace, focuses, and raises.
    pub fn activate(&mut self, id: WindowId) -> bool {
        let Some(w) = self.get_mut(id) else {
            return false;
        };
        w.minimized = false;
        let workspace = w.workspace;
        self.switch_workspace(workspace);
        self.focus(Some(id));
        true
    }

    pub fn set_minimized(&mut self, id: WindowId, minimized: bool) -> bool {
        let Some(w) = self.get_mut(id) else {
            return false;
        };
        w.minimized = minimized;
        if minimized {
            if self.focused == Some(id) {
                self.focused = None;
                self.focus_fallback();
            }
        } else if self.is_visible(id) {
            self.focus(Some(id));
        }
        true
    }

    pub fn set_mode(&mut self, id: WindowId, mode: WindowMode) -> bool {
        let layout = self.layout;
        let Some(w) = self.get_mut(id) else {
            return false;
        };
        if w.mode == mode {
            return true;
        }
        let was = std::mem::replace(&mut w.mode, mode);
        if mode == WindowMode::Normal && was != WindowMode::Normal && w.floats(layout) {
            w.restore_size = Some(w.floating.map(|r| r.size));
        }
        if mode == WindowMode::Fullscreen {
            self.raise(id);
        }
        true
    }

    pub fn toggle_mode(&mut self, id: WindowId, mode: WindowMode) -> bool {
        let current = self.get(id).map(|w| w.mode);
        match current {
            Some(m) if m == mode => self.set_mode(id, WindowMode::Normal),
            Some(_) => self.set_mode(id, mode),
            None => false,
        }
    }

    pub fn move_to_workspace(&mut self, id: WindowId, workspace: WorkspaceId) -> bool {
        if !self.workspaces.contains(workspace) {
            return false;
        }
        let Some(w) = self.get_mut(id) else {
            return false;
        };
        w.workspace = workspace;
        if !self.is_visible(id) && self.focused == Some(id) {
            self.focused = None;
            self.focus_fallback();
        }
        true
    }

    pub fn switch_workspace(&mut self, workspace: WorkspaceId) -> bool {
        if !self.workspaces.contains(workspace) {
            return false;
        }
        if self.workspaces.activate(workspace) {
            self.events.push(Event::WorkspaceActivated { workspace });
            self.focused = None;
            self.focus_fallback();
        }
        true
    }

    pub fn set_layout(&mut self, layout: LayoutMode) {
        if self.layout != layout {
            self.layout = layout;
            for w in &mut self.windows {
                w.user_floating = false;
                if layout == LayoutMode::Floating && w.mode == WindowMode::Normal {
                    w.restore_size = Some(w.floating.map(|r| r.size));
                }
            }
            self.events.push(Event::LayoutChanged { layout });
        }
    }

    pub fn set_workspace_count(&mut self, count: u32) {
        let before = self.workspaces.active();
        self.workspaces.set_count(count);
        for w in &mut self.windows {
            w.workspace = self.workspaces.clamp(w.workspace);
        }
        if self.workspaces.active() != before {
            let workspace = self.workspaces.active();
            self.events.push(Event::WorkspaceActivated { workspace });
            self.focus_fallback();
        }
    }

    pub fn set_gaps(&mut self, gaps: u32) {
        self.gaps = clamp_gaps(gaps);
    }

    /// Focuses the nearest visible window in `direction` from the focused one.
    pub fn focus_direction(&mut self, direction: Direction) -> bool {
        let Some(from) = self.focused_window().and_then(|w| self.space.element_geometry(&w.window))
        else {
            return false;
        };
        let focused = self.focused;
        let candidates: Vec<(WindowId, Rect)> = self
            .visible_stack()
            .filter(|w| Some(w.id) != focused)
            .filter_map(|w| self.space.element_geometry(&w.window).map(|g| (w.id, g)))
            .collect();
        match focus::pick_in_direction(from, candidates, direction) {
            Some(id) => {
                self.focus(Some(id));
                true
            }
            None => false,
        }
    }

    /// Moves a floating window during an interactive move.
    pub fn move_floating(&mut self, id: WindowId, location: Point<i32, Logical>) {
        let Some(w) = self.get_mut(id) else {
            return;
        };
        let size = w.window.geometry().size;
        w.floating = Some(Rect::new(location, size));
        let window = w.window.clone();
        if self.space.element_location(&window).is_some() {
            self.space.map_element(window, location, false);
        }
    }

    /// Prepares a window for an interactive move or resize: leaves maximize and tiling, keeping its current geometry.
    /// Returns the geometry the grab starts from.
    pub fn detach_for_grab(&mut self, id: WindowId, pointer: Point<f64, Logical>) -> Option<Rect> {
        let layout = self.layout;
        let current = self.get(id).and_then(|w| self.space.element_geometry(&w.window))?;
        let w = self.get_mut(id)?;
        match w.mode {
            WindowMode::Fullscreen => None,
            WindowMode::Maximized => {
                // Keep the pointer at the same relative position across the restored width.
                let restored = w.floating.map_or(current.size, |r| r.size);
                let rel_x =
                    (pointer.x - f64::from(current.loc.x)) / f64::from(current.size.w.max(1));
                let x = pointer.x - rel_x * f64::from(restored.w);
                let rect = Rect::new((x.round() as i32, current.loc.y).into(), restored);
                w.mode = WindowMode::Normal;
                w.floating = Some(rect);
                w.restore_size = Some(Some(restored));
                if !w.floats(layout) {
                    w.user_floating = true;
                }
                Some(rect)
            }
            WindowMode::Normal => {
                if !w.floats(layout) {
                    w.user_floating = true;
                    w.floating = Some(current);
                    w.restore_size = Some(Some(current.size));
                }
                Some(w.floating.unwrap_or(current))
            }
        }
    }

    /// Updates a window's output to the one containing its center, after a move.
    pub fn update_output(&mut self, id: WindowId, areas: &[OutputArea]) {
        let Some(geo) = self.get(id).and_then(|w| self.space.element_geometry(&w.window)) else {
            return;
        };
        let (cx, cy) = layout::center(geo);
        let point = Point::from((cx.round() as i32, cy.round() as i32));
        if let Some(area) = areas.iter().find(|a| a.geometry.contains(point))
            && let Some(w) = self.get_mut(id)
        {
            w.output = Some(area.name.clone());
        }
    }

    /// Moves windows off outputs that no longer exist, and keeps floating windows on their output
    /// after it moved by the offset in `moved` or changed size.
    pub fn reassign_outputs(
        &mut self,
        areas: &[OutputArea],
        moved: &[(String, Point<i32, Logical>)],
    ) {
        let Some(first) = areas.first() else {
            return;
        };
        for w in &mut self.windows {
            let area = w.output.as_deref().and_then(|name| areas.iter().find(|a| a.name == name));
            let delta = w
                .output
                .as_deref()
                .and_then(|name| moved.iter().find(|(n, _)| n == name))
                .map_or_else(Point::default, |(_, delta)| *delta);
            let area = match area {
                Some(area) => area,
                None if w.initial_commit => {
                    w.output = Some(first.name.clone());
                    first
                }
                None => continue,
            };
            if let Some(rect) = w.floating.as_mut() {
                *rect = floating::refit(*rect, delta, area.usable);
            }
        }
    }

    /// Computes every window's geometry and state, sends configures, and updates the space.
    pub fn arrange(&mut self, areas: &[OutputArea]) {
        let active = self.workspaces.active();
        let layout = self.layout;
        let focused = self.focused;
        let mut locations: Vec<(WindowId, Point<i32, Logical>)> = Vec::new();

        for area in areas {
            let members: Vec<usize> = self
                .windows
                .iter()
                .enumerate()
                .filter(|(_, w)| {
                    w.initial_commit
                        && !w.minimized
                        && w.workspace == active
                        && area_for(areas, w.output.as_deref()).is_some_and(|a| a.name == area.name)
                })
                .map(|(i, _)| i)
                .collect();
            let tiled: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| {
                    self.windows[i].mode == WindowMode::Normal && !self.windows[i].floats(layout)
                })
                .collect();
            let tiles = self.tiling.arrange(area.usable, tiled.len(), self.gaps);

            for &i in &members {
                let tile = tiled.iter().position(|&t| t == i).map(|p| tiles[p]);
                let w = &mut self.windows[i];
                let target = match w.mode {
                    WindowMode::Fullscreen => Some(area.geometry),
                    WindowMode::Maximized => Some(area.usable),
                    WindowMode::Normal => tile,
                };
                let restore = if target.is_none() { w.restore_size.take() } else { None };
                let is_focused = focused == Some(w.id);
                if let Some(toplevel) = w.window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        set_state(
                            state,
                            xdg_toplevel::State::Fullscreen,
                            w.mode == WindowMode::Fullscreen,
                        );
                        set_state(
                            state,
                            xdg_toplevel::State::Maximized,
                            w.mode == WindowMode::Maximized,
                        );
                        for tiled_state in [
                            xdg_toplevel::State::TiledLeft,
                            xdg_toplevel::State::TiledRight,
                            xdg_toplevel::State::TiledTop,
                            xdg_toplevel::State::TiledBottom,
                        ] {
                            set_state(
                                state,
                                tiled_state,
                                tile.is_some() && w.mode == WindowMode::Normal,
                            );
                        }
                        set_state(state, xdg_toplevel::State::Activated, is_focused);
                        set_state(
                            state,
                            xdg_toplevel::State::Resizing,
                            w.resize.is_some_and(|r| !r.finishing),
                        );
                        if let Some(rect) = target {
                            state.size = Some(rect.size);
                        } else if let Some(size) = restore {
                            state.size = size;
                        }
                        if w.mode != WindowMode::Fullscreen {
                            state.fullscreen_output = None;
                        }
                    });
                    if toplevel.is_initial_configure_sent() {
                        toplevel.send_pending_configure();
                    }
                }
                if !w.mapped {
                    continue;
                }
                let location = match target {
                    Some(rect) => rect.loc,
                    None => match w.floating {
                        Some(rect) => rect.loc,
                        None => {
                            let size = floating::clamp_size(w.window.geometry().size, area.usable);
                            let loc = floating::place(size, area.usable, &[]);
                            w.floating = Some(Rect::new(loc, size));
                            loc
                        }
                    },
                };
                locations.push((w.id, location));
            }
        }

        let shown: Vec<WindowId> = locations.iter().map(|(id, _)| *id).collect();
        let hidden: Vec<Window> = self
            .space
            .elements()
            .filter(|e| self.find_window(e).is_none_or(|id| !shown.contains(&id)))
            .cloned()
            .collect();
        for window in hidden {
            self.space.unmap_elem(&window);
        }
        let desired: Vec<(Window, Point<i32, Logical>)> = self
            .stack
            .iter()
            .filter_map(|id| {
                let location = locations.iter().find(|(w, _)| w == id)?.1;
                let window = self.windows.iter().find(|w| w.id == *id)?.window.clone();
                Some((window, location))
            })
            .collect();
        let current: Vec<(Window, Point<i32, Logical>)> = self
            .space
            .elements()
            .filter_map(|e| self.space.element_location(e).map(|loc| (e.clone(), loc)))
            .collect();
        if desired != current {
            // Mapping raises, so mapping bottom to top reproduces the stacking order.
            for (window, location) in desired {
                self.space.map_element(window, location, false);
            }
        }
        for w in &self.windows {
            w.window.set_activated(focused == Some(w.id));
        }
        if focused.is_some_and(|id| !self.is_visible(id)) {
            self.focused = None;
        }
    }

    /// Updates a window's app id and title from its toplevel; returns whether they changed.
    pub fn refresh_metadata(&mut self, id: WindowId) -> bool {
        let Some(w) = self.get_mut(id) else {
            return false;
        };
        let Some((app_id, title)) = w.toplevel().map(toplevel_metadata) else {
            return false;
        };
        if w.app_id == app_id && w.title == title {
            return false;
        }
        w.app_id = app_id;
        w.title = title;
        if let Some(handle) = &w.foreign {
            handle.send_title(&w.title);
            handle.send_app_id(&w.app_id);
            handle.send_done();
        }
        true
    }
}

fn set_state(
    state: &mut smithay::wayland::shell::xdg::ToplevelState,
    flag: xdg_toplevel::State,
    on: bool,
) {
    if on {
        state.states.set(flag);
    } else {
        state.states.unset(flag);
    }
}

/// The largest gap between tiled windows, in logical pixels.
pub const MAX_GAPS: u32 = 256;

fn clamp_gaps(gaps: u32) -> i32 {
    i32::try_from(gaps.min(MAX_GAPS)).unwrap_or(0)
}

/// The area for a window's output, falling back to the first output.
pub fn area_for<'a>(areas: &'a [OutputArea], output: Option<&str>) -> Option<&'a OutputArea> {
    output.and_then(|name| areas.iter().find(|a| a.name == name)).or_else(|| areas.first())
}

pub fn layout_from_config(layout: nimbus_config::DefaultLayout) -> LayoutMode {
    match layout {
        nimbus_config::DefaultLayout::Floating => LayoutMode::Floating,
        nimbus_config::DefaultLayout::Tiling => LayoutMode::Tiling,
    }
}
