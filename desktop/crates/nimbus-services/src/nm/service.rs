// SPDX-License-Identifier: MIT

//! The NetworkManager client's D-Bus side: snapshots of devices, networks, and saved connections, and the commands.

use std::collections::{HashMap, HashSet};

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Message};

use super::parse::{self, AccessPoint, Attempt, Settings};
use super::{Command, ConnectionDetails, Device, Event, NetworkState, Security};
use crate::bus::{self, BusService, Props, Wake};
use crate::network::{
    ACCESS_POINT_INTERFACE, ACTIVE_INTERFACE, INTERFACE, PATH, WIRELESS_INTERFACE, real_path,
    ssid_to_string,
};

const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS_INTERFACE: &str = "org.freedesktop.NetworkManager.Settings";
const CONNECTION_INTERFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const DEVICE_INTERFACE: &str = "org.freedesktop.NetworkManager.Device";
const WIRED_INTERFACE: &str = "org.freedesktop.NetworkManager.Device.Wired";
const IP4_INTERFACE: &str = "org.freedesktop.NetworkManager.IP4Config";
const IP6_INTERFACE: &str = "org.freedesktop.NetworkManager.IP6Config";

// NMDeviceType values.
const DEVICE_ETHERNET: u32 = 1;
const DEVICE_WIFI: u32 = 2;

type AddressData = Vec<HashMap<String, OwnedValue>>;

/// A connection attempt that NetworkManager accepted, until it succeeds or fails.
struct Attempting {
    ssid: Vec<u8>,
    /// The connection added for it, which is deleted again if it fails.
    added: Option<OwnedObjectPath>,
}

/// The objects behind the last snapshot, which commands act on.
#[derive(Default)]
struct Objects {
    wifi: Option<OwnedObjectPath>,
    /// The strongest access point of each SSID.
    access_points: HashMap<Vec<u8>, AccessPoint>,
    saved: HashMap<Vec<u8>, Vec<OwnedObjectPath>>,
}

pub(crate) struct NetworkSettings {
    on_event: Box<dyn Fn(Event) + Send + Sync>,
    /// By active connection.
    attempts: HashMap<OwnedObjectPath, Attempting>,
    last: Option<NetworkState>,
}

impl NetworkSettings {
    pub(crate) fn new(on_event: Box<dyn Fn(Event) + Send + Sync>) -> Self {
        Self { on_event, attempts: HashMap::new(), last: None }
    }

    fn publish(&mut self, state: NetworkState) {
        if self.last.as_ref() != Some(&state) {
            self.last = Some(state.clone());
            (self.on_event)(Event::State(Box::new(state)));
        }
    }

