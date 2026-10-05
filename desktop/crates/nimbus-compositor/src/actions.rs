// SPDX-License-Identifier: MIT

//! Control requests and keybinding actions, shared by the control socket, the shell, and the keyboard.

use crate::state::State;
use crate::wm::WindowMode;
use nimbus_config::Action;
use nimbus_ipc::{Direction, LayoutMode, Request, Response, WindowId};
use nimbus_services::ServiceCommand;
use nimbus_shell::Osd;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Change per volume or brightness key press.
const LEVEL_STEP: f32 = 0.05;

fn unknown_window(id: WindowId) -> Response {
    Response::Error { message: format!("no window with id {id}") }
}

fn no_shell() -> Response {
    Response::Error { message: "the shell isn't running (started with --no-shell)".into() }
}

impl State {
    pub fn handle_request(&mut self, request: Request) -> Response {
        let response = self.dispatch_request(request);
        self.nimbus.arrange();
        response
    }

    fn dispatch_request(&mut self, request: Request) -> Response {
        let wm = &mut self.nimbus.wm;
        match request {
            Request::GetState => Response::State(self.nimbus.wm_state()),
            // Connections switch to streaming in the IPC layer; elsewhere this is a no-op.
            Request::Subscribe => Response::Ok,
            Request::Activate { id } => {
                if wm.activate(id) {
                    self.nimbus.layer_focus = None;
                    Response::Ok
                } else {
                    unknown_window(id)
                }
            }
            Request::Close { id } => match wm.get(id).and_then(|w| w.toplevel()) {
                Some(toplevel) => {
                    toplevel.send_close();
                    Response::Ok
                }
                None => unknown_window(id),
            },
            Request::SetMinimized { id, minimized } => {
                if wm.set_minimized(id, minimized) {
                    Response::Ok
                } else {
                    unknown_window(id)
                }
            }
            Request::SetMaximized { id, maximized } => {
                self.set_mode(id, WindowMode::Maximized, maximized)
            }
            Request::SetFullscreen { id, fullscreen } => {
                self.set_mode(id, WindowMode::Fullscreen, fullscreen)
            }
            Request::MoveToWorkspace { id, workspace } => {
                if !wm.workspaces().contains(workspace) {
                    return Response::Error {
                        message: format!(
                            "workspace {workspace} doesn't exist; there are {}",
                            wm.workspaces().count()
                        ),
                    };
                }
                if wm.move_to_workspace(id, workspace) { Response::Ok } else { unknown_window(id) }
            }
            Request::FocusDirection { direction } => {
                wm.focus_direction(direction);
                Response::Ok
            }
            Request::SwitchWorkspace { workspace } => {
                if wm.switch_workspace(workspace) {
                    Response::Ok
                } else {
                    Response::Error {
                        message: format!(
                            "workspace {workspace} doesn't exist; there are {}",
                            wm.workspaces().count()
                        ),
                    }
                }
            }
            Request::SetLayout { layout } => {
                wm.set_layout(layout);
                Response::Ok
            }
            Request::Spawn { command } => match self.nimbus.children.spawn(&command) {
                Ok(_) => Response::Ok,
                Err(err) => Response::Error { message: format!("cannot run '{command}': {err}") },
            },
            Request::ToggleLauncher => {
                self.with_shell_on_active_output(|shell, output| shell.toggle_launcher(output))
            }
            Request::ToggleOverview => {
                self.with_shell_on_active_output(|shell, output| shell.toggle_overview(output))
            }
            Request::Lock => {
                if self.lock_session() {
                    Response::Ok
                } else {
                    no_shell()
                }
            }
            Request::ReloadConfig => match self.nimbus.config.reload() {
                Ok(config) => {
                    self.apply_config(config);
                    Response::Ok
                }
                Err(err) => Response::Error { message: err.to_string() },
            },
            Request::Screenshot { output, path } => self.screenshot_request(output, path),
            Request::Quit => {
                self.nimbus.stop();
                Response::Ok
            }
        }
    }

