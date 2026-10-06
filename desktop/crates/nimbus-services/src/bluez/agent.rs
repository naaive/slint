// SPDX-License-Identifier: MIT

//! The pairing agent, `org.bluez.Agent1`, and the pairing requests it has open.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;
use zbus::message::Header;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{Connection, interface};

use super::{Answer, Event, PairingKind, PairingRequest};
use crate::bluetooth::DEVICE_INTERFACE;
use crate::bus;

pub(crate) const AGENT_PATH: &str = "/org/nimbus/BluetoothAgent";

pub(crate) type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// The names of the devices in the last snapshot, by object path.
#[derive(Clone, Default)]
pub(crate) struct Names(Arc<Mutex<HashMap<String, String>>>);

impl Names {
    pub(crate) fn set(&self, names: HashMap<String, String>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = names;
    }

    pub(crate) fn get(&self, device: &str) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(device).cloned()
    }
}

/// The pairing requests that are open, by id.
#[derive(Clone, Default)]
pub(crate) struct Pairings(Arc<Mutex<Registry>>);

#[derive(Default)]
struct Registry {
    last_id: u32,
    open: HashMap<u32, Open>,
}

struct Open {
    device: OwnedObjectPath,
    /// `None` for codes shown to the user, which need no answer.
    answer: Option<oneshot::Sender<Answer>>,
}

impl Pairings {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Opens a request about `device`; a code shown for the same device keeps its id.
    fn open(&self, device: &ObjectPath<'_>, answer: Option<oneshot::Sender<Answer>>) -> u32 {
        let mut registry = self.lock();
        let shown = registry
            .open
            .iter()
            .find(|(_, open)| open.answer.is_none() && open.device.as_str() == device.as_str())
            .map(|(id, _)| *id);
        let id = match shown {
            Some(id) if answer.is_none() => id,
            _ => {
                registry.last_id = registry.last_id.wrapping_add(1);
                registry.last_id
            }
        };
        registry.open.insert(id, Open { device: device.to_owned().into(), answer });
        id
    }

    /// Delivers `answer` and returns whether a request with this id waited for one.
    pub(crate) fn answer(&self, id: u32, answer: Answer) -> bool {
        let Some(open) = self.lock().open.remove(&id) else { return false };
        open.answer.is_some_and(|sender| sender.send(answer).is_ok())
    }

    /// Closes the request with this id; returns whether it was open.
    fn close(&self, id: u32) -> bool {
        self.lock().open.remove(&id).is_some()
    }

    /// Closes the codes shown for `device`, which need no answer, and returns their ids.
    pub(crate) fn close_shown(&self, device: &str) -> Vec<u32> {
        let mut registry = self.lock();
        let ids: Vec<u32> = registry
            .open
            .iter()
            .filter(|(_, open)| open.answer.is_none() && open.device.as_str() == device)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            registry.open.remove(id);
        }
        ids
    }

    /// Closes the requests about `device`, or every request, and returns their ids.
    pub(crate) fn close_all(&self, device: Option<&str>) -> Vec<u32> {
        let mut registry = self.lock();
        let ids: Vec<u32> = registry
            .open
            .iter()
            .filter(|(_, open)| device.is_none_or(|device| open.device.as_str() == device))
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            registry.open.remove(id);
        }
        ids
    }
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

pub(crate) struct Agent {
    pub(crate) emit: Emit,
    pub(crate) pairings: Pairings,
    pub(crate) names: Names,
    /// BlueZ's unique name, the only caller the agent answers.
    pub(crate) bluez: Arc<Mutex<Option<OwnedUniqueName>>>,
}

/// Closes a request that waited for an answer when the method call ends, however it ends.
struct Closing<'a> {
    agent: &'a Agent,
    id: u32,
}

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        self.agent.pairings.close(self.id);
        (self.agent.emit)(Event::PairingEnded { id: self.id });
    }
}

impl Agent {
    fn check_caller(&self, header: &Header<'_>) -> Result<(), AgentError> {
        let bluez = self.bluez.lock().unwrap_or_else(PoisonError::into_inner);
        match (header.sender(), bluez.as_ref()) {
            (Some(sender), Some(bluez)) if *sender == *bluez => Ok(()),
            _ => Err(AgentError::Rejected("only BlueZ may call the agent".into())),
        }
    }

    /// The device's name, as BlueZ knows it.
    async fn name(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        device: &ObjectPath<'_>,
    ) -> String {
        if let Some(name) = self.names.get(device) {
            return name;
        }
        // BlueZ adds a device that asks to pair right before it asks, so the snapshot may not have it yet.
        let alias = match header.sender() {
            Some(bluez) => {
                bus::get_property(conn, bluez, device, DEVICE_INTERFACE, "Alias").await.ok()
            }
            None => None,
        };
        alias
            .and_then(|alias: OwnedValue| String::try_from(alias).ok())
            .unwrap_or_else(|| device.rsplit('/').next().unwrap_or_default().to_owned())
    }

