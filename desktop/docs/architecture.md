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
| `nimbus-ipc` | lib | Window/workspace model; JSON-lines protocol on the control socket; blocking client. |
| `nimbus-config` | lib | TOML configuration schema with defaults, atomic save, and file watching. |
| `nimbus-xdg` | lib | Desktop entries, icon theme lookup, fuzzy app search, launching. |
| `nimbus-services` | lib | Tokio + zbus: notifications server, UPower, NetworkManager, audio, backlight, MPRIS, BlueZ, logind. |
| `nimbus-theme` | lib + Slint library | Design tokens and components imported as `@nimbus/theme.slint`. |
| `nimbus-shell` | lib + preview bin | Panel, dock, launcher, overview, quick settings, notification center, toasts, OSD, lock screen. |
| `nimbus-compositor` | bin | Smithay compositor: backends, window management, input, shell hosting, control socket. |
| `nimbus-session` | bins | `nimbus-session` starts and supervises the compositor; `nimbusctl` is the command-line client. |
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
apps ──> nimbus-theme, nimbus-config
nimbus-session ──> nimbus-ipc, nimbus-config
```

## Compositor

Modules in `crates/nimbus-compositor/src`:

- `main.rs`: argument parsing (`--backend winit|udev|headless`), logging, startup.
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
- `shell_host.rs`: the Slint platform. It implements `slint::platform::Platform`,
  creates one `MinimalSoftwareWindow` per output, renders damaged regions into a `MemoryRenderBuffer`,
  forwards input inside `Shell::input_region()`, and maps `ShellAction`s to compositor requests, services, and launches.
- `auth.rs`: checks lock screen passwords through PAM on a worker thread and reports back through a `calloop` channel.
  It uses the `nimbus` PAM service from `data/pam.d/nimbus` when installed, otherwise `login`.
- `ipc.rs`: the control socket server, a `calloop` source per connection.
- `render.rs`: the scene shared by all backends, the wallpaper or built-in gradient backdrop, and screenshots.

Command line, which `nimbus-session` relies on:

```text
nimbus-compositor [--backend winit|udev|headless] [--socket <wayland socket name>] [--config <path>] [--no-shell]
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

## Theming

`nimbus-theme` owns all colors, spacing, radii, typography, and motion as a `Theme` global,
plus components (buttons, toggles, sliders, cards, list rows, search field, icons).
Every Nimbus UI imports it, so the shell and apps look like one product.
`ThemeSettings::from_config` resolves the user's choices, and `apply_theme!` pushes them into a component's `Theme` global.

## Configuration

`$XDG_CONFIG_HOME/nimbus/config.toml`; see `nimbus-config` for the schema.
The compositor watches the file and applies changes live, so the Settings app only writes the file.
The shell saves its own changes, such as the dark style toggle and dock pins, to the file the compositor was started with.

## Sessions and Logout

`nimbus-session` treats a compositor exit status of 0 as a logout and anything else as a crash to restart.
Logging out asks logind to end the session.
Without a logind session, as when nested, `nimbus-services` emits `ServiceEvent::LogoutRequested` and the compositor exits with status 0.

## Testing

- Domain crates have unit tests with fixture directories.
- The shell has tests on Slint's testing backend, and renders reference screenshots with the software renderer.
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