    fn screenshot_request(&mut self, output: Option<String>, path: Option<PathBuf>) -> Response {
        let output = match output {
            Some(name) => match self.nimbus.output_by_name(&name) {
                Some(output) => output,
                None => return Response::Error { message: format!("no output named '{name}'") },
            },
            None => match self.nimbus.active_output() {
                Some(output) => output,
                None => return Response::Error { message: "there are no outputs".into() },
            },
        };
        let Some(path) = path else {
            self.screenshot_output(&output);
            return Response::Ok;
        };
        match self
            .backend
            .capture(&self.nimbus, &output)
            .and_then(|capture| capture.save_png(&path))
        {
            Ok(()) => Response::Ok,
            Err(err) => Response::Error { message: format!("screenshot failed: {err:#}") },
        }
    }

    fn set_mode(&mut self, id: WindowId, mode: WindowMode, on: bool) -> Response {
        let wm = &mut self.nimbus.wm;
        let Some(current) = wm.get(id).map(|w| w.mode) else {
            return unknown_window(id);
        };
        if on {
            wm.set_mode(id, mode);
        } else if current == mode {
            wm.set_mode(id, WindowMode::Normal);
        }
        Response::Ok
    }

    fn with_shell_on_active_output(
        &mut self,
        f: impl FnOnce(&crate::shell_host::ShellHost, &str),
    ) -> Response {
        let output = self.nimbus.active_output().map(|o| o.name()).unwrap_or_default();
        match self.nimbus.shell.as_ref() {
            Some(shell) => {
                f(shell, &output);
                Response::Ok
            }
            None => no_shell(),
        }
    }

    /// Shows the shell's lock screen on every output; returns `false` without a shell.
    pub fn lock_session(&mut self) -> bool {
        match self.nimbus.shell.as_ref() {
            Some(shell) => {
                shell.set_locked(true);
                self.nimbus.queue_redraw_all();
                true
            }
            None => false,
        }
    }

    /// Runs a keybinding action.
    pub fn run_action(&mut self, action: Action) {
        let focused = self.nimbus.wm.focused();
        let request = match action {
            Action::Spawn(command) => Some(Request::Spawn { command }),
            Action::CloseWindow => focused.map(|id| Request::Close { id }),
            Action::ToggleMaximize => {
                focused.map(|id| self.nimbus.wm.toggle_mode(id, WindowMode::Maximized));
                None
            }
            Action::ToggleFullscreen => {
                focused.map(|id| self.nimbus.wm.toggle_mode(id, WindowMode::Fullscreen));
                None
            }
            Action::Minimize => focused.map(|id| Request::SetMinimized { id, minimized: true }),
            Action::ToggleLayout => Some(Request::SetLayout {
                layout: match self.nimbus.wm.layout() {
                    LayoutMode::Floating => LayoutMode::Tiling,
                    LayoutMode::Tiling => LayoutMode::Floating,
                },
            }),
            Action::FocusLeft => Some(Request::FocusDirection { direction: Direction::Left }),
            Action::FocusRight => Some(Request::FocusDirection { direction: Direction::Right }),
            Action::FocusUp => Some(Request::FocusDirection { direction: Direction::Up }),
            Action::FocusDown => Some(Request::FocusDirection { direction: Direction::Down }),
            Action::Workspace(workspace) => Some(Request::SwitchWorkspace { workspace }),
            Action::MoveToWorkspace(workspace) => {
                focused.map(|id| Request::MoveToWorkspace { id, workspace })
            }
            Action::NextWorkspace => {
                Some(Request::SwitchWorkspace { workspace: self.nimbus.wm.workspaces().next() })
            }
            Action::PreviousWorkspace => {
                Some(Request::SwitchWorkspace { workspace: self.nimbus.wm.workspaces().previous() })
            }
            Action::ToggleLauncher => Some(Request::ToggleLauncher),
            Action::ToggleOverview => Some(Request::ToggleOverview),
            Action::Lock => Some(Request::Lock),
            Action::Screenshot => {
                self.screenshot();
                None
            }
            Action::VolumeUp => {
                self.change_volume(LEVEL_STEP);
                None
            }
            Action::VolumeDown => {
                self.change_volume(-LEVEL_STEP);
                None
            }
            Action::ToggleMute => {
                self.toggle_mute();
                None
            }
            Action::BrightnessUp => {
                self.change_brightness(LEVEL_STEP);
                None
            }
            Action::BrightnessDown => {
                self.change_brightness(-LEVEL_STEP);
                None
            }
            Action::Quit => Some(Request::Quit),
        };
        match request {
            Some(request) => {
                if let Response::Error { message } = self.handle_request(request) {
                    tracing::warn!("keybinding failed: {message}");
                }
            }
            None => self.nimbus.arrange(),
        }
    }

