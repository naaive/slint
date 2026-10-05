// SPDX-License-Identifier: MIT

//! `nimbus-session`: the entry point that display managers and TTY logins start.

mod autostart;
mod env;
mod supervisor;

use clap::{Parser, ValueEnum};
use env::{DbusPlan, REEXEC_GUARD, SessionEnv};
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Backend {
    /// A window inside an existing Wayland or X11 session.
    Winit,
    /// DRM/KMS on a TTY.
    Udev,
    /// No output device, for tests.
    Headless,
}

impl Backend {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Winit => "winit",
            Self::Udev => "udev",
            Self::Headless => "headless",
        }
    }
}

/// Start a Nimbus desktop session.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// Compositor backend; defaults to winit inside another session and udev on a TTY.
    #[arg(long, value_enum)]
    backend: Option<Backend>,
    /// Wayland socket name for the compositor.
    #[arg(long, value_name = "NAME")]
    socket: Option<String>,
    /// Configuration file instead of $XDG_CONFIG_HOME/nimbus/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Run the compositor without the desktop shell.
    #[arg(long)]
    no_shell: bool,
    /// Skip autostart commands and XDG autostart entries.
    #[arg(long)]
    no_autostart: bool,
    /// Compositor executable instead of the one next to nimbus-session or on PATH.
    #[arg(long, value_name = "PATH")]
    compositor: Option<PathBuf>,
    /// Seconds to wait for the compositor to report readiness.
    #[arg(long, value_name = "SECONDS", default_value_t = 30)]
    ready_timeout: u64,
}

fn default_backend(lookup: impl Fn(&str) -> Option<String>) -> Backend {
    let set = |name: &str| lookup(name).is_some_and(|v| !v.is_empty());
    if set("WAYLAND_DISPLAY") || set("DISPLAY") { Backend::Winit } else { Backend::Udev }
}

fn compositor_args(cli: &Cli, backend: Backend) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--backend".into(), backend.as_arg().into()];
    if let Some(socket) = &cli.socket {
        args.extend(["--socket".into(), socket.into()]);
    }
    if let Some(config) = &cli.config {
        args.extend(["--config".into(), config.into()]);
    }
    if cli.no_shell {
        args.push("--no-shell".into());
    }
    args
}

fn lookup_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Replaces this process with `dbus-run-session -- <self> <args>`; returns only on failure.
fn reexec_under_dbus() -> std::io::Error {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return error,
    };
    Command::new("dbus-run-session")
        .arg("--")
        .arg(exe)
        .args(std::env::args_os().skip(1))
        .env(REEXEC_GUARD, "1")
        .exec()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut session_env = SessionEnv::new(lookup_env);
    let has_dbus_run_session =
        env::find_in_path("dbus-run-session", std::env::var_os("PATH")).is_some();
    let socket_exists = |path: &std::path::Path| {
        use std::os::unix::fs::FileTypeExt;
        std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
    };
    match env::dbus_plan(lookup_env, socket_exists, has_dbus_run_session) {
        DbusPlan::Inherited => {}
        DbusPlan::UserBus(address) => session_env.set("DBUS_SESSION_BUS_ADDRESS", address),
        DbusPlan::Reexec => {
            tracing::info!("no D-Bus session bus found; restarting under dbus-run-session");
            let error = reexec_under_dbus();
            tracing::warn!(
                "cannot run dbus-run-session ({error}); continuing without a session bus"
            );
        }
        DbusPlan::Unavailable => {
            tracing::warn!("no D-Bus session bus; desktop services will be unavailable")
        }
    }
    if lookup_env("XDG_RUNTIME_DIR").is_none() {
        tracing::warn!("XDG_RUNTIME_DIR isn't set; the compositor may fail to create its sockets");
    }

    let config_path = cli.config.clone().or_else(|| nimbus_config::default_path().ok());
    let config = match config_path.as_deref().map(nimbus_config::Config::load_from) {
        Some(Ok(config)) => config,
        Some(Err(error)) => {
            tracing::warn!("{error}; using the default configuration");
            nimbus_config::Config::default()
        }
        None => nimbus_config::Config::default(),
    };

    let backend = cli.backend.unwrap_or_else(|| default_backend(lookup_env));
    let program = cli
        .compositor
        .clone()
        .unwrap_or_else(|| env::compositor_program(std::env::current_exe().ok().as_deref()));
    let run_autostart = !cli.no_autostart;
    let autostart_commands = config.autostart;
    let desktops: Vec<String> = session_env
        .get("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(str::to_string)
        .collect();

    let plan = supervisor::SessionPlan {
        compositor: supervisor::CompositorCommand { program, args: compositor_args(&cli, backend) },
        env: session_env,
        autostart: Box::new(move || {
            if !run_autostart {
                return Vec::new();
            }
            let desktops: Vec<&str> = desktops.iter().map(String::as_str).collect();
            let path = std::env::var_os("PATH");
            autostart::plan(
                &autostart_commands,
                &autostart::autostart_dirs(lookup_env),
                &desktops,
                |program| env::find_in_path(program, path.clone()).is_some(),
            )
        }),
        ready_timeout: Duration::from_secs(cli.ready_timeout),
    };

    match supervisor::run(plan) {
        Ok(code) => code,
        Err(error) => {
            tracing::error!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_defaults_to_nested_inside_another_session() {
        let only = |name: &'static str| move |n: &str| (n == name).then(|| "x".to_string());
        assert_eq!(default_backend(only("WAYLAND_DISPLAY")), Backend::Winit);
        assert_eq!(default_backend(only("DISPLAY")), Backend::Winit);
        assert_eq!(default_backend(|_| None), Backend::Udev);
        assert_eq!(default_backend(|_| Some(String::new())), Backend::Udev);
    }

    #[test]
    fn options_pass_through_to_the_compositor() {
        let cli = Cli::try_parse_from([
            "nimbus-session",
            "--backend",
            "headless",
            "--socket",
            "wayland-9",
            "--config",
            "/tmp/c.toml",
            "--no-shell",
        ])
        .unwrap();
        let backend = cli.backend.unwrap();
        let args: Vec<_> =
            compositor_args(&cli, backend).into_iter().map(|a| a.into_string().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--backend",
                "headless",
                "--socket",
                "wayland-9",
                "--config",
                "/tmp/c.toml",
                "--no-shell"
            ]
        );

        let cli = Cli::try_parse_from(["nimbus-session"]).unwrap();
        assert_eq!(
            compositor_args(&cli, Backend::Udev),
            [OsString::from("--backend"), "udev".into()]
        );
        assert!(Cli::try_parse_from(["nimbus-session", "--backend", "x11"]).is_err());
    }

    #[test]
    fn shipped_example_config_matches_defaults() {
        let text = include_str!("../../../data/config.toml");
        let config: nimbus_config::Config = toml::from_str(text).unwrap();
        assert_eq!(config, nimbus_config::Config::default());
        // Unknown keys are ignored when loading, so compare the raw tables to catch misspelled keys.
        let shipped: toml::Table = toml::from_str(text).unwrap();
        assert_eq!(shipped, toml::Table::try_from(nimbus_config::Config::default()).unwrap());
    }
}