    async fn request(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        device: &ObjectPath<'_>,
        kind: PairingKind,
    ) -> Result<Answer, AgentError> {
        self.check_caller(header)?;
        let name = self.name(conn, header, device).await;
        let (sender, answer) = oneshot::channel();
        let id = self.pairings.open(device, Some(sender));
        let _closing = Closing { agent: self, id };
        (self.emit)(Event::Pairing(PairingRequest {
            id,
            device_id: device.to_string(),
            device: name,
            kind,
        }));
        // The sender goes away when BlueZ cancels the request or the client stops.
        answer.await.map_err(|_| AgentError::Canceled("the request was cancelled".into()))
    }

    async fn show(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        device: &ObjectPath<'_>,
        kind: PairingKind,
    ) -> Result<(), AgentError> {
        self.check_caller(header)?;
        let name = self.name(conn, header, device).await;
        let id = self.pairings.open(device, None);
        (self.emit)(Event::Pairing(PairingRequest {
            id,
            device_id: device.to_string(),
            device: name,
            kind,
        }));
        Ok(())
    }

    fn cancel_all(&self) {
        for id in self.pairings.close_all(None) {
            (self.emit)(Event::PairingEnded { id });
        }
    }
}

fn accepted(answer: Answer) -> Result<(), AgentError> {
    match answer {
        Answer::Accept => Ok(()),
        _ => Err(AgentError::Rejected("the user declined".into())),
    }
}

#[interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self, #[zbus(header)] header: Header<'_>) -> Result<(), AgentError> {
        self.check_caller(&header)?;
        self.cancel_all();
        Ok(())
    }

    async fn request_pin_code(
        &self,
        device: ObjectPath<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<String, AgentError> {
        match self.request(conn, &header, &device, PairingKind::EnterPin).await? {
            Answer::Pin(pin) => Ok(pin),
            _ => Err(AgentError::Rejected("no PIN".into())),
        }
    }

    async fn display_pin_code(
        &self,
        device: ObjectPath<'_>,
        pincode: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), AgentError> {
        self.show(conn, &header, &device, PairingKind::DisplayPin { pin: pincode }).await
    }

    async fn request_passkey(
        &self,
        device: ObjectPath<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<u32, AgentError> {
        match self.request(conn, &header, &device, PairingKind::EnterPasskey).await? {
            Answer::Passkey(passkey) => Ok(passkey),
            _ => Err(AgentError::Rejected("no passkey".into())),
        }
    }

    async fn display_passkey(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        entered: u16,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), AgentError> {
        self.show(conn, &header, &device, PairingKind::DisplayPasskey { passkey, entered }).await
    }

    async fn request_confirmation(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), AgentError> {
        accepted(self.request(conn, &header, &device, PairingKind::Confirm { passkey }).await?)
    }

    async fn request_authorization(
        &self,
        device: ObjectPath<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), AgentError> {
        accepted(self.request(conn, &header, &device, PairingKind::Authorize).await?)
    }

    async fn authorize_service(
        &self,
        device: ObjectPath<'_>,
        uuid: String,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), AgentError> {
        let kind = PairingKind::AuthorizeService { uuid };
        accepted(self.request(conn, &header, &device, kind).await?)
    }

    async fn cancel(&self, #[zbus(header)] header: Header<'_>) -> Result<(), AgentError> {
        self.check_caller(&header)?;
        self.cancel_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(device: &str) -> ObjectPath<'_> {
        ObjectPath::try_from(device).unwrap()
    }

    #[test]
    fn requests_open_answer_and_close() {
        let pairings = Pairings::default();
        let (sender, mut answer) = oneshot::channel();
        let asked = pairings.open(&path("/org/bluez/hci0/dev_A"), Some(sender));
        let shown = pairings.open(&path("/org/bluez/hci0/dev_B"), None);
        assert_ne!(asked, shown);
        assert_eq!(
            pairings.open(&path("/org/bluez/hci0/dev_B"), None),
            shown,
            "updating a shown code"
        );

        assert!(!pairings.answer(shown, Answer::Accept), "a shown code takes no answer");
        assert!(pairings.answer(asked, Answer::Passkey(7)));
        assert_eq!(answer.try_recv(), Ok(Answer::Passkey(7)));
        assert!(!pairings.answer(asked, Answer::Accept), "answered once");

        pairings.open(&path("/org/bluez/hci0/dev_A"), None);
        let other = pairings.open(&path("/org/bluez/hci0/dev_B"), None);
        assert_eq!(pairings.close_all(Some("/org/bluez/hci0/dev_B")), [other]);
        assert_eq!(pairings.close_all(None).len(), 1);
    }
}
