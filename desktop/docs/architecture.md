# Nimbus Architecture

Nimbus is a Wayland desktop environment written in Rust, with every user interface built in Slint.
It covers the same ground as GNOME and KDE Plasma: a compositor, a desktop shell, system services, and core apps.

## Design Principles

- **Wayland only.** There's no X11 session; XWayland is optional and runs on demand.
- **One process draws the desktop.** The compositor hosts the shell in-process.
  The shell is a Slint component rendered with Slint's software renderer into a buffer the compositor composites.
  This avoids a custom layer-shell backend for Slint, keeps input routing synchronous, and needs no shell IPC.
- **Crates are boundaries.** Domain crates (`nimbus-ipc`, `nimbus-config`, `nimbus-xdg`, `nimbus-services`) have no UI.
  The shell is a view over them and doesn't know how it's displayed.
  The compositor is the only crate that wires everything together.
- **Single-threaded UI, async edges.** The compositor runs one `calloop` event loop on the main thread, which also drives Slint.
  D-Bus work runs on a Tokio runtime in `nimbus-services` and reaches the main thread through a `calloop` channel.
- **Degrade, don't fail.** A missing D-Bus daemon, backlight, or battery hides that feature; it never stops the session.
- **Standards first.** Desktop entries, icon themes, `org.freedesktop.Notifications`, MPRIS, UPower, NetworkManager, logind,
  and `xdg-shell`, `xdg-decoration`, `wlr-layer-shell`, and `xdg-activation` on the Wayland side.

## Crates

| Crate | Kind | Responsibility |
| --- | --- | --- |
| `nimbus-ipc` | lib | Window/workspace model; JSON-lines protocol on the control socket; blocking client; runtime paths of the control socket and lock marker. |
| `nimbus-config` | lib | TOML configuration schema with defaults, atomic save, locked load-modify-save (`nimbus_config::update`), the key chord grammar (`chord`), and file watching. |
| `nimbus-xdg` | lib | Desktop entries, icon theme lookup, fuzzy app search, launching. |
| `nimbus-services` | lib | Tokio + zbus: notifications server, UPower, NetworkManager, audio, backlight, MPRIS, BlueZ, logind. |
| `nimbus-theme` | lib + Slint library | Design tokens and components imported as `@nimbus/theme.slint`; off-screen software rendering for screenshots behind the `headless` feature. |
| `nimbus-shell` | lib + preview bin | Panel, dock, launcher, overview, quick settings, notification center, toasts, OSD, lock screen. |
| `nimbus-compositor` | bin | Smithay compositor: backends, window management, input, shell hosting, control socket. |
| `nimbus-session` | bins | `nimbus-session` starts and supervises the compositor and runs autostart; `nimbusctl` is the command-line client. |
| `nimbus-settings` | app | System settings, editing `nimbus-config`. |
| `nimbus-files` | app | File manager. |
| `nimbus-terminal` | app | Terminal emulator on `alacritty_terminal`. |
| `nimbus-monitor` | app | System monitor on `sysinfo`. |

Dependency direction (arrows point at dependencies):

```text
nimbus-compositor ──> nimbus-shell ──> nimbus-theme ──> nimbus-config
        │                  │
        │                  ├──> nimbus-ipc
        │                  ├──> nimbus-xdg
        │                  └──> nimbus-services
        └──> (all of the above)
apps ──> nimbus-theme[headless], nimbus-config
nimbus-files ──> nimbus-xdg
nimbus-settings ──> nimbus-ipc
nimbus-session ──> nimbus-ipc, nimbus-config
```

## Compositor

Modules in `crates/nimbus-compositor/src`:

- `main.rs`: argument parsing (`--backend winit|udev|headless`), logging, startup.
- `lock_marker.rs`: keeps the lock marker in step with the lock state; see [Locking](#locking).
- `state.rs`: the `Nimbus` state struct and Smithay handler implementations
  (compositor, xdg-shell, xdg-decoration, layer-shell, seat, data device, primary selection, ext and wlr data control,
  output, shm, dmabuf, xdg-activation, presentation, viewporter, fractional scale).
- `backend/winit.rs`: nested session in a window, for development.
- `backend/udev.rs`: DRM/KMS, GBM, libinput, and libseat for a real session, with hotplug.
- `backend/headless.rs`: no output device; renders with Pixman into memory, for tests and screenshots.
- `wm/`: window management: workspaces, focus stack, floating placement, a tiling layout (master-stack),
  maximize/fullscreen/minimize, interactive move and resize, and the shell's exclusive zone.
  Layouts implement a `Layout` trait so more can be added.
- `input.rs`: keyboard shortcuts from `nimbus-config`, pointer and keyboard routing between the shell and clients.
- `keybindings.rs`: resolves chords parsed with `nimbus_config::chord` to XKB keysyms.
- `shell_host.rs`: the Slint platform. It implements `slint::platform::Platform`,
  creates one `MinimalSoftwareWindow` per output, renders damaged regions into a `MemoryRenderBuffer`,
  forwards input inside `Shell::input_region()`, and maps `ShellAction`s to compositor requests, services, and launches.
- `auth.rs`: checks lock screen passwords through PAM on a worker thread and reports back through a `calloop` channel.
  It uses the `nimbus` PAM service from `data/pam.d/nimbus` when installed, otherwise `login`.
- `ipc.rs`: the control socket server, a `calloop` source per connection.
- `render.rs`: the scene shared by all backends, the wallpaper or built-in gradient backdrop, and screenshots.

Command line, which `nimbus-session` relies on:

```text
nimbus-compositor [--backend winit|udev|headless] [--socket <wayland socket name>] [--config <path>] [--no-shell | --locked]
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

The shell is one `ShellWindow` per output: a transparent full-output window.
Everything outside `Shell::input_region()` passes through to client windows.
It exposes data setters and emits `ShellAction`s; see `crates/nimbus-shell/src/lib.rs`.

Surfaces:

- **Top panel**: activities button, workspace indicator, focused app, clock and calendar popup, status icons (network, audio, battery), quick settings.
- **Dock**: favorites and running apps, with indicators and per-app window lists.
- **Launcher**: a search field over `AppIndex::search`, keyboard navigation, app grid.
- **Overview**: workspace thumbnails as labeled window cards, with switching and moving between workspaces.
- **Quick settings**: volume and brightness sliders, Wi-Fi, Bluetooth, do-not-disturb, dark mode, media controls, power menu.
- **Notifications**: toasts with actions and timeouts, and a notification center with history in the calendar popup.
- **OSD**: volume and brightness feedback.
- **Lock screen**: clock and password field; authentication goes through PAM in the compositor.
  The shell hands passwords to the handler registered with `Shell::on_unlock_attempt`,
  and the compositor answers with `Shell::set_locked(false)` or `Shell::unlock_failed()`.
  logind's lock signal, `nimbusctl lock`, and `power.lock_after_minutes` of inactivity all lock the session.

## Locking

The session locks through the shell's lock screen, or through an `ext-session-lock` client.
While it's locked, the compositor draws only the lock screen over black, breaks client grabs, and ignores Ctrl+Alt+Backspace.
`Request::GetLockState` reports the lock state over the control socket.

The lock marker, `$XDG_RUNTIME_DIR/nimbus/locked` (`nimbus_ipc::lock_marker_path`), exists while the session is locked.
The compositor creates it, with mode 0600 in a 0700 directory, as a lock starts and before any locked frame.
It removes the marker once the session unlocks.
A compositor started with `--locked`, or with the marker present, starts locked before it creates any output.
`--locked` can't be combined with `--no-shell`.

`nimbus-session` deletes a stale marker when the session starts.
When the compositor crashes with the marker present, the session restarts it with `--locked`.
Without the shell, it ends the session instead.
The marker is per runtime directory, so a nested compositor in the same `XDG_RUNTIME_DIR` shares it with the real session.

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
- `cargo test --manifest-path desktop/Cargo.toml --workspace` runs everything.

## Running

- Nested, inside an existing Wayland or X11 session: `cargo run -p nimbus-compositor -- --backend winit`.
- On a TTY: `nimbus-session`, or select "Nimbus" from a display manager using `data/nimbus.desktop`.
- Headless, for tests and screenshots: `nimbus-compositor --backend headless`, with apps started as `SLINT_BACKEND=winit-software`.

The workspace's `dev` profile keeps only line tables for its own crates and no debug info for dependencies,
because Slint's generated code makes full debug info several hundred megabytes per binary.
