// SPDX-License-Identifier: MIT

//! Bluetooth adapter state from BlueZ on the system bus.

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedReceiver;
use zbus::Connection;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::Bluetooth;
use crate::bus::{self, BusService, Wake};
use crate::hub::{Update, Updates};

pub(crate) const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
pub(crate) const ADAPTER_INTERFACE: &str = "org.bluez.Adapter1";
pub(crate) const DEVICE_INTERFACE: &str = "org.bluez.Device1";

pub(crate) type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

#[derive(Debug)]
pub(crate) enum BluetoothCommand {
    SetPowered(bool),
}

fn bool_property(
    interfaces: &HashMap<String, HashMap<String, OwnedValue>>,
    interface: &str,
    name: &str,
) -> bool {
    interfaces
        .get(interface)
        .and_then(|properties| properties.get(name))
        .is_some_and(|value| matches!(**value, Value::Bool(true)))
}

/// Returns the state and the path of the first adapter, or `None` without an adapter.
pub(crate) fn summarize(objects: &ManagedObjects) -> Option<(Bluetooth, OwnedObjectPath)> {
    let adapter = objects
        .iter()
        .filter(|(_, interfaces)| interfaces.contains_key(ADAPTER_INTERFACE))
        .min_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()))?;
    let connected = objects
        .values()
        .filter(|interfaces| bool_property(interfaces, DEVICE_INTERFACE, "Connected"))
        .count();
    let bluetooth = Bluetooth {
        powered: bool_property(adapter.1, ADAPTER_INTERFACE, "Powered"),
        connected_devices: u32::try_from(connected).unwrap_or(u32::MAX),
    };
    Some((bluetooth, adapter.0.clone()))
}

pub(crate) struct Bluez {
    updates: Updates,
}

impl Bluez {
    pub(crate) fn new(updates: Updates) -> Self {
        Self { updates }
    }
}

impl BusService for Bluez {
    type Command = BluetoothCommand;
    const NAME: &'static str = "org.bluez";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<BluetoothCommand>,
    ) -> zbus::Result<()> {
        let objects_rule = bus::signal_rule()
            .sender(owner.to_owned().into_inner())?
            .path("/")?
            .interface(OBJECT_MANAGER)?
            .build();
        let rules = [bus::properties_changed_rule(owner, "/org/bluez")?, objects_rule];
        let mut signals = bus::signals(conn, rules).await?;
        loop {
            let objects: ManagedObjects =
                bus::call_for(conn, owner.as_str(), "/", OBJECT_MANAGER, "GetManagedObjects", &())
                    .await?;
            let summary = summarize(&objects);
            let adapter = summary.as_ref().map(|(_, path)| path.clone());
            self.updates.send(Update::Bluetooth(summary.map(|(bluetooth, _)| bluetooth)));
            match bus::next_wake(&mut signals, commands).await? {
                Some(Wake::Signal) => {}
                Some(Wake::Command(BluetoothCommand::SetPowered(powered))) => {
                    let Some(adapter) = adapter else {
                        tracing::debug!("No Bluetooth adapter to power");
                        continue;
                    };
                    let value = Value::Bool(powered);
                    let result = bus::set_property(
                        conn,
                        owner.as_str(),
                        adapter.as_str(),
                        ADAPTER_INTERFACE,
                        "Powered",
                        value,
                    )
                    .await;
                    if let Err(err) = result {
                        tracing::info!("BlueZ refused to switch the adapter: {err}");
                    }
                }
                None => return Ok(()),
            }
        }
    }

    fn unavailable(&mut self) {
        self.updates.send(Update::Bluetooth(None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(
        interface: &str,
        properties: &[(&str, bool)],
    ) -> HashMap<String, HashMap<String, OwnedValue>> {
        let properties = properties
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OwnedValue::from(*value)))
            .collect();
        HashMap::from([(interface.to_owned(), properties)])
    }

    fn path(path: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(path).unwrap()
    }

    #[test]
    fn summarizes_first_adapter_and_connected_devices() {
        let objects: ManagedObjects = HashMap::from([
            (path("/org/bluez/hci1"), object(ADAPTER_INTERFACE, &[("Powered", false)])),
            (path("/org/bluez/hci0"), object(ADAPTER_INTERFACE, &[("Powered", true)])),
            (path("/org/bluez/hci0/dev_A"), object(DEVICE_INTERFACE, &[("Connected", true)])),
            (path("/org/bluez/hci0/dev_B"), object(DEVICE_INTERFACE, &[("Connected", false)])),
            (path("/org/bluez/hci1/dev_C"), object(DEVICE_INTERFACE, &[("Connected", true)])),
            (path("/org/bluez"), object("org.bluez.AgentManager1", &[])),
        ]);
        let (bluetooth, adapter) = summarize(&objects).unwrap();
        assert_eq!(bluetooth, Bluetooth { powered: true, connected_devices: 2 });
        assert_eq!(adapter.as_str(), "/org/bluez/hci0");
    }

    #[test]
    fn no_adapter() {
        let objects: ManagedObjects =
            HashMap::from([(path("/org/bluez"), object("org.bluez.AgentManager1", &[]))]);
        assert!(summarize(&objects).is_none());
    }
}
