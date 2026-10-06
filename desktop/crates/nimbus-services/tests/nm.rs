// SPDX-License-Identifier: MIT

//! The NetworkManager client against a fake NetworkManager on a private `dbus-daemon`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nimbus_services::BusAddress;
use nimbus_services::nm::{Client, Command, Event, Link, NetworkState, Security};
use nimbus_test_support::PrivateBus;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, ObjectServer, interface};

const WAIT: Duration = Duration::from_secs(10);
const NM: &str = "/org/freedesktop/NetworkManager";
const WIFI: &str = "/org/freedesktop/NetworkManager/Devices/1";
const ETHERNET: &str = "/org/freedesktop/NetworkManager/Devices/2";
const HOME: &str = "/org/freedesktop/NetworkManager/AccessPoint/1";
const CAFE: &str = "/org/freedesktop/NetworkManager/AccessPoint/2";
const HOME_FAR: &str = "/org/freedesktop/NetworkManager/AccessPoint/3";
const WIRED_ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/1";
const WIRED_IP4: &str = "/org/freedesktop/NetworkManager/IP4Config/1";

type Settings = HashMap<String, HashMap<String, OwnedValue>>;

fn path(path: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(path).unwrap()
}

fn owned(value: Value<'_>) -> OwnedValue {
    value.try_to_owned().unwrap()
}

/// What the fake daemon knows; every object reads its properties from it.
struct World {
    wireless_enabled: bool,
    wifi_state: u32,
    wifi_active: String,
    active_access_point: String,
    connections: BTreeMap<u32, Settings>,
    next_id: u32,
    /// The reason the next activation fails with, if it should.
    fail_with: Option<u32>,
    deleted: Vec<String>,
    scans: u32,
}

type Shared = Arc<Mutex<World>>;

fn connection_path(id: u32) -> String {
    format!("{NM}/Settings/{id}")
}

/// Tells the client that something changed; it reads everything again.
async fn changed(conn: &Connection, object: &str) {
    let properties = HashMap::<&str, Value<'_>>::new();
    conn.emit_signal(
        None::<()>,
        object,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &("org.freedesktop.NetworkManager.Device", properties, Vec::<&str>::new()),
    )
    .await
    .unwrap();
}

struct FakeNm(Shared);

#[interface(name = "org.freedesktop.NetworkManager")]
impl FakeNm {
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.0.lock().unwrap().wireless_enabled
    }
    #[zbus(property)]
    fn set_wireless_enabled(&mut self, enabled: bool) {
        let mut world = self.0.lock().unwrap();
        world.wireless_enabled = enabled;
        world.wifi_state = if enabled { 30 } else { 20 };
    }
    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![path(WIFI), path(ETHERNET)]
    }

    async fn activate_connection(
        &self,
        connection: OwnedObjectPath,
        _device: OwnedObjectPath,
        specific_object: OwnedObjectPath,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let id: u32 = connection.rsplit('/').next().unwrap().parse().unwrap();
        Ok(self.activate(id, specific_object, conn, server).await)
    }

    async fn add_and_activate_connection(
        &self,
        settings: Settings,
        _device: OwnedObjectPath,
        specific_object: OwnedObjectPath,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let id = {
            let mut world = self.0.lock().unwrap();
            world.next_id += 1;
            let id = world.next_id;
            world.connections.insert(id, settings);
            id
        };
        server.at(connection_path(id), FakeConnection(self.0.clone(), id)).await.unwrap();
        let active = self.activate(id, specific_object, conn, server).await;
        Ok((path(&connection_path(id)), active))
    }
}

impl FakeNm {
    /// Starts activating connection `id`, which succeeds or fails a moment later.
    async fn activate(
        &self,
        id: u32,
        access_point: OwnedObjectPath,
        conn: &Connection,
        server: &ObjectServer,
    ) -> OwnedObjectPath {
        let active = format!("{NM}/ActiveConnection/{}", id + 10);
        let name = {
            let world = self.0.lock().unwrap();
            let settings = &world.connections[&id];
            String::try_from(settings["connection"]["id"].try_clone().unwrap()).unwrap()
        };
        server.at(active.as_str(), FakeActive(name)).await.unwrap();
        let fail_with = self.0.lock().unwrap().fail_with.take();
        if fail_with.is_none() {
            let mut world = self.0.lock().unwrap();
            world.wifi_state = 100;
            world.wifi_active = active.clone();
            world.active_access_point = access_point.to_string();
        }
        let conn = conn.clone();
        let signal_path = active.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let (state, reason) = match fail_with {
                Some(reason) => (4u32, reason),
                None => (2u32, 1u32),
            };
            changed(&conn, WIFI).await;
            conn.emit_signal(
                None::<()>,
                signal_path.as_str(),
                "org.freedesktop.NetworkManager.Connection.Active",
                "StateChanged",
                &(state, reason),
            )
            .await
            .unwrap();
        });
        path(&active)
    }
}

