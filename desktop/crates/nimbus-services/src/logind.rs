// SPDX-License-Identifier: MIT

//! Session and power control through systemd-logind on the system bus.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};
use zbus::{Connection, Message};

use crate::ServiceEvent;
use crate::bus::{self, BusService};
use crate::hub::Updates;

const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
const SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";
const AUTO_SESSION: &str = "/org/freedesktop/login1/session/auto";
/// How long suspend waits after a lock request, so the lock screen is on screen before the system sleeps.
const LOCK_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(crate) enum LoginCommand {
    Lock,
    Suspend,
    Reboot,
    PowerOff,
    Logout,
}

/// What a logind signal asks of the session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Lock,
    Unlock,
    PrepareForSleep(bool),
    Other,
}

pub(crate) fn classify(message: &Message) -> Signal {
    let header = message.header();
    match header.member().map(|member| member.as_str()) {
        Some("Lock") => Signal::Lock,
        Some("Unlock") => Signal::Unlock,
        Some("PrepareForSleep") => match message.body().deserialize::<bool>() {
            Ok(sleeping) => Signal::PrepareForSleep(sleeping),
            Err(_) => Signal::Other,
        },
        _ => Signal::Other,
    }
}

pub(crate) struct Logind {
    updates: Updates,
    sleep_inhibitor: Option<OwnedFd>,
}

impl Logind {
    pub(crate) fn new(updates: Updates) -> Self {
        Self { updates, sleep_inhibitor: None }
    }

    /// Resolves the object path of this process's session, whose signals carry its real path rather than `auto`.
    async fn session_path(conn: &Connection, owner: &str) -> Option<OwnedObjectPath> {
        let id = match bus::get_property(conn, owner, AUTO_SESSION, SESSION_INTERFACE, "Id").await {
            Ok(value) => String::try_from(value).ok(),
            Err(_) => std::env::var("XDG_SESSION_ID").ok(),
        }?;
        bus::call_for(conn, owner, MANAGER_PATH, MANAGER_INTERFACE, "GetSession", &(id.as_str(),))
            .await
            .ok()
    }

    async fn inhibit_sleep(&mut self, conn: &Connection, owner: &str) {
        let body = ("sleep", "Nimbus", "Lock the screen before suspend", "delay");
        match bus::call_for::<_, OwnedFd>(
            conn,
            owner,
            MANAGER_PATH,
            MANAGER_INTERFACE,
            "Inhibit",
            &body,
        )
        .await
        {
            Ok(fd) => self.sleep_inhibitor = Some(fd),
            Err(err) => tracing::debug!("Can't delay suspend for locking: {err}"),
        }
    }

    async fn command(
        &mut self,
        conn: &Connection,
        owner: &str,
        session: Option<&str>,
        command: LoginCommand,
    ) {
        let manager =
            |method| bus::call(conn, owner, MANAGER_PATH, MANAGER_INTERFACE, method, &(false,));
        let result = match (&command, session) {
            (LoginCommand::Lock, Some(session)) => {
                bus::call(conn, owner, session, SESSION_INTERFACE, "Lock", &()).await
            }
            // logind would lock whichever session `auto` resolves to, and its signal wouldn't reach us.
            (LoginCommand::Lock, None) => {
                self.updates.event(ServiceEvent::LockRequested);
                return;
            }
            (LoginCommand::Logout, Some(session)) => {
                bus::call(conn, owner, session, SESSION_INTERFACE, "Terminate", &()).await
            }
            (LoginCommand::Logout, None) => {
                tracing::info!("Not running in a logind session; asking the compositor to exit");
                self.updates.event(ServiceEvent::LogoutRequested);
                return;
            }
            (LoginCommand::Suspend, _) => manager("Suspend").await,
            (LoginCommand::Reboot, _) => manager("Reboot").await,
            (LoginCommand::PowerOff, _) => manager("PowerOff").await,
        };
        if let Err(err) = result {
            tracing::info!("logind refused {command:?}: {err}");
            if matches!(command, LoginCommand::Logout) {
                self.updates.event(ServiceEvent::LogoutRequested);
            }
            if matches!(command, LoginCommand::Lock) {
                self.updates.event(ServiceEvent::LockRequested);
            }
        }
    }
}

impl BusService for Logind {
    type Command = LoginCommand;
    const NAME: &'static str = "org.freedesktop.login1";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<LoginCommand>,
    ) -> zbus::Result<()> {
        let sender = || owner.to_owned().into_inner();
        let mut rules = vec![
            bus::signal_rule()
                .sender(sender())?
                .path(MANAGER_PATH)?
                .interface(MANAGER_INTERFACE)?
                .member("PrepareForSleep")?
                .build(),
        ];
        let session = Self::session_path(conn, owner.as_str()).await;
        match &session {
            Some(path) => rules.push(
                bus::signal_rule()
                    .sender(sender())?
                    .path(path.to_owned().into_inner())?
                    .interface(SESSION_INTERFACE)?
                    .build(),
            ),
            None => tracing::info!(
                "Not running in a logind session; lock requests from logind won't arrive"
            ),
        }
        let session = session.map(|path| path.as_str().to_owned());
        let mut signals = bus::signals(conn, rules).await?;
        self.inhibit_sleep(conn, owner.as_str()).await;
        loop {
            tokio::select! {
                message = signals.next() => {
                    let Some(message) = message else { return Err(bus::stream_ended()) };
                    match classify(&message) {
                        Signal::Lock => self.updates.event(ServiceEvent::LockRequested),
                        Signal::Unlock => self.updates.event(ServiceEvent::UnlockRequested),
                        Signal::PrepareForSleep(true) => {
                            self.updates.event(ServiceEvent::LockRequested);
                            let inhibitor = self.sleep_inhibitor.take();
                            tokio::spawn(async move {
                                tokio::time::sleep(LOCK_GRACE).await;
                                drop(inhibitor);
                            });
                        }
                        Signal::PrepareForSleep(false) => {
                            if self.sleep_inhibitor.is_none() {
                                self.inhibit_sleep(conn, owner.as_str()).await;
                            }
                        }
                        Signal::Other => {}
                    }
                }
                command = commands.recv() => match command {
                    Some(command) => self.command(conn, owner.as_str(), session.as_deref(), command).await,
                    None => return Ok(()),
                },
            }
        }
    }

    fn unavailable(&mut self) {
        self.sleep_inhibitor = None;
    }

    fn command_unavailable(&mut self, command: LoginCommand) {
        match command {
            LoginCommand::Lock => self.updates.event(ServiceEvent::LockRequested),
            LoginCommand::Logout => self.updates.event(ServiceEvent::LogoutRequested),
            command => tracing::info!("logind isn't available; can't {command:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(member: &str, body: Option<bool>) -> Message {
        let builder = Message::signal(MANAGER_PATH, MANAGER_INTERFACE, member).unwrap();
        match body {
            Some(body) => builder.build(&(body,)).unwrap(),
            None => builder.build(&()).unwrap(),
        }
    }

    #[test]
    fn classifies_signals() {
        assert_eq!(classify(&signal("Lock", None)), Signal::Lock);
        assert_eq!(classify(&signal("Unlock", None)), Signal::Unlock);
        assert_eq!(classify(&signal("PrepareForSleep", Some(true))), Signal::PrepareForSleep(true));
        assert_eq!(
            classify(&signal("PrepareForSleep", Some(false))),
            Signal::PrepareForSleep(false)
        );
        assert_eq!(classify(&signal("PrepareForSleep", None)), Signal::Other);
        assert_eq!(classify(&signal("SessionNew", None)), Signal::Other);
    }
}