    fn change_volume(&mut self, delta: f32) {
        let Some(shell) = self.nimbus.shell.as_ref() else {
            tracing::debug!("volume keys need the shell's services");
            return;
        };
        let Some(audio) = shell.system_state().and_then(|s| s.audio.clone()) else {
            return;
        };
        let level = (audio.volume + delta).clamp(0.0, 1.0);
        shell.send_service(ServiceCommand::SetVolume(level));
        shell.show_osd(Osd::Volume { level, muted: audio.muted && delta <= 0.0 });
    }

    fn toggle_mute(&mut self) {
        let Some(shell) = self.nimbus.shell.as_ref() else {
            return;
        };
        shell.send_service(ServiceCommand::ToggleMute);
        if let Some(audio) = shell.system_state().and_then(|s| s.audio.clone()) {
            shell.show_osd(Osd::Volume { level: audio.volume, muted: !audio.muted });
        }
    }

    fn change_brightness(&mut self, delta: f32) {
        let Some(shell) = self.nimbus.shell.as_ref() else {
            tracing::debug!("brightness keys need the shell's services");
            return;
        };
        let Some(level) = shell.system_state().and_then(|s| s.brightness) else {
            return;
        };
        // Never fully dark: a black screen looks like a hang.
        let level = (level + delta).clamp(0.01, 1.0);
        shell.send_service(ServiceCommand::SetBrightness(level));
        shell.show_osd(Osd::Brightness { level });
    }

    /// Captures the active output and saves it as a PNG in the screenshots directory.
    pub fn screenshot(&mut self) {
        if let Some(output) = self.nimbus.active_output() {
            self.screenshot_output(&output);
        }
    }

    fn screenshot_output(&mut self, output: &smithay::output::Output) {
        let capture = match self.backend.capture(&self.nimbus, output) {
            Ok(capture) => capture,
            Err(err) => {
                tracing::warn!("screenshot failed: {err:#}");
                return;
            }
        };
        let Some(dir) = screenshot_dir() else {
            tracing::warn!("screenshot failed: no pictures directory");
            return;
        };
        let path = dir.join(screenshot_name(SystemTime::now()));
        // PNG encoding takes long enough to drop frames, so it runs off the event loop.
        let spawned = std::thread::Builder::new().name("nimbus-screenshot".into()).spawn(
            move || match capture.save_png(&path) {
                Ok(()) => tracing::info!(path = %path.display(), "saved screenshot"),
                Err(err) => tracing::warn!("screenshot failed: {err:#}"),
            },
        );
        if let Err(err) = spawned {
            tracing::warn!("screenshot failed: {err}");
        }
    }
}

/// `$XDG_PICTURES_DIR/Screenshots`, or `~/Pictures/Screenshots`.
fn screenshot_dir() -> Option<PathBuf> {
    dirs::picture_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Pictures")))
        .map(|p| p.join("Screenshots"))
}

/// `Screenshot from 2026-10-05 14-03-09.png`, in UTC.
fn screenshot_name(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "Screenshot from {year:04}-{month:02}-{day:02} {:02}-{:02}-{:02}.png",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Converts days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn civil_dates_are_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_731), (2026, 10, 5));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn screenshot_names_sort_by_time() {
        let t = UNIX_EPOCH + Duration::from_secs(20_731 * 86_400 + 14 * 3600 + 3 * 60 + 9);
        assert_eq!(screenshot_name(t), "Screenshot from 2026-10-05 14-03-09.png");
    }
}
