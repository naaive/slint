// SPDX-License-Identifier: MIT

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::entry::{DesktopEntry, find_program};
use crate::exec::{ExecError, ExecLine, ExpandContext};

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("the desktop entry '{0}' has an empty Exec line")]
    EmptyExec(String),
    #[error("cannot start '{command}': {source}")]
    Spawn { command: String, source: std::io::Error },
}

/// Terminal emulators tried in order when `$TERMINAL` isn't set,
/// with the argument that precedes the command, if any.
const TERMINALS: [(&str, Option<&str>); 7] = [
    ("nimbus-terminal", Some("-e")),
    ("foot", None),
    ("alacritty", Some("-e")),
    ("kitty", None),
    ("gnome-terminal", Some("--")),
    ("konsole", Some("-e")),
    ("xterm", Some("-e")),
];

fn search_path() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default()
}

/// The terminal command line to put before a program, from `terminal_env` (the `$TERMINAL` value)
/// or the first installed entry of [`TERMINALS`].
fn terminal_prefix(terminal_env: Option<&str>, search_path: &[PathBuf]) -> Option<Vec<String>> {
    if let Some(value) = terminal_env.map(str::trim).filter(|t| !t.is_empty()) {
        let ctx = ExpandContext { name: "", icon: None, desktop_file: None };
        match ExecLine::parse(value) {
            Ok(line) => {
                let mut argv = line.expand_all(&ctx, &[]).into_iter().next().unwrap_or_default();
                if argv.len() == 1 {
                    let name = Path::new(&argv[0])
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    let separator =
                        TERMINALS.iter().find(|(t, _)| *t == name).map_or(Some("-e"), |(_, s)| *s);
                    argv.extend(separator.map(str::to_owned));
                }
                if !argv.is_empty() {
                    return Some(argv);
                }
            }
            Err(err) => tracing::warn!(%err, "ignoring invalid $TERMINAL"),
        }
    }
    TERMINALS.iter().find(|(name, _)| find_program(name, search_path).is_some()).map(
        |(name, separator)| {
            std::iter::once((*name).to_owned()).chain(separator.map(str::to_owned)).collect()
        },
    )
}

