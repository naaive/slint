// SPDX-License-Identifier: MIT

//! Commands and the context menus that offer them.

/// Everything the user can ask the file manager to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Open,
    OpenWith,
    OpenInNewWindow,
    Cut,
    Copy,
    Paste,
    Rename,
    Trash,
    Delete,
    Restore,
    EmptyTrash,
    Properties,
    NewFolder,
    SelectAll,
    CopyLocation,
    AddBookmark,
    RemoveBookmark,
    NewWindow,
    ToggleHidden,
    Reload,
}

/// One row of a context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        command: Command,
        label: &'static str,
        shortcut: &'static str,
        enabled: bool,
        destructive: bool,
    },
    /// A toggle with a check mark.
    Check {
        command: Command,
        label: &'static str,
        shortcut: &'static str,
        checked: bool,
    },
    Separator,
}

/// What the menu applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuContext {
    pub in_trash: bool,
    pub selected: usize,
    /// Every selected item is a folder.
    pub folders_only: bool,
    pub can_paste: bool,
    /// The current folder accepts new files.
    pub writable: bool,
    pub show_hidden: bool,
    /// The folder the menu applies to is bookmarked.
    pub bookmarked: bool,
}

fn item(command: Command, label: &'static str, shortcut: &'static str, enabled: bool) -> MenuEntry {
    MenuEntry::Item { command, label, shortcut, enabled, destructive: false }
}

fn bookmark(ctx: MenuContext) -> MenuEntry {
    if ctx.bookmarked {
        item(Command::RemoveBookmark, "Remove from Bookmarks", "Ctrl+D", true)
    } else {
        item(Command::AddBookmark, "Add to Bookmarks", "Ctrl+D", true)
    }
}

fn destructive(
    command: Command,
    label: &'static str,
    shortcut: &'static str,
    enabled: bool,
) -> MenuEntry {
    MenuEntry::Item { command, label, shortcut, enabled, destructive: true }
}

/// The menu for a right click on selected items.
pub fn item_menu(ctx: MenuContext) -> Vec<MenuEntry> {
    if ctx.in_trash {
        return vec![
            item(Command::Restore, "Restore", "", ctx.selected > 0),
            MenuEntry::Separator,
            destructive(Command::Delete, "Delete Permanently…", "Shift+Del", ctx.selected > 0),
        ];
    }
    let single = ctx.selected == 1;
    let mut entries = vec![item(Command::Open, "Open", "Enter", ctx.selected > 0)];
    if ctx.folders_only {
        entries.push(item(Command::OpenInNewWindow, "Open in New Window", "", ctx.selected > 0));
    } else {
        entries.push(item(Command::OpenWith, "Open With…", "", ctx.selected > 0));
    }
    entries.extend([
        MenuEntry::Separator,
        item(Command::Cut, "Cut", "Ctrl+X", ctx.writable),
        item(Command::Copy, "Copy", "Ctrl+C", true),
    ]);
    if ctx.folders_only && single {
        entries.push(item(Command::Paste, "Paste Into Folder", "", ctx.can_paste));
    }
    entries.extend([
        MenuEntry::Separator,
        item(Command::Rename, "Rename…", "F2", single && ctx.writable),
        item(Command::CopyLocation, "Copy Location", "", true),
    ]);
    if ctx.folders_only && single {
        entries.push(bookmark(ctx));
    }
    entries.extend([
        MenuEntry::Separator,
        item(Command::Trash, "Move to Trash", "Del", ctx.writable),
        destructive(Command::Delete, "Delete Permanently…", "Shift+Del", ctx.writable),
        MenuEntry::Separator,
        item(Command::Properties, "Properties", "Alt+Enter", true),
    ]);
    entries
}

/// The menu for a right click on empty space.
pub fn background_menu(ctx: MenuContext) -> Vec<MenuEntry> {
    if ctx.in_trash {
        return vec![
            item(Command::SelectAll, "Select All", "Ctrl+A", true),
            MenuEntry::Separator,
            destructive(Command::EmptyTrash, "Empty Trash…", "", true),
        ];
    }
    vec![
        item(Command::NewFolder, "New Folder…", "Ctrl+Shift+N", ctx.writable),
        item(Command::Paste, "Paste", "Ctrl+V", ctx.can_paste && ctx.writable),
        MenuEntry::Separator,
        item(Command::SelectAll, "Select All", "Ctrl+A", true),
        MenuEntry::Check {
            command: Command::ToggleHidden,
            label: "Show Hidden Files",
            shortcut: "Ctrl+H",
            checked: ctx.show_hidden,
        },
        item(Command::Reload, "Reload", "F5", true),
        MenuEntry::Separator,
        bookmark(ctx),
        item(Command::Properties, "Properties", "Alt+Enter", true),
    ]
}

