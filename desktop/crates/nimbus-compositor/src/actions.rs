// SPDX-License-Identifier: MIT

//! Control requests and keybinding actions, shared by the control socket and the keyboard.

use crate::state::State;
use crate::wm::WindowMode;
use nimbus_config::Action;
use nimbus_ipc::{Direction, Event, LayoutMode, Request, Response, ShellCommand, WindowId};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn unknown_window(id: WindowId) -> Response {
    Response::Error { message: format!("no window with id {id}") }
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
                self.shell_command(ShellCommand::ToggleLauncher);
                Response::Ok
            }
            Request::ToggleOverview => {
                self.shell_command(ShellCommand::ToggleOverview);
                Response::Ok
            }
            Request::Lock => {
                self.lock_session();
                self.shell_command(ShellCommand::Lock);
                Response::Ok
            }
            Request::GetLockState => Response::LockState { locked: self.nimbus.is_locked() },
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
        let output = match self.screenshot_target(output) {
            Ok(output) => output,
            Err(response) => return response,
        };
        match path {
            // The control socket answers through `screenshot_to` instead, once the file is written.
            Some(path) => self.save_screenshot(&output, path, |response| {
                if let Response::Error { message } = response {
                    tracing::warn!("{message}");
                }
            }),
            None => self.screenshot_output(&output),
        }
        Response::Ok
    }

    /// Captures `output`, or the active one, into `path` and answers through `reply` once the file is written.
    pub fn screenshot_to(
        &mut self,
        output: Option<String>,
        path: PathBuf,
        reply: crate::ipc::Deferred,
    ) {
        match self.screenshot_target(output) {
            Ok(output) => {
                self.save_screenshot(&output, path, move |response| reply.reply(response))
            }
            Err(response) => reply.reply(response),
        }
    }

    fn screenshot_target(
        &self,
        output: Option<String>,
    ) -> Result<smithay::output::Output, Response> {
        match output {
            Some(name) => self
                .nimbus
                .output_by_name(&name)
                .ok_or_else(|| Response::Error { message: format!("no output named '{name}'") }),
            None => self
                .nimbus
                .active_output()
                .ok_or_else(|| Response::Error { message: "there are no outputs".into() }),
        }
    }

    /// Captures on the event loop, which owns the renderer, then encodes and writes on a worker thread.
    fn save_screenshot(
        &mut self,
        output: &smithay::output::Output,
        path: PathBuf,
        reply: impl FnOnce(Response) + Send + 'static,
    ) {
        let capture = match self.backend.capture(&self.nimbus, output) {
            Ok(capture) => capture,
            Err(err) => {
                reply(Response::Error { message: format!("screenshot failed: {err:#}") });
                return;
            }
        };
        let spawned =
            std::thread::Builder::new().name("nimbus-screenshot".into()).spawn(move || {
                reply(match capture.save_png(&path) {
                    Ok(()) => Response::Ok,
                    Err(err) => Response::Error { message: format!("screenshot failed: {err:#}") },
                });
            });
        if let Err(err) = spawned {
            tracing::warn!("screenshot failed: {err}");
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

    /// Asks the shell, through control socket subscribers, to carry out `command` on the active output.
    pub fn shell_command(&mut self, command: ShellCommand) {
        let output = self.nimbus.active_output().map(|o| o.name());
        self.nimbus.emit(Event::ShellCommand { command, output });
    }

    /// Locks the session without a lock client; black covers every output until one takes over.
    pub fn lock_session(&mut self) {
        if self.nimbus.lock.lock() {
            self.lock_changed();
        }
    }

    pub fn unlock_session(&mut self) {
        tracing::info!("unlocked");
        self.nimbus.lock.unlock();
        self.lock_changed();
    }

    /// Brings the lock marker, grabs, and the screen in line with the lock state.
    pub fn lock_changed(&mut self) {
        self.nimbus.sync_lock_marker();
        self.break_grabs_for_lock();
        self.nimbus.queue_redraw_all();
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
                self.shell_command(ShellCommand::VolumeUp);
                None
            }
            Action::VolumeDown => {
                self.shell_command(ShellCommand::VolumeDown);
                None
            }
            Action::ToggleMute => {
                self.shell_command(ShellCommand::ToggleMute);
                None
            }
            Action::BrightnessUp => {
                self.shell_command(ShellCommand::BrightnessUp);
                None
            }
            Action::BrightnessDown => {
                self.shell_command(ShellCommand::BrightnessDown);
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
