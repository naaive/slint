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
- Themed titlebars for apps that don't draw their own, with minimize, maximize, and close buttons, moving by drag,
  maximizing by double click, and resize borders; tiled windows get a slim bar.
  They follow the color scheme, accent, and font of `[appearance]` as it changes.
- Protocols: xdg-shell, xdg-decoration, wlr-layer-shell, xdg-activation, linux-dmabuf, presentation-time, viewporter,
  fractional-scale, single-pixel-buffer, cursor-shape, idle-notify, idle-inhibit, ext-session-lock, keyboard-shortcuts-inhibit,
  ext-foreign-toplevel-list, wlr-output-management, wlr-output-power-management, wlr-screencopy, ext-image-copy-capture,
  primary selection, and ext/wlr data control for clipboard tools.
- Display configuration through wlr-output-management, from Settings or tools such as `wlr-randr` and `kanshi`,
  remembered per display and applied again at startup and on hotplug.
- Input methods such as fcitx5 and IBus, through text-input-v3, input-method-v2, and virtual-keyboard-v1,
  with their candidate popups below the text cursor.
- Screens turn off after `[power] blank_after_minutes` of inactivity, or with `nimbusctl blank`, and back on with the next input.
  A visible window or layer surface with an idle inhibitor, such as a playing video, keeps them on.
  Idle daemons such as `swayidle` and tools such as `wlopm` turn single screens off and on through wlr-output-power-management.
- Keyboard shortcuts from the configuration, with live reload.
- Alt+Tab and Super+Tab switch windows in the order they last had focus, with the shell's switcher or, without the shell, by focusing each window in turn.
- Touchpad gestures for apps through pointer-gestures, and a three-finger horizontal swipe that switches workspaces,
  with the finger count in `[input] workspace_swipe_fingers`.
- Games and remote desktops lock or confine the pointer through relative-pointer and pointer-constraints.
- Touchscreens through `wl_touch` and drawing tablets through tablet-v2, on the udev backend.
- A control socket that speaks JSON lines, used by the shell, `nimbusctl`, the Settings app, and scripts.
- No UI of its own: the shell is a separate client, so a crashed shell doesn't take windows down.
  A locked session stays locked, and black, until a lock client takes over again.
- Screenshots of any output, from a key binding or `nimbusctl screenshot`.
- Screen capture of outputs, regions, and windows for tools such as `grim` and for screen sharing through `xdg-desktop-portal-wlr`,
  into shm buffers or GPU dmabufs, with damage tracking; captures show only black while the session is locked.
