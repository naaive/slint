# Nimbus Architecture

Nimbus is a Wayland desktop environment written in Rust, with every user interface built in Slint.
It covers the same ground as GNOME and KDE Plasma: a compositor, a desktop shell, system services, and core apps.

## Design Principles

- **Wayland only.** There's no X11 session; XWayland is optional and runs on demand.
- **The shell is a client.** `nimbus-shell` shows the shell on `wlr-layer-shell`, `xdg_popup`, and `ext-session-lock` surfaces,
  and talks to the compositor over the control socket, so a crashed shell never takes windows down.
  The compositor has no Slint, no system services, and no PAM; it speaks Wayland protocols and the control socket.
- **Crates are boundaries.** Domain crates (`nimbus-ipc`, `nimbus-config`, `nimbus-xdg`, `nimbus-services`) have no UI.
  The shell is a view over them and doesn't know how it's displayed.
  The shell process is the only crate that wires them together.
- **Single-threaded UI, async edges.** The compositor and the shell process each run one `calloop` event loop on the main thread;
  the shell's also drives Slint.
  D-Bus work runs on a Tokio runtime in `nimbus-services` and reaches the main thread through a `calloop` channel.
- **Degrade, don't fail.** A missing D-Bus daemon, backlight, or battery hides that feature; it never stops the session.
- **Standards first.** Desktop entries, icon themes, `org.freedesktop.Notifications`, MPRIS, UPower, NetworkManager, logind, the portal Settings interface,
  and `xdg-shell`, `xdg-decoration`, `wlr-layer-shell`, `xdg-activation`, and `wlr-output-management` on the Wayland side.

## Crates

| Crate | Kind | Responsibility |
| --- | --- | --- |
| `nimbus-ipc` | lib | Window/workspace model; JSON-lines protocol on the control socket; blocking client; runtime paths of the control socket and lock marker; the compositor's ready line (`Ready`). |
| `nimbus-config` | lib | TOML configuration schema with defaults, the stored display layout (`[[outputs]]`) and its edge-to-edge geometry (`geometry`), atomic save, locked load-modify-save (`nimbus_config::update`), the key chord grammar (`chord`), and file watching. |
| `nimbus-xdg` | lib | Desktop entries, icon theme lookup, fuzzy app search, launching. |
| `nimbus-services` | lib | Tokio + zbus: notifications server, UPower, NetworkManager, audio, backlight, MPRIS, BlueZ, logind, udisks, and the polkit authentication agent. |
| `nimbus-theme` | lib + Slint library | Design tokens and components imported as `@nimbus/theme.slint`; off-screen software rendering for screenshots behind the `headless` feature. |
| `nimbus-shell` | lib + preview bin | Panel, dock, launcher, overview, quick settings, notification center, toasts, OSD, lock screen: one shared model shown by a view per output, in a window per part. |
| `nimbus-compositor` | bin | Smithay compositor: backends, displays and wlr-output-management, window management, input, keyboard shortcuts, the session lock, control socket. |
| `nimbus-shell-host` | bin `nimbus-shell` | The shell process: a Wayland client showing `nimbus-shell` on layer-shell and session-lock surfaces, with PAM, idle locking, and the system services. |
| `nimbus-portal` | bin + lib | `xdg-desktop-portal` Settings backend publishing `org.freedesktop.appearance` from `nimbus-config`. |
| `nimbus-session` | bins | `nimbus-session` starts and supervises the compositor and the shell, makes sure there's a Secret Service, and runs autostart; `nimbusctl` is the command-line client. |
| `nimbus-settings` | app | System settings, editing `nimbus-config`; display configuration through wlr-output-management; networks and Bluetooth through `nimbus-services`. |
| `nimbus-files` | app | File manager, with the volumes udisks can mount in its sidebar. |
| `nimbus-terminal` | app | Terminal emulator on `alacritty_terminal`. |
| `nimbus-monitor` | app | System monitor on `sysinfo`. |
| `nimbus-test-support` | dev lib | Harness for integration tests: the headless compositor, Wayland test clients, and a private `dbus-daemon`. |

Dependency direction (arrows point at dependencies):

```text
nimbus-shell-host ──> nimbus-shell ──> nimbus-theme ──> nimbus-config
        │                  ├──> nimbus-ipc
        │                  ├──> nimbus-xdg
        │                  └──> nimbus-services
        └──> (all of the above)
nimbus-compositor ──> nimbus-ipc, nimbus-config, nimbus-xdg
apps ──> nimbus-theme[headless], nimbus-config
nimbus-files ──> nimbus-xdg, nimbus-services
nimbus-settings ──> nimbus-ipc, nimbus-services
nimbus-session ──> nimbus-ipc, nimbus-config
nimbus-portal ──> nimbus-theme, nimbus-config
```

Processes in a session (arrows point from the starting process):

```text
display manager or TTY
  └─> nimbus-session ─┬─> nimbus-compositor   restarted after a crash, with --locked while the lock marker exists
                      ├─> nimbus-shell        started once the compositor is ready; restarted whenever it exits
                      └─> autostart           once per session, each in its own process group
nimbus-shell ──Wayland and control socket──> nimbus-compositor
D-Bus activation ──> nimbus-portal
```

