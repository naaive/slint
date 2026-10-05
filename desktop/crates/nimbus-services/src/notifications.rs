// SPDX-License-Identifier: MIT

//! An `org.freedesktop.Notifications` server, following the Desktop Notifications Specification 1.2.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::AbortHandle;
use zbus::fdo::RequestNameFlags;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, interface};

use crate::hub::Updates;
use crate::{CloseReason, Notification, ServiceEvent, Urgency};

pub(crate) const NAME: &str = "org.freedesktop.Notifications";
pub(crate) const PATH: &str = "/org/freedesktop/Notifications";
const SPEC_VERSION: &str = "1.2";
/// How long low and normal urgency notifications stay when the client leaves it to the server.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// How many expired notifications stay open for the shell's history, which holds at most 100.
const MAX_EXPIRED: usize = 128;

#[derive(Debug)]
pub(crate) enum NotificationCommand {
    InvokeAction { id: u32, action: String, activation_token: Option<String> },
    Close { id: u32, reason: CloseReason },
}

/// The arguments of a `Notify` call.
#[derive(Debug, Default)]
pub(crate) struct NotifyArgs {
    pub(crate) app_name: String,
    pub(crate) replaces_id: u32,
    pub(crate) app_icon: String,
    pub(crate) summary: String,
    pub(crate) body: String,
    pub(crate) actions: Vec<String>,
    pub(crate) hints: HashMap<String, OwnedValue>,
    pub(crate) expire_timeout: i32,
}

/// Server-side behavior from hints that [`Notification`] doesn't carry.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Behavior {
    pub(crate) resident: bool,
}

fn hint_integer(value: &Value<'_>) -> Option<i64> {
    match value {
        Value::U8(v) => Some(i64::from(*v)),
        Value::I16(v) => Some(i64::from(*v)),
        Value::U16(v) => Some(i64::from(*v)),
        Value::I32(v) => Some(i64::from(*v)),
        Value::U32(v) => Some(i64::from(*v)),
        Value::I64(v) => Some(*v),
        Value::U64(v) => i64::try_from(*v).ok(),
        Value::Value(inner) => hint_integer(inner),
        _ => None,
    }
}

fn hint_bool(value: &Value<'_>) -> Option<bool> {
    match value {
        Value::Bool(v) => Some(*v),
        Value::Value(inner) => hint_bool(inner),
        other => hint_integer(other).map(|v| v != 0),
    }
}

fn hint_string(value: &Value<'_>) -> Option<String> {
    match value {
        Value::Str(s) if !s.is_empty() => Some(s.to_string()),
        Value::Value(inner) => hint_string(inner),
        _ => None,
    }
}

/// Builds the notification for `args`; `id` is already resolved from `replaces_id`.
pub(crate) fn parse_notify(
    id: u32,
    args: NotifyArgs,
    received: SystemTime,
) -> (Notification, Behavior) {
    let hint = |key: &str| args.hints.get(key).map(|value| &**value);
    let urgency = match hint("urgency").and_then(hint_integer) {
        Some(0) => Urgency::Low,
        Some(2) => Urgency::Critical,
        _ => Urgency::Normal,
    };
    let resident = hint("resident").and_then(hint_bool).unwrap_or(false);
    let behavior = Behavior { resident };
    // The specification ranks image-path above app_icon, and deprecated image_path is still sent by older clients.
    let app_icon = hint("image-path")
        .or_else(|| hint("image_path"))
        .and_then(hint_string)
        .or_else(|| (!args.app_icon.is_empty()).then(|| args.app_icon.clone()))
        .or_else(|| hint("desktop-entry").and_then(hint_string))
        .unwrap_or_default();
    let mut actions = Vec::with_capacity(args.actions.len() / 2);
    let mut flat = args.actions.into_iter();
    while let (Some(key), Some(label)) = (flat.next(), flat.next()) {
        actions.push((key, label));
    }
    let expire_timeout = match args.expire_timeout {
        0 => Some(Duration::ZERO),
        ms if ms > 0 => Some(Duration::from_millis(ms.unsigned_abs().into())),
        _ => None,
    };
    let notification = Notification {
        id,
        app_name: args.app_name,
        app_icon,
        summary: args.summary,
        body: args.body,
        actions,
        urgency,
        expire_timeout,
        received,
        transient: hint("transient").and_then(hint_bool).unwrap_or(false),
        resident,
    };
    (notification, behavior)
}

