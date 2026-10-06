// SPDX-License-Identifier: MIT

//! The session's polkit authentication agent.
//!
//! It registers with `org.freedesktop.PolicyKit1.Authority` for this logind session and exports
//! `org.freedesktop.PolicyKit1.AuthenticationAgent`.
//! Each `BeginAuthentication` call becomes an authentication request for the shell's dialog;
//! the setuid `polkit-agent-helper-1` checks the responses through PAM and tells polkitd the result.

mod helper;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nix::unistd::{Gid, Group, Uid, User};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use zbus::message::Header;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, interface};

use crate::ServiceEvent;
use crate::bus::{self, BusService};
use crate::hub::Updates;
use session::{Input, Outcome};

const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";
const AGENT_PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";

/// A response typed into the authentication dialog, such as a password, which `Debug` leaves out.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(secret: String) -> Self {
        Self(secret)
    }
}

impl From<&str> for Secret {
    fn from(secret: &str) -> Self {
        Self(secret.to_owned())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// An application asks for an action that needs authentication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationRequest {
    pub id: u32,
    /// The polkit action, such as `org.freedesktop.systemd1.manage-units`.
    pub action_id: String,
    /// What the action does, in the user's language.
    pub message: String,
    /// A themed icon name, or empty.
    pub icon_name: String,
    /// The users who may authenticate, as login names; never empty.
    pub identities: Vec<String>,
    /// The index in `identities` asked for first: the current user if listed, otherwise the first.
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthenticationEvent {
    /// A new request; show it until [`AuthenticationEvent::Ended`].
    Started(AuthenticationRequest),
    /// Ask the user, as `identities[identity]`, and answer with [`AuthenticationCommand::Respond`].
    /// `echo` is set for responses shown as typed, such as a user name, and clear for passwords.
    Prompt { id: u32, identity: usize, prompt: String, echo: bool },
    /// A message from PAM, such as "Touch the security key" or, with `error`, "Account expired".
    Message { id: u32, text: String, error: bool },
    /// The response was wrong; another prompt follows.
    Failed { id: u32 },
    /// The request is over: authenticated, cancelled, or failed.
    Ended { id: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthenticationCommand {
    Respond {
        id: u32,
        response: Secret,
    },
    /// Authenticate as `identities[identity]` instead; a new prompt follows.
    SelectIdentity {
        id: u32,
        identity: usize,
    },
    Cancel {
        id: u32,
    },
}

/// The errors polkitd expects from an agent.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
    NotAuthorized(String),
}

/// The requests in progress, which commands and `CancelAuthentication` reach by id or cookie.
#[derive(Clone, Default)]
struct Sessions(Arc<Mutex<Registry>>);

#[derive(Default)]
struct Registry {
    last_id: u32,
    active: HashMap<u32, Active>,
}

struct Active {
    cookie: String,
    inputs: UnboundedSender<Input>,
}

impl Sessions {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, cookie: String) -> (u32, UnboundedReceiver<Input>) {
        let (inputs, receiver) = mpsc::unbounded_channel();
        let mut registry = self.registry();
        let id = loop {
            registry.last_id = registry.last_id.wrapping_add(1);
            if registry.last_id != 0 && !registry.active.contains_key(&registry.last_id) {
                break registry.last_id;
            }
        };
        registry.active.insert(id, Active { cookie, inputs });
        (id, receiver)
    }

    fn command(&self, command: AuthenticationCommand) {
        let (id, input) = match command {
            AuthenticationCommand::Respond { id, response } => (id, Input::Respond(response)),
            AuthenticationCommand::SelectIdentity { id, identity } => {
                (id, Input::SelectIdentity(identity))
            }
            AuthenticationCommand::Cancel { id } => (id, Input::Cancel),
        };
        match self.registry().active.get(&id) {
            Some(active) => {
                let _ = active.inputs.send(input);
            }
            None => tracing::debug!("Authentication request {id} is already over"),
        }
    }

    fn cancel_cookie(&self, cookie: &str) -> bool {
        let registry = self.registry();
        let active = registry.active.values().find(|active| active.cookie == cookie);
        active.is_some_and(|active| active.inputs.send(Input::Cancel).is_ok())
    }

    fn cancel_all(&self) {
        for active in self.registry().active.values() {
            let _ = active.inputs.send(Input::Cancel);
        }
    }
}

/// Ends request `id` when dropped, even if the call that started it is dropped first.
struct Ending {
    id: u32,
    sessions: Sessions,
    updates: Updates,
}

impl Drop for Ending {
    fn drop(&mut self) {
        self.sessions.registry().active.remove(&self.id);
        self.updates
            .event(ServiceEvent::Authentication(AuthenticationEvent::Ended { id: self.id }));
    }
}

type Identity = (String, HashMap<String, OwnedValue>);

/// The login names of polkit identities: a `unix-user`, or each member of a `unix-group`.
fn users(identities: &[Identity]) -> Vec<String> {
    let id = |details: &HashMap<String, OwnedValue>, key: &str| {
        details.get(key).and_then(|value| u32::try_from(&**value).ok())
    };
    let mut users = Vec::new();
    for (kind, details) in identities {
        let names = match kind.as_str() {
            "unix-user" => id(details, "uid")
                .and_then(|uid| User::from_uid(Uid::from_raw(uid)).ok().flatten())
                .map(|user| vec![user.name]),
            "unix-group" => id(details, "gid")
                .and_then(|gid| Group::from_gid(Gid::from_raw(gid)).ok().flatten())
                .map(|group| group.mem),
            _ => None,
        };
        for name in names.unwrap_or_default() {
            if !users.contains(&name) {
                users.push(name);
            }
        }
    }
    users
}

/// The index of the current user in `users`, or 0.
fn preferred(users: &[String]) -> usize {
    let current = User::from_uid(Uid::current()).ok().flatten().map(|user| user.name);
    current.and_then(|name| users.iter().position(|user| *user == name)).unwrap_or(0)
}

/// The `org.freedesktop.PolicyKit1.AuthenticationAgent` object.
struct Agent {
    sessions: Sessions,
    updates: Updates,
    helper: PathBuf,
    /// polkitd's unique name, the only caller the agent answers.
    authority: OwnedUniqueName,
}

impl Agent {
    fn check_caller(&self, header: &Header<'_>) -> Result<(), PolkitError> {
        if header.sender().is_some_and(|sender| *sender == self.authority) {
            Ok(())
        } else {
            Err(PolkitError::NotAuthorized("only polkitd may call the agent".into()))
        }
    }
}

#[interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl Agent {
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<Identity>,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), PolkitError> {
        self.check_caller(&header)?;
        let users = users(&identities);
        if users.is_empty() {
            return Err(PolkitError::Failed("none of the identities is a known user".into()));
        }
        let selected = preferred(&users);
        let (id, mut inputs) = self.sessions.add(cookie.clone());
        let _ending = Ending { id, sessions: self.sessions.clone(), updates: self.updates.clone() };
        let request = AuthenticationRequest {
            id,
            action_id,
            message,
            icon_name,
            identities: users.clone(),
            selected,
        };
        self.updates.event(ServiceEvent::Authentication(AuthenticationEvent::Started(request)));
        let updates = self.updates.clone();
        let emit = move |event| updates.event(ServiceEvent::Authentication(event));
        let outcome =
            session::authenticate(id, &self.helper, &cookie, &users, selected, &mut inputs, &emit)
                .await;
        match outcome {
            Outcome::Authenticated => Ok(()),
            Outcome::Cancelled => Err(PolkitError::Cancelled("the user cancelled".into())),
            Outcome::Failed(reason) => {
                tracing::info!("polkit authentication failed: {reason}");
                Err(PolkitError::Failed(reason))
            }
        }
    }

