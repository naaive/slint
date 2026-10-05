<!-- SPDX-License-Identifier: MIT -->

# Nimbus

Nimbus is a Wayland desktop environment written in Rust, with every user interface built in [Slint](https://slint.dev).
It covers the same ground as GNOME and KDE Plasma: a compositor, a desktop shell, system services, a settings app, and core apps.

![The Nimbus desktop with a terminal and the file manager tiled side by side](docs/screenshots/desktop-tiling.png)

## Features

### Compositor (`nimbus-compositor`)

- Built on [Smithay](https://github.com/Smithay/smithay), with three backends:
  a nested window for development (winit), DRM/KMS with libinput and libseat on a TTY (udev), and headless rendering into memory for tests.
- Window management with workspaces, floating placement, a master-stack tiling layout, maximize, fullscreen, minimize,
  interactive move and resize, and focus by direction.
- Protocols: xdg-shell, xdg-decoration, wlr-layer-shell, xdg-activation, linux-dmabuf, presentation-time, viewporter,
  fractional-scale, single-pixel-buffer, cursor-shape, idle-notify, idle-inhibit, ext-session-lock, keyboard-shortcuts-inhibit,
  ext-foreign-toplevel-list, primary selection, and ext/wlr data control for clipboard tools.
- Keyboard shortcuts from the configuration, with live reload.
- A control socket that speaks JSON lines, used by `nimbusctl`, the Settings app, and scripts.
- The shell runs in-process: it renders with Slint's software renderer into a buffer the compositor composites.
- Lock screen passwords go through PAM on a worker thread.
  logind lock requests and inactivity also lock the session.
- Screenshots of any output, from a key binding or `nimbusctl screenshot`.
- A built-in gradient backdrop when no wallpaper is set.

### Shell (`nimbus-shell`)

- A top panel with workspace dots, the focused app, a centered clock, and status icons.
- A dock with pinned and running apps, window indicators, and a context menu.
- A full-screen launcher with fuzzy search over installed desktop entries.
- An overview with workspace thumbnails and window cards you can drag between workspaces.
- Quick settings for volume, brightness, Wi-Fi, Bluetooth, do not disturb, dark style, media playback, and power.
- A notification server with toasts, actions, and a notification center next to the calendar.
- On-screen displays for volume and brightness keys, and a lock screen.

### Services (`nimbus-services`)

- `org.freedesktop.Notifications`, UPower, NetworkManager, BlueZ, MPRIS, logind, PipeWire or PulseAudio volume, and backlight.
- Each service degrades on its own: a missing daemon hides its feature and is picked up again when it appears.

### Apps

- **Settings** (`nimbus-settings`): appearance, panel and dock, workspaces, keyboard and mouse, shortcuts, power, displays, notifications, and about.
- **Files** (`nimbus-files`): grid and list views, search, a freedesktop trash, thumbnails, and copy and move with conflict handling and undo.
- **Terminal** (`nimbus-terminal`): tabs, color schemes, search, true color, and box drawing, on `alacritty_terminal`.
- **System Monitor** (`nimbus-monitor`): processes with sorting, tree view, and signals; CPU, memory, network, and disk graphs; and file systems.

### Session (`nimbus-session`)

- `nimbus-session` starts the compositor, sets up D-Bus and the session environment, runs autostart entries, and restarts the compositor after a crash.
- `nimbusctl` controls a running session from the command line.

## Architecture

Nimbus is a Cargo workspace of small crates with one-way dependencies.
Domain crates have no UI, the shell is a view over them, and the compositor wires everything together.
Every UI imports the `@nimbus/theme.slint` design system, so the shell and the apps look like one product.
See [docs/architecture.md](docs/architecture.md) for the crates, the threading model, and the protocols between them.

## Building

Install Rust 1.88 or newer and the development packages for Wayland, xkbcommon, libinput, libseat, GBM, DRM, EGL, udev, D-Bus, fontconfig, and PAM.
On Debian and Ubuntu:

```sh
sudo apt install libwayland-dev libxkbcommon-dev libinput-dev libseat-dev libgbm-dev libdrm-dev \
  libegl-dev libudev-dev libdbus-1-dev libfontconfig-dev libpam0g-dev
```

Then build from the repository root:

```sh
cargo build --manifest-path desktop/Cargo.toml --workspace --release
```

## Running

### Nested, Inside Another Desktop

Run the compositor in a window of your current Wayland or X11 session:

```sh
cargo run --manifest-path desktop/Cargo.toml -p nimbus-compositor -- --backend winit
```

Apps started from the launcher or with `nimbusctl spawn` open inside the nested session.

### On a TTY or From a Display Manager

Install Nimbus, then log in on a TTY and run `nimbus-session`, or pick "Nimbus" in your display manager:

```sh
desktop/data/install.sh --prefix /usr/local
```

The script builds every binary, installs the session file, desktop entries, the systemd user target, the portal configuration,
an example configuration, and the lock screen's PAM service in `/etc/pam.d/nimbus`.
Run it with `--uninstall` to remove everything again, and with `--help` for the other options.

### Headless

The headless backend renders into memory, which is useful for tests and screenshots:

```sh
NIMBUS_HEADLESS_OUTPUTS=1600x900 nimbus-compositor --backend headless
```

Apps need Slint's software renderer there, since there's no GPU: start them with `SLINT_BACKEND=winit-software`.

## Controlling the Desktop

`nimbusctl` talks to the compositor named by `$NIMBUS_SOCKET`, which the compositor sets for every app it starts.
Workspaces are numbered from 1, as in the panel.

```sh
nimbusctl state                 # windows, workspaces, and outputs
nimbusctl state --json          # the same as JSON
nimbusctl watch                 # one JSON line per event
nimbusctl spawn nimbus-files    # run a command in the session
nimbusctl workspace 2
nimbusctl move 7 3              # move window 7 to workspace 3
nimbusctl layout tiling
nimbusctl launcher              # toggle the launcher
nimbusctl overview              # toggle the overview
nimbusctl screenshot ~/shot.png
nimbusctl lock
nimbusctl quit
```

Run `nimbusctl --help` for every command.

## Configuration

Nimbus reads `$XDG_CONFIG_HOME/nimbus/config.toml` and applies changes as soon as the file is saved.
The Settings app edits the same file.
[data/config.toml](data/config.toml) lists every option with its default and a comment.

Default shortcuts:

| Keys | Action |
| --- | --- |
| Super+Space | Launcher |
| Super+Tab | Overview |
| Super+Return | Terminal |
| Super+E | Files |
| Super+Q | Close the window |
| Super+Up, Super+F, Super+H | Maximize, fullscreen, minimize |
| Super+T | Switch between floating and tiling |
| Super+1 to 9, Super+Shift+1 to 9 | Switch to, or move the window to, a workspace |
| Super+L | Lock the screen |
| Print | Screenshot |
| Super+Shift+E | Log out |

## Screenshots

These come from the headless compositor running the real shell and apps, and from each crate's screenshot tests.

| | |
| --- | --- |
| ![Desktop with the System Monitor](docs/screenshots/desktop-main.png) | ![Launcher](docs/screenshots/desktop-launcher.png) |
| ![Overview](docs/screenshots/desktop-overview.png) | ![A notification toast](docs/screenshots/desktop-notification.png) |
| ![Lock screen](docs/screenshots/desktop-lock.png) | ![Quick settings](docs/screenshots/shell-quick-settings.png) |
| ![Calendar and notifications](docs/screenshots/shell-calendar.png) | ![Settings](docs/screenshots/settings-main.png) |
| ![Files](docs/screenshots/files-main.png) | ![Terminal](docs/screenshots/terminal-main.png) |
| ![System Monitor resources](docs/screenshots/monitor-resources.png) | ![Theme gallery](docs/screenshots/theme-gallery-dark.png) |

More are in [docs/screenshots](docs/screenshots).
Regenerate the test screenshots with `NIMBUS_UPDATE_SCREENSHOTS=1 cargo test --manifest-path desktop/Cargo.toml --workspace`.

## Testing

```sh
cargo test --manifest-path desktop/Cargo.toml --workspace
cargo clippy --manifest-path desktop/Cargo.toml --workspace --all-targets -- -D warnings
```

The tests need no display.
The compositor tests start the real binary headless and drive it with a Wayland test client and the control socket,
with and without the shell.
The services tests start a private `dbus-daemon` with fake system daemons, and skip themselves when it's missing.

## Status and Known Limitations

Nimbus is young.
The headless backend and the shell run end to end in tests, but the udev and winit backends haven't been run on real hardware yet.

- There's no XWayland, so X11-only apps don't run.
- Nimbus draws no server-side decorations; apps draw their own title bars, which don't follow the Nimbus theme.
- Outputs are placed left to right at one global scale, and there's no output-management protocol;
  the Displays page in Settings only lists outputs.
- Touch, tablet, and pointer-constraint protocols aren't implemented.
- Screen blanking after inactivity and suspend on lid close aren't implemented yet; locking after inactivity is.
- Overview cards show app icons, not live window thumbnails.
- `xdg-desktop-portal-wlr` screen casting needs wlr-screencopy, which Nimbus doesn't offer yet.
- The UI is English only.
- Each crate's README lists its own gaps.
