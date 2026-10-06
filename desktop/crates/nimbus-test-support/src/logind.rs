// SPDX-License-Identifier: MIT

//! A fake systemd-logind: one session, `c1`, inhibitors whose release tests can see, and `PrepareForSleep` on demand.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};
use zbus::{Connection, interface};

use crate::PrivateBus;

const MANAGER: &str = "/org/freedesktop/login1";
const SESSION: &str = "/org/freedesktop/login1/session/c1";

/// An inhibitor that logind handed out, as [`FakeLogind::inhibitors`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inhibitor {
    /// What it inhibits, such as `sleep`.
    pub what: String,
    /// `block` or `delay`.
    pub mode: String,
    /// Whether its holder closed the file descriptor.
    pub released: bool,
}

struct Held {
    what: String,
    mode: String,
    /// Reads end of file once the holder closes its end.
    ours: UnixStream,
}

type Inhibitors = Arc<Mutex<Vec<Held>>>;

struct Manager {
    inhibitors: Inhibitors,
}

#[interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    fn get_session(&self, id: String) -> zbus::fdo::Result<OwnedObjectPath> {
        OwnedObjectPath::try_from(format!("{MANAGER}/session/{id}"))
            .map_err(|err| zbus::fdo::Error::InvalidArgs(err.to_string()))
    }

    fn inhibit(
        &self,
        what: String,
        _who: &str,
        _why: &str,
        mode: String,
    ) -> zbus::fdo::Result<OwnedFd> {
        let (ours, theirs) =
            UnixStream::pair().map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        ours.set_nonblocking(true).map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        self.inhibitors.lock().unwrap().push(Held { what, mode, ours });
        Ok(std::os::fd::OwnedFd::from(theirs).into())
    }

    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}

struct Session;

#[interface(name = "org.freedesktop.login1.Session")]
impl Session {
    #[zbus(property)]
    fn id(&self) -> String {
        "c1".into()
    }

    #[zbus(name = "Lock")]
    async fn lock_method(&self, #[zbus(connection)] conn: &Connection) -> zbus::fdo::Result<()> {
        Self::lock_signal(&SignalEmitter::new(conn, SESSION)?).await?;
        Ok(())
    }

    #[zbus(signal, name = "Lock")]
    async fn lock_signal(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn unlock(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The fake logind on a private bus, which leaves the bus when dropped.
pub struct FakeLogind {
    connection: Connection,
    inhibitors: Inhibitors,
}

impl FakeLogind {
    /// Serves the manager and session `c1`, which is also `session/auto`, as `org.freedesktop.login1`.
    pub async fn start(bus: &PrivateBus) -> Self {
        let connection = bus.connect().await;
        let inhibitors = Inhibitors::default();
        let server = connection.object_server();
        server.at(MANAGER, Manager { inhibitors: inhibitors.clone() }).await.unwrap();
        server.at(SESSION, Session).await.unwrap();
        server.at(format!("{MANAGER}/session/auto"), Session).await.unwrap();
        connection.request_name("org.freedesktop.login1").await.unwrap();
        Self { connection, inhibitors }
    }

    /// Every inhibitor handed out so far, oldest first.
    pub fn inhibitors(&self) -> Vec<Inhibitor> {
        let inhibitors = self.inhibitors.lock().unwrap();
        inhibitors
            .iter()
            .map(|held| Inhibitor {
                what: held.what.clone(),
                mode: held.mode.clone(),
                released: matches!((&held.ours).read(&mut [0]), Ok(0)),
            })
            .collect()
    }

    /// Emits `PrepareForSleep`, `true` before suspending and `false` after resuming.
    pub async fn prepare_for_sleep(&self, start: bool) {
        let emitter = SignalEmitter::new(&self.connection, MANAGER).unwrap();
        Manager::prepare_for_sleep(&emitter, start).await.unwrap();
    }

    /// Emits the session's `Unlock`, as `loginctl unlock-session` makes logind do.
    pub async fn unlock_session(&self) {
        Session::unlock(&SignalEmitter::new(&self.connection, SESSION).unwrap()).await.unwrap();
    }
}
