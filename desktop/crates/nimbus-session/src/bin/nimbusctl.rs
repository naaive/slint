// SPDX-License-Identifier: MIT

//! `nimbusctl`: control a running Nimbus compositor from the command line.

use clap::{Parser, Subcommand, ValueEnum};
use nimbus_ipc::{
    Client, CompositorState, Direction, LayoutMode, PowerState, Request, Response, WindowId,
};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

const EXIT_REJECTED: u8 = 1;
const EXIT_CONNECTION: u8 = 2;

/// Control the Nimbus desktop.
///
/// Workspaces are numbered from 1, as in the panel.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// Control socket instead of $NIMBUS_SOCKET or $XDG_RUNTIME_DIR/nimbus-$WAYLAND_DISPLAY.sock.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, PartialEq, Subcommand)]
enum Cmd {
    /// Show windows, workspaces, and outputs.
    State {
        /// Print the raw state as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show whether the session is locked and which screens are off.
    Status,
    /// Print compositor events as JSON lines until the compositor exits.
    Watch,
    /// Focus a window, switching to its workspace.
    Activate { id: WindowId },
    /// Ask a window to close.
    Close { id: WindowId },
    /// Hide a window until it's activated.
    Minimize { id: WindowId },
    /// Show a minimized window again.
    Unminimize { id: WindowId },
    /// Maximize a window.
    Maximize {
        id: WindowId,
        /// Restore the window instead.
        #[arg(long)]
        off: bool,
    },
    /// Make a window fullscreen.
    Fullscreen {
        id: WindowId,
        /// Leave fullscreen instead.
        #[arg(long)]
        off: bool,
    },
    /// Move a window to another workspace.
    Move {
        id: WindowId,
        #[arg(value_parser = clap::value_parser!(u32).range(1..))]
        workspace: u32,
    },
    /// Move focus to the nearest window in a direction.
    Focus {
        #[arg(value_enum)]
        direction: DirectionArg,
    },
    /// Switch to a workspace.
    Workspace {
        #[arg(value_parser = clap::value_parser!(u32).range(1..))]
        number: u32,
    },
    /// Set the active workspace's layout.
    Layout {
        #[arg(value_enum)]
        mode: LayoutArg,
    },
    /// Run a command in the session; a single argument is a shell command line.
    Spawn {
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Toggle the application launcher.
    Launcher,
    /// Toggle the workspace overview.
    Overview,
    /// Lock the screen.
    Lock,
    /// Turn the screens off until the next input.
    Blank,
    /// Reload the configuration file.
    Reload,
    /// Save a PNG of an output; without a path, it goes to the Screenshots folder.
    Screenshot {
        /// Where to write the PNG.
        path: Option<PathBuf>,
        /// Output name, such as eDP-1; defaults to the one with the pointer.
        #[arg(long)]
        output: Option<String>,
    },
    /// End the session.
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DirectionArg {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum LayoutArg {
    Floating,
    Tiling,
}

impl Cmd {
    fn request(&self) -> Request {
        match self {
            Self::State { .. } => Request::GetState,
            Self::Status => Request::GetLockState,
            Self::Watch => Request::Subscribe,
            Self::Activate { id } => Request::Activate { id: *id },
            Self::Close { id } => Request::Close { id: *id },
            Self::Minimize { id } => Request::SetMinimized { id: *id, minimized: true },
            Self::Unminimize { id } => Request::SetMinimized { id: *id, minimized: false },
            Self::Maximize { id, off } => Request::SetMaximized { id: *id, maximized: !off },
            Self::Fullscreen { id, off } => Request::SetFullscreen { id: *id, fullscreen: !off },
            Self::Move { id, workspace } => {
                Request::MoveToWorkspace { id: *id, workspace: workspace - 1 }
            }
            Self::Focus { direction } => Request::FocusDirection {
                direction: match direction {
                    DirectionArg::Left => Direction::Left,
                    DirectionArg::Right => Direction::Right,
                    DirectionArg::Up => Direction::Up,
                    DirectionArg::Down => Direction::Down,
                },
            },
            Self::Workspace { number } => Request::SwitchWorkspace { workspace: number - 1 },
            Self::Layout { mode } => Request::SetLayout {
                layout: match mode {
                    LayoutArg::Floating => LayoutMode::Floating,
                    LayoutArg::Tiling => LayoutMode::Tiling,
                },
            },
            Self::Spawn { command } => Request::Spawn { command: command_line(command) },
            Self::Launcher => Request::ToggleLauncher,
            Self::Overview => Request::ToggleOverview,
            Self::Lock => Request::Lock,
            Self::Blank => Request::Blank,
            Self::Reload => Request::ReloadConfig,
            Self::Screenshot { path, output } => Request::Screenshot {
                output: output.clone(),
                path: path.as_ref().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())),
            },
            Self::Quit => Request::Quit,
        }
    }
}

/// Joins arguments into a `sh -c` command line; a lone argument already is one.
fn command_line(args: &[String]) -> String {
    match args {
        [single] => single.clone(),
        _ => args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "),
    }
}

fn shell_quote(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_-./=:,+@%".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

fn format_state(state: &CompositorState) -> String {
    let mut out = String::new();
    let layout = match state.layout {
        LayoutMode::Floating => "floating",
        LayoutMode::Tiling => "tiling",
    };
    let _ = writeln!(
        out,
        "Workspace {} of {} ({layout} layout)",
        state.active_workspace + 1,
        state.workspace_count
    );

    out.push_str("\nOutputs\n");
    if state.outputs.is_empty() {
        out.push_str("  none\n");
    } else {
        let rows = state.outputs.iter().map(|o| {
            vec![
                o.name.clone(),
                format!("{}x{}", o.width, o.height),
                format!("{}", o.scale),
                format!("{} Hz", nimbus_config::format_hz(o.refresh_mhz, 2)),
            ]
        });
        table(&mut out, &["NAME", "SIZE", "SCALE", "REFRESH"], rows);
    }

    out.push_str("\nWindows\n");
    if state.windows.is_empty() {
        out.push_str("  none\n");
    } else {
        let mut windows: Vec<_> = state.windows.iter().collect();
        windows.sort_by_key(|w| (w.workspace, w.id));
        let rows = windows.into_iter().map(|w| {
            let flags: Vec<&str> = [
                (w.focused, "focused"),
                (w.minimized, "minimized"),
                (w.maximized, "maximized"),
                (w.fullscreen, "fullscreen"),
            ]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
            vec![
                w.id.to_string(),
                (w.workspace + 1).to_string(),
                w.output.clone(),
                w.app_id.clone(),
                if flags.is_empty() { "-".into() } else { flags.join(",") },
                w.title.clone(),
            ]
        });
        table(&mut out, &["ID", "WS", "OUTPUT", "APP", "STATE", "TITLE"], rows);
    }
    out
}

fn format_status(locked: bool, held: bool, power: &PowerState) -> String {
    let lock = match (locked, held) {
        (false, _) => "no",
        (true, true) => "yes",
        (true, false) => "yes, with no lock screen",
    };
    let screens = match (power.blanked, power.off.as_slice()) {
        (true, _) => "blanked".to_string(),
        (false, []) => "on".to_string(),
        (false, off) => format!("off: {}", off.join(", ")),
    };
    format!("Locked: {lock}\nScreens: {screens}\n")
}

/// Writes left-aligned columns; the last column isn't padded.
fn table(out: &mut String, header: &[&str], rows: impl Iterator<Item = Vec<String>>) {
    let rows: Vec<Vec<String>> =
        std::iter::once(header.iter().map(|h| h.to_string()).collect()).chain(rows).collect();
    let mut widths = vec![0; header.len()];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in &rows {
        let mut line = String::from("  ");
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                let _ = write!(line, "{cell:<width$}  ", width = widths[i]);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
}

fn exit_code(error: &nimbus_ipc::Error) -> ExitCode {
    match error {
        nimbus_ipc::Error::Rejected(_) => ExitCode::from(EXIT_REJECTED),
        _ => ExitCode::from(EXIT_CONNECTION),
    }
}

fn fail(error: &nimbus_ipc::Error) -> ExitCode {
    eprintln!("nimbusctl: {error}");
    exit_code(error)
}

fn run(cli: Cli) -> ExitCode {
    let client = match &cli.socket {
        Some(path) => Client::connect_to(path),
        None => Client::connect(),
    };
    let mut client = match client {
        Ok(client) => client,
        Err(error) => {
            eprintln!("nimbusctl: cannot connect to the compositor: {error}");
            return ExitCode::from(EXIT_CONNECTION);
        }
    };
    let mut stdout = std::io::stdout().lock();

    if cli.command == Cmd::Watch {
        let events = match client.subscribe() {
            Ok(events) => events,
            Err(error) => return fail(&error),
        };
        for event in events {
            let line = match event
                .and_then(|e| serde_json::to_string(&e).map_err(nimbus_ipc::Error::from))
            {
                Ok(line) => line,
                Err(error) => return fail(&error),
            };
            if writeln!(stdout, "{line}").and_then(|()| stdout.flush()).is_err() {
                // The reader went away, as with `nimbusctl watch | head`.
                return ExitCode::SUCCESS;
            }
        }
        return ExitCode::SUCCESS;
    }

    if cli.command == Cmd::Status {
        let lock = client.request(&Request::GetLockState);
        let power = client.request(&Request::GetPowerState);
        return match (lock, power) {
            (Ok(Response::LockState { locked, held }), Ok(Response::PowerState(power))) => {
                let _ = stdout.write_all(format_status(locked, held, &power).as_bytes());
                ExitCode::SUCCESS
            }
            (Err(error), _) | (_, Err(error)) => fail(&error),
            (lock, power) => {
                eprintln!("nimbusctl: unexpected reply to a status request: {lock:?}, {power:?}");
                ExitCode::from(EXIT_CONNECTION)
            }
        };
    }

    let response = match client.request(&cli.command.request()) {
        Ok(response) => response,
        Err(error) => return fail(&error),
    };
    let output = match (&cli.command, response) {
        (Cmd::State { json: true }, Response::State(state)) => {
            serde_json::to_string_pretty(&state).map(|s| s + "\n").unwrap_or_default()
        }
        (Cmd::State { json: false }, Response::State(state)) => format_state(&state),
        (Cmd::State { .. }, other) => {
            eprintln!("nimbusctl: unexpected reply to a state request: {other:?}");
            return ExitCode::from(EXIT_CONNECTION);
        }
        _ => String::new(),
    };
    let _ = stdout.write_all(output.as_bytes());
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    run(Cli::parse())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_ipc::{OutputInfo, WindowInfo};

    fn request(args: &[&str]) -> Request {
        let cli =
            Cli::try_parse_from(std::iter::once("nimbusctl").chain(args.iter().copied())).unwrap();
        cli.command.request()
    }

    #[test]
    fn subcommands_map_to_requests() {
        let cases: Vec<(&[&str], Request)> = vec![
            (&["state"], Request::GetState),
            (&["state", "--json"], Request::GetState),
            (&["status"], Request::GetLockState),
            (&["watch"], Request::Subscribe),
            (&["activate", "4"], Request::Activate { id: 4 }),
            (&["close", "4"], Request::Close { id: 4 }),
            (&["minimize", "4"], Request::SetMinimized { id: 4, minimized: true }),
            (&["unminimize", "4"], Request::SetMinimized { id: 4, minimized: false }),
            (&["maximize", "4"], Request::SetMaximized { id: 4, maximized: true }),
            (&["maximize", "4", "--off"], Request::SetMaximized { id: 4, maximized: false }),
            (&["fullscreen", "4"], Request::SetFullscreen { id: 4, fullscreen: true }),
            (&["fullscreen", "--off", "4"], Request::SetFullscreen { id: 4, fullscreen: false }),
            (&["move", "4", "2"], Request::MoveToWorkspace { id: 4, workspace: 1 }),
            (&["focus", "left"], Request::FocusDirection { direction: Direction::Left }),
            (&["focus", "down"], Request::FocusDirection { direction: Direction::Down }),
            (&["workspace", "1"], Request::SwitchWorkspace { workspace: 0 }),
            (&["layout", "tiling"], Request::SetLayout { layout: LayoutMode::Tiling }),
            (&["layout", "floating"], Request::SetLayout { layout: LayoutMode::Floating }),
            (&["spawn", "foot"], Request::Spawn { command: "foot".into() }),
            (&["spawn", "foot -e 'htop'"], Request::Spawn { command: "foot -e 'htop'".into() }),
            (
                &["spawn", "foot", "-e", "it's here"],
                Request::Spawn { command: r"foot -e 'it'\''s here'".into() },
            ),
            (&["launcher"], Request::ToggleLauncher),
            (&["overview"], Request::ToggleOverview),
            (&["lock"], Request::Lock),
            (&["blank"], Request::Blank),
            (&["reload"], Request::ReloadConfig),
            (&["quit"], Request::Quit),
            (&["screenshot"], Request::Screenshot { output: None, path: None }),
            (
                &["screenshot", "/tmp/a.png", "--output", "HEADLESS-1"],
                Request::Screenshot {
                    output: Some("HEADLESS-1".into()),
                    path: Some("/tmp/a.png".into()),
                },
            ),
        ];
        for (args, expected) in cases {
            assert_eq!(request(args), expected, "{args:?}");
        }
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        for args in [
            &["workspace", "0"][..],
            &["move", "1", "0"],
            &["focus", "sideways"],
            &["layout", "stacked"],
            &["close", "-3"],
            &["spawn"],
            &["frobnicate"],
        ] {
            assert!(
                Cli::try_parse_from(std::iter::once("nimbusctl").chain(args.iter().copied()))
                    .is_err(),
                "{args:?}"
            );
        }
        let cli = Cli::try_parse_from(["nimbusctl", "--socket", "/tmp/x.sock", "lock"]).unwrap();
        assert_eq!(cli.socket, Some(PathBuf::from("/tmp/x.sock")));
    }

    #[test]
    fn exit_codes_distinguish_rejection_from_connection_failure() {
        assert_eq!(exit_code(&nimbus_ipc::Error::Rejected("no".into())), ExitCode::from(1));
        assert_eq!(exit_code(&nimbus_ipc::Error::Closed), ExitCode::from(2));
        let missing =
            Cli::try_parse_from(["nimbusctl", "--socket", "/nonexistent/nimbus.sock", "lock"])
                .unwrap();
        assert_eq!(run(missing), ExitCode::from(2));
    }

    #[test]
    fn status_names_the_lock_and_screens() {
        let on = PowerState::default();
        assert_eq!(format_status(false, false, &on), "Locked: no\nScreens: on\n");
        let blanked = PowerState { blanked: true, off: vec!["DP-1".into()] };
        assert_eq!(format_status(true, true, &blanked), "Locked: yes\nScreens: blanked\n");
        let off = PowerState { blanked: false, off: vec!["DP-1".into(), "eDP-1".into()] };
        assert_eq!(
            format_status(true, false, &off),
            "Locked: yes, with no lock screen\nScreens: off: DP-1, eDP-1\n"
        );
    }

    #[test]
    fn state_is_rendered_as_tables() {
        let state = CompositorState {
            windows: vec![
                WindowInfo {
                    id: 12,
                    app_id: "org.nimbus.Files".into(),
                    title: "Home".into(),
                    workspace: 1,
                    output: "DP-1".into(),
                    focused: true,
                    maximized: true,
                    ..Default::default()
                },
                WindowInfo {
                    id: 3,
                    app_id: "foot".into(),
                    title: "~".into(),
                    output: "DP-1".into(),
                    ..Default::default()
                },
            ],
            outputs: vec![OutputInfo {
                name: "DP-1".into(),
                width: 2560,
                height: 1440,
                scale: 1.25,
                refresh_mhz: 59951,
            }],
            workspace_count: 4,
            active_workspace: 1,
            layout: LayoutMode::Tiling,
        };
        let expected = "\
Workspace 2 of 4 (tiling layout)

Outputs
  NAME  SIZE       SCALE  REFRESH
  DP-1  2560x1440  1.25   59.95 Hz

Windows
  ID  WS  OUTPUT  APP               STATE              TITLE
  3   1   DP-1    foot              -                  ~
  12  2   DP-1    org.nimbus.Files  focused,maximized  Home
";
        assert_eq!(format_state(&state), expected);
        let empty = format_state(&CompositorState { workspace_count: 1, ..Default::default() });
        assert!(empty.contains("Outputs\n  none\n") && empty.contains("Windows\n  none\n"));
    }

    #[test]
    fn talks_to_a_socket_and_reports_rejections() {
        use nimbus_ipc::{read_message, write_message};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nimbus.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in [Response::Ok, Response::Error { message: "no such window".into() }] {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                seen.push(read_message::<Request>(&mut reader).unwrap());
                write_message(&mut writer, &reply).unwrap();
            }
            seen
        });
        let socket = path.to_str().unwrap();
        assert_eq!(
            run(Cli::try_parse_from(["nimbusctl", "--socket", socket, "lock"]).unwrap()),
            ExitCode::SUCCESS
        );
        assert_eq!(
            run(Cli::try_parse_from(["nimbusctl", "--socket", socket, "close", "9"]).unwrap()),
            ExitCode::from(1)
        );
        assert_eq!(server.join().unwrap(), vec![Request::Lock, Request::Close { id: 9 }]);
    }
}
