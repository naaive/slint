# Nimbus Architecture

Nimbus is a Wayland desktop environment written in Rust, with every user interface built in Slint.
It covers the same ground as GNOME and KDE Plasma: a compositor, a desktop shell, system services, and core apps.

## Design Principles

- **Wayland only.** There's no X11 session; XWayland is optional and runs on demand.
- **The shell is a client.** `nimbus-shell` shows the shell on `wlr-layer-shell` and `ext-session-lock` surfaces,
  and talks to the compositor over the control socket, so a crashed shell never takes windows down.
  Until `nimbus-session` starts it, the compositor still hosts the same shell in-process unless started with `--no-shell`.
- **Crates are boundaries.** Domain crates (`nimbus-ipc`, `nimbus-config`, `nimbus-xdg`, `nimbus-services`) have no UI.
  The shell is a view over them and doesn't know how it's displayed.
  The compositor and the shell process are the only crates that wire them together.
- **Single-threaded UI, async edges.** The compositor and the shell process each run one `calloop` event loop on the main thread, which also drives Slint.
  D-Bus work runs on a Tokio runtime in `nimbus-services` and reaches the main thread through a `calloop` channel.
- **Degrade, don't fail.** A missing D-Bus daemon, backlight, or battery hides that feature; it never stops the session.
- **Standards first.** Desktop entries, icon themes, `org.freedesktop.Notifications`, MPRIS, UPower, NetworkManager, logind, the portal Settings interface,
  and `xdg-shell`, `xdg-decoration`, `wlr-layer-shell`, and `xdg-activation` on the Wayland side.

## Crates

| Crate | Kind | Responsibility |
| --- | --- | --- |
| `nimbus-ipc` | lib | Window/workspace model; JSON-lines protocol on the control socket; blocking client; runtime paths of the control socket and lock marker. |
| `nimbus-config` | lib | TOML configuration schema with defaults, atomic save, locked load-modify-save (`nimbus_config::update`), the key chord grammar (`chord`), and file watching. |
| `nimbus-xdg` | lib | Desktop entries, icon theme lookup, fuzzy app search, launching. |
| `nimbus-services` | lib | Tokio + zbus: notifications server, UPower, NetworkManager, audio, backlight, MPRIS, BlueZ, logind. |
| `nimbus-theme` | lib + Slint library | Design tokens and components imported as `@nimbus/theme.slint`; off-screen software rendering for screenshots behind the `headless` feature. |
| `nimbus-shell` | lib + preview bin | Panel, dock, launcher, overview, quick settings, notification center, toasts, OSD, lock screen: one shared model shown by a view per output. |
| `nimbus-compositor` | bin | Smithay compositor: backends, window management, input, shell hosting, control socket. |
| `nimbus-shell-host` | bin `nimbus-shell` | The shell process: a Wayland client showing `nimbus-shell` on layer-shell and session-lock surfaces, with PAM, idle locking, and the system services. |
| `nimbus-portal` | bin + lib | `xdg-desktop-portal` Settings backend publishing `org.freedesktop.appearance` from `nimbus-config`. |
| `nimbus-session` | bins | `nimbus-session` starts and supervises the compositor and runs autostart; `nimbusctl` is the command-line client. |
| `nimbus-settings` | app | System settings, editing `nimbus-config`. |
| `nimbus-files` | app | File manager. |
| `nimbus-terminal` | app | Terminal emulator on `alacritty_terminal`. |
| `nimbus-monitor` | app | System monitor on `sysinfo`. |

Dependency direction (arrows point at dependencies):

