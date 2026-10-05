// SPDX-License-Identifier: MIT

//! The shell's copy of the compositor's windows, and the dock built from them.

use nimbus_ipc::{Request, WindowId, WindowInfo, WorkspaceId};
use nimbus_xdg::AppIndex;

/// Windows in the order they opened, with a most-recently-focused list.
#[derive(Debug, Default)]
pub struct Windows {
    list: Vec<WindowInfo>,
    /// Window ids, most recently focused first.
    recent: Vec<WindowId>,
}

/// What [`Windows::upsert`] did.
#[derive(Debug, PartialEq)]
pub enum Upsert {
    Inserted(usize),
    Updated(usize),
}

impl Windows {
    pub fn list(&self) -> &[WindowInfo] {
        &self.list
    }

    pub fn get(&self, row: usize) -> Option<&WindowInfo> {
        self.list.get(row)
    }

    pub fn row_of(&self, id: WindowId) -> Option<usize> {
        self.list.iter().position(|w| w.id == id)
    }

    pub fn focused(&self) -> Option<&WindowInfo> {
        self.list.iter().find(|w| w.focused)
    }

    /// Replaces everything, keeping the focus history of windows that still exist.
    pub fn replace(&mut self, windows: &[WindowInfo]) {
        self.list = windows.to_vec();
        self.recent.retain(|id| windows.iter().any(|w| w.id == *id));
        for window in windows {
            if !self.recent.contains(&window.id) {
                self.recent.push(window.id);
            }
        }
        if let Some(id) = self.focused().map(|w| w.id) {
            self.note_focus(id);
        }
    }

    /// Adds a window or updates the one with the same id.
    pub fn upsert(&mut self, window: &WindowInfo) -> Upsert {
        if window.focused {
            // Only one window has the focus; the compositor may not report the others losing it.
            for other in self.list.iter_mut().filter(|w| w.id != window.id) {
                other.focused = false;
            }
            self.note_focus(window.id);
        } else if !self.recent.contains(&window.id) {
            self.recent.push(window.id);
        }
        match self.row_of(window.id) {
            Some(row) => {
                self.list[row] = window.clone();
                Upsert::Updated(row)
            }
            None => {
                self.list.push(window.clone());
                Upsert::Inserted(self.list.len() - 1)
            }
        }
    }

    pub fn remove(&mut self, id: WindowId) -> Option<usize> {
        self.recent.retain(|r| *r != id);
        let row = self.row_of(id)?;
        self.list.remove(row);
        Some(row)
    }

    fn note_focus(&mut self, id: WindowId) {
        self.recent.retain(|r| *r != id);
        self.recent.insert(0, id);
    }

    /// The position of each window among the windows of its workspace shown on `output`,
    /// as `(slot, count, on_output)` per row.
    pub fn slots(&self, output: &str) -> Vec<(usize, usize, bool)> {
        let on_output =
            |w: &WindowInfo| output.is_empty() || w.output.is_empty() || w.output == output;
        let mut counts = std::collections::HashMap::<WorkspaceId, usize>::new();
        let slots: Vec<(usize, bool, WorkspaceId)> = self
            .list
            .iter()
            .map(|w| {
                if !on_output(w) {
                    return (0, false, w.workspace);
                }
                let count = counts.entry(w.workspace).or_default();
                *count += 1;
                (*count - 1, true, w.workspace)
            })
            .collect();
        slots
            .into_iter()
            .map(|(slot, shown, workspace)| {
                let count = if shown { counts.get(&workspace).copied().unwrap_or(1) } else { 0 };
                (slot, count, shown)
            })
            .collect()
    }

    /// Window ids in most-recently-focused order.
    pub fn recent(&self) -> &[WindowId] {
        &self.recent
    }
}

/// One application in the dock.
#[derive(Clone, Debug, PartialEq)]
pub struct DockEntry {
    /// The desktop entry id, or the window's app id when there's no desktop entry.
    pub id: String,
    pub name: String,
    /// The icon name or path from the desktop entry.
    pub icon: Option<String>,
    /// Whether the desktop entry exists, so that the application can be launched.
    pub launchable: bool,
    pub favorite: bool,
    /// This application's windows, in the order they opened.
    pub windows: Vec<WindowId>,
    pub focused: bool,
}

/// The key that groups a window with an application: its desktop entry id, else its app id.
pub fn app_key(apps: &AppIndex, app_id: &str) -> String {
    apps.find_by_app_id(app_id).map_or_else(|| app_id.trim().to_owned(), |entry| entry.id.clone())
}

