// SPDX-License-Identifier: MIT

//! Connectivity from NetworkManager on the system bus.

use tokio::sync::mpsc::UnboundedReceiver;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::{Connection, Message};

use crate::bus::{self, BusService, Props, Wake};
use crate::hub::{Update, Updates};
use crate::{ConnectionKind, Network};

pub(crate) const PATH: &str = "/org/freedesktop/NetworkManager";
pub(crate) const INTERFACE: &str = "org.freedesktop.NetworkManager";
pub(crate) const ACTIVE_INTERFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
pub(crate) const WIRELESS_INTERFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
pub(crate) const ACCESS_POINT_INTERFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";

// NMConnectivityState values.
const CONNECTIVITY_UNKNOWN: u32 = 0;
const CONNECTIVITY_FULL: u32 = 4;

#[derive(Debug)]
pub(crate) enum NetworkCommand {
    SetWifiEnabled(bool),
}

/// Maps an NM connection type to the panel's coarse kind: anything that isn't Wi-Fi shows as wired.
pub(crate) fn classify(connection_type: &str) -> ConnectionKind {
    match connection_type {
        "" | "loopback" => ConnectionKind::None,
        "802-11-wireless" => ConnectionKind::Wifi,
        _ => ConnectionKind::Ethernet,
    }
}

/// Whether the internet is reachable; with connectivity checking off, NM reports unknown.
pub(crate) fn is_available(connectivity: u32, has_primary: bool) -> bool {
    match connectivity {
        CONNECTIVITY_FULL => true,
        CONNECTIVITY_UNKNOWN => has_primary,
        _ => false,
    }
}

/// Converts an SSID, which is raw bytes, for display.
pub(crate) fn ssid_to_string(ssid: &[u8]) -> Option<String> {
    let ssid = String::from_utf8_lossy(ssid).trim_matches(char::from(0)).to_owned();
    (!ssid.is_empty()).then_some(ssid)
}

pub(crate) fn real_path(path: Option<OwnedObjectPath>) -> Option<OwnedObjectPath> {
    path.filter(|path| path.as_str() != "/")
}

/// Whether `message` comes from one of the `watched` object paths.
pub(crate) fn is_watched(message: &Message, watched: &[OwnedObjectPath]) -> bool {
    message.header().path().is_some_and(|path| watched.iter().any(|w| w.as_str() == path.as_str()))
}

pub(crate) struct NetworkManager {
    updates: Updates,
}

impl NetworkManager {
    pub(crate) fn new(updates: Updates) -> Self {
        Self { updates }
    }

    /// Reads the current state and replaces `watched` with the object paths it was read from.
    async fn snapshot(
        conn: &Connection,
        owner: &str,
        watched: &mut Vec<OwnedObjectPath>,
    ) -> zbus::Result<Network> {
        watched.clear();
        watched.push(OwnedObjectPath::try_from(PATH)?);
        let mut nm = bus::get_all(conn, owner, PATH, INTERFACE).await?;
        let wifi_enabled = nm.take::<bool>("WirelessEnabled").unwrap_or(false);
        let connectivity = nm.take::<u32>("Connectivity").unwrap_or(CONNECTIVITY_UNKNOWN);
        let primary = real_path(nm.take("PrimaryConnection"));
        let mut network = Network {
            wifi_enabled,
            available: is_available(connectivity, primary.is_some()),
            ..Network::default()
        };
        let Some(primary) = primary else {
            return Ok(network);
        };
        watched.push(primary.clone());
        // The primary connection can vanish between the two calls; that's not an error.
        let mut active =
            bus::get_all(conn, owner, primary.as_str(), ACTIVE_INTERFACE).await.unwrap_or_default();
        let connection_type = nm
            .take::<String>("PrimaryConnectionType")
            .filter(|t| !t.is_empty())
            .or_else(|| active.take("Type"))
            .unwrap_or_default();
        network.kind = classify(&connection_type);
        if network.kind == ConnectionKind::Wifi {
            let name = active.take::<String>("Id");
            if let Some(mut ap) = Self::access_point(conn, owner, &mut active, watched).await {
                network.ssid = ap.take::<Vec<u8>>("Ssid").as_deref().and_then(ssid_to_string);
                network.strength =
                    f32::from(ap.take::<u8>("Strength").unwrap_or(0).min(100)) / 100.0;
            }
            network.ssid = network.ssid.or(name);
        }
        Ok(network)
    }