    async fn handle(
        &mut self,
        conn: &Connection,
        owner: &str,
        objects: &Objects,
        command: Command,
    ) {
        let result = match command {
            Command::SetWifiEnabled(enabled) => {
                let value = Value::Bool(enabled);
                bus::set_property(conn, owner, PATH, INTERFACE, "WirelessEnabled", value).await
            }
            Command::Scan => {
                if let Some(device) = &objects.wifi {
                    let options = (HashMap::<&str, Value<'_>>::new(),);
                    let scan =
                        bus::call(conn, owner, device, WIRELESS_INTERFACE, "RequestScan", &options);
                    // NetworkManager refuses scans right after another one, which isn't worth reporting.
                    if let Err(err) = scan.await {
                        tracing::debug!("NetworkManager didn't scan: {err}");
                    }
                }
                Ok(())
            }
            Command::Connect { ssid, password } => {
                self.connect(conn, owner, objects, ssid, password.as_deref()).await
            }
            Command::Disconnect => match &objects.wifi {
                Some(device) => bus::call(conn, owner, device, DEVICE_INTERFACE, "Disconnect", &())
                    .await
                    .map(drop),
                None => Ok(()),
            },
            Command::Forget { ssid } => {
                let mut result = Ok(());
                for connection in objects.saved.get(&ssid).into_iter().flatten() {
                    let deleted =
                        bus::call(conn, owner, connection, CONNECTION_INTERFACE, "Delete", &())
                            .await;
                    result = result.and(deleted.map(drop));
                }
                result
            }
        };
        if let Err(err) = result {
            tracing::info!("NetworkManager refused a request: {err}");
            (self.on_event)(Event::Failed(bus::reason(&err)));
        }
    }

    async fn connect(
        &mut self,
        conn: &Connection,
        owner: &str,
        objects: &Objects,
        ssid: Vec<u8>,
        password: Option<&str>,
    ) -> zbus::Result<()> {
        let device = objects.wifi.as_ref().ok_or_else(|| failure("There's no Wi-Fi device"))?;
        let access_point = objects.access_points.get(&ssid);
        let specific = access_point
            .map(|ap| ap.path.clone())
            .unwrap_or_else(|| OwnedObjectPath::try_from("/").expect("the root path"));
        let saved = objects.saved.get(&ssid).and_then(|saved| saved.first());
        let (active, added) = match saved {
            Some(connection) => {
                if let Some(password) = password {
                    let security = access_point.map_or(Security::Psk, |ap| ap.security);
                    let mut settings: Settings = bus::call_for(
                        conn,
                        owner,
                        connection,
                        CONNECTION_INTERFACE,
                        "GetSettings",
                        &(),
                    )
                    .await?;
                    parse::set_password(&mut settings, security, password);
                    bus::call(
                        conn,
                        owner,
                        connection,
                        CONNECTION_INTERFACE,
                        "Update",
                        &(settings,),
                    )
                    .await?;
                }
                let body = (connection, device, &specific);
                let active: OwnedObjectPath =
                    bus::call_for(conn, owner, PATH, INTERFACE, "ActivateConnection", &body)
                        .await?;
                (active, None)
            }
            None => {
                let ap = access_point.ok_or_else(|| failure("The network is out of range"))?;
                if !ap.security.can_connect() {
                    return Err(failure(
                        "Enterprise networks need settings that Nimbus can't set yet",
                    ));
                }
                if ap.security.needs_password() && password.is_none() {
                    (self.on_event)(Event::ConnectFailed { ssid, needs_password: true });
                    return Ok(());
                }
                let name = ssid_to_string(&ssid).unwrap_or_default();
                let settings = parse::new_connection(&ssid, &name, ap.security, password);
                let body = (settings, device, &specific);
                let (added, active): (OwnedObjectPath, OwnedObjectPath) =
                    bus::call_for(conn, owner, PATH, INTERFACE, "AddAndActivateConnection", &body)
                        .await?;
                (active, Some(added))
            }
        };
        self.attempts.insert(active, Attempting { ssid, added });
        Ok(())
    }

    /// Follows an active connection's `StateChanged` signal, for the connection attempts.
    async fn activation_changed(&mut self, conn: &Connection, owner: &str, message: &Message) {
        let header = message.header();
        let Some(path) = header.path() else { return };
        let Ok((state, reason)) = message.body().deserialize::<(u32, u32)>() else { return };
        let path = OwnedObjectPath::from(path.to_owned());
        match parse::attempt(state, reason) {
            Attempt::Pending => {}
            Attempt::Finished => {
                self.attempts.remove(&path);
            }
            Attempt::Failed { needs_password } => {
                let Some(attempt) = self.attempts.remove(&path) else { return };
                if let Some(added) = attempt.added {
                    let deleted =
                        bus::call(conn, owner, &added, CONNECTION_INTERFACE, "Delete", &()).await;
                    if let Err(err) = deleted {
                        tracing::debug!("Can't delete the failed connection: {err}");
                    }
                }
                (self.on_event)(Event::ConnectFailed { ssid: attempt.ssid, needs_password });
            }
        }
    }
}

fn failure(message: &str) -> zbus::Error {
    zbus::Error::Failure(message.to_owned())
}

impl BusService for NetworkSettings {
    type Command = Command;
    const NAME: &'static str = "org.freedesktop.NetworkManager";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<Command>,
    ) -> zbus::Result<()> {
        let mut changes = bus::signals(conn, [bus::properties_changed_rule(owner, PATH)?]).await?;
        let state_changed = bus::signal_rule()
            .sender(owner.to_owned().into_inner())?
            .path_namespace(PATH)?
            .interface(ACTIVE_INTERFACE)?
            .member("StateChanged")?
            .build();
        let mut activations = bus::signals(conn, [state_changed]).await?;
        loop {
            let (state, objects) = snapshot(conn, owner.as_str()).await?;
            self.publish(state);
            tokio::select! {
                message = activations.next() => {
                    let message = message.ok_or_else(bus::stream_ended)?;
                    self.activation_changed(conn, owner.as_str(), &message).await;
                }
                wake = bus::next_wake(&mut changes, commands) => match wake? {
                    Some(Wake::Signal) => {}
                    Some(Wake::Command(command)) => {
                        self.handle(conn, owner.as_str(), &objects, command).await;
                    }
                    None => return Ok(()),
                },
            }
        }
    }

