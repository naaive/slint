// SPDX-License-Identifier: MIT

//! The BlueZ client's D-Bus side: the adapter and its devices from the object manager, the commands, and the agent's registration.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use zbus::Connection;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::agent::{AGENT_PATH, Agent, Emit, Names, Pairings};
use super::{Adapter, BluetoothState, Command, Device, Event};
use crate::bluetooth::{ADAPTER_INTERFACE, DEVICE_INTERFACE, ManagedObjects, OBJECT_MANAGER};
use crate::bus::{self, BusService, Wake};

const AGENT_MANAGER: &str = "org.bluez.AgentManager1";
const BATTERY_INTERFACE: &str = "org.bluez.Battery1";
/// The agent can show codes and take them, so BlueZ picks the safest pairing method each device supports.
const CAPABILITY: &str = "KeyboardDisplay";
/// BlueZ waits for the device and the user while pairing or connecting, longer than [`bus::call`] waits.
const DEVICE_CALL_TIMEOUT: Duration = Duration::from_secs(90);

type Interfaces = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Pair,
    Connect,
    Disconnect,
}

impl Action {
    fn method(self) -> &'static str {
        match self {
            Action::Pair => "Pair",
            Action::Connect => "Connect",
            Action::Disconnect => "Disconnect",
        }
    }
}

/// A finished device call.
struct Done {
    device: String,
    action: Action,
    result: zbus::Result<()>,
}

pub(crate) struct BluetoothSettings {
    emit: Emit,
    pairings: Pairings,
    bluez: Arc<Mutex<Option<OwnedUniqueName>>>,
    /// Devices with a call in progress.
    busy: HashSet<String>,
    done_sender: UnboundedSender<Done>,
    done: UnboundedReceiver<Done>,
    names: Names,
    last: Option<BluetoothState>,
}

impl BluetoothSettings {
    pub(crate) fn new(emit: Emit) -> Self {
        let (done_sender, done) = mpsc::unbounded_channel();
        Self {
            emit,
            pairings: Pairings::default(),
            bluez: Arc::default(),
            busy: HashSet::new(),
            done_sender,
            done,
            names: Names::default(),
            last: None,
        }
    }

    fn publish(&mut self, state: BluetoothState) {
        self.names.set(state.devices.iter().map(|d| (d.id.clone(), d.name.clone())).collect());
        if self.last.as_ref() != Some(&state) {
            self.last = Some(state.clone());
            (self.emit)(Event::State(state));
        }
    }

    fn fail(&self, device: &str, err: &zbus::Error) {
        tracing::info!("BlueZ refused a request for {device}: {err}");
        let device = self.names.get(device).unwrap_or_default();
        (self.emit)(Event::Failed { device, message: bus::reason(err) });
    }

    /// Exports the agent and makes it BlueZ's default.
    async fn register_agent(&self, conn: &Connection, owner: &str) {
        let agent = Agent {
            emit: self.emit.clone(),
            pairings: self.pairings.clone(),
            names: self.names.clone(),
            bluez: self.bluez.clone(),
        };
        if let Err(err) = conn.object_server().at(AGENT_PATH, agent).await {
            tracing::warn!("Can't export the Bluetooth agent: {err}");
            return;
        }
        let path = ObjectPath::from_static_str_unchecked(AGENT_PATH);
        let registered = bus::call(
            conn,
            owner,
            "/org/bluez",
            AGENT_MANAGER,
            "RegisterAgent",
            &(&path, CAPABILITY),
        )
        .await;
        match registered {
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.AlreadyExists" => {}
            Err(err) => {
                tracing::info!(
                    "BlueZ didn't take the pairing agent, so pairing needs another one: {err}"
                );
                return;
            }
            Ok(_) => {}
        }
        let default =
            bus::call(conn, owner, "/org/bluez", AGENT_MANAGER, "RequestDefaultAgent", &(&path,))
                .await;
        if let Err(err) = default {
            tracing::debug!("The pairing agent isn't BlueZ's default: {err}");
        }
    }

