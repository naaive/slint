// SPDX-License-Identifier: MIT

//! Shared D-Bus plumbing: connecting, calls with timeouts, property bags,
//! signal subscriptions, and supervision of services that come and go on the bus.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use futures_util::stream::{BoxStream, StreamExt, select_all};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use zbus::message::Type as MessageType;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{DynamicType, OwnedValue, Value};
use zbus::{Connection, MatchRule, Message, MessageStream};

use crate::BusAddress;

pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const DBUS_NAME: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const SIGNAL_QUEUE: usize = 64;
/// A burst of signals is coalesced until it has been quiet this long, or for at most [`MAX_SETTLE`].
const QUIET: Duration = Duration::from_millis(50);
const MAX_SETTLE: Duration = Duration::from_millis(500);
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
pub(crate) enum BusKind {
    Session,
    System,
}

pub(crate) async fn connect(address: &BusAddress, kind: BusKind) -> Option<Connection> {
    let builder = match (address, kind) {
        (BusAddress::Disabled, _) => return None,
        (BusAddress::Default, BusKind::Session) => zbus::connection::Builder::session(),
        (BusAddress::Default, BusKind::System) => zbus::connection::Builder::system(),
        (BusAddress::Address(address), _) => zbus::connection::Builder::address(address.as_str()),
    };
    let result = match builder {
        Ok(builder) => timeout(CONNECT_TIMEOUT, builder.build())
            .await
            .unwrap_or_else(|_| Err(zbus::Error::Failure("timed out".into()))),
        Err(err) => Err(err),
    };
    match result {
        Ok(connection) => Some(connection),
        Err(err) => {
            tracing::info!("{kind:?} bus unavailable, its services stay disabled: {err}");
            None
        }
    }
}

pub(crate) fn timed_out() -> zbus::Error {
    zbus::Error::Failure("D-Bus call timed out".into())
}

pub(crate) fn stream_ended() -> zbus::Error {
    zbus::Error::Failure("signal stream ended".into())
}

/// The reason in an error, for people to read: a daemon's own message when it sent one.
pub(crate) fn reason(err: &zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(message), _) | zbus::Error::Failure(message) => {
            message.clone()
        }
        other => other.to_string(),
    }
}

/// Calls `method` and returns the reply, failing after [`CALL_TIMEOUT`] since the bus never times out calls itself.
pub(crate) async fn call<B>(
    conn: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<Message>
where
    B: Serialize + DynamicType,
{
    timeout(CALL_TIMEOUT, conn.call_method(Some(destination), path, Some(interface), method, body))
        .await
        .unwrap_or_else(|_| Err(timed_out()))
}

pub(crate) async fn call_for<B, R>(
    conn: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<R>
where
    B: Serialize + DynamicType,
    R: DeserializeOwned + zbus::zvariant::Type,
{
    call(conn, destination, path, interface, method, body).await?.body().deserialize()
}

pub(crate) async fn get_all(
    conn: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
) -> zbus::Result<Props> {
    call_for(conn, destination, path, PROPERTIES, "GetAll", &(interface,)).await.map(Props)
}

pub(crate) async fn get_property(
    conn: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    name: &str,
) -> zbus::Result<OwnedValue> {
    call_for(conn, destination, path, PROPERTIES, "Get", &(interface, name)).await
}

pub(crate) async fn set_property(
    conn: &Connection,
    destination: &str,
    path: &str,
    interface: &str,
    name: &str,
    value: Value<'_>,
) -> zbus::Result<()> {
    call(conn, destination, path, PROPERTIES, "Set", &(interface, name, value)).await.map(drop)
}

/// The `a{sv}` property bag returned by `org.freedesktop.DBus.Properties.GetAll`.
#[derive(Debug, Default)]
pub(crate) struct Props(pub(crate) HashMap<String, OwnedValue>);

impl Props {
    /// Removes `key` and converts it, or returns `None` if it's missing or has another type.
    pub(crate) fn take<T: TryFrom<OwnedValue>>(&mut self, key: &str) -> Option<T> {
        self.0.remove(key).and_then(|value| T::try_from(value).ok())
    }

    #[cfg(test)]
    pub(crate) fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, Value<'a>)>) -> Self {
        Self(
            pairs
                .into_iter()
                .filter_map(|(key, value)| Some((key.to_owned(), value.try_to_owned().ok()?)))
                .collect(),
        )
    }
}

/// Starts building a rule for signals.
pub(crate) fn signal_rule() -> zbus::match_rule::Builder<'static> {
    MatchRule::builder().msg_type(MessageType::Signal)
}

