// SPDX-License-Identifier: MIT

//! Window-management domain model shared by the compositor, the shell, and `nimbusctl`.
//!
//! The control socket speaks newline-delimited JSON: a client writes one [`Request`] per line,
//! and the compositor answers each with one [`Response`] line.
//! A client that sends [`Request::Subscribe`] then receives an [`Event`] line for every change.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Compositor-assigned toplevel identifier, unique for the compositor's lifetime.
pub type WindowId = u64;
/// Zero-based workspace index.
pub type WorkspaceId = u32;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: WindowId,
    /// The `xdg_toplevel` app id, which usually matches a desktop entry id without `.desktop`.
    pub app_id: String,
    pub title: String,
    pub workspace: WorkspaceId,
    pub output: String,
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    /// Width of the current mode in physical pixels.
    pub width: i32,
    /// Height of the current mode in physical pixels.
    pub height: i32,
    pub scale: f64,
    pub refresh_mhz: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutMode {
    #[default]
    Floating,
    Tiling,
}

/// The compositor's complete observable state, sent in reply to [`Request::GetState`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompositorState {
    pub windows: Vec<WindowInfo>,
    pub outputs: Vec<OutputInfo>,
    pub workspace_count: u32,
    pub active_workspace: WorkspaceId,
    pub layout: LayoutMode,
}

/// Which outputs are off, sent in [`Response::PowerState`] and [`Event::PowerState`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerState {
    /// Whether outputs are blanked after inactivity or by [`Request::Blank`], until the next input.
    pub blanked: bool,
    /// The names of the enabled outputs that are off, blanked or turned off through wlr-output-power-management.
    pub off: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A command for the compositor from a control socket client, such as the shell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case")]
pub enum Request {
    GetState,
    /// Switch the connection to event streaming; see [`Event`].
    Subscribe,
    Activate {
        id: WindowId,
    },
    Close {
        id: WindowId,
    },
    SetMinimized {
        id: WindowId,
        minimized: bool,
    },
    SetMaximized {
        id: WindowId,
        maximized: bool,
    },
    SetFullscreen {
        id: WindowId,
        fullscreen: bool,
    },
    MoveToWorkspace {
        id: WindowId,
        workspace: WorkspaceId,
    },
    FocusDirection {
        direction: Direction,
    },
    SwitchWorkspace {
        workspace: WorkspaceId,
    },
    SetLayout {
        layout: LayoutMode,
    },
    /// Run a command line through `sh -c` with the session's Wayland environment.
    Spawn {
        command: String,
    },
    /// Emit [`ShellCommand::ToggleLauncher`] to subscribers.
    ToggleLauncher,
    /// Emit [`ShellCommand::ToggleOverview`] to subscribers.
    ToggleOverview,
    /// Lock the session; see [`Event::LockState`].
    Lock,
    /// Ask whether the session is locked; the answer is [`Response::LockState`].
    GetLockState,
    /// Turn every output off until the next input; see [`Event::PowerState`].
    Blank,
    /// Ask which outputs are off; the answer is [`Response::PowerState`].
    GetPowerState,
    ReloadConfig,
    /// Save a PNG of an output: the named one, or the one with the pointer.
    /// With a `path`, the compositor answers once the file is written;
    /// without one, it saves into `$XDG_PICTURES_DIR/Screenshots` in the background.
    Screenshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
    Quit,
    /// Move the pointer to a point of the named output, in logical pixels, and click the left button there.
    /// Only the headless backend accepts it, for tests.
    Click {
        output: String,
        x: f64,
        y: f64,
    },
    /// Press and release the key with this Linux input event code, such as 1 for Escape.
    /// Only the headless backend accepts it, for tests.
    PressKey {
        code: u32,
    },
    /// Press or release the key with this Linux input event code, to hold modifiers such as Alt.
    /// Only the headless backend accepts it, for tests.
    Key {
        code: u32,
        pressed: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "kebab-case")]
