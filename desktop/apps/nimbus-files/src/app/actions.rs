// SPDX-License-Identifier: MIT

//! What happens when the user clicks, types, or picks a command.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use slint::{ComponentHandle as _, ModelRc, VecModel};

use super::controller::{Clipboard, Controller, Dialog, ToastAction};
use super::convert;
use super::workers;
use crate::core::apps;
use crate::core::entry::FileEntry;
use crate::core::history::Location;
use crate::core::keymap::{self, KeyAction, Modifiers, Scope, Step};
use crate::core::menu::{self, Command, MenuContext, MenuEntry};
use crate::core::names::{self, Style};
use crate::core::ops::{Decision, Operation, Resolution};
use crate::core::pathbar;
use crate::core::places::Target;
use crate::core::prefs::ViewMode;
use crate::core::rubberband::{self, GridGeometry};
use crate::core::search;
use crate::core::selection::Modifiers as SelectModifiers;
use crate::core::sort::SortKey;
use crate::{DialogKind, IconKind};

/// Opening this many files at once asks for confirmation first.
const MANY_FILES: usize = 10;

impl Controller {
    pub(super) fn go_back(&self) {
        let location = self.state.borrow_mut().history.back().cloned();
        if let Some(location) = location {
            self.load_location(location);
        }
    }

    pub(super) fn go_forward(&self) {
        let location = self.state.borrow_mut().history.forward().cloned();
        if let Some(location) = location {
            self.load_location(location);
        }
    }

    pub(super) fn go_up(&self) {
        let Some(dir) = self.current_dir() else { return };
        if let Some(parent) = dir.parent() {
            self.state.borrow_mut().pending_select = vec![dir.clone()];
            self.navigate(Location::Dir(parent.to_path_buf()));
        }
    }

    pub(super) fn crumb_clicked(&self, index: usize) {
        let Some(dir) = self.current_dir() else { return };
        let segments = pathbar::segments(&dir, &self.env.home);
        if let Some(segment) = segments.get(index) {
            if let Some(child) = segments.get(index + 1) {
                self.state.borrow_mut().pending_select = vec![child.path.clone()];
            }
            self.navigate(Location::Dir(segment.path.clone()));
        }
    }

    pub(super) fn place_clicked(&self, index: usize) {
        let target = self.state.borrow().places.get(index).map(|p| p.target.clone());
        match target {
            Some(Target::Dir(path)) => self.navigate(Location::Dir(path)),
            Some(Target::Trash) => self.navigate(Location::Trash),
            Some(Target::Volume(id)) => self.open_volume(id),
            None => {}
        }
    }

    pub(super) fn edit_location(&self) {
        self.ui.set_editing_location(true);
        self.ui.invoke_focus_location();
    }

    pub(super) fn location_accepted(&self, text: &str) {
        self.ui.set_editing_location(false);
        let current = self.current_dir().unwrap_or_else(|| self.env.home.clone());
        if text.trim() == "trash:///" || text.trim() == "trash:" {
            self.navigate(Location::Trash);
        } else if let Some(path) = pathbar::resolve_typed(text, &current, &self.env.home) {
            self.navigate(Location::Dir(path));
        }
        self.ui.invoke_focus_view();
    }

    pub(super) fn location_cancelled(&self) {
        if self.ui.get_editing_location() {
            self.ui.set_editing_location(false);
            self.sync_location();
        }
    }

    pub(super) fn toggle_search(self: &Rc<Self>) {
        if self.state.borrow().search_active {
            self.stop_search();
        } else if self.current_dir().is_some() {
            self.state.borrow_mut().search_active = true;
            self.ui.set_search_active(true);
            self.ui.invoke_focus_search();
        }
    }

    pub(super) fn search_edited(self: &Rc<Self>, text: &str) {
        let recursive = {
            let mut state = self.state.borrow_mut();
            state.filter = text.to_string();
            state.search_recursive
        };
        if recursive {
            self.schedule_search();
        } else {
            self.rebuild_view();
        }
    }

    pub(super) fn toggle_recursive(self: &Rc<Self>) {
        let recursive = {
            let mut state = self.state.borrow_mut();
            state.search_recursive = !state.search_recursive;
            state.search_recursive
        };
        self.ui.set_search_recursive(recursive);
        self.start_search();
        self.ui.invoke_focus_search();
    }

