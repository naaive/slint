<!-- SPDX-License-Identifier: MIT -->

# Nimbus Files

The file manager of the Nimbus desktop (`nimbus-files`, app id `org.nimbus.Files`).

![Files, dark](../../docs/screenshots/files-main.png)

```text
nimbus-files [PATH]
```

`PATH` may be a folder or a `file://` URI; without it, Files opens the home folder.

## Features

- Icon grid with three zoom levels and a list with sortable Name, Size, Type, and Modified columns.
- Thumbnails for PNG, JPEG, GIF, WebP, BMP, and SVG, shared with other desktops through the freedesktop.org thumbnail cache.
  High-density screens get the 256-pixel "large" size.
- Breadcrumb path bar; click a segment, or press Ctrl+L to type a path, `~/…`, or a `file://` URI.
- Filter the folder as you type, or search its subfolders on a background thread.
- Sidebar with the XDG user folders that exist, Trash, mounted drives, and GTK bookmarks, updated live.
- Copy, move, trash, delete, rename, and new folder, on background threads with progress, cancellation,
  and Skip, Replace (or Merge), and Keep Both for name conflicts.
- Trash per the freedesktop.org specification, including per-drive trash folders; restore, delete, empty, and undo.
- Properties with type, recursive size, dates, owner, and permissions; Open With from installed desktop entries.
- The folder refreshes when its content changes, touching only the rows that changed.

## Keyboard

| Keys | Action |
| --- | --- |
| Arrows, Home, End, Page Up/Down | Move; with Shift, extend the selection; with Ctrl, move without selecting |
| Ctrl+Space | Toggle the item under the cursor |
| Enter | Open |
| Backspace, Alt+Up | Parent folder |
| Alt+Left, Alt+Right | Back, forward |
| Typing a name | Jump to a matching item |
| Ctrl+A | Select all |
| Ctrl+C, Ctrl+X, Ctrl+V | Copy, cut, paste |
| F2 | Rename |
| Delete, Shift+Delete | Move to the Trash, delete permanently |
| Ctrl+Shift+N | New folder |
| Alt+Enter | Properties |
| Shift+F10, Menu | Context menu |
| Ctrl+L, Ctrl+F | Edit the location, search |
| Ctrl+H | Show hidden files |
| Ctrl+1, Ctrl+2 | Grid, list |
| Ctrl+Plus, Ctrl+Minus, Ctrl+0 | Zoom the grid |
| Ctrl+D | Add or remove a bookmark |
| F5, Ctrl+R | Reload |
| Ctrl+N, Ctrl+W | New window, close |

## Design

- `src/core/` is the model and has no UI dependency: listing, MIME types, sorting, selection, history,
  the operations engine, trash, places, thumbnails, and properties.
- `src/app/` connects it to the window. Blocking work runs on worker threads that send messages over a channel;
  the UI thread receives them with `slint::spawn_local`.
- `ui/` holds the Slint files, built on the `@nimbus/theme.slint` design system.
- View preferences are saved to `$XDG_CONFIG_HOME/nimbus/files.toml`; the theme follows the Nimbus configuration live.

## Testing

```sh
cargo test --manifest-path desktop/Cargo.toml -p nimbus-files
```

The unit tests cover `core` against temporary folders, `tests/ui.rs` drives the window on Slint's testing backend,
and `tests/screenshot.rs` renders it with the software renderer.
Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/files-main.png`.
The hidden `--screenshot <PNG>` option renders sample data, with `--light`, `--list`,
and `--scene menu|properties|rename|search|toast|trash`.
