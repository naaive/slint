<!-- SPDX-License-Identifier: MIT -->

# nimbus-shell

The Nimbus desktop shell: top panel, dock, launcher, overview, quick settings, calendar and notification center,
toasts, on-screen display, window switcher, and lock screen.
It's one shared model shown by a view per output, whose parts each have a Slint window,
built on the `@nimbus/theme.slint` design system.

![Idle desktop](../../docs/screenshots/shell-idle.png)

| Overview | Launcher |
| --- | --- |
| ![Overview](../../docs/screenshots/shell-overview.png) | ![Launcher](../../docs/screenshots/shell-launcher.png) |
| **Quick settings** | **Calendar and notifications** |
| ![Quick settings](../../docs/screenshots/shell-quick-settings.png) | ![Calendar](../../docs/screenshots/shell-calendar.png) |
| **Toasts and OSD** | **Lock screen** |
| ![Toasts and OSD](../../docs/screenshots/shell-toast-osd.png) | ![Lock screen](../../docs/screenshots/shell-lock.png) |
| **Window switcher** | |
| ![Window switcher](../../docs/screenshots/shell-switcher.png) | |

## Hosting the Shell

The host creates one `ShellModel` after setting the Slint platform, then:

- Feeds it data: `set_config`, `set_compositor_state` and `handle_compositor_event`,
  `set_apps`, `handle_service_event`, and `show_osd` for hardware keys.
- Handles each `ShellAction` it emits: compositor requests, service commands, launches, and opening Settings.
- Calls `set_config_path` when it runs with `--config`, because the dark style toggle and dock pins save there.

For each output, it creates a `ShellView` with `ShellView::new(&model, output)`, then:

- Keeps a window for each of the view's `parts()`, made with `create(part)` and dropped once the part is no longer listed.
  The panel, dock, overlay, toasts, and OSD go where `PartWindow::placement()` says, on a layer surface for example.
  A popup opens next to its parent part as `popup_placement()` says; call `close_popup` when the host dismisses it.
  Drop popups before the parts below them.
- Routes pointer input inside each window's `input_region()` to it, and keyboard input to the overlay and popups.
  Query the region after rendering, since new toasts count from the frame that draws them.
- Calls `toggle_launcher` and `toggle_overview` for the Super key bindings.

Views share everything else, so a toast closed on one output closes on all of them.

## Locking

`ShellModel::set_locked(true)` closes everything open in the views.
While locked, the host shows a `LockView` on each output, a window apart from the output's `ShellView`,
and drops it after unlocking.

The lock screen doesn't check passwords itself.
Register a handler with `ShellModel::on_unlock_attempt`; it receives the typed password while every lock screen shows a spinner.
Answer with `set_locked(false)` on success or `unlock_failed()` on failure, for example after a PAM conversation.

```rust
model.on_unlock_attempt(move |password| auth.submit(password));
// Once the check finishes:
if ok { model.set_locked(false) } else { model.unlock_failed() }
```

## Preview and Tests

`cargo run -p nimbus-shell --features preview --bin nimbus-shell-preview` runs the shell in a 1280x800 window
with a mock session that reacts to clicks, and logs every action.
The parts render off screen, and the window shows them composited.
Locking shows the lock screen, which accepts any password except `wrong`.

`cargo test -p nimbus-shell` runs unit tests, behavior tests on Slint's testing backend,
and renders every state with the software renderer, compositing the parts as the compositor would.
Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/shell-*.png`.