/// How long until the server expires `notification`, or `None` if it stays until closed.
pub(crate) fn expiry(notification: &Notification) -> Option<Duration> {
    match notification.expire_timeout {
        Some(Duration::ZERO) => None,
        Some(timeout) => Some(timeout),
        None if notification.urgency == Urgency::Critical => None,
        None => Some(DEFAULT_TIMEOUT),
    }
}

struct Entry {
    generation: u64,
    resident: bool,
    transient: bool,
    action_keys: Vec<String>,
    expiry: Option<AbortHandle>,
    /// The timeout elapsed, but the notification stays open while the shell shows it in its history.
    expired: bool,
}

#[derive(Default)]
struct Registry {
    last_id: u32,
    generation: u64,
    active: HashMap<u32, Entry>,
}

impl Registry {
    /// Removes and returns the oldest expired notification once more than [`MAX_EXPIRED`] are open.
    fn evict_expired(&mut self) -> Option<u32> {
        let expired = self.active.iter().filter(|(_, entry)| entry.expired);
        if expired.clone().count() <= MAX_EXPIRED {
            return None;
        }
        let id = expired.min_by_key(|(_, entry)| entry.generation).map(|(id, _)| *id)?;
        self.active.remove(&id);
        Some(id)
    }

    fn allocate(&mut self) -> u32 {
        loop {
            self.last_id = self.last_id.wrapping_add(1);
            if self.last_id != 0 && !self.active.contains_key(&self.last_id) {
                return self.last_id;
            }
        }
    }
}

#[derive(Clone)]
struct Core {
    registry: Arc<Mutex<Registry>>,
    conn: Connection,
    updates: Updates,
}

impl Core {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self, args: NotifyArgs) -> u32 {
        let notification = {
            let mut registry = self.registry();
            let id = if args.replaces_id != 0 { args.replaces_id } else { registry.allocate() };
            if let Some(handle) = registry.active.remove(&id).and_then(|entry| entry.expiry) {
                handle.abort();
            }
            let (notification, behavior) = parse_notify(id, args, SystemTime::now());
            registry.generation += 1;
            let generation = registry.generation;
            let expiry = expiry(&notification).map(|delay| {
                let core = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    core.expire(id, generation).await;
                })
                .abort_handle()
            });
            let action_keys = notification.actions.iter().map(|(key, _)| key.clone()).collect();
            let entry = Entry {
                generation,
                resident: behavior.resident,
                transient: notification.transient,
                action_keys,
                expiry,
                expired: false,
            };
            registry.active.insert(id, entry);
            notification
        };
        let id = notification.id;
        self.updates.event(ServiceEvent::Notification(notification));
        id
    }

    async fn close(&self, id: u32, reason: CloseReason) {
        let entry = self.registry().active.remove(&id);
        if let Some(entry) = entry {
            if let Some(handle) = entry.expiry {
                handle.abort();
            }
            self.closed(id, reason).await;
        }
    }

    /// Handles the expiry timer of `id`, unless it was replaced meanwhile.
    /// Transient notifications close; others only leave the screen and stay open for the shell's history.
    async fn expire(&self, id: u32, generation: u64) {
        let (close, evicted) = {
            let mut registry = self.registry();
            match registry.active.get_mut(&id) {
                Some(entry) if entry.generation == generation => {
                    entry.expiry = None;
                    if entry.transient {
                        registry.active.remove(&id);
                        (true, None)
                    } else {
                        entry.expired = true;
                        (false, registry.evict_expired())
                    }
                }
                _ => return,
            }
        };
        if close {
            self.closed(id, CloseReason::Expired).await;
        } else {
            self.updates
                .event(ServiceEvent::NotificationClosed { id, reason: CloseReason::Expired });
        }
        if let Some(evicted) = evicted {
            self.emit_closed(evicted, CloseReason::Expired).await;
        }
    }

    async fn closed(&self, id: u32, reason: CloseReason) {
        self.updates.event(ServiceEvent::NotificationClosed { id, reason });
        self.emit_closed(id, reason).await;
    }

    async fn emit_closed(&self, id: u32, reason: CloseReason) {
        let result = match SignalEmitter::new(&self.conn, PATH) {
            Ok(emitter) => Server::notification_closed(&emitter, id, reason as u32).await,
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            tracing::debug!("Can't emit NotificationClosed for {id}: {err}");
        }
    }

    async fn invoke_action(&self, id: u32, action: &str, activation_token: Option<&str>) {
        let resident = {
            let registry = self.registry();
            match registry.active.get(&id) {
                Some(entry) if entry.action_keys.iter().any(|key| key == action) => entry.resident,
                Some(_) => {
                    tracing::debug!("Notification {id} has no action {action:?}");
                    return;
                }
                None => {
                    tracing::debug!("Notification {id} is already closed");
                    return;
                }
            }
        };
        match SignalEmitter::new(&self.conn, PATH) {
            Ok(emitter) => {
                // The specification sends ActivationToken ahead of ActionInvoked.
                if let Some(token) = activation_token
                    && let Err(err) = Server::activation_token(&emitter, id, token).await
                {
                    tracing::debug!("Can't emit ActivationToken for {id}: {err}");
                }
                if let Err(err) = Server::action_invoked(&emitter, id, action).await {
                    tracing::debug!("Can't emit ActionInvoked for {id}: {err}");
                }
            }
            Err(err) => tracing::debug!("Can't emit ActionInvoked for {id}: {err}"),
        }
        if !resident {
            self.close(id, CloseReason::Dismissed).await;
        }
    }
}