pub enum Response {
    Ok,
    State(CompositorState),
    LockState {
        locked: bool,
        /// Whether a live `ext-session-lock` client holds the lock.
        held: bool,
    },
    PowerState(PowerState),
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    WindowOpened(WindowInfo),
    WindowChanged(WindowInfo),
    WindowClosed {
        id: WindowId,
    },
    WorkspaceActivated {
        workspace: WorkspaceId,
    },
    OutputsChanged {
        outputs: Vec<OutputInfo>,
    },
    LayoutChanged {
        layout: LayoutMode,
    },
    /// Something the shell should do, triggered in the compositor by a shortcut or a request.
    ShellCommand {
        command: ShellCommand,
        /// The output the user is working on, the one with the pointer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// The session lock changed.
    /// A session that's `locked` but not `held` shows black until a lock screen client takes over.
    LockState {
        locked: bool,
        /// Whether a live `ext-session-lock` client holds the lock.
        held: bool,
    },
    /// Outputs turned off or on.
    PowerState(PowerState),
}

/// A command for the shell, delivered as [`Event::ShellCommand`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellCommand {
    ToggleLauncher,
    ToggleOverview,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    BrightnessUp,
    BrightnessDown,
    /// Show the window switcher with `windows`, most recently focused first, highlighting `selected`.
    SwitcherOpen {
        windows: Vec<WindowId>,
        selected: WindowId,
    },
    /// Highlight another window in the open switcher.
    SwitcherStep {
        selected: WindowId,
    },
    /// Hide the switcher; the compositor focuses the highlighted window.
    SwitcherCommit,
    /// Hide the switcher without changing focus.
    SwitcherCancel,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on the Nimbus control socket: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed message on the Nimbus control socket: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the Nimbus control socket closed the connection")]
    Closed,
    #[error("the compositor rejected the request: {0}")]
    Rejected(String),
}

/// Environment variable through which the compositor advertises its control socket.
pub const SOCKET_ENV: &str = "NIMBUS_SOCKET";

/// The line the compositor prints on standard output once its sockets accept connections.
///
/// It reads `NIMBUS_READY WAYLAND_DISPLAY=<name> [DISPLAY=<x11 display>] NIMBUS_SOCKET=<path>`;
/// the path may contain spaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub wayland_display: String,
    /// The X11 display for X11 apps, such as `:1`, when the compositor serves one.
    pub x11_display: Option<String>,
    pub socket: PathBuf,
}

impl Ready {
    const PREFIX: &str = "NIMBUS_READY ";
    const DISPLAY_KEY: &str = "WAYLAND_DISPLAY=";
    const X11_DISPLAY_KEY: &str = "DISPLAY=";
    const SOCKET_KEY: &str = " NIMBUS_SOCKET=";

    /// Parses a ready line, with or without its line break.
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.trim_end_matches(['\r', '\n']).strip_prefix(Self::PREFIX)?;
        let rest = rest.trim_start().strip_prefix(Self::DISPLAY_KEY)?;
        let (displays, socket) = rest.split_once(Self::SOCKET_KEY)?;
        let mut displays = displays.split_whitespace();
        let wayland_display = displays.next()?.to_string();
        let x11_display = match displays.next() {
            Some(field) => Some(field.strip_prefix(Self::X11_DISPLAY_KEY)?.to_string()),
            None => None,
        };
        if x11_display.as_deref() == Some("") || displays.next().is_some() || socket.is_empty() {
            return None;
        }
        Some(Self { wayland_display, x11_display, socket: PathBuf::from(socket) })
    }
}

impl std::fmt::Display for Ready {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (prefix, display_key, socket_key) = (Self::PREFIX, Self::DISPLAY_KEY, Self::SOCKET_KEY);
        write!(f, "{prefix}{display_key}{}", self.wayland_display)?;
        if let Some(x11_display) = &self.x11_display {
            write!(f, " {}{x11_display}", Self::X11_DISPLAY_KEY)?;
        }
        write!(f, "{socket_key}{}", self.socket.display())
    }
}

/// Returns the control socket path: `$NIMBUS_SOCKET`, or `$XDG_RUNTIME_DIR/nimbus-<wayland display>.sock`.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV) {
        return Some(path.into());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    Some(socket_path_for(Path::new(&runtime), &display))
}

/// Returns the control socket of the compositor serving the Wayland socket `display` in `runtime_dir`.
pub fn socket_path_for(runtime_dir: &Path, display: &str) -> PathBuf {
    runtime_dir.join(format!("nimbus-{display}.sock"))
}

/// Returns the file that exists while the session is locked, in `runtime_dir`.
///
/// `nimbus-session` restarts a compositor that exits while this file exists with `--locked`,
/// so a crash never unlocks the session.
pub fn lock_marker_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("nimbus").join("locked")
}