struct FakeSettings(Shared);

#[interface(name = "org.freedesktop.NetworkManager.Settings")]
impl FakeSettings {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        self.0.lock().unwrap().connections.keys().map(|id| path(&connection_path(*id))).collect()
    }
}

struct FakeConnection(Shared, u32);

#[interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl FakeConnection {
    fn get_settings(&self) -> Settings {
        let world = self.0.lock().unwrap();
        let mut settings: Settings = world.connections[&self.1]
            .iter()
            .map(|(section, values)| {
                let values = values.iter().map(|(k, v)| (k.clone(), v.try_clone().unwrap()));
                (section.clone(), values.collect())
            })
            .collect();
        // NetworkManager leaves secrets out.
        if let Some(security) = settings.get_mut("802-11-wireless-security") {
            security.remove("psk");
        }
        settings
    }

    fn update(&self, settings: Settings) {
        self.0.lock().unwrap().connections.insert(self.1, settings);
    }

    async fn delete(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
    ) {
        {
            let mut world = self.0.lock().unwrap();
            world.connections.remove(&self.1);
            world.deleted.push(connection_path(self.1));
        }
        let path = connection_path(self.1);
        server.remove::<FakeConnection, _>(path.as_str()).await.unwrap();
        changed(conn, &format!("{NM}/Settings")).await;
    }
}

struct FakeDevice {
    world: Shared,
    wifi: bool,
}

#[interface(name = "org.freedesktop.NetworkManager.Device")]
impl FakeDevice {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        if self.wifi { 2 } else { 1 }
    }
    #[zbus(property)]
    fn interface(&self) -> String {
        if self.wifi { "wlan0" } else { "eth0" }.into()
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        if self.wifi { self.world.lock().unwrap().wifi_state } else { 100 }
    }
    #[zbus(property)]
    fn hw_address(&self) -> String {
        if self.wifi { "AA:BB:CC:00:00:01" } else { "AA:BB:CC:00:00:02" }.into()
    }
    #[zbus(property)]
    fn active_connection(&self) -> OwnedObjectPath {
        if self.wifi { path(&self.world.lock().unwrap().wifi_active) } else { path(WIRED_ACTIVE) }
    }
    #[zbus(property)]
    fn ip4_config(&self) -> OwnedObjectPath {
        path(if self.wifi { "/" } else { WIRED_IP4 })
    }
    #[zbus(property)]
    fn ip6_config(&self) -> OwnedObjectPath {
        path("/")
    }

    async fn disconnect(&self, #[zbus(connection)] conn: &Connection) {
        {
            let mut world = self.world.lock().unwrap();
            world.wifi_state = 30;
            world.wifi_active = "/".into();
            world.active_access_point = "/".into();
        }
        changed(conn, WIFI).await;
    }
}

struct FakeWireless(Shared);

#[interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl FakeWireless {
    #[zbus(property)]
    fn access_points(&self) -> Vec<OwnedObjectPath> {
        if self.0.lock().unwrap().wireless_enabled {
            vec![path(HOME), path(CAFE), path(HOME_FAR)]
        } else {
            Vec::new()
        }
    }
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        path(&self.0.lock().unwrap().active_access_point)
    }
    #[zbus(property)]
    fn bitrate(&self) -> u32 {
        866_000
    }

    fn request_scan(&self, _options: HashMap<String, OwnedValue>) {
        self.0.lock().unwrap().scans += 1;
    }
}

struct FakeWired;

#[interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
impl FakeWired {
    #[zbus(property)]
    fn carrier(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn speed(&self) -> u32 {
        1000
    }
}

struct FakeAccessPoint {
    ssid: &'static str,
    strength: u8,
    rsn_flags: u32,
}

#[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl FakeAccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        self.ssid.as_bytes().to_vec()
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        self.strength
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        u32::from(self.rsn_flags != 0)
    }
    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.rsn_flags
    }
    #[zbus(property)]
    fn frequency(&self) -> u32 {
        if self.strength > 60 { 5180 } else { 2437 }
    }
}

struct FakeActive(String);

#[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl FakeActive {
    #[zbus(property)]
    fn id(&self) -> String {
        self.0.clone()
    }
}

struct FakeIp4;