    async fn cancel_authentication(
        &self,
        cookie: String,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), PolkitError> {
        self.check_caller(&header)?;
        if self.sessions.cancel_cookie(&cookie) {
            Ok(())
        } else {
            Err(PolkitError::Failed("no authentication with this cookie".into()))
        }
    }
}

/// The locale polkitd translates messages into.
fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "C".into())
}

struct PolkitAgent {
    updates: Updates,
    helper: PathBuf,
    sessions: Sessions,
}

impl BusService for PolkitAgent {
    type Command = AuthenticationCommand;
    const NAME: &'static str = "org.freedesktop.PolicyKit1";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<AuthenticationCommand>,
    ) -> zbus::Result<()> {
        let Some(session) = crate::logind::session_id(conn).await else {
            tracing::info!("Not running in a logind session; another agent has to answer polkit");
            while let Some(command) = commands.recv().await {
                self.sessions.command(command);
            }
            return Ok(());
        };
        let agent = Agent {
            sessions: self.sessions.clone(),
            updates: self.updates.clone(),
            helper: self.helper.clone(),
            authority: owner.clone(),
        };
        let server = conn.object_server();
        // The object of an earlier run knows an earlier polkitd.
        let _ = server.remove::<Agent, _>(AGENT_PATH).await;
        server.at(AGENT_PATH, agent).await?;
        let subject =
            ("unix-session", HashMap::from([("session-id", Value::from(session.as_str()))]));
        let register = (&subject, locale(), AGENT_PATH);
        let method = "RegisterAuthenticationAgent";
        bus::call(conn, owner, AUTHORITY_PATH, AUTHORITY_INTERFACE, method, &register).await?;
        tracing::info!("Answering polkit for session {session}");
        while let Some(command) = commands.recv().await {
            self.sessions.command(command);
        }
        // Lets polkitd turn to another agent right away.
        let unregister = (&subject, AGENT_PATH);
        let method = "UnregisterAuthenticationAgent";
        let _ =
            bus::call(conn, owner, AUTHORITY_PATH, AUTHORITY_INTERFACE, method, &unregister).await;
        Ok(())
    }

    fn unavailable(&mut self) {
        // The calls of a polkitd that left are moot.
        self.sessions.cancel_all();
    }

    fn command_unavailable(&mut self, command: AuthenticationCommand) {
        self.sessions.command(command);
    }
}

