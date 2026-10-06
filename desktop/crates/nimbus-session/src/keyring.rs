// SPDX-License-Identifier: MIT

//! The Secret Service (`org.freedesktop.secrets`), where apps keep passwords.
//!
//! When no provider owns the name, the session runs `gnome-keyring-daemon --start --components=secrets`.
//! That takes over a daemon `pam_gnome_keyring` started at login, with the login keyring already unlocked,
//! or starts a new one.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::env::SessionEnv;

pub const SECRETS: &str = "org.freedesktop.secrets";
/// Variables `gnome-keyring-daemon --start` prints that the session exports.
pub const VARIABLES: &[&str] = &["GNOME_KEYRING_CONTROL", "SSH_AUTH_SOCK"];
const TIMEOUT: Duration = Duration::from_secs(5);

/// How the session gets a Secret Service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyringPlan {
    /// A provider owns the name already, such as KeePassXC or a keyring the display manager started.
    Running,
    /// Run this `gnome-keyring-daemon`.
    Start(PathBuf),
    /// D-Bus starts a provider on the first call.
    Activatable,
    /// No provider; apps that keep secrets fail or ask for them every time.
    Unavailable,
}

/// The bus's view of [`SECRETS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusState {
    pub owned: bool,
    pub activatable: bool,
}

/// Prefers a running provider, then gnome-keyring, which can join the daemon PAM started, then D-Bus activation.
pub fn plan(bus: BusState, daemon: Option<PathBuf>) -> KeyringPlan {
    match (bus, daemon) {
        (BusState { owned: true, .. }, _) => KeyringPlan::Running,
        (_, Some(daemon)) => KeyringPlan::Start(daemon),
        (BusState { activatable: true, .. }, None) => KeyringPlan::Activatable,
        _ => KeyringPlan::Unavailable,
    }
}

/// The [`VARIABLES`] among the `NAME=value` lines `gnome-keyring-daemon --start` prints.
pub fn parse_env(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .filter(|(name, value)| VARIABLES.contains(name) && !value.is_empty())
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// Exports `vars`; an `SSH_AUTH_SOCK` the user already set, such as another agent's, wins.
pub fn export(
    session: &mut SessionEnv,
    vars: &[(String, String)],
    lookup: impl Fn(&str) -> Option<String>,
) {
    for (name, value) in vars {
        if name == "SSH_AUTH_SOCK" && lookup(name).is_some_and(|v| !v.is_empty()) {
            continue;
        }
        session.set(name, value.clone());
    }
}

/// Asks the bus at `address` about [`SECRETS`], or returns `None` when it can't be reached.
pub fn query_bus(address: &str) -> Option<BusState> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let result = runtime.block_on(async {
        let connect = zbus::connection::Builder::address(address)?.build();
        let conn = tokio::time::timeout(TIMEOUT, connect)
            .await
            .map_err(|_| zbus::Error::Failure("timed out".into()))??;
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let name = zbus::names::BusName::try_from(SECRETS)?;
        let owned = dbus.name_has_owner(name).await?;
        let activatable =
            dbus.list_activatable_names().await?.iter().any(|n| n.as_str() == SECRETS);
        zbus::Result::Ok(BusState { owned, activatable })
    });
    result
        .inspect_err(|error| tracing::warn!("cannot ask D-Bus for a Secret Service: {error}"))
        .ok()
}

/// Runs `gnome-keyring-daemon --start --components=secrets` and returns the variables it prints.
fn start_daemon(daemon: &PathBuf, session: &SessionEnv) -> Option<Vec<(String, String)>> {
    let mut command = Command::new(daemon);
    command.args(["--start", "--components=secrets"]);
    session.apply(&mut command);
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = command
        .spawn()
        .inspect_err(|error| tracing::warn!("cannot run {}: {error}", daemon.display()))
        .ok()?;
    // The daemon it forks may keep the pipe open, so lines are read apart from waiting for the exit.
    let (lines, received) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
    }
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            result => {
                tracing::warn!("{} didn't finish starting: {result:?}", daemon.display());
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        tracing::warn!("{} failed: {status}", daemon.display());
        return None;
    }
    let mut output = String::new();
    while let Ok(line) = received.recv_timeout(Duration::from_millis(200)) {
        output.push_str(&line);
        output.push('\n');
    }
    Some(parse_env(&output))
}

/// Makes sure the session has a Secret Service when the bus at `address` can be reached,
/// and exports what gnome-keyring prints.
pub fn ensure(
    address: &str,
    daemon: Option<PathBuf>,
    session: &mut SessionEnv,
    lookup: impl Fn(&str) -> Option<String>,
) {
    let Some(bus) = query_bus(address) else { return };
    match plan(bus, daemon) {
        KeyringPlan::Running => tracing::info!("using the running Secret Service"),
        KeyringPlan::Start(daemon) => {
            if let Some(vars) = start_daemon(&daemon, session) {
                tracing::info!("started gnome-keyring for the Secret Service");
                export(session, &vars, lookup);
            }
        }
        KeyringPlan::Activatable => {
            tracing::info!("D-Bus starts the Secret Service when an app asks")
        }
        KeyringPlan::Unavailable => {
            tracing::warn!("no Secret Service; install gnome-keyring so apps can keep passwords");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_prefers_the_running_provider_then_gnome_keyring() {
        let daemon = Some(PathBuf::from("/usr/bin/gnome-keyring-daemon"));
        let bus = |owned, activatable| BusState { owned, activatable };
        assert_eq!(plan(bus(true, true), daemon.clone()), KeyringPlan::Running);
        assert_eq!(
            plan(bus(false, true), daemon.clone()),
            KeyringPlan::Start(PathBuf::from("/usr/bin/gnome-keyring-daemon"))
        );
        assert_eq!(plan(bus(false, true), None), KeyringPlan::Activatable);
        assert_eq!(plan(bus(false, false), None), KeyringPlan::Unavailable);
    }

    #[test]
    fn parses_the_variables_gnome_keyring_prints() {
        let output = "GNOME_KEYRING_CONTROL=/run/user/1000/keyring\nSSH_AUTH_SOCK=/run/user/1000/keyring/ssh\n\
                      OTHER=x\nGNOME_KEYRING_PID=\nnonsense\n";
        assert_eq!(
            parse_env(output),
            [
                ("GNOME_KEYRING_CONTROL".to_string(), "/run/user/1000/keyring".to_string()),
                ("SSH_AUTH_SOCK".to_string(), "/run/user/1000/keyring/ssh".to_string()),
            ]
        );
    }

    #[test]
    fn an_ssh_agent_the_user_set_wins() {
        let vars = parse_env("GNOME_KEYRING_CONTROL=/k\nSSH_AUTH_SOCK=/k/ssh\n");
        let mut session = SessionEnv::default();
        export(&mut session, &vars, |name| (name == "SSH_AUTH_SOCK").then(|| "/agent".to_string()));
        assert_eq!(session.get("GNOME_KEYRING_CONTROL"), Some("/k"));
        assert_eq!(session.get("SSH_AUTH_SOCK"), None);
        let mut session = SessionEnv::default();
        export(&mut session, &vars, |_| None);
        assert_eq!(session.get("SSH_AUTH_SOCK"), Some("/k/ssh"));
    }
}
