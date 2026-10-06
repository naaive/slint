# Nimbus Architecture

Nimbus is a Wayland desktop environment written in Rust, with every user interface built in Slint.
It covers the same ground as GNOME and KDE Plasma: a compositor, a desktop shell, system services, and core apps.

## Design Principles

- **Wayland only.** There's no X11 session; XWayland is optional and runs on demand.
- **The shell is a client.** `nimbus-shell` shows the shell on `wlr-layer-shell` and `ext-session-lock` surfaces,
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
| `nimbus-ipc` | lib | Window/workspace model; JSON-lines protocol on the control socket; blocking client; runtime paths of the control socket and lock marker. |
| `nimbus-config` | lib | TOML configuration schema with defaults, the stored display layout (`[[outputs]]`), atomic save, locked load-modify-save (`nimbus_config::update`), the key chord grammar (`chord`), and file watching. |
| `nimbus-xdg` | lib | Desktop entries, icon theme lookup, fuzzy app search, launching. |
| `nimbus-services` | lib | Tokio + zbus: notifications server, UPower, NetworkManager, audio, backlight, MPRIS, BlueZ, logind. |
| `nimbus-theme` | lib + Slint library | Design tokens and components imported as `@nimbus/theme.slint`; off-screen software rendering for screenshots behind the `headless` feature. |
| `nimbus-shell` | lib + preview bin | Panel, dock, launcher, overview, quick settings, notification center, toasts, OSD, lock screen: one shared model shown by a view per output. |
| `nimbus-compositor` | bin | Smithay compositor: backends, displays and wlr-output-management, window management, input, keyboard shortcuts, the session lock, control socket. |
| `nimbus-shell-host` | bin `nimbus-shell` | The shell process: a Wayland client showing `nimbus-shell` on layer-shell and session-lock surfaces, with PAM, idle locking, and the system services. |
| `nimbus-portal` | bin + lib | `xdg-desktop-portal` Settings backend publishing `org.freedesktop.appearance` from `nimbus-config`. |
| `nimbus-session` | bins | `nimbus-session` starts and supervises the compositor and the shell, and runs autostart; `nimbusctl` is the command-line client. |
| `nimbus-settings` | app | System settings, editing `nimbus-config`, and display configuration through wlr-output-management. |
| `nimbus-files` | app | File manager. |
| `nimbus-terminal` | app | Terminal emulator on `alacritty_terminal`. |
| `nimbus-monitor` | app | System monitor on `sysinfo`. |

Dependency direction (arrows point at dependencies):