/// Runs the agent with the helper at `helper`, or the installed one, until the command channel closes.
pub(crate) async fn run(
    conn: Connection,
    updates: Updates,
    helper: Option<PathBuf>,
    mut commands: UnboundedReceiver<AuthenticationCommand>,
) {
    let Some(helper) = helper.or_else(helper::find) else {
        tracing::info!("polkit-agent-helper-1 isn't installed; another agent has to answer polkit");
        while commands.recv().await.is_some() {}
        return;
    };
    // polkitd may be started on demand, and the agent can only register once it runs.
    let start = ("org.freedesktop.PolicyKit1", 0u32);
    if let Err(err) = bus::call(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "StartServiceByName",
        &start,
    )
    .await
    {
        tracing::debug!("Can't start polkitd: {err}");
    }
    let agent = PolkitAgent { updates, helper, sessions: Sessions::default() };
    bus::supervise(agent, conn, commands).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(kind: &str, key: &str, id: u32) -> Identity {
        let value = OwnedValue::from(id);
        (kind.into(), HashMap::from([(key.to_owned(), value)]))
    }

    #[test]
    fn resolves_users_and_groups() {
        let root = identity("unix-user", "uid", 0);
        assert_eq!(users(std::slice::from_ref(&root)), ["root"]);
        let unknown = identity("unix-user", "uid", 4_000_000_000);
        let netgroup = identity("unix-netgroup", "name", 0);
        assert_eq!(users(&[unknown, netgroup, root.clone(), root]), ["root"]);
        let no_uid = ("unix-user".to_owned(), HashMap::new());
        assert!(users(&[no_uid]).is_empty());
    }

    #[test]
    fn prefers_the_current_user() {
        let current = User::from_uid(Uid::current()).unwrap().unwrap().name;
        let users = vec!["nobody-else".to_owned(), current];
        assert_eq!(preferred(&users), 1);
        assert_eq!(preferred(&["nobody-else".to_owned()]), 0);
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let command = AuthenticationCommand::Respond { id: 1, response: Secret::from("hunter2") };
        assert!(!format!("{command:?}").contains("hunter2"));
        assert_eq!(Secret::from("hunter2").expose(), "hunter2");
    }

    #[test]
    fn commands_reach_their_request() {
        let sessions = Sessions::default();
        let (first, mut first_inputs) = sessions.add("a".into());
        let (second, mut second_inputs) = sessions.add("b".into());
        assert_ne!(first, second);
        sessions.command(AuthenticationCommand::SelectIdentity { id: second, identity: 2 });
        assert!(matches!(second_inputs.try_recv(), Ok(Input::SelectIdentity(2))));
        assert!(first_inputs.try_recv().is_err());
        assert!(sessions.cancel_cookie("a"));
        assert!(matches!(first_inputs.try_recv(), Ok(Input::Cancel)));
        assert!(!sessions.cancel_cookie("c"));
        sessions.registry().active.remove(&first);
        sessions.command(AuthenticationCommand::Cancel { id: first });
    }
}