    async fn access_point(
        conn: &Connection,
        owner: &str,
        active: &mut Props,
        watched: &mut Vec<OwnedObjectPath>,
    ) -> Option<Props> {
        let mut path = real_path(active.take("SpecificObject"));
        if path.is_none() {
            let device = active.take::<Vec<OwnedObjectPath>>("Devices")?.into_iter().next()?;
            watched.push(device.clone());
            let value = bus::get_property(
                conn,
                owner,
                device.as_str(),
                WIRELESS_INTERFACE,
                "ActiveAccessPoint",
            )
            .await
            .ok()?;
            path = real_path(OwnedObjectPath::try_from(value).ok());
        }
        let path = path?;
        watched.push(path.clone());
        bus::get_all(conn, owner, path.as_str(), ACCESS_POINT_INTERFACE).await.ok()
    }
}

impl BusService for NetworkManager {
    type Command = NetworkCommand;
    const NAME: &'static str = "org.freedesktop.NetworkManager";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<NetworkCommand>,
    ) -> zbus::Result<()> {
        let mut signals = bus::signals(conn, [bus::properties_changed_rule(owner, PATH)?]).await?;
        let mut watched = Vec::new();
        loop {
            let network = Self::snapshot(conn, owner.as_str(), &mut watched).await?;
            self.updates.send(Update::Network(network));
            let relevant = |message: &Message| is_watched(message, &watched);
            match bus::next_wake_where(&mut signals, commands, relevant).await? {
                Some(Wake::Signal) => {}
                Some(Wake::Command(NetworkCommand::SetWifiEnabled(enabled))) => {
                    let value = Value::Bool(enabled);
                    if let Err(err) = bus::set_property(
                        conn,
                        owner.as_str(),
                        PATH,
                        INTERFACE,
                        "WirelessEnabled",
                        value,
                    )
                    .await
                    {
                        tracing::info!("NetworkManager refused to switch Wi-Fi: {err}");
                    }
                }
                None => return Ok(()),
            }
        }
    }

    fn unavailable(&mut self) {
        self.updates.send(Update::Network(Network::default()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_connection_types() {
        assert_eq!(classify("802-11-wireless"), ConnectionKind::Wifi);
        assert_eq!(classify("802-3-ethernet"), ConnectionKind::Ethernet);
        assert_eq!(classify("vpn"), ConnectionKind::Ethernet);
        assert_eq!(classify(""), ConnectionKind::None);
        assert_eq!(classify("loopback"), ConnectionKind::None);
    }

    #[test]
    fn availability() {
        assert!(is_available(4, true));
        assert!(is_available(0, true));
        assert!(!is_available(0, false));
        assert!(!is_available(2, true));
        assert!(!is_available(1, true));
    }

    #[test]
    fn watches_only_snapshot_paths() {
        let changed = |path: &str| {
            Message::signal(path, "org.freedesktop.DBus.Properties", "PropertiesChanged")
                .unwrap()
                .build(&())
                .unwrap()
        };
        let watched = [
            OwnedObjectPath::try_from(PATH).unwrap(),
            OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/AccessPoint/7").unwrap(),
        ];
        assert!(is_watched(&changed(PATH), &watched));
        assert!(is_watched(&changed("/org/freedesktop/NetworkManager/AccessPoint/7"), &watched));
        assert!(!is_watched(&changed("/org/freedesktop/NetworkManager/AccessPoint/70"), &watched));
        assert!(!is_watched(&changed("/org/freedesktop/NetworkManager/Devices/2"), &watched));
    }

    #[test]
    fn ssids() {
        assert_eq!(ssid_to_string(b"home"), Some("home".into()));
        assert_eq!(ssid_to_string(b"caf\xc3\xa9\0"), Some("caf\u{e9}".into()));
        assert_eq!(ssid_to_string(b""), None);
        assert_eq!(ssid_to_string(b"\xff"), Some("\u{fffd}".into()));
    }
}