## Compositor

Modules in `crates/nimbus-compositor/src`:

- `main.rs`: argument parsing (`--backend winit|udev|headless`), logging, startup.
- `lock.rs`: the session lock state, the `ext-session-lock` client holding it, and its lock surfaces; see [Locking](#locking).
- `lock_marker.rs`: keeps the lock marker in step with the lock state.
- `state.rs`: the `Nimbus` state struct and Smithay handler implementations
  (compositor, xdg-shell, xdg-decoration, layer-shell, seat, data device, primary selection, ext and wlr data control,
  output, shm, dmabuf, xdg-activation, presentation, viewporter, fractional scale,
  ext-foreign-toplevel-list, ext-idle-notify, and idle-inhibit).
  `state/protocols.rs` also serves `ext-session-lock` itself, without Smithay's implementation, on the state in `lock.rs`.
- `outputs/`: displays; see [Displays](#displays).
  `layout.rs` turns `[[outputs]]` into a layout and back, `management.rs` serves wlr-output-management,
  and `edid.rs` reads the make, model, and serial number of a DRM connector's display.
- `backend/winit.rs`: nested session in a window, for development.
- `backend/udev.rs`: DRM/KMS, GBM, libinput, and libseat for a real session, with hotplug and display configuration.
- `backend/headless.rs`: no output device; renders with Pixman into memory, for tests and screenshots.
- `wm/`: window management: workspaces, focus stack, floating placement, a tiling layout (master-stack),
  maximize/fullscreen/minimize, interactive move and resize,
  and the exclusive zones of layer-shell surfaces, such as the shell's panel and dock.
  Layouts implement a `Layout` trait so more can be added.
- `input/`: input routing and user activity for `ext-idle-notify`.
  `keyboard.rs` runs keyboard shortcuts from `nimbus-config`, VT switches, and the emergency exit;
  `pointer.rs` finds the surface under the pointer, focuses on click or hover, and runs interactive move and resize.
  Layer surfaces with `exclusive` keyboard interactivity on the top or overlay layer take the keyboard;
  `on_demand` ones take it when clicked.
- `keybindings.rs`: resolves chords parsed with `nimbus_config::chord` to XKB keysyms.
- `actions.rs`: control requests and keybinding actions.
  Shortcuts aimed at the shell become `Event::ShellCommand`s; see [Shell](#shell).
  On the headless backend, `Request::Click` and `Request::PressKey` feed input as if a user made it, for tests.
- `ipc.rs`: the control socket server, a `calloop` source per connection.
- `render.rs`: the scene shared by all backends, the wallpaper or built-in gradient backdrop, and screenshots.
- `xwayland/`: the X11 display for X11 apps; see [XWayland](#xwayland).

Command line, which `nimbus-session` relies on:

```text
nimbus-compositor [--backend winit|udev|headless] [--socket <wayland socket name>] [--x11-display :<n>] [--config <path>] [--locked]
```

When the Wayland and control sockets accept connections, the compositor sets `WAYLAND_DISPLAY`, `NIMBUS_SOCKET`,
and `DISPLAY` when it serves X11 apps, for its children, and prints exactly one line to standard output:
`NIMBUS_READY WAYLAND_DISPLAY=<name> [DISPLAY=:<n>] NIMBUS_SOCKET=<path>`.
`nimbus_ipc::Ready` formats and parses this line.
All logging goes to standard error.
The headless backend creates one 1920x1080 virtual output, `HEADLESS-1`, or one per size listed in `NIMBUS_HEADLESS_OUTPUTS` such as `1280x720,1920x1080`.
`Request::Quit` and the emergency exit end the compositor with status 0, which `nimbus-session` takes as a logout.

### XWayland

X11 apps run through [`xwayland-satellite`](https://github.com/Supreeeme/xwayland-satellite),
a separate process that runs `Xwayland` rootless and is an ordinary Wayland client of the compositor, as in niri.
At startup, unless `[xwayland] enabled = false`, the compositor asks the satellite (`[xwayland] path`, or `xwayland-satellite` in `PATH`)
whether it takes `-listenfd`, which needs version 0.6 or later.
It then takes the X11 display that `--x11-display` names, or the lowest free one, the way X servers do:
the lock file `/tmp/.X<n>-lock` with its process id, replacing one whose process is gone,
and listening sockets at `/tmp/.X11-unix/X<n>` and the abstract address of the same name.
`xwayland/display.rs` holds them and removes the files when the compositor exits.

The compositor watches the sockets, and on the first connection starts `xwayland-satellite :<n> -listenfd <fd> -listenfd <fd>`
with both sockets inherited, then stops watching until the satellite exits.
Clients wait in the sockets' backlog meanwhile, so none is lost while it starts or restarts;
the next connection after an exit starts it again.
Without a usable satellite, the compositor logs why once and leaves `DISPLAY` unset.
`NIMBUS_X11_DIR` replaces `/tmp` in tests.

## Displays

A head is a connected display, which the backend describes as a smithay `Output` with its modes, preferred mode, and EDID identity.
It's enabled while it's mapped in the window manager's space; only enabled heads have a `wl_output` global.
Every change of a head's mode, position, scale, transform, or enabled state goes through `Nimbus::configure_outputs`:

1. `outputs::layout::validate` checks what every backend needs: one head stays on, scales from 0.25 to 8, supported modes,
   and each enabled head shares an edge with another, so the pointer can reach it.
2. The backend's `OutputBackend::apply_outputs` sets up the devices, or with `test` only checks that it could.
3. The compositor applies the layout to the heads, moves windows along with moved outputs, and tells clients.

What each backend can do:

| Backend | Applies |
| --- | --- |
| udev | Everything. It plans the CRTCs first, then turns heads off, changes modes, and turns heads on, and rolls back on failure. A test checks modes and free CRTCs; the kernel's own check comes with the apply. |
| headless | Everything; the modes are the sizes from `NIMBUS_HEADLESS_OUTPUTS`. |
| winit | Position and scale. The window's size is the mode, and the transform is fixed, because GL draws bottom-up. |

The configuration's `[[outputs]]` entries store the layout, one per display.
An entry matches a display by make, model, and serial number when both have a serial number, and by connector otherwise.
`Nimbus::reconfigure_outputs` applies the stored layout at startup, on hotplug, after a VT switch back, and when the entries or `appearance.scale` change in the file.
It falls back to the defaults when the backend refuses the stored layout:
the preferred mode, `appearance.scale`, the native transform, and a place to the right of the other displays.
When stored positions leave displays apart from the rest, as after unplugging the middle one of three,
they move to the nearest edge of the largest group (`nimbus_config::geometry::join`).
When heads grow into each other, as when `appearance.scale` goes down, they're pushed apart to the right or down,
and their new positions are saved.

wlr-output-management (version 4) advertises every head, enabled or not, and sends each client only what changed, followed by `done` with a new serial.
A configuration made with an older serial is cancelled.
Custom modes are accepted when they match a supported mode within 1 Hz, and adaptive sync can't be turned on.
The compositor saves every applied configuration to `[[outputs]]` through `nimbus_config::update` on a worker thread, unless nothing changed.
An entry records whether each head is on and its position,
plus the mode, transform, and scale that the client set; what it didn't set keeps the stored value.
So `appearance.scale` keeps applying to a head until a client sets its scale.

The Displays page in Settings is a wlr-output-management client on its own thread and Wayland connection (`displays/wlr.rs`).
It arranges displays by dragging, attaching each to the nearest edge of another, sets mode, scale, rotation, and whether each display is on,
sends only what the user changed,
and reverts an applied configuration unless the user keeps it within 15 seconds.
Without the protocol, it lists the outputs that the control socket reports, read-only.

## Network and Bluetooth

The Network and Bluetooth pages in Settings use two clients in `nimbus-services`, apart from `Services`:
`nm::Client` for NetworkManager and `bluez::Client` for BlueZ.
Each runs a `BusService` on a thread of its own with a Tokio runtime (`worker.rs`), so it follows its daemon as it comes and goes,
and reports a whole snapshot, re-read after each burst of signals, as one event.
The shell can use them too, such as for a network list in quick settings.

`nm::Client` lists the first Wi-Fi device's access points, one network per SSID at its strongest access point,
marked when a saved connection has its SSID, and the Ethernet devices, with the addresses, gateway, and DNS of each active connection.
Connecting activates the saved connection to the SSID, after storing a new password in it if one was typed.
Without one, it sends `AddAndActivateConnection` with `802-11-wireless-security` set from the access point's flags:
`wpa-psk`, `sae`, `owe`, or WEP, with the password; enterprise networks aren't supported.
It follows the active connection's `StateChanged` until it's activated or fails.
A connection it added that fails is deleted again.
A failure for missing secrets or a failed login asks for the password through `Event::ConnectFailed`.
Forgetting deletes every saved connection with the SSID; disconnecting calls `Disconnect` on the Wi-Fi device, which keeps it from connecting again by itself.

`bluez::Client` lists the first adapter and its devices from the object manager, leaving out devices that are neither paired nor named.
It registers `org.bluez.Agent1` with the `KeyboardDisplay` capability and makes it the default agent while it runs,
so pairing from Settings, or a device that asks to pair, reaches the app as `Event::Pairing`.
The agent answers only BlueZ's unique name, and waits for `Command::Answer` until BlueZ cancels the request.
`Pair`, `Connect`, and `Disconnect` calls run in the background with a 90-second timeout, since BlueZ waits for the device and the user;
a device that paired is trusted and connected.

The Network page scans when it opens, and the Bluetooth page looks for devices while it shows and the adapter is on.
Both report failures in the banner.
The shell's quick settings open the pages with `nimbus-settings --page network` and `--page bluetooth`.

## Shell

The shell is one `ShellModel`, the single source of truth, shown by lightweight views.
Data flows into the model, and every view emits `ShellAction`s through it; see `crates/nimbus-shell/src/lib.rs`.

- `ShellView`: the shell on one output.
  It keeps only what's particular to its output, such as its open popup, launcher, and the windows on it.
  It has no window of its own: `ShellView::parts()` lists the parts to show now,
  and `ShellView::create()` makes the window of one, which a host shows on a surface of its own.
- `LockView`: the lock screen for one output, a window of its own,
  so a host can put it on its own surface, such as an `ext-session-lock` surface.

The parts of an output, in the order a host creates their surfaces:

| Part | Window | Where it goes |
| --- | --- | --- |
| `Panel` | `PanelWindow` | Along the top or bottom edge, as tall as the panel, reserving its height. It slides away while a fullscreen window is focused. |
| `Dock` | `DockWindow` | Centered on the bottom edge, sized to the dock with room for its tooltips, reserving the dock and its margins unless it hides automatically. Only the dock, or the strip along the edge that reveals it, takes input. |
| `Overlay` | `OverlayWindow` | Over the whole output and above fullscreen windows, with the keyboard, while the launcher, overview, or power dialog is open. It draws the panel and dock above them. |
| `Popup` | `PopupWindow` | The calendar, quick settings, or dock menu, next to the button of the part that opened it, which a host passes as `ShellView::popup_placement()`; it closes when that part goes. |
| `Toasts` | `ToastWindow` | In the top right corner, below the panel, sized to the toasts, while there are toasts and no popup or power dialog. |
| `Osd` | `OsdWindow` | Above the bottom edge, while the OSD shows and fades out. It takes no input. |
| `Auth` | `AuthWindow` | Over the whole output and above everything else, with the keyboard, on one output while a polkit request is open and the session is unlocked; see [polkit Authentication](#polkit-authentication). |

`PartWindow::placement()` describes where a part goes in terms of output edges, size, margin, exclusive zone, stacking, and keyboard,
which map directly onto a layer surface.
Floating parts leave room around them for their shadows: `PartWindow::geometry()` is the part itself, without that room.

What every view shows alike, such as the clock, system status, toasts, and OSD, lives in the model.
What the parts of one output show alike, such as its workspaces and open popup, lives in its view.
Each Slint window holds its own copy of a global, so the model sets the `Desktop` global on every window,
and the view sets the `ShellOutput` global on every window of its parts and handles its callbacks.
The shared models, such as toasts and the output's windows, are attached to each window.

Surfaces:

- **Top panel**: activities button, workspace indicator, focused app, clock and calendar popup, status icons (network, audio, battery), quick settings.
- **Dock**: favorites and running apps, with indicators and per-app window lists.
- **Launcher**: a search field over `AppIndex::search`, keyboard navigation, app grid.
- **Overview**: workspace thumbnails as labeled window cards, with switching and moving between workspaces.
- **Quick settings**: volume and brightness sliders, Wi-Fi, Bluetooth, do-not-disturb, dark mode, media controls, power menu.
- **Notifications**: toasts with actions and timeouts, and a notification center with history in the calendar popup.
- **OSD**: volume and brightness feedback.
- **Authentication**: the polkit dialog, with the action's message and icon, an identity picker, the PAM prompt, and errors.
- **Lock screen**: clock and password field; authentication goes through PAM in the process hosting the shell.
  The shell hands passwords to the handler registered with `ShellModel::on_unlock_attempt`,
  and its host answers with `ShellModel::set_locked(false)` or `ShellModel::unlock_failed()`.
  logind's lock signal, `nimbusctl lock`, and `power.lock_after_minutes` of inactivity all lock the session.

Shortcuts and requests aimed at the shell reach it on the control socket as `Event::ShellCommand`:
toggling the launcher or overview, and volume and brightness keys.
The compositor handles the keys and names the output under the pointer; the shell carries the commands out.

## Shell Process

`nimbus-shell` runs the shell as a Wayland client of the compositor; `nimbus-session` starts it once the compositor is ready.
It finds the compositor through `WAYLAND_DISPLAY` and `NIMBUS_SOCKET`, and takes `--config <path>` like the compositor.
It exits with status 0 on SIGTERM or SIGINT, and with a failure when it loses either connection.
One `calloop` loop drives the Wayland connection (through `smithay-client-toolkit`), Slint's timers,
the control socket, and the channels from the services, the PAM worker, the configuration watcher, and the application scanner.

Modules in `crates/nimbus-shell-host/src`:

- `main.rs`: arguments, logging, signals, and the event loop.
- `state.rs`: the `State` the loop runs on, outputs, and the work after each dispatch: actions, timers, rendering.
- `wayland.rs`: the protocol handlers.
- `platform.rs`: the Slint platform, which gives each Slint window a `Renderer` for the surface it shows on.
- `render/`: the `Renderer` trait and its two implementations; see [Rendering](#rendering).
- `surface.rs`: a Slint window on a `wl_surface`.
  It renders at the buffer scale, through a viewport at `wp_fractional_scale_v1` scales or with `set_buffer_scale` otherwise,
  and only when Slint has changes and the previous frame's callback arrived.
- `output.rs`: per output, a surface for each part of its `ShellView`, created when the part appears and destroyed when it goes.
  Parts other than popups are layer surfaces set up from `PartWindow::placement()`:
  on the top layer, or on the overlay layer above fullscreen windows, with exclusive keyboard interactivity for the overlay.
  Their size, exclusive zone, and margin follow the part's window.
  The compositor stacks exclusive zones in the order surfaces are created, so the panel comes before the dock.
  A popup is an `xdg_popup` of the part that opened it, through `zwlr_layer_surface_v1.get_popup`,
  placed by an `xdg_positioner` next to the button, with the window geometry leaving out its shadow.
  It grabs the pointer and keyboard with the last press, so the compositor dismisses it with `popup_done` on a click on another client.
  A click on the dock closes it in the shell, and so does hiding the part it opened from.
  When its content changes size, `xdg_popup.reposition` moves it, or a new popup replaces it before version 3 of `xdg_wm_base`.
  Each surface takes pointer input only inside `PartWindow::input_region()`.
  Popups go before the parts below them, so a popup never outlives its parent.
- `lock.rs`: locking through `ext-session-lock-v1`; see [Locking](#locking).
- `auth.rs`: checks lock screen passwords through PAM on a worker thread and reports back through a `calloop` channel.
  It uses the `nimbus` PAM service from `data/pam.d/nimbus` when installed, otherwise `login`.
- `idle.rs`: an `ext-idle-notify-v1` notification after `power.lock_after_minutes`, which locks.
- `input.rs`: pointer and keyboard input on the first seat; keys go through the compositor's XKB keymap, with its repeat rate.
  Key presses go through the compose table for the locale in `LC_ALL`, `LC_CTYPE`, or `LANG`, for dead keys and Compose sequences.
  Keys in a sequence produce no text and don't repeat, and neither does the composed text.
  Without a compose table, keys go straight to Slint.
- `ipc.rs`: one control socket connection, subscribed to events, which also carries requests; responses reach callbacks in request order.
  Requests wait in a buffer until the socket takes them, so writing never blocks.
  The events of one read reach the model together, which updates the views once.
- `services.rs`: `nimbus-services` and the compositor's `Event::ShellCommand`s, such as volume keys and launcher toggles.
  Volume and brightness keys go through `ShellModel::step_level`, so rapid presses build on the level the model shows.
- `actions.rs`: `ShellAction`s, launching applications with an `xdg-activation` token, and the application index.
- `media.rs`: removable media; see [Removable Media](#removable-media).

### Rendering

Each Slint window draws onto its `wl_surface` through a `Renderer`, which also requests the frame callback.
Every part has its own surface and renderer, so buffers are only as large as the parts shown.
With the software renderer on a 1920x1080 output, the idle panel and dock take 0.66 MiB of `wl_shm` buffers,
and the overlay adds 15.8 MiB while it's open; at 3840x2160, that's 1.1 MiB and 63.3 MiB.

- `SoftwareRenderer` is Slint's software renderer drawing into two alternating `wl_shm` buffers
  with `RepaintBufferType::SwappedBuffers`, so each frame redraws only what changed.
- `GlRenderer` is Slint's FemtoVG renderer drawing with OpenGL ES through EGL (`glutin`),
  with a context per surface and an EGL window surface created at the first frame's size.
  After a resize it draws once more if EGL reports the old size, as Mesa's software rasterizer does for one frame.
  FemtoVG redraws the whole window for each frame.
  EGL's own frame pacing is off; swapping buffers commits the surface, so the frame callback is requested first.

At startup the shell opens EGL on its Wayland connection and makes a context current, which the first surface then takes.
It uses `GlRenderer` when that works on a GPU, and `SoftwareRenderer` otherwise,
since a software rasterizer such as llvmpipe redraws more than Slint's software renderer.
It recognizes one by the EGL device's `EGL_MESA_device_software` extension, or else by the OpenGL renderer's name.
`NIMBUS_SHELL_RENDERER=software` skips OpenGL, and `NIMBUS_SHELL_RENDERER=gl` takes it even on a software rasterizer.
When OpenGL fails, at startup or for one surface, the shell logs a warning and renders in software.

## Locking

Locked is compositor state, apart from whatever draws the lock screen.
An `ext-session-lock` lock, `Request::Lock`, `--locked`, or the lock marker at startup locks the session.
Every change of the lock state emits `Event::LockState` with `locked` and `held`, whether a live lock client holds it.
While it's locked, the compositor draws the lock client's surfaces over black, breaks client grabs, and ignores Ctrl+Alt+Backspace.
Volume, mute, and brightness keys still emit their `Event::ShellCommand`; every other key goes to the lock client.
Without a live lock client, the compositor draws black, gives keyboard and pointer to no one,
and accepts a new `ext-session-lock` lock.
It refuses a lock while a live client holds the session, and never displays the refused lock's surfaces.
A lock client that dies, or destroys its lock before `locked`, leaves the session locked but not held.
Only the holder's `unlock_and_destroy` unlocks.
`Request::GetLockState` reports the same state over the control socket.
Each lock surface is sized to its output, and told its output, scale, and transform, whenever the outputs change.
The holder gets `locked` once every output has presented a frame drawn after the lock; on udev, that's at the frame's page flip.

Each lock owns its lock surfaces, at most one per output, so a new lock never inherits a dead one's outputs.
A refused lock's `get_lock_surface` makes an inert object, which is never configured and never shown.
Every lock surface follows the protocol's role and `already_constructed` rules,
so a `wl_surface` that showed a buffer, or still has a live lock surface, can't become one.
The holder's surfaces also follow the `duplicate_output` rule,
and their commits the acknowledged-configure, null buffer, and size rules.
The size is checked on commits that attach a buffer.
When the holder destroys a lock surface, its output shows black.

`nimbus-shell` locks with `ext-session-lock` whenever the session is locked but not held,
from `Request::GetLockState` when it starts and from `Event::LockState` after that.
So `Request::Lock` and the lock shortcut get its lock screen, and a restarted shell shows it again.
logind's lock signal and inactivity also make it lock.
It creates its lock surfaces once the compositor confirms the lock.

The lock marker, `$XDG_RUNTIME_DIR/nimbus/locked` (`nimbus_ipc::lock_marker_path`), exists while the session is locked.
The compositor creates it, with mode 0600 in a 0700 directory, as a lock starts and before any locked frame.
It removes the marker once the session unlocks.
A compositor started with `--locked`, or with the marker present, starts locked before it creates any output.

`nimbus-session` deletes a stale marker when the session starts.
When the compositor crashes with the marker present, the session restarts it with `--locked`, then restarts the shell, which locks again.
The marker is per runtime directory, so a nested compositor in the same `XDG_RUNTIME_DIR` shares it with the real session.

## polkit Authentication

The shell process is the session's polkit authentication agent; `crates/nimbus-services/src/polkit` implements it.
Once polkitd runs, which the agent asks D-Bus to start, the agent exports `org.freedesktop.PolicyKit1.AuthenticationAgent`
at `/org/freedesktop/PolicyKit1/AuthenticationAgent` and registers it for its logind session,
with the session id from logind or `XDG_SESSION_ID`.
It registers again whenever polkitd comes back, and it answers no one but polkitd.
Without a session or an installed `polkit-agent-helper-1`, it stays off, and another agent has to answer.

Each `BeginAuthentication` resolves its identities to login names, a `unix-group` to its members,
and asks as the current user when it's one of them.
It runs the setuid `polkit-agent-helper-1` for the chosen user, passing the cookie on standard input.
The helper runs PAM and tells polkitd the result itself, so no password check happens in the shell.
Its lines become `AuthenticationEvent`s: `PAM_PROMPT_ECHO_OFF` and `PAM_PROMPT_ECHO_ON` prompts,
`PAM_TEXT_INFO` and `PAM_ERROR_MSG` messages, and `SUCCESS` or `FAILURE`.
After a wrong response, or when the user picks another identity, the agent starts the helper again.
A helper that fails before asking anything ends the request with `org.freedesktop.PolicyKit1.Error.Failed`.
Cancelling, from the dialog or by polkitd's `CancelAuthentication`, kills the helper and ends the call with `Error.Cancelled`.

The shell model queues requests and shows the first in the `Auth` part of one view:
the output of the focused window when the request comes up, or the first output.
Opening the dialog closes that view's launcher, overview, power dialog, and popup, and toasts step aside while it shows.
Responses go back as `ServiceCommand::Authentication`; `Secret` keeps them out of debug logs.

## Removable Media

`nimbus_services::udisks` is a udisks2 client on the system bus.
It lists the file systems udisks doesn't mark `HintSystem` or `HintIgnore`, with their drives,
from `GetManagedObjects` again after each burst of `InterfacesAdded`, `InterfacesRemoved`, and `PropertiesChanged`.
`Event::Volumes` carries the list whenever it changes, and `Event::Added` each volume that appears after the first list since udisks appeared.
Commands run on tasks of their own, since udisks asks polkit first, and the dialog waits for the user:
`Mount` calls `Filesystem.Mount` and reports the mount point;
`Eject` and `PowerOff` unmount every file system on the drive, then call `Drive.Eject` or `Drive.PowerOff`.
udisks's refusals come back as `Event::Failed` with its message, except when the user dismissed the polkit dialog.
The shell runs the client among its services, as `ServiceEvent::Disks` and `ServiceCommand::Disks`, and apps run it alone with `udisks::Client`.

The shell mounts a removable volume that appears while the session is unlocked, unless `[media] automount` is off,
and shows a toast for it with an "Open" action.
Opening mounts the volume if needed, then starts the default application for `inode/directory`, or Files, on the mount point,
with the toast's activation token.
The toast closes when its volume goes.

Files adds the volumes to the devices in its sidebar.
A mounted volume takes the place of its mount point from the mount table; opening one that isn't mounted mounts it and shows it.
An eject button and a menu unmount it, eject its medium, or power off its drive, after leaving its folder if it's showing.
A toast says when an ejected or powered off volume can be removed.

## Portal

`nimbus-portal` implements `org.freedesktop.impl.portal.Settings` for `xdg-desktop-portal`.
It owns `org.freedesktop.impl.portal.desktop.nimbus` on the session bus and is started by D-Bus activation;
`data/nimbus-portals.conf` selects it for Settings when `XDG_CURRENT_DESKTOP` is `Nimbus`.

It serves the `org.freedesktop.appearance` namespace from the configuration:

| Key | Type | Value |
| --- | --- | --- |
| `color-scheme` | `u` | 1 for `dark`, 2 for `light`, 0 for `system` |
| `accent-color` | `(ddd)` | `appearance.accent` as sRGB in 0 to 1, or the default accent if it doesn't parse |
| `contrast` | `u` | always 0 |

`system` has no preference because `nimbus-theme` resolves it by asking the portal.
The portal watches the configuration with `nimbus_config::watch` and emits `SettingChanged` for each key whose value changed.
`Read` on any other namespace or key fails with `org.freedesktop.portal.Error.NotFound`.

## Theming

`nimbus-theme` owns all colors, spacing, radii, typography, and motion as a `Theme` global,
plus components (buttons, toggles, sliders, cards, list rows, search field, icons).
Every Nimbus UI imports it, so the shell and apps look like one product.
`ThemeSettings::from_config` resolves the user's choices, and `apply_theme!` pushes them into a component's `Theme` global.

## Configuration

`$XDG_CONFIG_HOME/nimbus/config.toml`; see `nimbus-config` for the schema.
The compositor and the shell watch the file and apply changes live, so the Settings app only writes the file.
Displays are the exception: Settings configures them through wlr-output-management, and the compositor saves `[[outputs]]`.
The shell saves its own changes, such as the dark style toggle and dock pins, to the file named by `--config`, or the default one.
The shell, Settings, and the compositor all write through `nimbus_config::update` or `update_with`,
which hold a lock on `config.toml.lock` from load to save,
so none loses another's changes.
Settings and the compositor parse key chords with the same `nimbus_config::chord` grammar.

## Sessions and Logout

`nimbus-session` treats a compositor exit status of 0 as a logout and anything else as a crash.
It restarts a crashed compositor up to three times a minute, then ends the session.
A restarted compositor reuses the first one's Wayland socket name, and its X11 display when it's still free,
so clients keep a valid `WAYLAND_DISPLAY` and `DISPLAY`.

It starts `nimbus-shell` once the compositor reports readiness, with the compositor's `WAYLAND_DISPLAY`, `NIMBUS_SOCKET`, and `DISPLAY`,
and passes it `--config` when the session has one.
A shell that exits, for any reason, is restarted after a delay that doubles from half a second up to 30 seconds,
and drops back to half a second once a shell ran for 30 seconds.
A shell never ends the session.
When the compositor crashes, the session gives the shell half a second to exit before killing it,
and starts a new one with the restarted compositor.

`nimbus-session` owns autostart, and runs it once per session, after the first compositor reports readiness.
It runs each `config.autostart` command through `/bin/sh -c`, then the XDG autostart entries.
Each autostarted process leads its own process group.
When the session ends, for any reason, it sends SIGTERM to the shell and these groups together,
and SIGKILL to whatever still runs three seconds later.
It leaves the groups running when the compositor restarts.
Before it starts the compositor, `nimbus-session` makes sure apps find a Secret Service (`org.freedesktop.secrets`).
When nothing owns the name, it runs `gnome-keyring-daemon --start --components=secrets`,
which takes over a daemon that `pam_gnome_keyring` started at login, with the login keyring unlocked, or starts a new one.
It exports the `GNOME_KEYRING_CONTROL` and `SSH_AUTH_SOCK` that gnome-keyring prints to everything it starts, and to D-Bus activation,
but keeps an `SSH_AUTH_SOCK` the user already set.
Without gnome-keyring, it relies on D-Bus activation of whatever provides the name, and logs a warning when nothing does.
The lock screen's PAM service in `data/pam.d/nimbus` includes `pam_gnome_keyring`, so unlocking the screen unlocks the keyring again.

Logging out asks logind to end the session.
Without a logind session, as when nested, `nimbus-services` emits `ServiceEvent::LogoutRequested`,
and the shell sends `Request::Quit`, so the compositor exits with status 0.

### Panics

The release profile aborts on panic, because every process is supervised or restarted on demand, and each one restarts into a safe state:

- The compositor keeps the lock marker on disk while locked, so a restarted compositor starts locked.
  Unwinding gains nothing, since the session restarts it either way.
- The shell asks the compositor for the lock state when it starts, so a restarted shell shows the lock screen again.
  Aborting also covers its worker threads, such as the D-Bus runtime and the PAM worker:
  a panic there ends the process instead of leaving a shell that silently stops answering.
- `nimbus-portal` is started again by D-Bus activation on the next request.
- `nimbus-session` is the root of the session, which a panic ends whether it unwinds or not.

## Testing

- Domain crates have unit tests with fixture directories.
- The shell has tests on Slint's testing backend.
  A stand-in host in `crates/nimbus-shell/tests/support/desk.rs` shows a view's parts on an output of a fixed size,
  laid out as the compositor arranges layer surfaces and popups, routes input to them, and composites them;
  the behavior tests, the shell's screenshots, and `nimbus-shell-preview` use it.
  The apps render reference screenshots off screen through `nimbus_theme::headless`.
- `nimbus-test-support` holds the shared harness: it starts the headless compositor in a temporary directory,
  provides Wayland test clients, and starts a private `dbus-daemon`.
- The compositor runs headless in tests: a test client connects over Wayland, maps windows, and checks the control socket.
  Protocol tests drive `ext-session-lock`, `ext-foreign-toplevel-list`, `ext-idle-notify`, `wlr-layer-shell`, and `wlr-output-management` with their own clients.
  A headless test checks that synthetic clicks focus the window under them and synthetic keys run shortcuts.
  The output management tests list two headless heads, apply and save a scale and position change, restore it after a restart,
  and check refused, outdated, and disabling configurations.
  The session lock tests check that lock surfaces follow their output's size and that `Event::LockState` reports each change,
  that a client locks again after destroying its unconfirmed lock, that a second lock surface on an output is `duplicate_output`,
  that a surface that showed a buffer is `already_constructed`,
  and that a refused lock's surfaces are inert but still follow the role rule.
  The XWayland tests check display allocation, stale and live lock files, and cleanup,
  and, with a Python script standing in for `xwayland-satellite`, that the first X11 client starts it with both sockets,
  the next client after it exits starts it again, and that `DISPLAY` stays unset without it.
- The `nimbus-shell` tests run it against the headless compositor, which they build first,
  check the composited output through `Request::Screenshot`, click and type through `Request::Click` and `Request::PressKey`,
  and follow the shell's log of the surfaces it opens and closes:
  the panel renders, the launcher and overview open and close an overlay surface through shell command events,
  the panel and dock reserve their space and an autohidden dock none, the launcher takes typing and closes on Escape,
  quick settings open in a popup that a click outside dismisses, a toast shows on its own surface until it expires,
  `Request::Lock` shows the lock screen on a lock surface,
  a killed shell leaves the session locked and a restarted one locks again, and the exit statuses are right.
  They render in software unless `NIMBUS_SHELL_RENDERER` is set; `NIMBUS_SHELL_RENDERER=gl` runs them on `GlRenderer`.
- The `nimbus-session` tests run it with shell scripts standing in for the compositor and the shell.
  With a private `dbus-daemon`, they check that a script standing in for `gnome-keyring-daemon` runs when nothing owns `org.freedesktop.secrets`,
  that its variables reach the compositor, and that it doesn't run when the name is owned.
- The Settings display client configures the headless compositor in a test, which builds the compositor first.
  The Settings behavior tests drive the Network and Bluetooth pages against in-process sample clients:
  passwords, saved networks, forgetting, pairing by confirming a code, and discovery that follows the page.
- `nimbus-services`, `nimbus-portal`, and the `nimbus-shell` toast test run against a private `dbus-daemon`,
  and skip with a message when it's missing.
  The polkit test registers the agent with a fake polkitd and logind there, and calls `BeginAuthentication` and `CancelAuthentication`;
  unit tests drive the helper protocol with shell scripts standing in for `polkit-agent-helper-1`.
  The NetworkManager client test lists networks and wired details from a fake NetworkManager,
  connects with a wrong and a right password, changes the password, disconnects, and forgets.
  The BlueZ client test powers a fake adapter, discovers, pairs through the agent by confirming and by declining, disconnects and removes devices,
  and checks that the agent refuses callers other than BlueZ.
  `nimbus-test-support` has a fake udisks with one removable drive.
  The udisks tests list, mount, add, and power off volumes on it, and check failures and dismissed dialogs.
  A `nimbus-shell` test mounts a stick inserted into it and shows its toast, and a Files test mounts a volume from the sidebar and powers its drive off.
- `cargo test --manifest-path desktop/Cargo.toml --workspace` runs everything.

## Running

- Nested, inside an existing Wayland or X11 session: `nimbus-session --backend winit`, after building the workspace,
  so it finds `nimbus-compositor` and `nimbus-shell` next to itself.
- On a TTY: `nimbus-session`, or select "Nimbus" from a display manager using `data/nimbus.desktop`.
- Headless, for tests and screenshots: `nimbus-compositor --backend headless`, with apps started as `SLINT_BACKEND=winit-software`.
- By hand: start `nimbus-compositor`, then `nimbus-shell` with the `WAYLAND_DISPLAY` and `NIMBUS_SOCKET` it prints.

The workspace's `dev` profile keeps only line tables for its own crates and no debug info for dependencies,
because Slint's generated code makes full debug info several hundred megabytes per binary.