#[interface(name = "org.freedesktop.NetworkManager.IP4Config")]
impl FakeIp4 {
    #[zbus(property)]
    fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
        vec![HashMap::from([
            ("address".to_owned(), owned(Value::from("192.168.1.20"))),
            ("prefix".to_owned(), owned(Value::from(24u32))),
        ])]
    }
    #[zbus(property)]
    fn gateway(&self) -> String {
        "192.168.1.1".into()
    }
    #[zbus(property)]
    fn nameserver_data(&self) -> Vec<HashMap<String, OwnedValue>> {
        vec![HashMap::from([("address".to_owned(), owned(Value::from("192.168.1.1")))])]
    }
}

/// Starts the fake daemon on `bus`, with Wi-Fi on and disconnected, and a wired connection.
async fn fake_network_manager(bus: &PrivateBus) -> (Connection, Shared) {
    let wired = Settings::from([(
        "connection".to_owned(),
        HashMap::from([
            ("id".to_owned(), owned(Value::from("Wired connection 1"))),
            ("type".to_owned(), owned(Value::from("802-3-ethernet"))),
        ]),
    )]);
    let world = Arc::new(Mutex::new(World {
        wireless_enabled: true,
        wifi_state: 30,
        wifi_active: "/".into(),
        active_access_point: "/".into(),
        connections: BTreeMap::from([(1, wired)]),
        next_id: 1,
        fail_with: None,
        deleted: Vec::new(),
        scans: 0,
    }));
    let daemon = bus.connect().await;
    let server = daemon.object_server();
    server.at(NM, FakeNm(world.clone())).await.unwrap();
    server.at(format!("{NM}/Settings"), FakeSettings(world.clone())).await.unwrap();
    server.at(connection_path(1), FakeConnection(world.clone(), 1)).await.unwrap();
    server.at(WIFI, FakeDevice { world: world.clone(), wifi: true }).await.unwrap();
    server.at(WIFI, FakeWireless(world.clone())).await.unwrap();
    server.at(ETHERNET, FakeDevice { world: world.clone(), wifi: false }).await.unwrap();
    server.at(ETHERNET, FakeWired).await.unwrap();
    let psk = 0x188;
    server.at(HOME, FakeAccessPoint { ssid: "home", strength: 70, rsn_flags: psk }).await.unwrap();
    server.at(CAFE, FakeAccessPoint { ssid: "cafe", strength: 50, rsn_flags: 0 }).await.unwrap();
    server
        .at(HOME_FAR, FakeAccessPoint { ssid: "home", strength: 30, rsn_flags: psk })
        .await
        .unwrap();
    server.at(WIRED_ACTIVE, FakeActive("Wired connection 1".into())).await.unwrap();
    server.at(WIRED_IP4, FakeIp4).await.unwrap();
    daemon.request_name("org.freedesktop.NetworkManager").await.unwrap();
    (daemon, world)
}

fn spawn(address: BusAddress) -> (Client, UnboundedReceiver<Event>) {
    let (sender, receiver) = unbounded_channel();
    let client = Client::spawn(address, move |event| {
        let _ = sender.send(event);
    });
    (client, receiver)
}

async fn next_event(events: &mut UnboundedReceiver<Event>) -> Event {
    timeout(WAIT, events.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("the client stopped")
}

async fn state_where(
    events: &mut UnboundedReceiver<Event>,
    predicate: impl Fn(&NetworkState) -> bool,
) -> NetworkState {
    loop {
        if let Event::State(state) = next_event(events).await
            && predicate(&state)
        {
            return *state;
        }
    }
}

async fn next_non_state(events: &mut UnboundedReceiver<Event>) -> Event {
    loop {
        match next_event(events).await {
            Event::State(_) => {}
            event => return event,
        }
    }
}

#[tokio::test]
async fn lists_networks_and_wired_details() {
    let Some(bus) = PrivateBus::start() else { return };
    let (_daemon, world) = fake_network_manager(&bus).await;
    let (client, mut events) = spawn(BusAddress::Address(bus.address.clone()));

    let state = state_where(&mut events, |state| state.running).await;
    assert!(state.wifi_enabled && state.wifi_hardware_enabled);
    let wifi = state.wifi.as_ref().expect("a Wi-Fi device");
    assert_eq!((wifi.interface.as_str(), wifi.link), ("wlan0", Link::Disconnected));
    assert_eq!(wifi.speed_mbps, 866);
    let names: Vec<&str> = state.networks.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, ["home", "cafe"]);
    assert_eq!(state.networks[0].security, Security::Psk);
    assert_eq!((state.networks[0].strength, state.networks[0].frequency_mhz), (70, 5180));
    assert_eq!(state.networks[1].security, Security::Open);
    assert!(!state.networks[0].known && !state.networks[0].active);

    let [wired] = state.wired.as_slice() else { panic!("one wired device: {:?}", state.wired) };
    assert_eq!(
        (wired.interface.as_str(), wired.link, wired.carrier),
        ("eth0", Link::Connected, true)
    );
    assert_eq!((wired.speed_mbps, wired.hw_address.as_str()), (1000, "AA:BB:CC:00:00:02"));
    let details = wired.connection.as_ref().expect("the wired connection");
    assert_eq!(details.name, "Wired connection 1");
    assert_eq!(details.ipv4, ["192.168.1.20/24"]);
    assert_eq!(details.gateway.as_deref(), Some("192.168.1.1"));
    assert_eq!(details.dns, ["192.168.1.1"]);

    client.send(Command::Scan);
    client.send(Command::SetWifiEnabled(false));
    let state = state_where(&mut events, |state| !state.wifi_enabled).await;
    assert!(state.networks.is_empty());
    assert_eq!(state.wifi.map(|wifi| wifi.link), Some(Link::Unavailable));
    assert_eq!(world.lock().unwrap().scans, 1);
}