- X11 apps through [xwayland-satellite](https://github.com/Supreeeme/xwayland-satellite),
  which starts when the first X11 app connects and again after it exits.
- A built-in gradient backdrop when no wallpaper is set.

### Shell (`nimbus-shell`)

- A top panel with workspace dots, the focused app, a centered clock, and status icons.
- A dock with pinned and running apps, window indicators, and a context menu.
- A full-screen launcher with fuzzy search over installed desktop entries.
- An overview with workspace thumbnails and window cards you can drag between workspaces.
- Quick settings for volume, brightness, Wi-Fi, Bluetooth, do not disturb, dark style, media playback, and power.
- A notification server with toasts, actions, and a notification center next to the calendar.
- On-screen displays for volume and brightness keys, and a lock screen.
- A window switcher in the middle of the output, with the icon and title of each window, while Alt+Tab is held.
- A polkit authentication dialog on the focused output, with an identity picker when several admins may answer.
- Input methods such as fcitx5 and IBus type into the launcher search, the lock screen, and the polkit dialog through text-input-v3;
  password fields tell the input method they're passwords and never share their text.
- Inserted USB sticks and memory cards mount on their own, unless `[media] automount` is off,
  and a toast offers to open them in the file manager.
- The `nimbus-shell` binary runs the shell as a Wayland client, on layer-shell and session-lock surfaces.
  It hosts the system services and the polkit agent, checks lock screen passwords through PAM on a worker thread,
  and locks on logind requests and after inactivity.

### Services (`nimbus-services`)

- `org.freedesktop.Notifications`, UPower, NetworkManager, BlueZ, MPRIS, logind, udisks, PipeWire or PulseAudio volume, and backlight.
- A udisks client for removable media: the volumes worth showing, and mounting, unmounting, ejecting, and powering off,
  which apps can run on a thread of its own.
- The session's polkit authentication agent, which checks responses through polkit's setuid `polkit-agent-helper-1`, never in process.
- NetworkManager and BlueZ clients for network and Bluetooth settings, with a BlueZ pairing agent, which apps run on a thread of their own.
- Clients for sound settings, through `pactl`, and date and time settings, through systemd-timedated.
- Each service degrades on its own: a missing daemon hides its feature and is picked up again when it appears.

### Portal (`nimbus-portal`)

- An `xdg-desktop-portal` Settings backend, so apps that follow the portal pick up the Nimbus color scheme and accent color, live.

### Apps

- **Settings** (`nimbus-settings`): appearance, panel and dock, workspaces, keyboard and mouse, shortcuts, power, notifications, about,
  and displays, which you arrange by dragging and set up with resolution, refresh rate, scale, and rotation.
  Join Wi-Fi networks with a password, forget them, and see the addresses of each connection, wired ones too, through NetworkManager.
  Pair Bluetooth devices by confirming or entering a code, then connect, disconnect, or remove them, through BlueZ.
  Quick settings open these pages from the Wi-Fi and Bluetooth tiles.
  Choose the output and input device, and set each device's volume and mute, through PipeWire or PulseAudio.
  Pick the time zone from a searchable list, turn network time on or off through systemd-timedated, and switch the clock to 24 hours.
  Choose the default browser, mail client, file manager, terminal, text editor, and image, video, and music players.
- **Files** (`nimbus-files`): grid and list views, search, a freedesktop trash, thumbnails, and copy and move with conflict handling and undo.
  The sidebar lists drives and partitions from udisks: open one to mount it,
  and unmount, eject, or safely remove it from its menu or eject button.
- **Terminal** (`nimbus-terminal`): tabs, color schemes, search, true color, and box drawing, on `alacritty_terminal`.
- **System Monitor** (`nimbus-monitor`): processes with sorting, tree view, and signals; CPU, memory, network, and disk graphs; and file systems.

### Session (`nimbus-session`)

- `nimbus-session` starts the compositor and the shell, sets up D-Bus and the session environment, and runs autostart once per session.
  It starts gnome-keyring's Secret Service when nothing else provides one, and exports the variables it prints.
  It restarts the shell whenever it exits, with a growing delay,
  and both after a compositor crash, locked if the screen was locked.
- `nimbusctl` controls a running session from the command line.

## Architecture

Nimbus is a Cargo workspace of small crates with one-way dependencies.
Domain crates have no UI, the shell is a view over them, and the shell process wires them together.
The compositor knows only Wayland and the control socket.
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

Build the workspace, then run a session in a window of your current Wayland or X11 session.
`nimbus-session` finds the compositor and the shell next to itself:

```sh
cargo build --manifest-path desktop/Cargo.toml --workspace
target/debug/nimbus-session --backend winit --no-autostart
```

Apps started from the launcher or with `nimbusctl spawn` open inside the nested session.

### On a TTY or From a Display Manager

Install Nimbus, then log in on a TTY and run `nimbus-session`, or pick "Nimbus" in your display manager:

```sh
desktop/data/install.sh --prefix /usr/local
```

The script builds every binary, installs the session file, desktop entries, the systemd user target, the portal backend and its configuration,
an example configuration, and the lock screen's PAM service in `/etc/pam.d/nimbus`.
Run it with `--uninstall` to remove everything again, and with `--help` for the other options.

To unlock the login keyring with your login password, add `pam_gnome_keyring` to the PAM service you log in through,
such as `/etc/pam.d/login`, `gdm-password`, or `sddm`, as your distribution documents:

```text
auth     optional  pam_gnome_keyring.so
session  optional  pam_gnome_keyring.so auto_start
```

`nimbus-session` then hands the daemon PAM started its Secret Service, already unlocked.
The lock screen's `/etc/pam.d/nimbus` unlocks the keyring again with the password that unlocks the screen.

### Headless

The headless backend renders into memory, which is useful for tests and screenshots:

```sh
NIMBUS_HEADLESS_OUTPUTS=1600x900 nimbus-compositor --backend headless
```

Apps need Slint's software renderer there, since there's no GPU: start them with `SLINT_BACKEND=winit-software`.

### The Compositor and Shell by Hand

Start the compositor, then start `nimbus-shell` with the variables the compositor prints:

```sh
nimbus-compositor --backend winit
# NIMBUS_READY WAYLAND_DISPLAY=wayland-1 NIMBUS_SOCKET=/run/user/1000/nimbus-wayland-1.sock
WAYLAND_DISPLAY=wayland-1 NIMBUS_SOCKET=/run/user/1000/nimbus-wayland-1.sock nimbus-shell
```

The shell renders with OpenGL on a GPU and in software otherwise.
Set `NIMBUS_SHELL_RENDERER=software` or `NIMBUS_SHELL_RENDERER=gl` to choose.

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
nimbusctl blank                 # turn the screens off until the next input
nimbusctl quit
```

Run `nimbusctl --help` for every command.

## Configuration

Nimbus reads `$XDG_CONFIG_HOME/nimbus/config.toml` and applies changes as soon as the file is saved.
The Settings app edits the same file.
The compositor saves display settings under `[[outputs]]` whenever a display configuration is applied.
[data/config.toml](data/config.toml) lists every option with its default and a comment.

Default shortcuts:

| Keys | Action |
| --- | --- |
| Super+Space | Launcher |
| Super+S | Overview |
| Alt+Tab or Super+Tab, with Shift to go back | Switch windows |
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
The compositor tests start the real binary headless and drive it with a Wayland test client and the control socket.
The shell tests run the `nimbus-shell` binary against the headless compositor and check its screenshots.
The session tests supervise shell scripts that stand in for the compositor and the shell.
The services tests start a private `dbus-daemon` with fake system daemons, and skip themselves when it's missing.

Before a first run on a real machine, follow the [hardware testing checklist](docs/hardware-testing.md).
Attach the archive from `data/nimbus-bug-report.sh` to bug reports; it redacts secrets, but read it before you share it.

## Status and Known Limitations

Nimbus is young.
The headless backend and the shell run end to end in tests, but the udev and winit backends haven't been run on real hardware yet.

- The shell process hasn't run on the winit or udev backends yet.
  It doesn't take touch input, and it draws the default cursor everywhere.
- The shell's OpenGL renderer has only run on Mesa's llvmpipe, against the headless compositor.
  It's untested on GPUs, and it redraws whole surfaces for each frame.
- Typing on the shell's lock screen and unlocking have no end-to-end test.
  The headless compositor feeds synthetic clicks and keys, but PAM needs a real account.
- The shell's popups and its overlay with the launcher and overview don't animate when they close, since their surfaces go right away.
- Toasts and the OSD show above fullscreen windows.
- A click on an empty spot of the panel doesn't close an open popup; a click on a window or the dock does.
- The shell handles dead keys and Compose sequences with the locale's XKB compose table.
  A key that completes or cancels a sequence still sends its own key release.
- X11 apps need xwayland-satellite 0.6 or later; without it, `DISPLAY` stays unset.
  XWayland has only run against a script standing in for xwayland-satellite.
  Changing `[xwayland]` takes effect when the compositor restarts.
- Apps that draw their own title bars, such as GTK apps, don't follow the Nimbus theme.
  An app that doesn't use xdg-decoration and doesn't set its window geometry gets a titlebar, even if it draws one of its own.
  A client that destroys its decoration object keeps the titlebar.
- Titlebars have no window menu, take no touch or tablet input, and don't show in window captures.
  The `system` color scheme always draws dark titlebars.
  Titlebars have only run headless.
- Display configuration has only run headless.
  On udev, a test checks modes and free CRTCs but not the kernel's bandwidth limits; an apply that hits them rolls back.
- Display settings are stored per display, not as profiles for each set of connected displays.
  Custom modes and adaptive sync aren't supported.
- Any client may configure displays through wlr-output-management, as in other wlroots-style compositors.
- Touch, tablets, gestures, and pointer constraints have only run headless; no real touchscreen, tablet, or touchpad has driven them.
  A touchscreen maps to the built-in panel, or else the first output, and a tablet to the whole desktop; neither can be assigned to an output.
  Touch has no compositor gestures, and tablet pads aren't supported.
  The workspace swipe switches when the fingers lift, without following them.
  A confined pointer slides along its region's edges, but a fast motion can jump a gap between two of the region's rectangles.
- Input methods have only run against test clients, not fcitx5 or IBus.
  Apps that speak only text-input-v1 or v2, such as Chromium and Electron by default, get no input method.
  Any client may become the input method or create a virtual keyboard, as in other wlroots-style compositors.
  While the session is locked, the input method gets no keys and its popups don't show.
  The shell applies an input method's changes and reports its text fields without checking the serial of `done`.
  Typing into the polkit dialog through an input method has no end-to-end test.
- Suspend on lid close isn't implemented yet.
- Screen blanking has only run headless; turning screens off through DPMS on udev is untested on real displays.
  Blanking doesn't come sooner while the session is locked, and an output that's off shows black in screen captures.
  Any input but a key release turns blanked screens on, and so does plugging in an input device.
- Overview cards show app icons, not live window thumbnails.
- The window switcher lists the windows of every workspace, one per window, not grouped by app.
  It takes no clicks or arrow keys, and keys other than Tab and Escape reach the focused window while it's open.
- When an `ext-session-lock` client dies, the session stays locked and black until another client locks it.
  The shell locks again right away, or once `nimbus-session` restarts it after a crash.
- The compositor checks a lock surface's size only on commits that attach a buffer.
- The lock marker is per `XDG_RUNTIME_DIR`, so a nested compositor or session in the same runtime directory can create or remove the real session's marker.
- If `nimbus-session` is killed with SIGKILL, autostarted apps keep running.
  An app that moves itself to a new process group or session isn't stopped at logout either.
- Saving the configuration replaces `config.toml` with a new file.
  A symlinked `config.toml` becomes a regular file, and an editor that saves in place doesn't take the configuration lock.
- Screen capture has only run headless against test clients, into shm buffers; dmabuf capture and `xdg-desktop-portal-wlr` are untested.
  Any client may capture the screen, as in other wlroots-style compositors.
  Cursor capture sessions stop right away, and window captures never draw the cursor.
- The portal backend answers only `org.freedesktop.appearance`.
  It reports no contrast preference, since the configuration has no high contrast option.
  Older `xdg-desktop-portal` releases look for backends only in their own data directory, usually `/usr/share`;
  install with `--prefix /usr` for them to find `nimbus.portal`.
- The polkit agent has only run against a fake polkitd and a script standing in for `polkit-agent-helper-1`.
  It registers only inside a logind session, so a nested session leaves polkit to the host desktop's agent.
- Removable media have only run against a fake udisks, and gnome-keyring against a script standing in for it.
- Files lists only the volumes udisks doesn't mark as system or ignored, so partitions of internal disks don't appear unless mounted.
  Encrypted volumes don't appear, since there's no way to unlock them yet.
- Settings has no switch for `[media] automount` yet.
  It doesn't use polkit's socket-activated helper, which distributions that drop the helper's setuid bit need.
- polkit requests wait while the session is locked, and their dialog shows once it unlocks.
- The Network and Bluetooth pages have only run against fake NetworkManager and BlueZ daemons.
  They show only the first Wi-Fi device and the first Bluetooth adapter.
- Settings can't join enterprise or hidden Wi-Fi networks, and doesn't edit IP settings, VPNs, or proxies.
  It saves Wi-Fi passwords with the connection, for every user, as `nmcli` does, and isn't a NetworkManager secret agent.
- Settings is BlueZ's pairing agent only while it runs; with it closed, a device that asks to pair needs another agent.
- The Sound page needs `pactl` 16 or later, which `pipewire-pulse` serves on PipeWire, and has only run against a script standing in for it.
  It doesn't choose ports, profiles, or per-app volumes.
- The Date & Time page has only run against a fake timedated, and can't set the date or time by hand.
  Dismissing polkit's dialog shows the refusal in a banner.
- Settings writes the default terminal to `xdg-terminals.list`, which `xdg-terminal-exec` reads; the Super+Return shortcut still starts `nimbus-terminal`.
- The UI is English only.
- Each crate's README lists its own gaps.