    fn unavailable(&mut self) {
        self.attempts.clear();
        self.publish(NetworkState::default());
    }

    fn command_unavailable(&mut self, command: Command) {
        tracing::debug!("NetworkManager isn't available; ignoring {command:?}");
        (self.on_event)(Event::Failed("NetworkManager isn't running".into()));
    }
}

async fn snapshot(conn: &Connection, owner: &str) -> zbus::Result<(NetworkState, Objects)> {
    let mut nm = bus::get_all(conn, owner, PATH, INTERFACE).await?;
    let mut state = NetworkState {
        running: true,
        wifi_enabled: nm.take("WirelessEnabled").unwrap_or(false),
        wifi_hardware_enabled: nm.take("WirelessHardwareEnabled").unwrap_or(true),
        ..NetworkState::default()
    };
    let mut objects = Objects { saved: saved_connections(conn, owner).await, ..Objects::default() };
    let known: HashSet<Vec<u8>> = objects.saved.keys().cloned().collect();
    for path in nm.take::<Vec<OwnedObjectPath>>("Devices").unwrap_or_default() {
        // Devices come and go while they're read.
        let Ok(mut props) = bus::get_all(conn, owner, &path, DEVICE_INTERFACE).await else {
            continue;
        };
        let kind = props.take::<u32>("DeviceType").unwrap_or_default();
        if !(kind == DEVICE_ETHERNET || kind == DEVICE_WIFI && objects.wifi.is_none()) {
            continue;
        }
        let mut device = Device {
            interface: props.take("Interface").unwrap_or_default(),
            link: parse::link(props.take("State").unwrap_or_default()),
            hw_address: props.take("HwAddress").unwrap_or_default(),
            ..Device::default()
        };
        device.connection = connection_details(conn, owner, &mut props).await;
        if kind == DEVICE_WIFI {
            let mut wireless =
                bus::get_all(conn, owner, &path, WIRELESS_INTERFACE).await.unwrap_or_default();
            device.speed_mbps = wireless.take::<u32>("Bitrate").unwrap_or_default() / 1000;
            fill_hw_address(&mut device, &mut wireless);
            let active = real_path(wireless.take("ActiveAccessPoint"));
            let paths = wireless.take::<Vec<OwnedObjectPath>>("AccessPoints").unwrap_or_default();
            let access_points = access_points(conn, owner, paths).await;
            state.networks = parse::networks(&access_points, active.as_ref(), &known);
            for ap in access_points {
                match objects.access_points.get(&ap.ssid) {
                    Some(strongest) if strongest.strength >= ap.strength => {}
                    _ => {
                        objects.access_points.insert(ap.ssid.clone(), ap);
                    }
                }
            }
            state.wifi = Some(device);
            objects.wifi = Some(path);
        } else {
            let mut wired =
                bus::get_all(conn, owner, &path, WIRED_INTERFACE).await.unwrap_or_default();
            device.carrier = wired.take("Carrier").unwrap_or_default();
            device.speed_mbps = wired.take("Speed").unwrap_or_default();
            fill_hw_address(&mut device, &mut wired);
            state.wired.push(device);
        }
    }
    Ok((state, objects))
}