#[tokio::test]
async fn connects_with_a_password_and_forgets() {
    let Some(bus) = PrivateBus::start() else { return };
    let (_daemon, world) = fake_network_manager(&bus).await;
    let (client, mut events) = spawn(BusAddress::Address(bus.address.clone()));
    state_where(&mut events, |state| state.running).await;

    client.send(Command::Connect { ssid: b"home".to_vec(), password: None });
    assert_eq!(
        next_non_state(&mut events).await,
        Event::ConnectFailed { ssid: b"home".to_vec(), needs_password: true },
        "a secured network needs a password before NetworkManager is asked"
    );

    world.lock().unwrap().fail_with = Some(9);
    client.send(Command::Connect { ssid: b"home".to_vec(), password: Some("wrong".into()) });
    assert_eq!(
        next_non_state(&mut events).await,
        Event::ConnectFailed { ssid: b"home".to_vec(), needs_password: true }
    );
    assert_eq!(
        world.lock().unwrap().deleted,
        [connection_path(2)],
        "the failed connection is removed"
    );

    client.send(Command::Connect { ssid: b"home".to_vec(), password: Some("hunter22".into()) });
    let state =
        state_where(&mut events, |state| state.networks.first().is_some_and(|n| n.active)).await;
    assert!(state.networks[0].known);
    let wifi = state.wifi.unwrap();
    assert_eq!(wifi.link, Link::Connected);
    assert_eq!(wifi.connection.map(|c| c.name), Some("home".into()));
    {
        let world = world.lock().unwrap();
        let security = &world.connections[&3]["802-11-wireless-security"];
        assert_eq!(<&str>::try_from(&*security["key-mgmt"]).unwrap(), "wpa-psk");
        assert_eq!(<&str>::try_from(&*security["psk"]).unwrap(), "hunter22");
        assert_eq!(
            Vec::<u8>::try_from(
                world.connections[&3]["802-11-wireless"]["ssid"].try_clone().unwrap()
            )
            .unwrap(),
            b"home"
        );
    }

    // A new password for a saved network updates it.
    client.send(Command::Connect { ssid: b"home".to_vec(), password: Some("changed!".into()) });
    let psk = || {
        let world = world.lock().unwrap();
        <&str>::try_from(&*world.connections[&3]["802-11-wireless-security"]["psk"])
            .map(str::to_owned)
            .ok()
    };
    timeout(WAIT, async {
        while psk().as_deref() != Some("changed!") {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the saved password changes");

    client.send(Command::Disconnect);
    state_where(&mut events, |state| state.wifi.as_ref().is_some_and(|w| w.connection.is_none()))
        .await;

    client.send(Command::Forget { ssid: b"home".to_vec() });
    state_where(&mut events, |state| {
        state.networks.first().is_some_and(|n| n.name == "home" && !n.known)
    })
    .await;
    assert!(world.lock().unwrap().deleted.contains(&connection_path(3)));
}

#[tokio::test]
async fn without_network_manager_commands_fail() {
    let Some(bus) = PrivateBus::start() else { return };
    let (client, mut events) = spawn(BusAddress::Address(bus.address.clone()));
    assert_eq!(next_event(&mut events).await, Event::State(Box::default()));
    client.send(Command::Scan);
    assert_eq!(
        next_non_state(&mut events).await,
        Event::Failed("NetworkManager isn't running".into())
    );

    let (client, mut events) = spawn(BusAddress::Disabled);
    assert_eq!(next_event(&mut events).await, Event::State(Box::default()));
    client.send(Command::SetWifiEnabled(true));
    assert!(matches!(next_non_state(&mut events).await, Event::Failed(_)));
}