/// Rule for `PropertiesChanged` signals sent by `owner` under `path_namespace`.
pub(crate) fn properties_changed_rule(
    owner: &OwnedUniqueName,
    path_namespace: &'static str,
) -> zbus::Result<MatchRule<'static>> {
    Ok(signal_rule()
        .sender(owner.to_owned().into_inner())?
        .path_namespace(path_namespace)?
        .interface(PROPERTIES)?
        .member("PropertiesChanged")?
        .build())
}

pub(crate) type Signals = BoxStream<'static, Message>;

/// Subscribes to all `rules` and merges the matching messages into one stream.
pub(crate) async fn signals(
    conn: &Connection,
    rules: impl IntoIterator<Item = MatchRule<'static>>,
) -> zbus::Result<Signals> {
    let mut streams = Vec::new();
    for rule in rules {
        streams.push(MessageStream::for_match_rule(rule, conn, Some(SIGNAL_QUEUE)).await?);
    }
    Ok(select_all(streams).filter_map(|message| std::future::ready(message.ok())).boxed())
}

/// Drains `stream` until it has been quiet for a moment; returns `false` if it ended.
pub(crate) async fn settle<S: futures_util::Stream + Unpin>(stream: &mut S) -> bool {
    let deadline = Instant::now() + MAX_SETTLE;
    loop {
        let wait_until = (Instant::now() + QUIET).min(deadline);
        match timeout_at(wait_until, stream.next()).await {
            Ok(Some(_)) => {}
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

pub(crate) enum Wake<C> {
    /// One or more signals arrived; the burst has settled.
    Signal,
    Command(C),
}

/// Waits for the next signal burst or command.
/// Returns `Ok(None)` once the command channel closes, which means the services shut down.
pub(crate) async fn next_wake<C>(
    signals: &mut Signals,
    commands: &mut UnboundedReceiver<C>,
) -> zbus::Result<Option<Wake<C>>> {
    next_wake_where(signals, commands, |_| true).await
}

/// Like [`next_wake`], but ignores signals for which `relevant` returns `false`.
pub(crate) async fn next_wake_where<C>(
    signals: &mut Signals,
    commands: &mut UnboundedReceiver<C>,
    relevant: impl Fn(&Message) -> bool,
) -> zbus::Result<Option<Wake<C>>> {
    let mut signals = signals.filter(|message| std::future::ready(relevant(message)));
    tokio::select! {
        message = signals.next() => {
            if message.is_some() && settle(&mut signals).await {
                Ok(Some(Wake::Signal))
            } else {
                Err(stream_ended())
            }
        }
        command = commands.recv() => Ok(command.map(Wake::Command)),
    }
}

/// Tracks the unique name that owns a well-known name.
pub(crate) struct NameWatch {
    changes: MessageStream,
    owner: Option<OwnedUniqueName>,
}

impl NameWatch {
    pub(crate) async fn new(conn: &Connection, name: &'static str) -> zbus::Result<Self> {
        let rule = signal_rule()
            .sender(DBUS_NAME)?
            .path(DBUS_PATH)?
            .interface(DBUS_NAME)?
            .member("NameOwnerChanged")?
            .arg(0, name)?
            .build();
        let changes = MessageStream::for_match_rule(rule, conn, Some(SIGNAL_QUEUE)).await?;
        let owner = match call_for::<_, OwnedUniqueName>(
            conn,
            DBUS_NAME,
            DBUS_PATH,
            DBUS_NAME,
            "GetNameOwner",
            &(name,),
        )
        .await
        {
            Ok(owner) => Some(owner),
            Err(zbus::Error::MethodError(error_name, _, _))
                if error_name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" =>
            {
                None
            }
            Err(err) => return Err(err),
        };
        Ok(Self { changes, owner })
    }

    pub(crate) fn owner(&self) -> Option<&OwnedUniqueName> {
        self.owner.as_ref()
    }

    /// Waits until the owner changes and returns the new one, or `None` if the connection closed.
    pub(crate) async fn changed(&mut self) -> Option<Option<OwnedUniqueName>> {
        loop {
            let Ok(message) = self.changes.next().await? else {
                continue;
            };
            let Ok((_, _, new_owner)) = message.body().deserialize::<(String, String, String)>()
            else {
                continue;
            };
            let new_owner = OwnedUniqueName::try_from(new_owner).ok();
            if new_owner != self.owner {
                self.owner.clone_from(&new_owner);
                return Some(new_owner);
            }
        }
    }
}

/// A client of one D-Bus daemon, run by [`supervise`] whenever the daemon owns its name.
pub(crate) trait BusService: Send + 'static {
    type Command: Send + 'static;
    /// The daemon's well-known bus name.
    const NAME: &'static str;

    /// Publishes state and handles commands until the command channel closes (`Ok`) or the daemon fails (`Err`).
    fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<Self::Command>,
    ) -> impl Future<Output = zbus::Result<()>> + Send;

    /// Publishes the state for "daemon not available".
    fn unavailable(&mut self);

    /// Handles a command that arrives while the daemon isn't available.
    fn command_unavailable(&mut self, command: Self::Command) {
        let _ = command;
        tracing::debug!("{} isn't available; ignoring command", Self::NAME);
    }
}

/// Answers every command as unavailable, for a service whose bus can't be reached.
pub(crate) async fn reject_all<S: BusService>(
    mut service: S,
    mut commands: UnboundedReceiver<S::Command>,
) {
    while let Some(command) = commands.recv().await {
        service.command_unavailable(command);
    }
}

enum Outcome {
    Ended(zbus::Result<()>),
    OwnerChanged(Option<Option<OwnedUniqueName>>),
}

/// Runs `service` whenever its daemon is on the bus, and keeps retrying with backoff after failures.
pub(crate) async fn supervise<S: BusService>(
    mut service: S,
    conn: Connection,
    mut commands: UnboundedReceiver<S::Command>,
) {
    let mut watch = match NameWatch::new(&conn, S::NAME).await {
        Ok(watch) => watch,
        Err(err) => {
            tracing::info!("Can't watch {}, service disabled: {err}", S::NAME);
            return;
        }
    };
    if watch.owner().is_none() {
        tracing::debug!("{} isn't running yet", S::NAME);
        service.unavailable();
    }
    let mut retry = RETRY_MIN;
    loop {
        let Some(owner) = watch.owner().cloned() else {
            match idle(&mut service, &mut watch, &mut commands, None).await {
                Idle::Stop => return,
                Idle::OwnerChanged => {
                    retry = RETRY_MIN;
                    continue;
                }
                Idle::Retry => continue,
            }
        };
        let outcome = tokio::select! {
            result = service.run(&conn, &owner, &mut commands) => Outcome::Ended(result),
            change = watch.changed() => Outcome::OwnerChanged(change),
        };
        match outcome {
            Outcome::Ended(Ok(())) | Outcome::OwnerChanged(None) => return,
            Outcome::OwnerChanged(Some(new_owner)) => {
                service.unavailable();
                retry = RETRY_MIN;
                if new_owner.is_none() {
                    tracing::info!("{} left the bus", S::NAME);
                }
            }
            Outcome::Ended(Err(err)) => {
                tracing::debug!("{} failed, retrying in {retry:?}: {err}", S::NAME);
                service.unavailable();
                match idle(&mut service, &mut watch, &mut commands, Some(retry)).await {
                    Idle::Stop => return,
                    Idle::OwnerChanged => retry = RETRY_MIN,
                    Idle::Retry => retry = (retry * 2).min(RETRY_MAX),
                }
            }
        }
    }
}

enum Idle {
    Stop,
    OwnerChanged,
    Retry,
}

async fn idle<S: BusService>(
    service: &mut S,
    watch: &mut NameWatch,
    commands: &mut UnboundedReceiver<S::Command>,
    retry_after: Option<Duration>,
) -> Idle {
    let deadline = retry_after.map(|delay| Instant::now() + delay);
    loop {
        let retry = async {
            match deadline {
                Some(deadline) => sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            change = watch.changed() => {
                return match change {
                    Some(Some(_)) => {
                        tracing::info!("{} appeared on the bus", S::NAME);
                        Idle::OwnerChanged
                    }
                    Some(None) => Idle::OwnerChanged,
                    None => Idle::Stop,
                };
            }
            command = commands.recv() => match command {
                Some(command) => service.command_unavailable(command),
                None => return Idle::Stop,
            },
            () = retry => return Idle::Retry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(path: &str) -> Message {
        Message::signal(path, PROPERTIES, "PropertiesChanged").unwrap().build(&()).unwrap()
    }

    fn stream(paths: &[&str]) -> Signals {
        let messages: Vec<Message> = paths.iter().map(|path| changed(path)).collect();
        futures_util::stream::iter(messages).chain(futures_util::stream::pending()).boxed()
    }

    #[tokio::test(start_paused = true)]
    async fn wakes_only_for_relevant_signals() {
        let (_sender, mut commands) = tokio::sync::mpsc::unbounded_channel::<()>();
        let relevant = |message: &Message| message.header().path().is_some_and(|p| p == "/a");

        let mut signals = stream(&["/b", "/c", "/b"]);
        let wake = next_wake_where(&mut signals, &mut commands, relevant);
        assert!(timeout(Duration::from_secs(5), wake).await.is_err());

        let mut signals = stream(&["/b", "/a", "/c"]);
        let wake = next_wake_where(&mut signals, &mut commands, relevant).await;
        assert!(matches!(wake, Ok(Some(Wake::Signal))));
    }
}