    pub(super) fn set_list_mode(&self, list: bool) {
        self.change_prefs(|p| p.view = if list { ViewMode::List } else { ViewMode::Grid });
    }

    pub(super) fn set_sort_key(&self, key: i32) {
        let key = match key {
            1 => SortKey::Size,
            2 => SortKey::Modified,
            3 => SortKey::Type,
            _ => SortKey::Name,
        };
        self.change_prefs(|p| p.sort.key = key);
    }

    /// A click on a column header sorts by it, or reverses the order when it's already the sort column.
    pub(super) fn column_clicked(&self, key: i32) {
        let current = self.ui.get_sort_key();
        if current == key {
            self.change_prefs(|p| p.sort.descending = !p.sort.descending);
        } else {
            self.set_sort_key(key);
        }
    }

    pub(super) fn toggle_hidden(self: &Rc<Self>) {
        self.change_prefs(|p| p.show_hidden = !p.show_hidden);
        if self.state.borrow().search.is_some() {
            self.start_search();
        }
    }

    fn zoom(&self, delta: i8) {
        self.change_prefs(|p| p.zoom = (p.zoom as i8 + delta).clamp(0, 2) as u8);
    }

    pub(super) fn item_pressed(&self, index: usize, ctrl: bool, shift: bool) {
        self.ui.set_keyboard_cursor(false);
        self.change_selection(|s| s.click(index, SelectModifiers { toggle: ctrl, extend: shift }));
    }

    pub(super) fn background_pressed(&self, ctrl: bool) {
        self.ui.set_keyboard_cursor(false);
        if !ctrl {
            self.change_selection(|s| s.clear());
        }
        let base = self.state.borrow().selection.indices();
        self.state.borrow_mut().band_base = base;
    }

    pub(super) fn band(
        &self,
        a: (f32, f32),
        b: (f32, f32),
        columns: i32,
        cell: (f32, f32),
        origin_x: f32,
    ) {
        let count = self.state.borrow().view.len();
        let geometry = GridGeometry {
            columns: columns.max(1) as usize,
            count,
            cell_width: cell.0,
            cell_height: cell.1,
            origin_x,
            origin_y: 0.0,
        };
        let cells = rubberband::cells_in_rect(geometry, a, b);
        let base = self.state.borrow().band_base.clone();
        self.change_selection(|s| s.select_band(cells, Some(&base)));
    }

    pub(super) fn item_activated(self: &Rc<Self>, index: usize) {
        self.change_selection(|s| {
            if !s.is_selected(index) {
                s.click(index, SelectModifiers::default());
            }
        });
        self.open_selection();
    }

    pub(super) fn item_context(&self, index: usize, x: f32, y: f32) {
        self.change_selection(|s| s.context_click(index));
        let entries = self.selected_entries();
        let in_trash = self.location() == Location::Trash;
        let ctx = MenuContext {
            in_trash,
            selected: entries.len(),
            folders_only: !entries.is_empty() && entries.iter().all(FileEntry::is_dir),
            can_paste: self.state.borrow().clipboard.is_some(),
            writable: self.writable(),
            show_hidden: self.state.borrow().prefs.show_hidden,
            bookmarked: false,
        };
        let target = match entries.as_slice() {
            [one] if one.is_dir() => Some(one.path.clone()),
            _ => None,
        };
        let ctx = MenuContext {
            bookmarked: target.as_deref().is_some_and(|t| self.is_bookmarked(t)),
            ..ctx
        };
        self.show_menu(menu::item_menu(ctx), target, x, y);
    }

    pub(super) fn background_context(&self, x: f32, y: f32) {
        self.change_selection(|s| s.clear());
        let ctx = self.background_menu_context();
        self.show_menu(menu::background_menu(ctx), None, x, y);
    }

    pub(super) fn main_menu(&self, x: f32, y: f32) {
        let ctx = self.background_menu_context();
        self.show_menu(menu::main_menu(ctx), None, x, y);
    }