/// Builds the commands to run for `entry`, without spawning them.
fn commands(
    entry: &DesktopEntry,
    files: &[PathBuf],
    activation_token: Option<&str>,
    terminal_env: Option<&str>,
    search_path: &[PathBuf],
) -> Result<Vec<Command>, LaunchError> {
    let lines = match entry.command_lines(files) {
        Ok(lines) => lines,
        // GLib's `gapplication` activates over org.freedesktop.Application, which these entries implement.
        Err(ExecError::Empty)
            if entry.dbus_activatable && find_program("gapplication", search_path).is_some() =>
        {
            let files = files.iter().map(|f| f.to_string_lossy().into_owned());
            vec![
                ["gapplication", "launch", entry.id.as_str()]
                    .map(str::to_owned)
                    .into_iter()
                    .chain(files)
                    .collect(),
            ]
        }
        Err(ExecError::Empty) => return Err(LaunchError::EmptyExec(entry.id.clone())),
        Err(err) => {
            return Err(LaunchError::Spawn {
                command: entry.exec.clone(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidInput, err),
            });
        }
    };
    let prefix = if entry.terminal {
        Some(terminal_prefix(terminal_env, search_path).ok_or_else(|| LaunchError::Spawn {
            command: entry.exec.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal emulator found"),
        })?)
    } else {
        None
    };
    let working_dir = entry.working_dir.as_deref().filter(|dir| {
        let exists = dir.is_dir();
        if !exists {
            tracing::warn!(id = %entry.id, dir = %dir.display(), "ignoring missing working directory");
        }
        exists
    });

    let mut commands = Vec::new();
    for line in lines {
        let argv: Vec<String> = prefix.iter().flatten().cloned().chain(line).collect();
        let Some((program, args)) = argv.split_first() else {
            return Err(LaunchError::EmptyExec(entry.id.clone()));
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        if let Some(dir) = working_dir {
            command.current_dir(dir);
        }
        match activation_token {
            Some(token) => {
                command.env("XDG_ACTIVATION_TOKEN", token).env("DESKTOP_STARTUP_ID", token);
            }
            None => {
                command.env_remove("XDG_ACTIVATION_TOKEN").env_remove("DESKTOP_STARTUP_ID");
            }
        }
        commands.push(command);
    }
    Ok(commands)
}

/// Waits for `child` on a background thread so it doesn't stay a zombie.
fn reap(mut child: Child) {
    let pid = child.id();
    let spawned = std::thread::Builder::new()
        .name(format!("reap-{pid}"))
        .stack_size(64 * 1024)
        .spawn(move || {
            if let Err(err) = child.wait() {
                tracing::debug!(pid, %err, "waiting for launched process failed");
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(pid, %err, "cannot reap launched process");
    }
}

/// Starts `entry` detached from the caller, in a terminal when `Terminal=true`,
/// setting `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID` when `activation_token` is given.
///
/// When the application takes one file per instance (`%f`, `%u`), one instance per file is started.
pub fn launch(
    entry: &DesktopEntry,
    files: &[PathBuf],
    activation_token: Option<&str>,
) -> Result<(), LaunchError> {
    let terminal_env = std::env::var("TERMINAL").ok();
    let mut first_error = None;
    for mut command in
        commands(entry, files, activation_token, terminal_env.as_deref(), &search_path())?
    {
        match command.spawn() {
            Ok(child) => reap(child),
            Err(source) => {
                let command = command.get_program().to_string_lossy().into_owned();
                tracing::warn!(id = %entry.id, %command, %source, "launch failed");
                first_error.get_or_insert(LaunchError::Spawn { command, source });
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn env(command: &Command, key: &str) -> Option<Option<String>> {
        command
            .get_envs()
            .find(|(k, _)| *k == OsStr::new(key))
            .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    fn executable(dir: &Path, name: &str) {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn builds_detached_commands() {
        let entry = DesktopEntry {
            id: "viewer".into(),
            exec: "viewer --open %f".into(),
            working_dir: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let cmds =
            commands(&entry, &[PathBuf::from("/a"), PathBuf::from("/b")], Some("tok"), None, &[])
                .expect("commands");
        assert_eq!(cmds.len(), 2);
        assert_eq!(argv(&cmds[0]), vec!["viewer", "--open", "/a"]);
        assert_eq!(argv(&cmds[1]), vec!["viewer", "--open", "/b"]);
        assert_eq!(cmds[0].get_current_dir(), Some(std::env::temp_dir().as_path()));
        assert_eq!(env(&cmds[0], "XDG_ACTIVATION_TOKEN"), Some(Some("tok".into())));
        assert_eq!(env(&cmds[0], "DESKTOP_STARTUP_ID"), Some(Some("tok".into())));

        let cmds = commands(&entry, &[], None, None, &[]).expect("commands");
        assert_eq!(env(&cmds[0], "XDG_ACTIVATION_TOKEN"), Some(None));
        let missing_dir = DesktopEntry { working_dir: Some("/nonexistent/nimbus".into()), ..entry };
        let cmds = commands(&missing_dir, &[], None, None, &[]).expect("commands");
        assert_eq!(cmds[0].get_current_dir(), None);
    }

    #[test]
    fn dbus_activatable_without_exec() {
        let bin = tempfile::tempdir().expect("tempdir");
        let entry = DesktopEntry {
            id: "org.example.App".into(),
            dbus_activatable: true,
            ..Default::default()
        };
        assert!(matches!(commands(&entry, &[], None, None, &[]), Err(LaunchError::EmptyExec(_))));
        executable(bin.path(), "gapplication");
        let cmds =
            commands(&entry, &[PathBuf::from("/x")], None, None, &[bin.path().to_path_buf()])
                .expect("commands");
        assert_eq!(argv(&cmds[0]), vec!["gapplication", "launch", "org.example.App", "/x"]);
    }

    #[test]
    fn terminal_selection() {
        let bin = tempfile::tempdir().expect("tempdir");
        let path = vec![bin.path().to_path_buf()];
        assert_eq!(terminal_prefix(None, &path), None);
        executable(bin.path(), "xterm");
        assert_eq!(terminal_prefix(None, &path), Some(vec!["xterm".to_owned(), "-e".to_owned()]));
        executable(bin.path(), "foot");
        assert_eq!(terminal_prefix(None, &path), Some(vec!["foot".to_owned()]));
        executable(bin.path(), "nimbus-terminal");
        assert_eq!(
            terminal_prefix(None, &path),
            Some(vec!["nimbus-terminal".to_owned(), "-e".to_owned()])
        );
        assert_eq!(
            terminal_prefix(Some("wezterm"), &path),
            Some(vec!["wezterm".to_owned(), "-e".to_owned()])
        );
        assert_eq!(terminal_prefix(Some("/opt/kitty"), &path), Some(vec!["/opt/kitty".to_owned()]));
        assert_eq!(
            terminal_prefix(Some("wezterm start --"), &path),
            Some(vec!["wezterm".to_owned(), "start".to_owned(), "--".to_owned()])
        );
        assert_eq!(
            terminal_prefix(Some("  "), &path),
            Some(vec!["nimbus-terminal".to_owned(), "-e".to_owned()])
        );
    }

    #[test]
    fn terminal_entries_are_wrapped() {
        let bin = tempfile::tempdir().expect("tempdir");
        executable(bin.path(), "alacritty");
        let entry = DesktopEntry {
            id: "htop".into(),
            exec: "htop".into(),
            terminal: true,
            ..Default::default()
        };
        let cmds =
            commands(&entry, &[], None, None, &[bin.path().to_path_buf()]).expect("commands");
        assert_eq!(argv(&cmds[0]), vec!["alacritty", "-e", "htop"]);
        let err = commands(&entry, &[], None, None, &[]).err();
        assert!(matches!(err, Some(LaunchError::Spawn { .. })), "{err:?}");
    }

    #[test]
    fn invalid_exec_lines() {
        let empty = DesktopEntry { id: "empty".into(), ..Default::default() };
        assert!(
            matches!(launch(&empty, &[], None), Err(LaunchError::EmptyExec(id)) if id == "empty")
        );
        let quote =
            DesktopEntry { id: "q".into(), exec: "app \"open".into(), ..Default::default() };
        assert!(matches!(launch(&quote, &[], None), Err(LaunchError::Spawn { .. })));
        let field_only = DesktopEntry { id: "f".into(), exec: "%f".into(), ..Default::default() };
        assert!(matches!(launch(&field_only, &[], None), Err(LaunchError::EmptyExec(_))));
    }

    #[test]
    fn spawns_and_reaps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("marker");
        let entry = DesktopEntry {
            id: "touch".into(),
            exec: format!("/bin/sh -c \"pwd > {}\"", marker.display()),
            working_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        launch(&entry, &[], Some("token")).expect("launch");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() || fs::read_to_string(&marker).map(|s| s.is_empty()).unwrap_or(true)
        {
            assert!(std::time::Instant::now() < deadline, "the launched process didn't run");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pwd = fs::read_to_string(&marker).expect("read");
        assert_eq!(Path::new(pwd.trim()).canonicalize().ok(), dir.path().canonicalize().ok());

        let missing = DesktopEntry {
            id: "m".into(),
            exec: "/nonexistent/nimbus-program".into(),
            ..Default::default()
        };
        assert!(matches!(launch(&missing, &[], None), Err(LaunchError::Spawn { .. })));
    }
}