/// A name for an app id without a desktop entry, such as "Inkscape" for `org.inkscape.Inkscape`.
pub fn readable_app_id(app_id: &str) -> String {
    let app_id = app_id.trim();
    let last = app_id.rsplit('.').next().unwrap_or(app_id);
    let name = if last.starts_with(|c: char| c.is_alphabetic()) { last } else { app_id };
    let words = name.replace(['-', '_'], " ");
    let mut chars = words.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Favorites in order, followed by the other running applications in the order they opened.
/// A favorite without a desktop entry only shows while it has windows.
pub fn dock_entries(favorites: &[String], windows: &Windows, apps: &AppIndex) -> Vec<DockEntry> {
    let mut entries: Vec<DockEntry> = Vec::new();
    for favorite in favorites {
        let entry = apps.get(favorite).or_else(|| apps.find_by_app_id(favorite));
        let id = entry.map_or_else(|| favorite.trim().to_owned(), |e| e.id.clone());
        if id.is_empty() || entries.iter().any(|e| e.id == id) {
            continue;
        }
        entries.push(DockEntry {
            name: entry.map_or_else(|| id.clone(), |e| e.name.clone()),
            icon: entry.and_then(|e| e.icon.clone()),
            launchable: entry.is_some(),
            id,
            favorite: true,
            windows: Vec::new(),
            focused: false,
        });
    }
    for window in windows.list() {
        let key = if window.app_id.trim().is_empty() {
            format!("window-{}", window.id)
        } else {
            app_key(apps, &window.app_id)
        };
        let index = match entries.iter().position(|e| e.id == key) {
            Some(index) => index,
            None => {
                let entry = apps.get(&key);
                let name = match entry {
                    Some(entry) => entry.name.clone(),
                    None if window.app_id.trim().is_empty() => window.title.clone(),
                    None => readable_app_id(&window.app_id),
                };
                entries.push(DockEntry {
                    id: key,
                    name,
                    icon: entry.and_then(|e| e.icon.clone()),
                    launchable: entry.is_some(),
                    favorite: false,
                    windows: Vec::new(),
                    focused: false,
                });
                entries.len() - 1
            }
        };
        entries[index].windows.push(window.id);
        entries[index].focused |= window.focused;
    }
    entries.retain(|e| e.launchable || !e.windows.is_empty());
    entries
}

/// What a click on a dock entry does: launch the application, focus its most recent window,
/// cycle to its next window when it's focused, or minimize its only, focused window.
pub fn dock_click(entry: &DockEntry, windows: &Windows) -> Option<DockClick> {
    if entry.windows.is_empty() {
        return entry.launchable.then(|| DockClick::Launch(entry.id.clone()));
    }
    let focused = windows.focused().map(|w| w.id).filter(|id| entry.windows.contains(id));
    let request = match focused {
        Some(id) if entry.windows.len() == 1 => Request::SetMinimized { id, minimized: true },
        Some(id) => {
            let position = entry.windows.iter().position(|w| *w == id).unwrap_or(0);
            Request::Activate { id: entry.windows[(position + 1) % entry.windows.len()] }
        }
        None => {
            let id = windows
                .recent()
                .iter()
                .find(|id| entry.windows.contains(id))
                .copied()
                .unwrap_or(entry.windows[0]);
            Request::Activate { id }
        }
    };
    Some(DockClick::Compositor(request))
}

#[derive(Clone, Debug, PartialEq)]
pub enum DockClick {
    Launch(String),
    Compositor(Request),
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_xdg::DesktopEntry;

    fn window(id: WindowId, app_id: &str, workspace: WorkspaceId, focused: bool) -> WindowInfo {
        WindowInfo {
            id,
            app_id: app_id.into(),
            title: format!("{app_id} {id}"),
            workspace,
            output: "eDP-1".into(),
            focused,
            ..Default::default()
        }
    }

    fn apps() -> AppIndex {
        let entry = |id: &str, name: &str, wm_class: Option<&str>| DesktopEntry {
            id: id.into(),
            name: name.into(),
            icon: Some(id.into()),
            exec: id.into(),
            startup_wm_class: wm_class.map(Into::into),
            ..Default::default()
        };
        AppIndex {
            entries: vec![
                entry("org.nimbus.Files", "Files", None),
                entry("org.nimbus.Terminal", "Terminal", Some("nimbus-terminal")),
                entry("firefox", "Firefox", None),
            ],
        }
    }

    #[test]
    fn upsert_moves_focus_and_tracks_history() {
        let mut windows = Windows::default();
        assert_eq!(windows.upsert(&window(1, "firefox", 0, true)), Upsert::Inserted(0));
        assert_eq!(windows.upsert(&window(2, "foot", 0, true)), Upsert::Inserted(1));
        assert!(!windows.list()[0].focused);
        assert_eq!(windows.recent(), &[2, 1]);
        assert_eq!(windows.upsert(&window(1, "firefox", 1, true)), Upsert::Updated(0));
        assert_eq!(windows.recent(), &[1, 2]);
        assert_eq!(windows.focused().map(|w| w.id), Some(1));
        assert_eq!(windows.remove(1), Some(0));
        assert_eq!(windows.remove(1), None);
        assert_eq!(windows.recent(), &[2]);
    }

    #[test]
    fn replace_keeps_history_of_surviving_windows() {
        let mut windows = Windows::default();
        windows.upsert(&window(1, "a", 0, true));
        windows.upsert(&window(2, "b", 0, true));
        windows.replace(&[
            window(1, "a", 0, false),
            window(2, "b", 0, false),
            window(3, "c", 0, true),
        ]);
        assert_eq!(windows.recent(), &[3, 2, 1]);
    }

    #[test]
    fn slots_count_per_workspace_on_this_output() {
        let mut windows = Windows::default();
        windows.upsert(&window(1, "a", 0, false));
        windows.upsert(&window(2, "b", 1, false));
        windows.upsert(&WindowInfo { output: "HDMI-1".into(), ..window(3, "c", 0, false) });
        windows.upsert(&window(4, "d", 0, false));
        assert_eq!(
            windows.slots("eDP-1"),
            vec![(0, 2, true), (0, 1, true), (0, 0, false), (1, 2, true)]
        );
        // Without a known output, every window counts.
        assert_eq!(windows.slots("")[3], (2, 3, true));
    }

    #[test]
    fn dock_merges_favorites_with_running_apps() {
        let apps = apps();
        let mut windows = Windows::default();
        windows.upsert(&window(1, "nimbus-terminal", 0, false));
        windows.upsert(&window(2, "gimp", 0, true));
        windows.upsert(&window(3, "org.nimbus.Terminal", 1, false));
        windows.upsert(&WindowInfo {
            app_id: String::new(),
            title: "Untitled".into(),
            ..window(4, "", 0, false)
        });
        let favorites = vec![
            "org.nimbus.Files".into(),
            "org.nimbus.Terminal".into(),
            "missing".into(),
            "firefox".into(),
        ];
        let dock = dock_entries(&favorites, &windows, &apps);
        let ids: Vec<&str> = dock.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["org.nimbus.Files", "org.nimbus.Terminal", "firefox", "gimp", "window-4"]);
        assert_eq!(dock[1].windows, vec![1, 3]);
        assert!(dock[1].favorite && !dock[1].focused);
        assert!(!dock[3].favorite && dock[3].focused && !dock[3].launchable);
        assert_eq!(dock[3].name, "Gimp");
        assert_eq!(dock[4].name, "Untitled");
    }

    #[test]
    fn app_ids_become_names() {
        assert_eq!(readable_app_id("org.inkscape.Inkscape"), "Inkscape");
        assert_eq!(readable_app_id("gimp-2.10"), "Gimp 2.10");
        assert_eq!(readable_app_id("foot"), "Foot");
        assert_eq!(readable_app_id("my_tool"), "My tool");
        assert_eq!(readable_app_id(""), "");
    }

    #[test]
    fn dock_clicks() {
        let apps = apps();
        let mut windows = Windows::default();
        let dock = |windows: &Windows| {
            dock_entries(&["firefox".into(), "org.nimbus.Files".into()], windows, &apps)
        };
        assert_eq!(
            dock_click(&dock(&windows)[0], &windows),
            Some(DockClick::Launch("firefox".into()))
        );

        windows.upsert(&window(1, "firefox", 0, false));
        windows.upsert(&window(2, "firefox", 0, false));
        windows.upsert(&window(3, "org.nimbus.Files", 0, true));
        windows.upsert(&window(2, "firefox", 0, true));
        windows.upsert(&window(3, "org.nimbus.Files", 0, true));
        // Unfocused: the most recently focused window of the application.
        assert_eq!(
            dock_click(&dock(&windows)[0], &windows),
            Some(DockClick::Compositor(Request::Activate { id: 2 }))
        );
        // Focused with several windows: the next one, wrapping around.
        windows.upsert(&window(2, "firefox", 0, true));
        assert_eq!(
            dock_click(&dock(&windows)[0], &windows),
            Some(DockClick::Compositor(Request::Activate { id: 1 }))
        );
        // Focused with one window: minimize it.
        windows.upsert(&window(3, "org.nimbus.Files", 0, true));
        assert_eq!(
            dock_click(&dock(&windows)[1], &windows),
            Some(DockClick::Compositor(Request::SetMinimized { id: 3, minimized: true }))
        );
    }
}