    fn is_bookmarked(&self, dir: &Path) -> bool {
        self.state.borrow().places.iter().any(|p| {
            p.section == crate::core::places::Section::Bookmarks
                && p.target == Target::Dir(dir.to_path_buf())
        })
    }

    fn background_menu_context(&self) -> MenuContext {
        let bookmarked = self.current_dir().is_some_and(|d| self.is_bookmarked(&d));
        let state = self.state.borrow();
        MenuContext {
            bookmarked,
            in_trash: *state.history.current() == Location::Trash,
            selected: state.selection.count(),
            folders_only: false,
            can_paste: state.clipboard.is_some(),
            writable: state.writable && *state.history.current() != Location::Trash,
            show_hidden: state.prefs.show_hidden,
        }
    }

    pub(super) fn show_menu(
        &self,
        entries: Vec<MenuEntry>,
        target: Option<PathBuf>,
        x: f32,
        y: f32,
    ) {
        let rows = convert::menu_rows(&entries);
        let separators = rows.iter().filter(|r| r.separator).count();
        self.ui.set_menu_item_count((rows.len() - separators) as i32);
        self.ui.set_menu_separator_count(separators as i32);
        self.ui.set_menu_rows(ModelRc::new(VecModel::from(rows)));
        {
            let mut state = self.state.borrow_mut();
            state.menu = entries;
            state.menu_target = target;
        }
        self.ui.invoke_show_menu(x, y);
    }

    pub(super) fn menu_step(&self, current: i32, delta: i32) -> i32 {
        let rows = convert::menu_rows(&self.state.borrow().menu);
        convert::menu_step(&rows, current, delta)
    }

    pub(super) fn menu_activated(self: &Rc<Self>, index: usize) {
        let entry = self.state.borrow().menu.get(index).cloned();
        let command = match entry {
            Some(MenuEntry::Item { command, enabled: true, .. })
            | Some(MenuEntry::Check { command, .. }) => command,
            _ => return,
        };
        self.run_command(command);
        self.state.borrow_mut().menu_target = None;
    }

    /// Handles a key press from the file view or the window; false lets it propagate.
    pub(super) fn key(self: &Rc<Self>, text: &str, modifiers: Modifiers, scope: Scope) -> bool {
        if self.ui.get_dialog() != DialogKind::None {
            return false;
        }
        // Keyboard commands act on the current folder, not on a folder a dismissed menu was opened for.
        self.state.borrow_mut().menu_target = None;
        let plain = modifiers == Modifiers::default();
        if scope == Scope::Window
            && plain
            && self.state.borrow().search_active
            && text == char::from(slint::platform::Key::DownArrow).to_string()
        {
            self.search_accepted();
            return true;
        }
        let Some(action) = keymap::map(text, modifiers, scope) else { return false };
        match action {
            KeyAction::Command(command) => self.run_command(command),
            KeyAction::Move { step, extend, toggle } => {
                return self.move_cursor(step, extend, toggle);
            }
            KeyAction::Activate => self.open_selection(),
            KeyAction::GoUp => self.go_up(),
            KeyAction::Back => self.go_back(),
            KeyAction::Forward => self.go_forward(),
            KeyAction::ToggleCursor => self.change_selection(|s| s.toggle_cursor()),
            KeyAction::EditLocation => {
                if self.state.borrow().search_active {
                    self.stop_search();
                }
                self.edit_location();
            }
            KeyAction::StartSearch => {
                if !self.state.borrow().search_active {
                    self.toggle_search();
                } else {
                    self.ui.invoke_focus_search();
                }
            }
            KeyAction::TypeAhead(text) => self.type_ahead(&text),
            KeyAction::Escape => return self.escape(),
            KeyAction::ViewGrid => self.set_list_mode(false),
            KeyAction::ViewList => self.set_list_mode(true),
            KeyAction::ZoomIn => self.zoom(1),
            KeyAction::ZoomOut => self.zoom(-1),
            KeyAction::ZoomReset => self.change_prefs(|p| p.zoom = 1),
            KeyAction::NewWindow => self.open_new_window(self.current_dir().as_deref()),
            KeyAction::ContextMenu => {
                // The pointer position is unknown, so the menu opens near the view's top-left corner.
                let (x, y) =
                    (self.ui.get_view_origin_x() + 24.0, self.ui.get_view_origin_y() + 24.0);
                let cursor = {
                    let state = self.state.borrow();
                    state.selection.cursor().filter(|&c| state.selection.is_selected(c))
                };
                match cursor {
                    Some(cursor) => self.item_context(cursor, x, y),
                    None => self.background_context(x, y),
                }
            }
            KeyAction::CloseWindow => {
                let _ = self.ui.hide();
                let _ = slint::quit_event_loop();
            }
        }
        true
    }