struct Server {
    core: Core,
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        self.core.notify(NotifyArgs {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints,
            expire_timeout,
        })
    }

    async fn close_notification(&self, id: u32) {
        self.core.close(id, CloseReason::Closed).await;
    }

    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["actions", "body", "icon-static", "persistence"]
    }

    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("Nimbus", "Nimbus", env!("CARGO_PKG_VERSION"), SPEC_VERSION)
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;
}

pub(crate) async fn run(
    conn: Connection,
    updates: Updates,
    mut commands: UnboundedReceiver<NotificationCommand>,
) {
    let core = Core { registry: Arc::default(), conn: conn.clone(), updates };
    if let Err(err) = conn.object_server().at(PATH, Server { core: core.clone() }).await {
        tracing::warn!("Can't serve {PATH}, notifications disabled: {err}");
        return;
    }
    match conn.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(_) => tracing::info!("Serving {NAME}"),
        Err(zbus::Error::NameTaken) => {
            tracing::info!(
                "Another notification daemon owns {NAME}; Nimbus notifications disabled"
            );
            let _ = conn.object_server().remove::<Server, _>(PATH).await;
            return;
        }
        Err(err) => {
            tracing::warn!("Can't own {NAME}, notifications disabled: {err}");
            let _ = conn.object_server().remove::<Server, _>(PATH).await;
            return;
        }
    }
    while let Some(command) = commands.recv().await {
        match command {
            NotificationCommand::InvokeAction { id, action, activation_token } => {
                core.invoke_action(id, &action, activation_token.as_deref()).await
            }
            NotificationCommand::Close { id, reason } => core.close(id, reason).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints<'a>(
        pairs: impl IntoIterator<Item = (&'a str, Value<'a>)>,
    ) -> HashMap<String, OwnedValue> {
        crate::bus::Props::from_pairs(pairs).0
    }

    #[test]
    fn parses_hints_and_actions() {
        let args = NotifyArgs {
            app_name: "mail".into(),
            app_icon: "mail-unread".into(),
            summary: "New mail".into(),
            body: "Hello".into(),
            actions: vec![
                "default".into(),
                "Open".into(),
                "archive".into(),
                "Archive".into(),
                "odd".into(),
            ],
            hints: hints([
                ("urgency", Value::U8(2)),
                ("image-path", Value::from("/tmp/avatar.png")),
                ("resident", Value::Bool(true)),
            ]),
            expire_timeout: -1,
            ..Default::default()
        };
        let (n, behavior) = parse_notify(7, args, SystemTime::UNIX_EPOCH);
        assert_eq!(n.id, 7);
        assert_eq!(n.urgency, Urgency::Critical);
        assert_eq!(n.app_icon, "/tmp/avatar.png");
        assert_eq!(
            n.actions,
            vec![("default".into(), "Open".into()), ("archive".into(), "Archive".into())]
        );
        assert_eq!(n.expire_timeout, None);
        assert_eq!(behavior, Behavior { resident: true });
        assert_eq!(expiry(&n), None);
    }

    #[test]
    fn icon_fallbacks() {
        let args = NotifyArgs {
            hints: hints([("image_path", Value::from("legacy.png"))]),
            app_icon: "app".into(),
            ..Default::default()
        };
        assert_eq!(parse_notify(1, args, SystemTime::UNIX_EPOCH).0.app_icon, "legacy.png");

        let args = NotifyArgs {
            hints: hints([("desktop-entry", Value::from("org.gnome.Maps"))]),
            ..Default::default()
        };
        assert_eq!(parse_notify(1, args, SystemTime::UNIX_EPOCH).0.app_icon, "org.gnome.Maps");
    }

    #[test]
    fn tolerates_odd_hint_types() {
        let args = NotifyArgs {
            hints: hints([
                ("urgency", Value::I32(0)),
                ("resident", Value::U32(1)),
                ("image-path", Value::U32(5)),
            ]),
            ..Default::default()
        };
        let (n, behavior) = parse_notify(1, args, SystemTime::UNIX_EPOCH);
        assert_eq!(n.urgency, Urgency::Low);
        assert!(behavior.resident);
        assert!(n.resident && !n.transient);
        assert_eq!(n.app_icon, "");

        let args =
            NotifyArgs { hints: hints([("transient", Value::Bool(true))]), ..Default::default() };
        assert!(parse_notify(1, args, SystemTime::UNIX_EPOCH).0.transient);

        let args = NotifyArgs { hints: hints([("urgency", Value::U8(9))]), ..Default::default() };
        assert_eq!(parse_notify(1, args, SystemTime::UNIX_EPOCH).0.urgency, Urgency::Normal);
    }

    #[test]
    fn expire_timeouts() {
        let parse = |expire_timeout, urgency: u8| {
            let args = NotifyArgs {
                expire_timeout,
                hints: hints([("urgency", Value::U8(urgency))]),
                ..Default::default()
            };
            parse_notify(1, args, SystemTime::UNIX_EPOCH).0
        };
        let n = parse(-1, 1);
        assert_eq!(n.expire_timeout, None);
        assert_eq!(expiry(&n), Some(DEFAULT_TIMEOUT));
        let n = parse(0, 1);
        assert_eq!(n.expire_timeout, Some(Duration::ZERO));
        assert_eq!(expiry(&n), None);
        let n = parse(1500, 2);
        assert_eq!(n.expire_timeout, Some(Duration::from_millis(1500)));
        assert_eq!(expiry(&n), Some(Duration::from_millis(1500)));
        assert_eq!(parse(-7, 0).expire_timeout, None);
    }

    #[test]
    fn ids_skip_zero_and_active() {
        let mut registry = Registry { last_id: u32::MAX - 1, ..Default::default() };
        registry.active.insert(
            1,
            Entry {
                generation: 0,
                resident: false,
                transient: false,
                action_keys: Vec::new(),
                expiry: None,
                expired: false,
            },
        );
        assert_eq!(registry.allocate(), u32::MAX);
        assert_eq!(registry.allocate(), 2);
    }

    #[test]
    fn evicts_the_oldest_expired() {
        let mut registry = Registry::default();
        let entry = |generation, expired| Entry {
            generation,
            resident: false,
            transient: false,
            action_keys: Vec::new(),
            expiry: None,
            expired,
        };
        registry.active.insert(1, entry(1, false));
        for id in 2..=(MAX_EXPIRED as u32 + 1) {
            registry.active.insert(id, entry(u64::from(id), true));
        }
        assert_eq!(registry.evict_expired(), None);
        registry.active.insert(1000, entry(1000, true));
        assert_eq!(registry.evict_expired(), Some(2));
        assert_eq!(registry.evict_expired(), None);
        assert!(registry.active.contains_key(&1));
    }
}