/// Writes `message` as one JSON line and flushes.
pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<(), Error> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Reads one JSON line, or returns [`Error::Closed`] at end of stream.
pub fn read_message<T: for<'de> Deserialize<'de>>(reader: &mut impl BufRead) -> Result<T, Error> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(Error::Closed);
    }
    Ok(serde_json::from_str(&line)?)
}

/// A blocking client for the control socket, as used by `nimbusctl`.
pub struct Client {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

impl Client {
    pub fn connect() -> Result<Self, Error> {
        let path = socket_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither NIMBUS_SOCKET nor XDG_RUNTIME_DIR is set",
            )
        })?;
        Self::connect_to(&path)
    }

    pub fn connect_to(path: &std::path::Path) -> Result<Self, Error> {
        let writer = std::os::unix::net::UnixStream::connect(path)?;
        let reader = std::io::BufReader::new(writer.try_clone()?);
        Ok(Self { reader, writer })
    }

    pub fn request(&mut self, request: &Request) -> Result<Response, Error> {
        write_message(&mut self.writer, request)?;
        match read_message(&mut self.reader)? {
            Response::Error { message } => Err(Error::Rejected(message)),
            response => Ok(response),
        }
    }

    /// Subscribes and returns an iterator over events; it ends when the compositor exits.
    pub fn subscribe(mut self) -> Result<impl Iterator<Item = Result<Event, Error>>, Error> {
        self.request(&Request::Subscribe)?;
        let mut reader = self.reader;
        Ok(std::iter::from_fn(move || match read_message(&mut reader) {
            Err(Error::Closed) => None,
            other => Some(other),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_parsing() {
        assert_eq!(
            Ready::parse(
                "NIMBUS_READY WAYLAND_DISPLAY=wayland-1 NIMBUS_SOCKET=/run/user/1000/nimbus-wayland-1.sock\n"
            ),
            Some(Ready {
                wayland_display: "wayland-1".into(),
                x11_display: None,
                socket: "/run/user/1000/nimbus-wayland-1.sock".into()
            })
        );
        assert_eq!(
            Ready::parse("NIMBUS_READY WAYLAND_DISPLAY=w DISPLAY=:2 NIMBUS_SOCKET=/x")
                .and_then(|r| r.x11_display),
            Some(":2".into())
        );
        assert_eq!(Ready::parse("NIMBUS_READY WAYLAND_DISPLAY=w DISPLAY= NIMBUS_SOCKET=/x"), None);
        assert_eq!(Ready::parse("NIMBUS_READY WAYLAND_DISPLAY=w OTHER=1 NIMBUS_SOCKET=/x"), None);
        assert_eq!(
            Ready::parse("NIMBUS_READY WAYLAND_DISPLAY=w NIMBUS_SOCKET=/tmp/a b.sock")
                .map(|r| r.socket),
            Some(PathBuf::from("/tmp/a b.sock"))
        );
        assert_eq!(Ready::parse("NIMBUS_READY WAYLAND_DISPLAY= NIMBUS_SOCKET=/x"), None);
        assert_eq!(Ready::parse("NIMBUS_READY WAYLAND_DISPLAY=w NIMBUS_SOCKET="), None);
        assert_eq!(Ready::parse("NIMBUS_READY NIMBUS_SOCKET=/x"), None);
        assert_eq!(Ready::parse("starting compositor"), None);
    }

    #[test]
    fn ready_line_round_trips() {
        let mut ready = Ready {
            wayland_display: "wayland-2".into(),
            x11_display: None,
            socket: "/tmp/a b.sock".into(),
        };
        let line = ready.to_string();
        assert_eq!(line, "NIMBUS_READY WAYLAND_DISPLAY=wayland-2 NIMBUS_SOCKET=/tmp/a b.sock");
        assert_eq!(Ready::parse(&line), Some(ready.clone()));

        ready.x11_display = Some(":1".into());
        let line = ready.to_string();
        assert_eq!(
            line,
            "NIMBUS_READY WAYLAND_DISPLAY=wayland-2 DISPLAY=:1 NIMBUS_SOCKET=/tmp/a b.sock"
        );
        assert_eq!(Ready::parse(&line), Some(ready));
    }

    #[test]
    fn request_wire_format_is_tagged_kebab_case() {
        let json = serde_json::to_string(&Request::SwitchWorkspace { workspace: 2 }).unwrap();
        assert_eq!(json, r#"{"request":"switch-workspace","workspace":2}"#);
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Request::SwitchWorkspace { workspace: 2 });
    }

    #[test]
    fn screenshot_fields_are_optional() {
        let json =
            serde_json::to_string(&Request::Screenshot { output: None, path: None }).unwrap();
        assert_eq!(json, r#"{"request":"screenshot"}"#);
        let back: Request =
            serde_json::from_str(r#"{"request":"screenshot","path":"/tmp/a.png"}"#).unwrap();
        assert_eq!(back, Request::Screenshot { output: None, path: Some("/tmp/a.png".into()) });
    }

    #[test]
    fn runtime_paths() {
        let runtime = Path::new("/run/user/1000");
        assert_eq!(
            socket_path_for(runtime, "wayland-1"),
            Path::new("/run/user/1000/nimbus-wayland-1.sock")
        );
        assert_eq!(lock_marker_path(runtime), Path::new("/run/user/1000/nimbus/locked"));
    }

    #[test]
    fn lock_state_wire_format() {
        let json = serde_json::to_string(&Request::GetLockState).unwrap();
        assert_eq!(json, r#"{"request":"get-lock-state"}"#);
        let json =
            serde_json::to_string(&Response::LockState { locked: true, held: false }).unwrap();
        assert_eq!(json, r#"{"response":"lock-state","locked":true,"held":false}"#);
        let json = serde_json::to_string(&Event::LockState { locked: true, held: true }).unwrap();
        assert_eq!(json, r#"{"event":"lock-state","locked":true,"held":true}"#);
    }

    #[test]
    fn power_state_wire_format() {
        let json = serde_json::to_string(&Request::GetPowerState).unwrap();
        assert_eq!(json, r#"{"request":"get-power-state"}"#);
        let state = PowerState { blanked: true, off: vec!["DP-1".into()] };
        let json = serde_json::to_string(&Response::PowerState(state.clone())).unwrap();
        assert_eq!(json, r#"{"response":"power-state","blanked":true,"off":["DP-1"]}"#);
        let json = serde_json::to_string(&Event::PowerState(state.clone())).unwrap();
        assert_eq!(json, r#"{"event":"power-state","blanked":true,"off":["DP-1"]}"#);
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), Event::PowerState(state));
    }

    #[test]
    fn shell_command_wire_format() {
        let event = Event::ShellCommand {
            command: ShellCommand::ToggleLauncher,
            output: Some("DP-1".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"event":"shell-command","command":"toggle-launcher","output":"DP-1"}"#
        );
        let back: Event =
            serde_json::from_str(r#"{"event":"shell-command","command":"volume-up"}"#).unwrap();
        assert_eq!(back, Event::ShellCommand { command: ShellCommand::VolumeUp, output: None });
    }

    #[test]
    fn switcher_command_wire_format() {
        let event = Event::ShellCommand {
            command: ShellCommand::SwitcherOpen { windows: vec![3, 1, 2], selected: 1 },
            output: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"event":"shell-command","command":{"switcher-open":{"windows":[3,1,2],"selected":1}}}"#
        );
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), event);
        let step: ShellCommand =
            serde_json::from_str(r#"{"switcher-step":{"selected":2}}"#).unwrap();
        assert_eq!(step, ShellCommand::SwitcherStep { selected: 2 });
        let commit: ShellCommand = serde_json::from_str(r#""switcher-commit""#).unwrap();
        assert_eq!(commit, ShellCommand::SwitcherCommit);
    }

    #[test]
    fn messages_round_trip_through_lines() {
        let mut buffer = Vec::new();
        let event =
            Event::WindowOpened(WindowInfo { id: 7, app_id: "foot".into(), ..Default::default() });
        write_message(&mut buffer, &event).unwrap();
        write_message(&mut buffer, &Event::WindowClosed { id: 7 }).unwrap();
        let mut reader = std::io::BufReader::new(buffer.as_slice());
        assert_eq!(read_message::<Event>(&mut reader).unwrap(), event);
        assert_eq!(read_message::<Event>(&mut reader).unwrap(), Event::WindowClosed { id: 7 });
        assert!(matches!(read_message::<Event>(&mut reader), Err(Error::Closed)));
    }

    #[test]
    fn client_reports_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let _: Request = read_message(&mut reader).unwrap();
            write_message(&mut writer, &Response::Error { message: "no such window".into() })
                .unwrap();
        });
        let mut client = Client::connect_to(&path).unwrap();
        let result = client.request(&Request::Close { id: 1 });
        assert!(matches!(result, Err(Error::Rejected(m)) if m == "no such window"));
        server.join().unwrap();
    }
}