    fn move_cursor(&self, step: Step, extend: bool, toggle: bool) -> bool {
        let list = self.ui.get_list_mode();
        let columns = self.ui.get_grid_columns().max(1) as isize;
        let page = self.ui.get_page_rows().max(1) as isize;
        let delta = match step {
            Step::Left if list => return false,
            Step::Right if list => return false,
            Step::Left => -1,
            Step::Right => 1,
            Step::Up => -columns,
            Step::Down => columns,
            Step::PageUp => -page * columns,
            Step::PageDown => page * columns,
            Step::Home => isize::MIN,
            Step::End => isize::MAX,
        };
        self.ui.set_keyboard_cursor(true);
        let mut target = None;
        self.change_selection(|s| {
            target = s.move_cursor(delta, SelectModifiers { toggle, extend })
        });
        if let Some(target) = target {
            self.ui.invoke_scroll_to(target as i32);
        }
        true
    }

    fn type_ahead(&self, text: &str) {
        let (prefix, start) = {
            let mut state = self.state.borrow_mut();
            let fresh = state
                .type_ahead_at
                .is_none_or(|t| t.elapsed() > super::controller::TYPE_AHEAD_TIMEOUT);
            if fresh {
                state.type_ahead.clear();
            }
            state.type_ahead.push_str(text);
            state.type_ahead_at = Some(Instant::now());
            let cursor = state.selection.cursor().unwrap_or(0);
            // A repeated single letter cycles through matches; a longer prefix refines the current one.
            let single_repeat = state.type_ahead.chars().count() > 1
                && state.type_ahead.chars().all(|c| text.starts_with(c));
            if single_repeat {
                state.type_ahead = text.to_string();
                (state.type_ahead.clone(), cursor + 1)
            } else {
                (state.type_ahead.clone(), if fresh { cursor + 1 } else { cursor })
            }
        };
        let found = {
            let state = self.state.borrow();
            let names: Vec<&str> = state.view.iter().map(|e| e.name.as_str()).collect();
            search::find_by_prefix(&names, &prefix, start)
        };
        if let Some(index) = found {
            self.ui.set_keyboard_cursor(true);
            self.change_selection(|s| s.click(index, SelectModifiers::default()));
            self.ui.invoke_scroll_to(index as i32);
        }
    }

    fn escape(&self) -> bool {
        if self.ui.get_editing_location() {
            self.location_cancelled();
            self.ui.invoke_focus_view();
            return true;
        }
        if self.state.borrow().search_active {
            self.stop_search();
            return true;
        }
        if self.ui.get_toast_visible() {
            self.hide_toast();
            return true;
        }
        if self.state.borrow().selection.count() > 0 {
            self.change_selection(|s| s.clear());
            return true;
        }
        false
    }