    /// Calls `action` on `device` in the background; the outcome arrives through `done`.
    fn start(&mut self, conn: &Connection, owner: &str, device: String, action: Action) {
        if !self.busy.insert(device.clone()) {
            return;
        }
        let conn = conn.clone();
        let owner = owner.to_owned();
        let done = self.done_sender.clone();
        tokio::spawn(async move {
            let call = conn.call_method(
                Some(owner.as_str()),
                device.as_str(),
                Some(DEVICE_INTERFACE),
                action.method(),
                &(),
            );
            let result = match tokio::time::timeout(DEVICE_CALL_TIMEOUT, call).await {
                Ok(result) => result.map(drop),
                Err(_) => Err(bus::timed_out()),
            };
            let _ = done.send(Done { device, action, result });
        });
    }

    async fn finish(&mut self, conn: &Connection, owner: &str, done: Done) {
        self.busy.remove(&done.device);
        if done.action == Action::Pair {
            for id in self.pairings.close_all(Some(&done.device)) {
                (self.emit)(Event::PairingEnded { id });
            }
        }
        match (done.action, done.result) {
            (Action::Pair, Ok(())) => {
                let trusted = bus::set_property(
                    conn,
                    owner,
                    &done.device,
                    DEVICE_INTERFACE,
                    "Trusted",
                    Value::Bool(true),
                )
                .await;
                if let Err(err) = trusted {
                    tracing::debug!("Can't trust {}: {err}", done.device);
                }
                self.start(conn, owner, done.device, Action::Connect);
            }
            (_, Ok(())) => {}
            // The user cancelled through `Command::CancelPairing`.
            (Action::Pair, Err(zbus::Error::MethodError(name, _, _)))
                if name.as_str() == "org.bluez.Error.AuthenticationCanceled" => {}
            (_, Err(err)) => self.fail(&done.device, &err),
        }
    }

    async fn handle(
        &mut self,
        conn: &Connection,
        owner: &str,
        adapter: Option<&str>,
        command: Command,
    ) {
        let set = |name: &'static str, value: bool| async move {
            let Some(adapter) = adapter else { return Ok(()) };
            bus::set_property(conn, owner, adapter, ADAPTER_INTERFACE, name, Value::Bool(value))
                .await
        };
        let (device, result) = match command {
            Command::SetPowered(powered) => (String::new(), set("Powered", powered).await),
            Command::SetDiscoverable(on) => (String::new(), set("Discoverable", on).await),
            Command::SetDiscovering(on) => {
                let method = if on { "StartDiscovery" } else { "StopDiscovery" };
                let result = match adapter {
                    Some(adapter) => {
                        bus::call(conn, owner, adapter, ADAPTER_INTERFACE, method, &())
                            .await
                            .map(drop)
                    }
                    None => Ok(()),
                };
                (String::new(), result)
            }
            Command::Pair(device) => return self.start(conn, owner, device, Action::Pair),
            Command::Connect(device) => return self.start(conn, owner, device, Action::Connect),
            Command::Disconnect(device) => {
                return self.start(conn, owner, device, Action::Disconnect);
            }
            Command::Remove(device) => {
                let result = match (adapter, ObjectPath::try_from(device.as_str())) {
                    (Some(adapter), Ok(path)) => {
                        bus::call(conn, owner, adapter, ADAPTER_INTERFACE, "RemoveDevice", &(path,))
                            .await
                            .map(drop)
                    }
                    (_, Err(err)) => Err(err.into()),
                    (None, _) => Ok(()),
                };
                (device, result)
            }
            Command::CancelPairing(device) => {
                let result =
                    bus::call(conn, owner, &device, DEVICE_INTERFACE, "CancelPairing", &())
                        .await
                        .map(drop);
                (device, result)
            }
            Command::Answer { id, answer } => {
                if !self.pairings.answer(id, answer) {
                    tracing::debug!("Pairing request {id} is no longer open");
                }
                return;
            }
        };
        if let Err(err) = result {
            self.fail(&device, &err);
        }
    }
}

