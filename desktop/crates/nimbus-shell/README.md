<!-- SPDX-License-Identifier: MIT -->

# nimbus-shell

The Nimbus desktop shell: top panel, dock, launcher, overview, quick settings, calendar and notification center,
toasts, on-screen display, and lock screen.
It's one transparent Slint window per output, built on the `@nimbus/theme.slint` design system.

![Idle desktop](../../docs/screenshots/shell-idle.png)

| Overview | Launcher |
| --- | --- |
| ![Overview](../../docs/screenshots/shell-overview.png) | ![Launcher](../../docs/screenshots/shell-launcher.png) |
| **Quick settings** | **Calendar and notifications** |
| ![Quick settings](../../docs/screenshots/shell-quick-settings.png) | ![Calendar](../../docs/screenshots/shell-calendar.png) |
| **Toasts and OSD** | **Lock screen** |
| ![Toasts and OSD](../../docs/screenshots/shell-toast-osd.png) | ![Lock screen](../../docs/screenshots/shell-lock.png) |

## Hosting the Shell

The compositor creates a `Shell` per output after setting the Slint platform, then:

- Feeds it data: `set_config`, `set_output_name`, `set_compositor_state` and `handle_compositor_event`,
  `set_apps`, `handle_service_event`, and `show_osd` for hardware keys.
- Handles each `ShellAction` it emits: compositor requests, service commands, launches, and opening Settings.
- Calls `toggle_launcher` and `toggle_overview` for the Super key bindings.
- Routes pointer input inside `input_region()` to the shell, and keyboard input while `wants_keyboard()` is true.
  Query the region after rendering, since new toasts count from the frame that draws them.
- Keeps maximized and tiled windows out of `exclusive_zone()`.
- Calls `set_config_path` when it runs with `--config`, because the dark style toggle and dock pins save there.

## Unlocking

The lock screen doesn't check passwords itself.
Register a handler with `on_unlock_attempt`; it receives the typed password while the lock screen shows a spinner.
Answer with `set_locked(false)` on success or `unlock_failed()` on failure, for example after a PAM conversation.

```rust
let shell = Rc::new(shell);
let weak = Rc::downgrade(&shell);
shell.on_unlock_attempt(move |password| {
    let Some(shell) = weak.upgrade() else { return };
    if pam_authenticate(&password) { shell.set_locked(false) } else { shell.unlock_failed() }
});
```

## Preview and Tests

`cargo run -p nimbus-shell --features preview --bin nimbus-shell-preview` runs the shell in a 1280x800 window
with a mock session that reacts to clicks, and logs every action.
The lock screen accepts any password except `wrong`.

`cargo test -p nimbus-shell` runs unit tests, behavior tests on Slint's testing backend,
and renders every state with the software renderer.
Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/shell-*.png`.