    pub(super) fn run_command(self: &Rc<Self>, command: Command) {
        let in_trash = self.location() == Location::Trash;
        match command {
            Command::Open => self.open_selection(),
            Command::OpenWith => self.show_open_with(),
            Command::OpenInNewWindow => {
                for entry in self.selected_entries().into_iter().filter(FileEntry::is_dir) {
                    self.open_new_window(Some(&entry.path));
                }
            }
            Command::Cut | Command::Copy if !in_trash => {
                let paths = self.selected_paths();
                if paths.is_empty() {
                    return;
                }
                let cut = command == Command::Cut;
                let count = paths.len();
                self.state.borrow_mut().clipboard = Some(Clipboard { paths, cut });
                self.sync_cut_flags();
                let verb = if cut { "cut" } else { "copied" };
                let what = if count == 1 { "1 item".to_string() } else { format!("{count} items") };
                self.show_toast(
                    format!("{what} {verb}; paste with Ctrl+V"),
                    ToastAction::None,
                    None,
                );
            }
            Command::Paste => self.paste(),
            Command::Rename if !in_trash => self.show_rename(),
            Command::Trash if !in_trash => {
                let paths = self.selected_paths();
                if !paths.is_empty() {
                    self.start_job(Operation::Trash { paths }, false);
                }
            }
            Command::Trash | Command::Delete if in_trash => {
                self.confirm_delete_trashed(self.selected_trash_items())
            }
            Command::Delete => self.confirm_delete(self.selected_paths()),
            Command::Restore => self.restore_selection(),
            Command::EmptyTrash => self.confirm_empty_trash(),
            Command::Properties => self.show_properties(),
            Command::OpenPlace
            | Command::Mount
            | Command::Unmount
            | Command::Eject
            | Command::PowerOff => self.volume_command(command),
            Command::NewFolder if !in_trash => self.show_new_folder(),
            Command::SelectAll => self.change_selection(|s| s.select_all()),
            Command::CopyLocation => self.copy_location(),
            Command::AddBookmark | Command::RemoveBookmark => {
                let dir = self.state.borrow().menu_target.clone().or_else(|| self.current_dir());
                if let Some(dir) = dir {
                    // Ctrl+D toggles, so the command follows the folder's current state.
                    let bookmarked = !self.is_bookmarked(&dir);
                    let name = dir.file_name().map_or_else(
                        || dir.to_string_lossy().into_owned(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    let message = if bookmarked {
                        format!("“{name}” added to Bookmarks")
                    } else {
                        format!("“{name}” removed from Bookmarks")
                    };
                    workers::set_bookmark(
                        self.tx.clone(),
                        self.env.bookmarks_file(),
                        dir,
                        bookmarked,
                    );
                    self.show_toast(message, ToastAction::None, None);
                }
            }
            Command::NewWindow => self.open_new_window(self.current_dir().as_deref()),
            Command::ToggleHidden => self.toggle_hidden(),
            Command::Reload => self.reload(),
            _ => {}
        }
    }

    /// Opens the selection: folders in this window (or new windows), files with their default application.
    pub(super) fn open_selection(self: &Rc<Self>) {
        let entries = self.selected_entries();
        if entries.is_empty() {
            return;
        }
        if self.location() == Location::Trash {
            self.show_toast("Restore the item to open it".into(), ToastAction::None, None);
            return;
        }
        let (dirs, files): (Vec<FileEntry>, Vec<FileEntry>) =
            entries.into_iter().partition(FileEntry::is_dir);
        if files.len() > MANY_FILES {
            self.show_toast(
                format!("Select at most {MANY_FILES} files to open at once"),
                ToastAction::None,
                None,
            );
            return;
        }
        for file in &files {
            if let Err(error) = apps::open_default(&file.path) {
                self.show_toast(
                    format!("Couldn't open “{}”: {error}", file.name),
                    ToastAction::None,
                    None,
                );
            }
        }
        let mut dirs = dirs.into_iter();
        if let Some(first) = dirs.next() {
            for other in dirs.take(MANY_FILES) {
                self.open_new_window(Some(&other.path));
            }
            if files.is_empty() {
                self.navigate(Location::Dir(first.path));
            } else {
                self.open_new_window(Some(&first.path));
            }
        }
    }

    pub(super) fn open_new_window(&self, dir: Option<&Path>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let args: Vec<String> = dir.map(|d| d.to_string_lossy().into_owned()).into_iter().collect();
        if let Err(error) = apps::spawn_detached(&exe.to_string_lossy(), &args, None) {
            self.show_toast(
                format!("Couldn't open a new window: {error}"),
                ToastAction::None,
                None,
            );
        }
    }

    fn paste(self: &Rc<Self>) {
        let target = self.state.borrow().menu_target.clone().or_else(|| self.current_dir());
        let Some(dest) = target else { return };
        let Some(clipboard) = self.state.borrow().clipboard.clone() else { return };
        if clipboard.cut {
            // Cut items move once; the clipboard empties like in other file managers.
            self.state.borrow_mut().clipboard = None;
            self.sync_cut_flags();
            self.start_job(Operation::Move { sources: clipboard.paths, dest }, true);
        } else {
            self.start_job(Operation::Copy { sources: clipboard.paths, dest }, true);
        }
    }

    fn copy_location(&self) {
        let paths = self.selected_paths();
        let paths = if paths.is_empty() { self.current_dir().into_iter().collect() } else { paths };
        let text =
            paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n");
        if text.is_empty() {
            return;
        }
        let copied = ["wl-copy", "xclip"].iter().any(|tool| {
            let mut command = std::process::Command::new(tool);
            if *tool == "xclip" {
                command.args(["-selection", "clipboard"]);
            }
            let child = command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            let Ok(mut child) = child else { return false };
            let written = child.stdin.take().is_some_and(|mut stdin| {
                use std::io::Write as _;
                stdin.write_all(text.as_bytes()).is_ok()
            });
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            written
        });
        let message =
            if copied { "Location copied" } else { "No clipboard tool (wl-copy) is installed" };
        self.show_toast(message.into(), ToastAction::None, None);
    }

    fn restore_selection(self: &Rc<Self>) {
        let items = self.selected_trash_items();
        if !items.is_empty() {
            self.start_job(Operation::Restore { items }, false);
        }
    }

    pub(super) fn restore_clicked(self: &Rc<Self>) {
        self.restore_selection();
    }

    pub(super) fn toast_action(self: &Rc<Self>) {
        let action =
            std::mem::replace(&mut self.state.borrow_mut().toast_action, ToastAction::None);
        self.hide_toast();
        if let ToastAction::UndoTrash(items) = action {
            self.start_job(Operation::Restore { items }, true);
        }
    }

    fn show_rename(&self) {
        let entries = self.selected_entries();
        let [entry] = entries.as_slice() else { return };
        let select = if entry.is_dir() { entry.name.len() } else { names::stem_len(&entry.name) };
        let title = if entry.is_dir() { "Rename Folder" } else { "Rename File" };
        self.state.borrow_mut().dialog = Dialog::Rename { path: entry.path.clone() };
        self.open_name_dialog(title, "Rename", &entry.name, select);
    }

    fn show_new_folder(&self) {
        let Some(parent) = self.current_dir() else { return };
        let name = {
            let state = self.state.borrow();
            let exists = |n: &str| state.entries.iter().any(|e| e.name == n);
            if exists("New Folder") {
                names::unique("New Folder", Style::Numbered, exists)
            } else {
                "New Folder".to_string()
            }
        };
        self.state.borrow_mut().dialog = Dialog::NewFolder { parent };
        self.open_name_dialog("New Folder", "Create", &name, name.len());
    }

    fn open_name_dialog(&self, title: &str, accept: &str, text: &str, select: usize) {
        let ui = &self.ui;
        ui.set_dialog_title(title.into());
        ui.set_dialog_accept_label(accept.into());
        ui.set_dialog_initial_text(text.into());
        ui.set_dialog_select_length(select as i32);
        ui.set_dialog_error("".into());
        ui.set_dialog(DialogKind::Name);
    }

    /// Validates a name as it's typed, against the rules and the names already in the folder.
    pub(super) fn name_edited(&self, text: &str) {
        let error = self.name_error(text).unwrap_or_default();
        self.ui.set_dialog_error(error.into());
    }

    fn name_error(&self, text: &str) -> Option<String> {
        let state = self.state.borrow();
        let original = match &state.dialog {
            Dialog::Rename { path } => path.file_name().map(|n| n.to_string_lossy().into_owned()),
            Dialog::NewFolder { .. } => None,
            _ => return None,
        };
        if let Err(error) = names::validate(text) {
            return (!text.is_empty()).then(|| error.to_string());
        }
        if original.as_deref() != Some(text) && state.entries.iter().any(|e| e.name == text) {
            return Some(format!("A file named “{text}” already exists"));
        }
        None
    }

    pub(super) fn name_accepted(self: &Rc<Self>, text: &str) {
        if let Some(error) = self.name_error(text) {
            self.ui.set_dialog_error(error.into());
            return;
        }
        if names::validate(text).is_err() {
            return;
        }
        let dialog = std::mem::replace(&mut self.state.borrow_mut().dialog, Dialog::None);
        self.close_dialog();
        match dialog {
            Dialog::Rename { path } => {
                if path.file_name().is_some_and(|n| n.to_string_lossy() == text) {
                    return;
                }
                self.start_job(Operation::Rename { path, new_name: text.to_string() }, true);
            }
            Dialog::NewFolder { parent } => {
                self.start_job(Operation::CreateFolder { parent, name: text.to_string() }, true);
            }
            _ => {}
        }
    }

    fn confirm_delete(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let title = match paths.as_slice() {
            [one] => format!(
                "Permanently Delete “{}”?",
                one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            many => format!("Permanently Delete {} Items?", many.len()),
        };
        self.state.borrow_mut().dialog = Dialog::ConfirmDelete { paths };
        self.open_confirm(
            &title,
            "Deleted items aren't moved to the Trash and can't be restored.",
            "Delete",
        );
    }

    fn confirm_delete_trashed(&self, items: Vec<crate::core::trash::TrashItem>) {
        if items.is_empty() {
            return;
        }
        let title = match items.as_slice() {
            [one] => format!("Permanently Delete “{}”?", one.name),
            many => format!("Permanently Delete {} Items?", many.len()),
        };
        self.state.borrow_mut().dialog = Dialog::ConfirmDeleteTrashed { items };
        self.open_confirm(
            &title,
            "These items will be deleted permanently and can't be restored.",
            "Delete",
        );
    }

    pub(super) fn confirm_empty_trash(&self) {
        let items: Vec<_> = self.state.borrow().trash_items.values().cloned().collect();
        if items.is_empty() && self.location() == Location::Trash {
            return;
        }
        let count = items.len();
        self.state.borrow_mut().dialog = Dialog::ConfirmDeleteTrashed { items };
        let message = format!(
            "All {} in the Trash will be deleted permanently and can't be restored.",
            crate::core::format::item_count(count as u64)
        );
        self.open_confirm("Empty the Trash?", &message, "Empty Trash");
    }

    fn open_confirm(&self, title: &str, message: &str, accept: &str) {
        let ui = &self.ui;
        ui.set_dialog_title(title.into());
        ui.set_dialog_message(message.into());
        ui.set_dialog_accept_label(accept.into());
        ui.set_dialog_destructive(true);
        ui.set_dialog(DialogKind::Confirm);
    }

    pub(super) fn confirm_accepted(self: &Rc<Self>) {
        let dialog = std::mem::replace(&mut self.state.borrow_mut().dialog, Dialog::None);
        self.close_dialog();
        match dialog {
            Dialog::ConfirmDelete { paths } => self.start_job(Operation::Delete { paths }, false),
            Dialog::ConfirmDeleteTrashed { items } => {
                self.start_job(Operation::DeleteTrashed { items }, false)
            }
            _ => {}
        }
    }

    pub(super) fn conflict_resolved(&self, choice: i32, apply_to_all: bool) {
        let resolution = match choice {
            0 => Resolution::Skip,
            1 => Resolution::Replace,
            _ => Resolution::KeepBoth,
        };
        self.resolve_conflict(Some(Decision { resolution, apply_to_all }));
    }

    pub(super) fn dialog_cancelled(&self) {
        if matches!(self.state.borrow().dialog, Dialog::Conflict) {
            self.resolve_conflict(None);
        } else {
            self.close_dialog();
        }
    }

    fn show_properties(&self) {
        if self.location() == Location::Trash {
            self.show_trashed_properties();
            return;
        }
        let entries = self.selected_entries();
        let (paths, name, kind, badge, thumbnail) = match entries.as_slice() {
            [] => {
                let Some(dir) = self.current_dir() else { return };
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into());
                let kind = self
                    .state
                    .borrow()
                    .special
                    .folders
                    .iter()
                    .find(|(p, _)| *p == dir)
                    .map_or(IconKind::Folder, |(_, k)| *k);
                (vec![dir], name, kind, String::new(), None)
            }
            [one] => {
                let state = self.state.borrow();
                let thumbnail = state.thumbnails.get(&one.path).cloned();
                (
                    vec![one.path.clone()],
                    one.name.clone(),
                    convert::entry_icon(one, &state.special),
                    convert::badge(one).to_string(),
                    thumbnail,
                )
            }
            many => (
                many.iter().map(|e| e.path.clone()).collect(),
                format!("{} Items", many.len()),
                IconKind::File,
                String::new(),
                None,
            ),
        };
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_request += 1;
            state.next_request
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.state.borrow_mut().dialog = Dialog::Properties { id, cancel: cancel.clone() };
        let ui = &self.ui;
        ui.set_props_name(name.into());
        ui.set_props_kind(kind);
        ui.set_props_badge(badge.into());
        ui.set_props_has_thumbnail(thumbnail.is_some());
        ui.set_props_thumbnail(thumbnail.unwrap_or_default());
        ui.set_props_rows(ModelRc::default());
        ui.set_props_computing(true);
        ui.set_dialog(DialogKind::Properties);
        workers::properties(self.tx.clone(), id, paths, self.env.home.clone(), cancel);
    }

    fn show_trashed_properties(&self) {
        let items = self.selected_trash_items();
        let [item] = items.as_slice() else { return };
        let entry = super::controller::trash_entry(item);
        let rows: Vec<crate::PropertyRow> =
            crate::core::properties::describe_trashed(item, &self.env.home)
                .into_iter()
                .map(|(label, value)| crate::PropertyRow {
                    label: label.into(),
                    value: value.into(),
                })
                .collect();
        let kind = convert::entry_icon(&entry, &self.state.borrow().special);
        self.state.borrow_mut().dialog =
            Dialog::Properties { id: 0, cancel: Arc::new(AtomicBool::new(false)) };
        let ui = &self.ui;
        ui.set_props_name(item.name.as_str().into());
        ui.set_props_kind(kind);
        ui.set_props_badge(convert::badge(&entry));
        ui.set_props_has_thumbnail(false);
        ui.set_props_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_props_computing(false);
        ui.set_dialog(DialogKind::Properties);
    }

    /// Moves from the search field to the results, selecting the first.
    pub(super) fn search_accepted(&self) {
        if self.state.borrow().view.is_empty() {
            return;
        }
        self.ui.invoke_focus_view();
        self.ui.set_keyboard_cursor(true);
        self.change_selection(|s| s.click(0, SelectModifiers::default()));
        self.ui.invoke_scroll_to(0);
    }

    fn show_open_with(&self) {
        let entries: Vec<FileEntry> =
            self.selected_entries().into_iter().filter(|e| !e.is_dir()).collect();
        let Some(first) = entries.first() else { return };
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_request += 1;
            state.next_request
        };
        let paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        let title = match entries.as_slice() {
            [one] => format!("Open “{}” With", one.name),
            many => format!("Open {} Files With", many.len()),
        };
        self.state.borrow_mut().dialog = Dialog::OpenWith { id, paths, apps: Vec::new() };
        self.ui.set_dialog_title(title.into());
        self.ui.set_apps(ModelRc::default());
        self.ui.set_apps_loading(true);
        self.ui.set_dialog(DialogKind::OpenWith);
        workers::find_apps(self.tx.clone(), id, first.mime.clone());
    }

    pub(super) fn app_chosen(&self, index: usize) {
        let chosen = match &self.state.borrow().dialog {
            Dialog::OpenWith { paths, apps, .. } => {
                apps.get(index).cloned().map(|app| (app, paths.clone()))
            }
            _ => None,
        };
        self.close_dialog();
        if let Some((app, paths)) = chosen
            && let Err(error) = nimbus_xdg::launch(&app, &paths, None)
        {
            self.show_toast(
                format!("Couldn't start {}: {error}", app.name),
                ToastAction::None,
                None,
            );
        }
    }

    pub(super) fn open_default_chosen(&self) {
        let paths = match &self.state.borrow().dialog {
            Dialog::OpenWith { paths, .. } => paths.clone(),
            _ => Vec::new(),
        };
        self.close_dialog();
        for path in paths {
            if let Err(error) = apps::open_default(&path) {
                self.show_toast(
                    format!("Couldn't open the file: {error}"),
                    ToastAction::None,
                    None,
                );
            }
        }
    }
}