impl BusService for BluetoothSettings {
    type Command = Command;
    const NAME: &'static str = "org.bluez";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<Command>,
    ) -> zbus::Result<()> {
        *self.bluez.lock().unwrap_or_else(PoisonError::into_inner) = Some(owner.clone());
        let objects_rule = bus::signal_rule()
            .sender(owner.to_owned().into_inner())?
            .path("/")?
            .interface(OBJECT_MANAGER)?
            .build();
        let rules = [bus::properties_changed_rule(owner, "/org/bluez")?, objects_rule];
        let mut signals = bus::signals(conn, rules).await?;
        self.register_agent(conn, owner.as_str()).await;
        loop {
            let objects: ManagedObjects =
                bus::call_for(conn, owner.as_str(), "/", OBJECT_MANAGER, "GetManagedObjects", &())
                    .await?;
            let (state, adapter) = summarize(&objects, &self.busy);
            self.publish(state);
            tokio::select! {
                Some(done) = self.done.recv() => self.finish(conn, owner.as_str(), done).await,
                wake = bus::next_wake(&mut signals, commands) => match wake? {
                    Some(Wake::Signal) => {}
                    Some(Wake::Command(command)) => {
                        self.handle(conn, owner.as_str(), adapter.as_deref(), command).await;
                    }
                    None => return Ok(()),
                },
            }
        }
    }

    fn unavailable(&mut self) {
        *self.bluez.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.busy.clear();
        for id in self.pairings.close_all(None) {
            (self.emit)(Event::PairingEnded { id });
        }
        self.publish(BluetoothState::default());
    }

    fn command_unavailable(&mut self, command: Command) {
        tracing::debug!("BlueZ isn't available; ignoring {command:?}");
        if !matches!(command, Command::Answer { .. }) {
            let message = "Bluetooth isn't available".to_owned();
            (self.emit)(Event::Failed { device: String::new(), message });
        }
    }
}

fn property<T: TryFrom<OwnedValue>>(
    interfaces: &Interfaces,
    interface: &str,
    name: &str,
) -> Option<T> {
    let value = interfaces.get(interface)?.get(name)?.try_clone().ok()?;
    T::try_from(value).ok()
}

fn flag(interfaces: &Interfaces, interface: &str, name: &str) -> bool {
    property(interfaces, interface, name).unwrap_or(false)
}