```text
nimbus-compositor ──> nimbus-shell ──> nimbus-theme ──> nimbus-config
nimbus-shell-host ──┘      │
        │                  ├──> nimbus-ipc
        │                  ├──> nimbus-xdg
        │                  └──> nimbus-services
        └──> (all of the above)
apps ──> nimbus-theme[headless], nimbus-config
nimbus-files ──> nimbus-xdg
nimbus-settings ──> nimbus-ipc
nimbus-session ──> nimbus-ipc, nimbus-config
nimbus-portal ──> nimbus-config
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
- `backend/winit.rs`: nested session in a window, for development.
- `backend/udev.rs`: DRM/KMS, GBM, libinput, and libseat for a real session, with hotplug.
- `backend/headless.rs`: no output device; renders with Pixman into memory, for tests and screenshots.
- `wm/`: window management: workspaces, focus stack, floating placement, a tiling layout (master-stack),
  maximize/fullscreen/minimize, interactive move and resize,
  and the exclusive zones of the shell and of layer-shell surfaces.
  Layouts implement a `Layout` trait so more can be added.
- `input.rs`: keyboard shortcuts from `nimbus-config`, pointer and keyboard routing between the shell and clients,
  and user activity for `ext-idle-notify`.
  Layer surfaces with `exclusive` keyboard interactivity on the top or overlay layer take the keyboard;
  `on_demand` ones take it when clicked.
- `keybindings.rs`: resolves chords parsed with `nimbus_config::chord` to XKB keysyms.
- `shell_host.rs`: the Slint platform. It implements `slint::platform::Platform` and holds the `ShellModel`.
  Each output gets a `ShellView`, plus a `LockView` above it while locked, each in a `MinimalSoftwareWindow`
  that renders damaged regions into a `MemoryRenderBuffer`.
  It forwards input inside `ShellView::input_region()`, or to the lock screen while locked,
  and maps `ShellAction`s to compositor requests, services, and launches.
- `auth.rs`: checks lock screen passwords through PAM on a worker thread and reports back through a `calloop` channel.
  It uses the `nimbus` PAM service from `data/pam.d/nimbus` when installed, otherwise `login`.
- `ipc.rs`: the control socket server, a `calloop` source per connection.
- `render.rs`: the scene shared by all backends, the wallpaper or built-in gradient backdrop, and screenshots.

Command line, which `nimbus-session` relies on:

```text
nimbus-compositor [--backend winit|udev|headless] [--socket <wayland socket name>] [--config <path>] [--no-shell] [--locked]
```

When the Wayland and control sockets accept connections, the compositor sets `WAYLAND_DISPLAY` and `NIMBUS_SOCKET`
for its children and prints exactly one line to standard output:
`NIMBUS_READY WAYLAND_DISPLAY=<name> NIMBUS_SOCKET=<path>`.
All logging goes to standard error.
The headless backend creates one 1920x1080 virtual output, or the sizes listed in `NIMBUS_HEADLESS_OUTPUTS` such as `1280x720,1920x1080`.

Slint's platform is process-global and must be set before the first component is created.
Its timers and animations advance from the `calloop` loop through `slint::platform::update_timers_and_animations()`,
and the loop wakes at `slint::platform::duration_until_next_timer_update()`.

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
  and the compositor answers with `ShellModel::set_locked(false)` or `ShellModel::unlock_failed()`.
  logind's lock signal, `nimbusctl lock`, and `power.lock_after_minutes` of inactivity all lock the session.

Shortcuts and requests aimed at the shell also go out on the control socket as `Event::ShellCommand`:
toggling the launcher or overview, volume and brightness keys, and lock requests.
The in-process shell carries them out directly; the events are for a shell in its own process.

## Shell Process

`nimbus-shell` runs the shell as a client of a compositor started with `--no-shell`.
It finds the compositor through `WAYLAND_DISPLAY` and `NIMBUS_SOCKET`, and takes `--config <path>` like the compositor.
It exits with status 0 on SIGTERM or SIGINT, and with a failure when it loses either connection.
One `calloop` loop drives the Wayland connection (through `smithay-client-toolkit`), Slint's timers,
the control socket, and the channels from the services, the PAM worker, the configuration watcher, and the application scanner.

Modules in `crates/nimbus-shell-host/src`:

- `main.rs`: arguments, logging, signals, and the event loop.
- `state.rs`: the `State` the loop runs on, outputs, and the work after each dispatch: actions, timers, rendering.
- `wayland.rs`: the protocol handlers.
- `platform.rs`: the Slint platform, which gives each Slint window a `Renderer`.
- `render/`: the `Renderer` trait, and `SoftwareRenderer`, Slint's software renderer drawing into two alternating
  `wl_shm` buffers with `RepaintBufferType::SwappedBuffers`, so each frame redraws only what changed.
- `surface.rs`: a Slint window on a `wl_surface`.
  It renders at the buffer scale, through a viewport at `wp_fractional_scale_v1` scales or with `set_buffer_scale` otherwise,
  and only when Slint has changes and the previous frame's callback arrived.
- `output.rs`: per output, the `ShellView` on a transparent top-layer surface anchored to every edge with exclusive zone -1,
  and a transparent single-pixel strip per edge whose exclusive zone keeps windows out of the panel and dock.
  The view's surface takes pointer input only inside `ShellView::input_region()`.
  It has no keyboard interactivity until the view wants the keyboard; then it's exclusive and moves to the overlay layer, above fullscreen windows.
- `lock.rs`: locking through `ext-session-lock-v1`; see [Locking](#locking).
- `auth.rs`: PAM on a worker thread, as in the compositor.
- `idle.rs`: an `ext-idle-notify-v1` notification after `power.lock_after_minutes`, which locks.
- `input.rs`: pointer and keyboard input; keys go through the compositor's XKB keymap, with its repeat rate.
- `ipc.rs`: one control socket connection, subscribed to events, which also carries requests; responses reach callbacks in request order.
- `services.rs`: `nimbus-services` and the compositor's `Event::ShellCommand`s, such as volume keys and launcher toggles.
- `actions.rs`: `ShellAction`s, launching applications with an `xdg-activation` token, and the application index.

## Locking

Locked is compositor state, apart from whatever draws the lock screen.
An `ext-session-lock` lock, `Request::Lock`, `--locked`, or the lock marker at startup locks the session.
`Request::Lock` also emits `ShellCommand::Lock`, which asks the shell for a lock screen.
While it's locked, the compositor draws the lock client's surfaces over black, breaks client grabs, and ignores Ctrl+Alt+Backspace.
Without a live lock client, it draws the in-process shell's lock screen, or black without the shell.
`nimbus-shell` asks for the lock state when it starts, and locks with `ext-session-lock` if the session is locked,
so a restarted shell shows its lock screen again.
It creates its lock surfaces once the compositor confirms the lock, because the compositor rejects surfaces of a lock it refused.
logind's lock signal, `Event::ShellCommand` with `lock`, and inactivity also make it lock.
It then accepts a new `ext-session-lock` lock, but refuses one while a live client holds the session.
A lock client that dies leaves the session locked, so a restarted shell or compositor can lock again.
Only the holder's `unlock_and_destroy` unlocks.
Without a live lock client, a password the in-process lock screen accepts, or logind's unlock signal, unlocks too.
`Request::GetLockState` reports the lock state over the control socket.

The lock marker, `$XDG_RUNTIME_DIR/nimbus/locked` (`nimbus_ipc::lock_marker_path`), exists while the session is locked.
The compositor creates it, with mode 0600 in a 0700 directory, as a lock starts and before any locked frame.
It removes the marker once the session unlocks.
A compositor started with `--locked`, or with the marker present, starts locked before it creates any output.

`nimbus-session` deletes a stale marker when the session starts.
When the compositor crashes with the marker present, the session restarts it with `--locked`.
Without the shell, it ends the session instead.
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
The compositor watches the file and applies changes live, so the Settings app only writes the file.
The shell saves its own changes, such as the dark style toggle and dock pins, to the file the compositor was started with.
The shell and Settings both write through `nimbus_config::update` or `update_with`,
which hold a lock on `config.toml.lock` from load to save,
so neither loses the other's changes.
Settings and the compositor parse key chords with the same `nimbus_config::chord` grammar.

## Sessions and Logout

`nimbus-session` treats a compositor exit status of 0 as a logout and anything else as a crash to restart.
A restarted compositor reuses the first one's Wayland socket name, so clients keep a valid `WAYLAND_DISPLAY`.

`nimbus-session` owns autostart, and runs it once per session, after the first compositor reports readiness.
It runs each `config.autostart` command through `/bin/sh -c`, then the XDG autostart entries.
Each autostarted process leads its own process group.
The session terminates these groups when it ends, for any reason, but not when the compositor restarts.
Logging out asks logind to end the session.
Without a logind session, as when nested, `nimbus-services` emits `ServiceEvent::LogoutRequested` and the compositor exits with status 0.

## Testing

- Domain crates have unit tests with fixture directories.
- The shell has tests on Slint's testing backend.
  The shell and apps render reference screenshots off screen through `nimbus_theme::headless`.
- The compositor runs headless in tests: a test client connects over Wayland, maps windows, and checks the control socket.
  The shell tests run it with the real shell and check the composited output through `Request::Screenshot`:
  the panel renders, the launcher and overview toggle over IPC, maximized windows stay below the panel, and notifications show toasts.
  Protocol tests drive `ext-session-lock`, `ext-foreign-toplevel-list`, `ext-idle-notify`, and `wlr-layer-shell` with their own clients.
- The `nimbus-shell` tests run it against the headless compositor with `--no-shell`, which they build first:
  the panel renders, the launcher toggles through shell command events, maximized windows stay below the panel,
  notifications show toasts, `Request::Lock` shows the lock screen on a lock surface,
  a killed shell leaves the session locked and a restarted one locks again, and the exit statuses are right.
- `nimbus-services` and `nimbus-portal` run their D-Bus tests against a private `dbus-daemon`, and skip them with a message when it's missing.
- `cargo test --manifest-path desktop/Cargo.toml --workspace` runs everything.

## Running

- Nested, inside an existing Wayland or X11 session: `cargo run -p nimbus-compositor -- --backend winit`.
- On a TTY: `nimbus-session`, or select "Nimbus" from a display manager using `data/nimbus.desktop`.
- Headless, for tests and screenshots: `nimbus-compositor --backend headless`, with apps started as `SLINT_BACKEND=winit-software`.
- The shell as its own process: start `nimbus-compositor --no-shell`, then `nimbus-shell` with the `WAYLAND_DISPLAY` and `NIMBUS_SOCKET` it prints.

The workspace's `dev` profile keeps only line tables for its own crates and no debug info for dependencies,
because Slint's generated code makes full debug info several hundred megabytes per binary.