/// Takes the hardware address from the device type's interface, where NetworkManager before 1.24 has it.
fn fill_hw_address(device: &mut Device, props: &mut Props) {
    if device.hw_address.is_empty() {
        device.hw_address = props.take("HwAddress").unwrap_or_default();
    }
}

async fn access_points(
    conn: &Connection,
    owner: &str,
    paths: Vec<OwnedObjectPath>,
) -> Vec<AccessPoint> {
    let mut access_points = Vec::with_capacity(paths.len());
    for path in paths {
        let Ok(mut ap) = bus::get_all(conn, owner, &path, ACCESS_POINT_INTERFACE).await else {
            continue;
        };
        access_points.push(AccessPoint {
            ssid: ap.take("Ssid").unwrap_or_default(),
            strength: ap.take::<u8>("Strength").unwrap_or_default().min(100),
            security: Security::classify(
                ap.take("Flags").unwrap_or_default(),
                ap.take("WpaFlags").unwrap_or_default(),
                ap.take("RsnFlags").unwrap_or_default(),
            ),
            frequency_mhz: ap.take("Frequency").unwrap_or_default(),
            path,
        });
    }
    access_points
}

/// The saved Wi-Fi connections by SSID.
async fn saved_connections(
    conn: &Connection,
    owner: &str,
) -> HashMap<Vec<u8>, Vec<OwnedObjectPath>> {
    let mut saved: HashMap<Vec<u8>, Vec<OwnedObjectPath>> = HashMap::new();
    let paths: Vec<OwnedObjectPath> =
        bus::call_for(conn, owner, SETTINGS_PATH, SETTINGS_INTERFACE, "ListConnections", &())
            .await
            .unwrap_or_default();
    for path in paths {
        let settings: zbus::Result<Settings> =
            bus::call_for(conn, owner, &path, CONNECTION_INTERFACE, "GetSettings", &()).await;
        if let Some(ssid) = settings.ok().as_ref().and_then(parse::saved_ssid) {
            saved.entry(ssid).or_default().push(path);
        }
    }
    saved
}

async fn connection_details(
    conn: &Connection,
    owner: &str,
    device: &mut Props,
) -> Option<ConnectionDetails> {
    let active = real_path(device.take("ActiveConnection"))?;
    let mut active = bus::get_all(conn, owner, &active, ACTIVE_INTERFACE).await.ok()?;
    let mut details = ConnectionDetails {
        name: active.take("Id").unwrap_or_default(),
        ..ConnectionDetails::default()
    };
    if let Some(path) = real_path(device.take("Ip4Config")) {
        let mut ip4 = bus::get_all(conn, owner, &path, IP4_INTERFACE).await.unwrap_or_default();
        details.ipv4 =
            parse::addresses(&ip4.take::<AddressData>("AddressData").unwrap_or_default());
        details.gateway = ip4.take::<String>("Gateway").filter(|gateway| !gateway.is_empty());
        details.dns = match ip4.take::<AddressData>("NameserverData") {
            Some(data) => parse::nameservers(&data),
            None => {
                parse::legacy_nameservers(&ip4.take::<Vec<u32>>("Nameservers").unwrap_or_default())
            }
        };
    }
    if let Some(path) = real_path(device.take("Ip6Config")) {
        let mut ip6 = bus::get_all(conn, owner, &path, IP6_INTERFACE).await.unwrap_or_default();
        details.ipv6 =
            parse::addresses(&ip6.take::<AddressData>("AddressData").unwrap_or_default());
        if details.gateway.is_none() {
            details.gateway = ip6.take::<String>("Gateway").filter(|gateway| !gateway.is_empty());
        }
    }
    Some(details)
}