/// The window menu in the header bar.
pub fn main_menu(ctx: MenuContext) -> Vec<MenuEntry> {
    let mut entries = vec![item(Command::NewWindow, "New Window", "Ctrl+N", true)];
    if ctx.in_trash {
        entries.extend([
            item(Command::SelectAll, "Select All", "Ctrl+A", true),
            MenuEntry::Separator,
            destructive(Command::EmptyTrash, "Empty Trash…", "", true),
        ]);
        return entries;
    }
    entries.extend([
        item(Command::NewFolder, "New Folder…", "Ctrl+Shift+N", ctx.writable),
        MenuEntry::Separator,
        item(Command::SelectAll, "Select All", "Ctrl+A", true),
        MenuEntry::Check {
            command: Command::ToggleHidden,
            label: "Show Hidden Files",
            shortcut: "Ctrl+H",
            checked: ctx.show_hidden,
        },
        item(Command::Reload, "Reload", "F5", true),
        MenuEntry::Separator,
        bookmark(ctx),
        item(Command::CopyLocation, "Copy Location", "Ctrl+Shift+C", true),
        item(Command::Properties, "Properties", "Alt+Enter", true),
    ]);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_menu() {
        let ctx = MenuContext { writable: true, ..MenuContext::default() };
        let menu = commands(&main_menu(ctx));
        assert_eq!(menu[0], (Command::NewWindow, true));
        assert!(menu.contains(&(Command::NewFolder, true)));
        assert!(menu.contains(&(Command::AddBookmark, true)));
        let trash = commands(&main_menu(MenuContext { in_trash: true, ..ctx }));
        assert_eq!(trash.last(), Some(&(Command::EmptyTrash, true)));
    }

    fn commands(entries: &[MenuEntry]) -> Vec<(Command, bool)> {
        entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { command, enabled, .. } => Some((*command, *enabled)),
                MenuEntry::Check { command, .. } => Some((*command, true)),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn file_menu() {
        let ctx = MenuContext { selected: 1, writable: true, ..MenuContext::default() };
        let menu = commands(&item_menu(ctx));
        assert_eq!(menu[0], (Command::Open, true));
        assert!(menu.contains(&(Command::OpenWith, true)));
        assert!(menu.contains(&(Command::Rename, true)));
        assert!(!menu.iter().any(|(c, _)| *c == Command::Paste));

        let many = commands(&item_menu(MenuContext { selected: 3, ..ctx }));
        assert!(many.contains(&(Command::Rename, false)));

        let read_only = commands(&item_menu(MenuContext { writable: false, ..ctx }));
        assert!(read_only.contains(&(Command::Trash, false)));
        assert!(read_only.contains(&(Command::Copy, true)));
    }

    #[test]
    fn folder_menu() {
        let ctx = MenuContext {
            selected: 1,
            folders_only: true,
            writable: true,
            can_paste: true,
            ..MenuContext::default()
        };
        let menu = commands(&item_menu(ctx));
        assert!(menu.contains(&(Command::OpenInNewWindow, true)));
        assert!(menu.contains(&(Command::Paste, true)));
        assert!(menu.contains(&(Command::AddBookmark, true)));
        assert!(!menu.iter().any(|(c, _)| *c == Command::OpenWith));
        let bookmarked = commands(&item_menu(MenuContext { bookmarked: true, ..ctx }));
        assert!(bookmarked.contains(&(Command::RemoveBookmark, true)));
    }

    #[test]
    fn trash_and_background_menus() {
        let trash = MenuContext { in_trash: true, selected: 2, ..MenuContext::default() };
        assert_eq!(
            commands(&item_menu(trash)),
            [(Command::Restore, true), (Command::Delete, true)]
        );
        assert_eq!(
            commands(&background_menu(trash)),
            [(Command::SelectAll, true), (Command::EmptyTrash, true)]
        );

        let bg = background_menu(MenuContext {
            writable: true,
            can_paste: false,
            show_hidden: true,
            ..MenuContext::default()
        });
        assert!(commands(&bg).contains(&(Command::Paste, false)));
        assert!(bg.contains(&MenuEntry::Check {
            command: Command::ToggleHidden,
            label: "Show Hidden Files",
            shortcut: "Ctrl+H",
            checked: true
        }));
    }
}