/// The first adapter and its devices, and the adapter's path.
fn summarize(objects: &ManagedObjects, busy: &HashSet<String>) -> (BluetoothState, Option<String>) {
    let Some((path, interfaces)) = objects
        .iter()
        .filter(|(_, interfaces)| interfaces.contains_key(ADAPTER_INTERFACE))
        .min_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()))
    else {
        return (BluetoothState::default(), None);
    };
    let text = |name| property::<String>(interfaces, ADAPTER_INTERFACE, name);
    let adapter = Adapter {
        name: text("Alias").or_else(|| text("Name")).unwrap_or_default(),
        address: text("Address").unwrap_or_default(),
        powered: flag(interfaces, ADAPTER_INTERFACE, "Powered"),
        discoverable: flag(interfaces, ADAPTER_INTERFACE, "Discoverable"),
        discovering: flag(interfaces, ADAPTER_INTERFACE, "Discovering"),
    };
    let mut devices: Vec<Device> = objects
        .iter()
        .filter(|(device, interfaces)| {
            interfaces.contains_key(DEVICE_INTERFACE)
                && property::<OwnedValue>(interfaces, DEVICE_INTERFACE, "Adapter")
                    .and_then(|adapter| zbus::zvariant::OwnedObjectPath::try_from(adapter).ok())
                    .map_or_else(
                        || device.as_str().starts_with(&format!("{}/", path.as_str())),
                        |adapter| adapter == *path,
                    )
        })
        .filter_map(|(id, interfaces)| {
            let text = |name| property::<String>(interfaces, DEVICE_INTERFACE, name);
            let paired = flag(interfaces, DEVICE_INTERFACE, "Paired");
            let name = text("Name");
            if name.is_none() && !paired {
                return None;
            }
            let address = text("Address").unwrap_or_default();
            Some(Device {
                id: id.to_string(),
                name: text("Alias").or(name).unwrap_or_else(|| address.clone()),
                address,
                icon: text("Icon").unwrap_or_default(),
                paired,
                trusted: flag(interfaces, DEVICE_INTERFACE, "Trusted"),
                connected: flag(interfaces, DEVICE_INTERFACE, "Connected"),
                rssi: property(interfaces, DEVICE_INTERFACE, "RSSI"),
                battery: property(interfaces, BATTERY_INTERFACE, "Percentage"),
                busy: busy.contains(id.as_str()),
            })
        })
        .collect();
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    (BluetoothState { adapter: Some(adapter), devices }, Some(path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::OwnedObjectPath;

    fn object(
        interface: &str,
        properties: Vec<(&str, Value<'_>)>,
    ) -> (String, HashMap<String, OwnedValue>) {
        let properties = properties
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.try_to_owned().unwrap()))
            .collect();
        (interface.to_owned(), properties)
    }

    fn path(path: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(path).unwrap()
    }

    #[test]
    fn summarizes_the_first_adapter_and_its_named_devices() {
        let device = |name: Option<&str>, paired: bool, connected: bool| {
            let mut properties = vec![
                ("Address", Value::from("00:11:22:33:44:55")),
                ("Paired", Value::from(paired)),
                ("Connected", Value::from(connected)),
            ];
            if let Some(name) = name {
                properties.push(("Name", Value::from(name)));
                properties.push(("Alias", Value::from(name)));
            }
            HashMap::from([object(DEVICE_INTERFACE, properties)])
        };
        let mut headset = device(Some("Headset"), true, true);
        headset.extend([object(BATTERY_INTERFACE, vec![("Percentage", Value::from(80u8))])]);
        let objects: ManagedObjects = HashMap::from([
            (
                path("/org/bluez/hci0"),
                HashMap::from([object(
                    ADAPTER_INTERFACE,
                    vec![("Alias", Value::from("laptop")), ("Powered", Value::from(true))],
                )]),
            ),
            (path("/org/bluez/hci1"), HashMap::from([object(ADAPTER_INTERFACE, vec![])])),
            (path("/org/bluez/hci0/dev_1"), device(Some("mouse"), false, false)),
            (path("/org/bluez/hci0/dev_2"), headset),
            (path("/org/bluez/hci0/dev_3"), device(None, false, false)),
            (path("/org/bluez/hci0/dev_4"), device(None, true, false)),
            (path("/org/bluez/hci1/dev_5"), device(Some("Other"), true, true)),
        ]);
        let busy = HashSet::from(["/org/bluez/hci0/dev_1".to_owned()]);
        let (state, adapter) = summarize(&objects, &busy);
        assert_eq!(adapter.as_deref(), Some("/org/bluez/hci0"));
        let adapter = state.adapter.unwrap();
        assert_eq!(
            (adapter.name.as_str(), adapter.powered, adapter.discovering),
            ("laptop", true, false)
        );
        let names: Vec<&str> = state.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Headset", "00:11:22:33:44:55", "mouse"]);
        assert_eq!(state.devices[0].battery, Some(80));
        assert!(state.devices[2].busy && !state.devices[0].busy);

        assert_eq!(summarize(&ManagedObjects::new(), &busy), (BluetoothState::default(), None));
    }
}