```text
nimbus-shell-host ──> nimbus-shell ──> nimbus-theme ──> nimbus-config
        │                  ├──> nimbus-ipc
        │                  ├──> nimbus-xdg
        │                  └──> nimbus-services
        └──> (all of the above)
nimbus-compositor ──> nimbus-ipc, nimbus-config, nimbus-xdg
apps ──> nimbus-theme[headless], nimbus-config
nimbus-files ──> nimbus-xdg
nimbus-settings ──> nimbus-ipc
nimbus-session ──> nimbus-ipc, nimbus-config
nimbus-portal ──> nimbus-config
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
- `lock.rs`: the session lock state and the `ext-session-lock` client holding it; see [Locking](#locking).
- `lock_marker.rs`: keeps the lock marker in step with the lock state.
- `state.rs`: the `Nimbus` state struct and Smithay handler implementations
  (compositor, xdg-shell, xdg-decoration, layer-shell, seat, data device, primary selection, ext and wlr data control,
  output, shm, dmabuf, xdg-activation, presentation, viewporter, fractional scale,
  ext-session-lock, ext-foreign-toplevel-list, ext-idle-notify, and idle-inhibit).
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
- `ipc.rs`: the control socket server, a `calloop` source per connection.
- `render.rs`: the scene shared by all backends, the wallpaper or built-in gradient backdrop, and screenshots.

Command line, which `nimbus-session` relies on:

```text
nimbus-compositor [--backend winit|udev|headless] [--socket <wayland socket name>] [--config <path>] [--locked]
```

When the Wayland and control sockets accept connections, the compositor sets `WAYLAND_DISPLAY` and `NIMBUS_SOCKET`
for its children and prints exactly one line to standard output:
`NIMBUS_READY WAYLAND_DISPLAY=<name> NIMBUS_SOCKET=<path>`.
All logging goes to standard error.
The headless backend creates one 1920x1080 virtual output, `HEADLESS-1`, or one per size listed in `NIMBUS_HEADLESS_OUTPUTS` such as `1280x720,1920x1080`.
`Request::Quit` and the emergency exit end the compositor with status 0, which `nimbus-session` takes as a logout.

## Displays

A head is a connected display, which the backend describes as a smithay `Output` with its modes, preferred mode, and EDID identity.
It's enabled while it's mapped in the window manager's space; only enabled heads have a `wl_output` global.
Every change of a head's mode, position, scale, transform, or enabled state goes through `Nimbus::configure_outputs`:

1. `outputs::layout::validate` checks what every backend needs: one head stays on, scales from 0.25 to 8, and supported modes.
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
When stored positions leave a display apart from the rest, as after unplugging the middle one of three,
the displays line up from left to right in their stored order, so the pointer can reach each of them.

wlr-output-management (version 4) advertises every head, enabled or not, and sends each client only what changed, followed by `done` with a new serial.
A configuration made with an older serial is cancelled.
Custom modes are accepted when they match a supported mode within 1 Hz, and adaptive sync can't be turned on.
The compositor saves every applied configuration to `[[outputs]]` through `nimbus_config::update`,
leaving out what matches a display's defaults, so `appearance.scale` keeps applying to displays at the default scale.

The Displays page in Settings is a wlr-output-management client on its own thread and Wayland connection (`displays/wlr.rs`).
It arranges displays by dragging, attaching each to the nearest edge of another, sets mode, scale, rotation, and whether each display is on,
and reverts an applied configuration unless the user keeps it within 15 seconds.
Without the protocol, it lists the outputs that the control socket reports, read-only.

## Shell

The shell is one `ShellModel`, the single source of truth, shown by lightweight views.
Data flows into the model, and every view emits `ShellAction`s through it; see `crates/nimbus-shell/src/lib.rs`.

- `ShellView`: a transparent, full-output overlay per output.
  It keeps only what's particular to its output, such as its open popup, launcher, and the windows on it.
  Everything outside `ShellView::input_region()` passes through to client windows.
- `LockView`: the lock screen for one output, a window of its own,
  so a host can put it on its own surface, such as an `ext-session-lock` surface.

What every view shows alike, such as the clock, system status, toasts, and OSD, lives in the model.
Each Slint window holds its own copy of a global, so the model sets the `Desktop` global on every window,
and attaches the shared models, such as toasts, to each.

Surfaces:

- **Top panel**: activities button, workspace indicator, focused app, clock and calendar popup, status icons (network, audio, battery), quick settings.
- **Dock**: favorites and running apps, with indicators and per-app window lists.
- **Launcher**: a search field over `AppIndex::search`, keyboard navigation, app grid.
- **Overview**: workspace thumbnails as labeled window cards, with switching and moving between workspaces.
- **Quick settings**: volume and brightness sliders, Wi-Fi, Bluetooth, do-not-disturb, dark mode, media controls, power menu.
- **Notifications**: toasts with actions and timeouts, and a notification center with history in the calendar popup.
- **OSD**: volume and brightness feedback.
- **Lock screen**: clock and password field; authentication goes through PAM in the process hosting the shell.
  The shell hands passwords to the handler registered with `ShellModel::on_unlock_attempt`,
  and its host answers with `ShellModel::set_locked(false)` or `ShellModel::unlock_failed()`.
  logind's lock signal, `nimbusctl lock`, and `power.lock_after_minutes` of inactivity all lock the session.

Shortcuts and requests aimed at the shell reach it on the control socket as `Event::ShellCommand`:
toggling the launcher or overview, volume and brightness keys, and lock requests.
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
- `output.rs`: per output, the `ShellView` on a transparent top-layer surface anchored to every edge with exclusive zone -1,
  and a transparent single-pixel strip per edge whose exclusive zone keeps windows out of the panel and dock.
  The view's surface takes pointer input only inside `ShellView::input_region()`.
  It has no keyboard interactivity until the view wants the keyboard; then it's exclusive and moves to the overlay layer, above fullscreen windows.
- `lock.rs`: locking through `ext-session-lock-v1`; see [Locking](#locking).
- `auth.rs`: checks lock screen passwords through PAM on a worker thread and reports back through a `calloop` channel.
  It uses the `nimbus` PAM service from `data/pam.d/nimbus` when installed, otherwise `login`.
- `idle.rs`: an `ext-idle-notify-v1` notification after `power.lock_after_minutes`, which locks.
- `input.rs`: pointer and keyboard input; keys go through the compositor's XKB keymap, with its repeat rate.
- `ipc.rs`: one control socket connection, subscribed to events, which also carries requests; responses reach callbacks in request order.
- `services.rs`: `nimbus-services` and the compositor's `Event::ShellCommand`s, such as volume keys and launcher toggles.
- `actions.rs`: `ShellAction`s, launching applications with an `xdg-activation` token, and the application index.

### Rendering

Each Slint window draws onto its `wl_surface` through a `Renderer`, which also requests the frame callback.

- `SoftwareRenderer` is Slint's software renderer drawing into two alternating `wl_shm` buffers
  with `RepaintBufferType::SwappedBuffers`, so each frame redraws only what changed.
- `GlRenderer` is Slint's FemtoVG renderer drawing with OpenGL ES through EGL (`glutin`),
  with a context per surface and an EGL window surface created at the first frame's size.
  FemtoVG redraws the whole window for each frame.
  EGL's own frame pacing is off; swapping buffers commits the surface, so the frame callback is requested first.

At startup the shell opens EGL on its Wayland connection and makes a context current.
It uses `GlRenderer` when that works on a GPU, and `SoftwareRenderer` otherwise,
since a software rasterizer such as llvmpipe redraws more than Slint's software renderer.
`NIMBUS_SHELL_RENDERER=software` skips OpenGL, and `NIMBUS_SHELL_RENDERER=gl` takes it even on a software rasterizer.
When OpenGL fails, at startup or for one surface, the shell logs a warning and renders in software.

## Locking

Locked is compositor state, apart from whatever draws the lock screen.
An `ext-session-lock` lock, `Request::Lock`, `--locked`, or the lock marker at startup locks the session.
`Request::Lock` also emits `ShellCommand::Lock`, which asks the shell for a lock screen.
While it's locked, the compositor draws the lock client's surfaces over black, breaks client grabs, and ignores Ctrl+Alt+Backspace.
Volume, mute, and brightness keys still emit their `Event::ShellCommand`; every other key goes to the lock client.
Without a live lock client, the compositor draws black, gives keyboard and pointer to no one,
and accepts a new `ext-session-lock` lock.
It refuses a lock while a live client holds the session, and never displays the refused lock's surfaces.
A lock client that dies, or destroys its lock before `locked`, leaves the session locked,
and the compositor emits `ShellCommand::Lock` so the shell takes over.
Only the holder's `unlock_and_destroy` unlocks.
`Request::GetLockState` reports the lock state over the control socket.

`nimbus-shell` asks for the lock state when it starts, and locks with `ext-session-lock` if the session is locked,
so a restarted shell shows its lock screen again.
logind's lock signal, `Event::ShellCommand` with `lock`, and inactivity also make it lock.
It creates its lock surfaces once the compositor confirms the lock.

The lock marker, `$XDG_RUNTIME_DIR/nimbus/locked` (`nimbus_ipc::lock_marker_path`), exists while the session is locked.
The compositor creates it, with mode 0600 in a 0700 directory, as a lock starts and before any locked frame.
It removes the marker once the session unlocks.
A compositor started with `--locked`, or with the marker present, starts locked before it creates any output.

`nimbus-session` deletes a stale marker when the session starts.
When the compositor crashes with the marker present, the session restarts it with `--locked`, then restarts the shell, which locks again.
The marker is per runtime directory, so a nested compositor in the same `XDG_RUNTIME_DIR` shares it with the real session.

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
A restarted compositor reuses the first one's Wayland socket name, so clients keep a valid `WAYLAND_DISPLAY`.

It starts `nimbus-shell` once the compositor reports readiness, with the compositor's `WAYLAND_DISPLAY` and `NIMBUS_SOCKET`,
and passes it `--config` when the session has one.
A shell that exits, for any reason, is restarted after a delay that doubles from half a second up to 30 seconds,
and drops back to half a second once a shell ran for 30 seconds.
A shell never ends the session.
When the compositor exits, the session stops the shell, and starts a new one with the restarted compositor.

`nimbus-session` owns autostart, and runs it once per session, after the first compositor reports readiness.
It runs each `config.autostart` command through `/bin/sh -c`, then the XDG autostart entries.
Each autostarted process leads its own process group.
The session terminates these groups when it ends, for any reason, but not when the compositor restarts.
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
  The shell and apps render reference screenshots off screen through `nimbus_theme::headless`.
- The compositor runs headless in tests: a test client connects over Wayland, maps windows, and checks the control socket.
  Protocol tests drive `ext-session-lock`, `ext-foreign-toplevel-list`, `ext-idle-notify`, `wlr-layer-shell`, and `wlr-output-management` with their own clients.
  The output management tests list two headless heads, apply and save a scale and position change, restore it after a restart,
  and check refused, outdated, and disabling configurations.
- The `nimbus-shell` tests run it against the headless compositor, which they build first,
  and check the composited output through `Request::Screenshot`:
  the panel renders, the launcher and overview toggle through shell command events, maximized windows stay below the panel,
  notifications show toasts, `Request::Lock` shows the lock screen on a lock surface,
  a killed shell leaves the session locked and a restarted one locks again, and the exit statuses are right.
  They render in software unless `NIMBUS_SHELL_RENDERER` is set; `NIMBUS_SHELL_RENDERER=gl` runs them on `GlRenderer`.
- The `nimbus-session` tests run it with shell scripts standing in for the compositor and the shell.
- The Settings display client configures the headless compositor in a test, which builds the compositor first.
- `nimbus-services` and `nimbus-portal` run their D-Bus tests against a private `dbus-daemon`, and skip them with a message when it's missing.
- `cargo test --manifest-path desktop/Cargo.toml --workspace` runs everything.

## Running

- Nested, inside an existing Wayland or X11 session: `nimbus-session --backend winit`, after building the workspace,
  so it finds `nimbus-compositor` and `nimbus-shell` next to itself.
- On a TTY: `nimbus-session`, or select "Nimbus" from a display manager using `data/nimbus.desktop`.
- Headless, for tests and screenshots: `nimbus-compositor --backend headless`, with apps started as `SLINT_BACKEND=winit-software`.
- By hand: start `nimbus-compositor`, then `nimbus-shell` with the `WAYLAND_DISPLAY` and `NIMBUS_SOCKET` it prints.

The workspace's `dev` profile keeps only line tables for its own crates and no debug info for dependencies,
because Slint's generated code makes full debug info several hundred megabytes per binary.
