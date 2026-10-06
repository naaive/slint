// SPDX-License-Identifier: MIT

//! End-to-end tests against a private `dbus-daemon`, with fake system daemons where needed.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use nimbus_services::{
    BatteryWarning, Bluetooth, BusAddress, CloseReason, ConnectionKind, Network, ServiceCommand,
    ServiceEvent, Services, ServicesBuilder, ServicesConfig, SystemState, Urgency,
};
use nimbus_test_support::{FakeBattery, FakeLogind, FakeUPower, PrivateBus, WarningLevel};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, interface};

const WAIT: Duration = Duration::from_secs(10);

fn disabled() -> ServicesConfig {
    ServicesConfig {
        notifications: false,
        upower: false,
        network_manager: false,
        audio: false,
        backlight: false,
        mpris: false,
        bluetooth: false,
        logind: false,
        polkit: false,
        udisks: false,
    }
}

fn spawn(builder: ServicesBuilder) -> (Services, UnboundedReceiver<ServiceEvent>) {
    let (sender, receiver) = unbounded_channel();
    let services = builder.spawn(move |event| {
        let _ = sender.send(event);
    });
    (services, receiver)
}

async fn next_event(events: &mut UnboundedReceiver<ServiceEvent>) -> ServiceEvent {
    timeout(WAIT, events.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("services stopped")
}

/// Skips state events until one satisfies `predicate`, and returns it.
async fn state_where(
    events: &mut UnboundedReceiver<ServiceEvent>,
    predicate: impl Fn(&SystemState) -> bool,
) -> SystemState {
    loop {
        if let ServiceEvent::State(state) = next_event(events).await
            && predicate(&state)
        {
            return state;
        }
    }
}

/// Returns the next event that isn't a state change.
async fn next_non_state(events: &mut UnboundedReceiver<ServiceEvent>) -> ServiceEvent {
    loop {
        match next_event(events).await {
            ServiceEvent::State(_) => {}
            event => return event,
        }
    }
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    timeout(WAIT, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn wait_for_owner(conn: &Connection, name: &str) {
    let dbus = zbus::fdo::DBusProxy::new(conn).await.unwrap();
    let name = zbus::names::BusName::try_from(name).unwrap();
    timeout(WAIT, async {
        while !dbus.name_has_owner(name.clone()).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for the name owner");
}

#[tokio::test]
async fn without_services_state_and_local_commands_work() {
    let (services, mut events) = spawn(
        ServicesBuilder::new(disabled())
            .session_bus(BusAddress::Disabled)
            .system_bus(BusAddress::Disabled),
    );
    assert_eq!(next_event(&mut events).await, ServiceEvent::State(SystemState::default()));

    services.send(ServiceCommand::SetDoNotDisturb(true));
    let state = state_where(&mut events, |state| state.do_not_disturb).await;
    assert_eq!(state, SystemState { do_not_disturb: true, ..SystemState::default() });

    services.send(ServiceCommand::LockSession);
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::LockRequested);

    services.send(ServiceCommand::SetVolume(0.5));
    services.send(ServiceCommand::Suspend);
    drop(services);
    // The thread stopped, so the callback and its sender are gone.
    while let Ok(Some(_)) = timeout(WAIT, events.recv()).await {}
}

#[tokio::test]
async fn missing_buses_leave_services_disabled() {
    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig {
            audio: false,
            backlight: false,
            ..ServicesConfig::default()
        })
        .session_bus(BusAddress::Address("unix:path=/nonexistent/nimbus-bus".into()))
        .system_bus(BusAddress::Address("unix:path=/nonexistent/nimbus-system-bus".into())),
    );
    assert_eq!(next_event(&mut events).await, ServiceEvent::State(SystemState::default()));
    services.send(ServiceCommand::LockSession);
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::LockRequested);
    services.send(ServiceCommand::MediaPlayPause);
    services.send(ServiceCommand::CloseNotification { id: 1, reason: CloseReason::Dismissed });
}

async fn notify(
    conn: &Connection,
    replaces_id: u32,
    actions: &[&str],
    hints: HashMap<&str, Value<'_>>,
    expire_timeout: i32,
) -> u32 {
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "test-app",
            replaces_id,
            "dialog-information",
            "Summary",
            "Body",
            actions,
            hints,
            expire_timeout,
        ),
    )
    .await
    .unwrap()
    .body()
    .deserialize()
    .unwrap()
}

async fn next_signal(stream: &mut MessageStream) -> (String, u32, Value<'static>) {
    let message = timeout(WAIT, stream.next())
        .await
        .expect("timed out waiting for a signal")
        .unwrap()
        .unwrap();
    let member = message.header().member().unwrap().to_string();
    let body = message.body();
    let value = match member.as_str() {
        "NotificationClosed" => {
            let (id, reason): (u32, u32) = body.deserialize().unwrap();
            return (member, id, Value::U32(reason));
        }
        "ActionInvoked" | "ActivationToken" => {
            let (id, value): (u32, String) = body.deserialize().unwrap();
            return (member, id, Value::from(value));
        }
        _ => Value::U32(0),
    };
    (member, 0, value)
}

#[tokio::test]
async fn notification_server() {
    let Some(bus) = PrivateBus::start() else { return };
    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { notifications: true, ..disabled() })
            .session_bus(BusAddress::Address(bus.address.clone()))
            .system_bus(BusAddress::Disabled),
    );
    assert!(matches!(next_event(&mut events).await, ServiceEvent::State(_)));

    let client = bus.connect().await;
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.Notifications")
        .unwrap()
        .build();
    let mut signals = MessageStream::for_match_rule(rule, &client, None).await.unwrap();
    wait_for_owner(&client, "org.freedesktop.Notifications").await;

    let call = |method: &'static str| {
        let client = client.clone();
        async move {
            client
                .call_method(
                    Some("org.freedesktop.Notifications"),
                    "/org/freedesktop/Notifications",
                    Some("org.freedesktop.Notifications"),
                    method,
                    &(),
                )
                .await
                .unwrap()
        }
    };
    let info: (String, String, String, String) =
        call("GetServerInformation").await.body().deserialize().unwrap();
    assert_eq!(
        info,
        ("Nimbus".into(), "Nimbus".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    );
    let capabilities: Vec<String> = call("GetCapabilities").await.body().deserialize().unwrap();
    assert!(capabilities.iter().any(|c| c == "actions"));
    assert!(capabilities.iter().any(|c| c == "body"));
    assert!(!capabilities.iter().any(|c| c == "body-markup"));

    // A critical notification that never expires, with actions and an image.
    let hints =
        HashMap::from([("urgency", Value::U8(2)), ("image-path", Value::from("/tmp/photo.png"))]);
    let id = notify(&client, 0, &["default", "Open", "reply", "Reply"], hints, 0).await;
    assert_ne!(id, 0);
    let ServiceEvent::Notification(n) = next_non_state(&mut events).await else {
        panic!("expected a notification")
    };
    assert_eq!(n.id, id);
    assert_eq!(n.app_name, "test-app");
    assert_eq!(n.app_icon, "/tmp/photo.png");
    assert_eq!(n.summary, "Summary");
    assert_eq!(n.body, "Body");
    assert_eq!(n.urgency, Urgency::Critical);
    assert_eq!(n.expire_timeout, Some(Duration::ZERO));
    assert_eq!(
        n.actions,
        vec![("default".into(), "Open".into()), ("reply".into(), "Reply".into())]
    );

    // Replacing keeps the id.
    let replaced = notify(&client, id, &[], HashMap::new(), 0).await;
    assert_eq!(replaced, id);
    let ServiceEvent::Notification(n) = next_non_state(&mut events).await else {
        panic!("expected a notification")
    };
    assert_eq!((n.id, n.urgency, n.app_icon.as_str()), (id, Urgency::Normal, "dialog-information"));

    // CloseNotification from the client.
    call_close(&client, id).await;
    assert_eq!(next_signal(&mut signals).await, ("NotificationClosed".into(), id, Value::U32(3)));
    assert_eq!(
        next_non_state(&mut events).await,
        ServiceEvent::NotificationClosed { id, reason: CloseReason::Closed }
    );
    // Closing again is harmless and silent.
    call_close(&client, id).await;

    // Replacing a closed notification keeps its id.
    assert_eq!(notify(&client, id, &[], HashMap::new(), 0).await, id);
    assert!(
        matches!(next_non_state(&mut events).await, ServiceEvent::Notification(n) if n.id == id)
    );
    call_close(&client, id).await;
    assert_eq!(next_signal(&mut signals).await, ("NotificationClosed".into(), id, Value::U32(3)));
    assert!(matches!(next_non_state(&mut events).await, ServiceEvent::NotificationClosed { .. }));

    // Transient notifications close on expiry.
    let transient = HashMap::from([("transient", Value::Bool(true))]);
    let expiring = notify(&client, 0, &[], transient, 100).await;
    assert_ne!(expiring, id);
    assert!(
        matches!(next_non_state(&mut events).await, ServiceEvent::Notification(n) if n.id == expiring)
    );
    assert_eq!(
        next_signal(&mut signals).await,
        ("NotificationClosed".into(), expiring, Value::U32(1))
    );
    assert_eq!(
        next_non_state(&mut events).await,
        ServiceEvent::NotificationClosed { id: expiring, reason: CloseReason::Expired }
    );

    // Others stay open for the shell's history after expiry, and their actions still work.
    let expiring = notify(&client, 0, &["default", "Open"], HashMap::new(), 100).await;
    assert!(
        matches!(next_non_state(&mut events).await, ServiceEvent::Notification(n) if n.id == expiring)
    );
    assert_eq!(
        next_non_state(&mut events).await,
        ServiceEvent::NotificationClosed { id: expiring, reason: CloseReason::Expired }
    );
    services
        .send(ServiceCommand::InvokeNotificationAction { id: expiring, action: "default".into() });
    assert_eq!(
        next_signal(&mut signals).await,
        ("ActionInvoked".into(), expiring, Value::from("default"))
    );
    assert_eq!(
        next_signal(&mut signals).await,
        ("NotificationClosed".into(), expiring, Value::U32(2))
    );
    assert_eq!(
        next_non_state(&mut events).await,
        ServiceEvent::NotificationClosed { id: expiring, reason: CloseReason::Dismissed }
    );

    // An activation token arrives ahead of ActionInvoked.
    let with_token = notify(&client, 0, &["reply", "Reply"], HashMap::new(), 0).await;
    assert!(matches!(next_non_state(&mut events).await, ServiceEvent::Notification(_)));
    services.send(ServiceCommand::InvokeNotificationActionWithToken {
        id: with_token,
        action: "reply".into(),
        activation_token: "token-1".into(),
    });
    assert_eq!(
        next_signal(&mut signals).await,
        ("ActivationToken".into(), with_token, Value::from("token-1"))
    );
    assert_eq!(
        next_signal(&mut signals).await,
        ("ActionInvoked".into(), with_token, Value::from("reply"))
    );
    assert_eq!(
        next_signal(&mut signals).await,
        ("NotificationClosed".into(), with_token, Value::U32(2))
    );
    assert!(matches!(next_non_state(&mut events).await, ServiceEvent::NotificationClosed { .. }));

    // Invoking an action emits ActionInvoked and dismisses the notification.
    let with_action = notify(&client, 0, &["default", "Open"], HashMap::new(), 0).await;
    assert!(matches!(next_non_state(&mut events).await, ServiceEvent::Notification(_)));
    services.send(ServiceCommand::InvokeNotificationAction {
        id: with_action,
        action: "missing".into(),
    });
    services.send(ServiceCommand::InvokeNotificationAction {
        id: with_action,
        action: "default".into(),
    });
    assert_eq!(
        next_signal(&mut signals).await,
        ("ActionInvoked".into(), with_action, Value::from("default"))
    );
    assert_eq!(
        next_signal(&mut signals).await,
        ("NotificationClosed".into(), with_action, Value::U32(2))
    );
    assert_eq!(
        next_non_state(&mut events).await,
        ServiceEvent::NotificationClosed { id: with_action, reason: CloseReason::Dismissed }
    );

    // Resident notifications stay after an action.
    let resident = notify(
        &client,
        0,
        &["default", "Open"],
        HashMap::from([("resident", Value::Bool(true))]),
        0,
    )
    .await;
    assert!(matches!(next_non_state(&mut events).await, ServiceEvent::Notification(_)));
    services
        .send(ServiceCommand::InvokeNotificationAction { id: resident, action: "default".into() });
    assert_eq!(
        next_signal(&mut signals).await,
        ("ActionInvoked".into(), resident, Value::from("default"))
    );
    services
        .send(ServiceCommand::CloseNotification { id: resident, reason: CloseReason::Dismissed });
    assert_eq!(
        next_signal(&mut signals).await,
        ("NotificationClosed".into(), resident, Value::U32(2))
    );
    assert_eq!(
        next_non_state(&mut events).await,
        ServiceEvent::NotificationClosed { id: resident, reason: CloseReason::Dismissed }
    );

    drop(services);
}

async fn call_close(client: &Connection, id: u32) {
    client
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "CloseNotification",
            &(id,),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn notification_server_yields_to_existing_daemon() {
    let Some(bus) = PrivateBus::start() else { return };
    let other = bus.connect().await;
    other.request_name("org.freedesktop.Notifications").await.unwrap();
    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { notifications: true, ..disabled() })
            .session_bus(BusAddress::Address(bus.address.clone()))
            .system_bus(BusAddress::Disabled),
    );
    assert!(matches!(next_event(&mut events).await, ServiceEvent::State(_)));
    services.send(ServiceCommand::CloseNotification { id: 1, reason: CloseReason::Closed });
    tokio::time::sleep(Duration::from_millis(300)).await;

    let dbus = zbus::fdo::DBusProxy::new(&other).await.unwrap();
    let owner =
        dbus.get_name_owner("org.freedesktop.Notifications".try_into().unwrap()).await.unwrap();
    assert_eq!(owner.as_str(), other.unique_name().unwrap().as_str());
    assert!(timeout(Duration::from_millis(200), next_non_state(&mut events)).await.is_err());
}

#[tokio::test]
async fn upower_appears_changes_and_leaves() {
    let Some(bus) = PrivateBus::start() else { return };
    let (_services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { upower: true, ..disabled() })
            .session_bus(BusAddress::Disabled)
            .system_bus(BusAddress::Address(bus.address.clone())),
    );
    assert_eq!(next_event(&mut events).await, ServiceEvent::State(SystemState::default()));
    // Give the service time to start watching before the daemon appears.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let upower = FakeUPower::start(&bus, FakeBattery::discharging(50.0, WarningLevel::None)).await;
    let state = state_where(&mut events, |state| state.battery.is_some()).await;
    let battery = state.battery.unwrap();
    assert_eq!(battery.level, 0.5);
    assert!(!battery.charging);
    assert_eq!(battery.time_to_empty, Some(Duration::from_secs(5400)));
    assert_eq!(battery.warning, BatteryWarning::None);

    upower.set(FakeBattery::discharging(75.0, WarningLevel::None)).await;
    state_where(&mut events, |state| state.battery.as_ref().is_some_and(|b| b.level == 0.75)).await;

    upower.set(FakeBattery::discharging(4.0, WarningLevel::Critical)).await;
    state_where(&mut events, |state| {
        state.battery.as_ref().is_some_and(|b| b.warning == BatteryWarning::Critical)
    })
    .await;

    drop(upower);
    state_where(&mut events, |state| state.battery.is_none()).await;
}

#[tokio::test]
async fn logind_lock_unlock_and_sleep() {
    let Some(bus) = PrivateBus::start() else { return };
    let logind = FakeLogind::start(&bus).await;
    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { logind: true, ..disabled() })
            .session_bus(BusAddress::Disabled)
            .system_bus(BusAddress::Address(bus.address.clone())),
    );
    assert!(matches!(next_event(&mut events).await, ServiceEvent::State(_)));
    // The sleep inhibitor is the last step of setup, after subscribing to signals.
    wait_until("the sleep inhibitor", || logind.inhibitors().len() == 1).await;
    let inhibitor = &logind.inhibitors()[0];
    assert_eq!((inhibitor.what.as_str(), inhibitor.mode.as_str()), ("sleep", "delay"));

    services.send(ServiceCommand::LockSession);
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::LockRequested);
    logind.unlock_session().await;
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::UnlockRequested);

    // Suspend waits until the compositor presents the lock screen.
    let latest_released = || logind.inhibitors().last().is_some_and(|i| i.released);
    logind.prepare_for_sleep(true).await;
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::LockRequested);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!latest_released());
    services.send(ServiceCommand::LockPresented);
    wait_until("the inhibitor's release", latest_released).await;

    // Resuming takes a new inhibitor.
    logind.prepare_for_sleep(false).await;
    assert!(timeout(Duration::from_millis(200), next_non_state(&mut events)).await.is_err());
    wait_until("a new inhibitor", || logind.inhibitors().len() == 2).await;
    assert!(!latest_released());

    // Without that report, suspend proceeds after a timeout.
    logind.prepare_for_sleep(true).await;
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::LockRequested);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!latest_released());
    wait_until("the inhibitor's release", latest_released).await;
    logind.prepare_for_sleep(false).await;

    // Without logind, locking still reaches the shell.
    drop(logind);
    tokio::time::sleep(Duration::from_millis(300)).await;
    services.send(ServiceCommand::LockSession);
    assert_eq!(next_non_state(&mut events).await, ServiceEvent::LockRequested);
}

struct FakePlayer {
    playing: bool,
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        if self.playing { "Playing" } else { "Paused" }.into()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let pairs = [
            ("xesam:title", Value::from("Song")),
            ("xesam:artist", Value::from(vec!["Artist"])),
            ("mpris:artUrl", Value::from("file:///tmp/art.png")),
        ];
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.try_to_owned().unwrap()))
            .collect()
    }

    async fn play_pause(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.playing = !self.playing;
        let _ = self.playback_status_changed(&emitter).await;
    }
}

#[tokio::test]
async fn mpris_player_tracking() {
    let Some(bus) = PrivateBus::start() else { return };
    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { mpris: true, ..disabled() })
            .session_bus(BusAddress::Address(bus.address.clone()))
            .system_bus(BusAddress::Disabled),
    );
    assert!(matches!(next_event(&mut events).await, ServiceEvent::State(_)));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let player = bus.connect().await;
    player
        .object_server()
        .at("/org/mpris/MediaPlayer2", FakePlayer { playing: false })
        .await
        .unwrap();
    player.request_name("org.mpris.MediaPlayer2.fake").await.unwrap();

    let state = state_where(&mut events, |state| state.media.is_some()).await;
    let media = state.media.unwrap();
    assert_eq!(media.player, "org.mpris.MediaPlayer2.fake");
    assert_eq!(media.title, "Song");
    assert_eq!(media.artist, "Artist");
    assert_eq!(media.art_url.as_deref(), Some("file:///tmp/art.png"));
    assert!(!media.playing);

    services.send(ServiceCommand::MediaPlayPause);
    state_where(&mut events, |state| state.media.as_ref().is_some_and(|m| m.playing)).await;

    drop(player);
    state_where(&mut events, |state| state.media.is_none()).await;
}

struct FakeNetworkManager {
    wireless_enabled: bool,
}

#[interface(name = "org.freedesktop.NetworkManager")]
impl FakeNetworkManager {
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.wireless_enabled
    }
    #[zbus(property)]
    fn set_wireless_enabled(&mut self, enabled: bool) {
        self.wireless_enabled = enabled;
    }
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        4
    }
    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/ActiveConnection/1").unwrap()
    }
    #[zbus(property)]
    fn primary_connection_type(&self) -> String {
        "802-11-wireless".into()
    }
}

struct FakeActiveConnection;

#[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl FakeActiveConnection {
    #[zbus(property)]
    fn id(&self) -> String {
        "Home connection".into()
    }
    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "802-11-wireless".into()
    }
    #[zbus(property)]
    fn specific_object(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/AccessPoint/7").unwrap()
    }
}

struct FakeAccessPoint;

#[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl FakeAccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        b"home".to_vec()
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        80
    }
}

#[tokio::test]
async fn network_manager_wifi() {
    let Some(bus) = PrivateBus::start() else { return };
    let daemon = bus.connect().await;
    let server = daemon.object_server();
    server
        .at("/org/freedesktop/NetworkManager", FakeNetworkManager { wireless_enabled: true })
        .await
        .unwrap();
    server
        .at("/org/freedesktop/NetworkManager/ActiveConnection/1", FakeActiveConnection)
        .await
        .unwrap();
    server.at("/org/freedesktop/NetworkManager/AccessPoint/7", FakeAccessPoint).await.unwrap();
    daemon.request_name("org.freedesktop.NetworkManager").await.unwrap();

    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { network_manager: true, ..disabled() })
            .session_bus(BusAddress::Disabled)
            .system_bus(BusAddress::Address(bus.address.clone())),
    );
    let state = state_where(&mut events, |state| state.network.available).await;
    assert_eq!(
        state.network,
        Network {
            kind: ConnectionKind::Wifi,
            ssid: Some("home".into()),
            strength: 0.8,
            wifi_enabled: true,
            available: true,
        }
    );

    services.send(ServiceCommand::SetWifiEnabled(false));
    state_where(&mut events, |state| !state.network.wifi_enabled).await;
}

struct FakeAdapter {
    powered: bool,
}

#[interface(name = "org.bluez.Adapter1")]
impl FakeAdapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered
    }
    #[zbus(property)]
    fn set_powered(&mut self, powered: bool) {
        self.powered = powered;
    }
}

struct FakeDevice;

#[interface(name = "org.bluez.Device1")]
impl FakeDevice {
    #[zbus(property)]
    fn connected(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn bluez_adapter() {
    let Some(bus) = PrivateBus::start() else { return };
    let daemon = bus.connect().await;
    let server = daemon.object_server();
    server.at("/", zbus::fdo::ObjectManager).await.unwrap();
    server.at("/org/bluez/hci0", FakeAdapter { powered: false }).await.unwrap();
    server.at("/org/bluez/hci0/dev_00_11_22_33_44_55", FakeDevice).await.unwrap();
    daemon.request_name("org.bluez").await.unwrap();

    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { bluetooth: true, ..disabled() })
            .session_bus(BusAddress::Disabled)
            .system_bus(BusAddress::Address(bus.address.clone())),
    );
    let state = state_where(&mut events, |state| state.bluetooth.is_some()).await;
    assert_eq!(state.bluetooth, Some(Bluetooth { powered: false, connected_devices: 1 }));

    services.send(ServiceCommand::SetBluetoothPowered(true));
    state_where(&mut events, |state| state.bluetooth.as_ref().is_some_and(|b| b.powered)).await;

    server.at("/org/bluez/hci0/dev_66_77_88_99_AA_BB", FakeDevice).await.unwrap();
    state_where(&mut events, |state| {
        state.bluetooth.as_ref().is_some_and(|b| b.connected_devices == 2)
    })
    .await;
}

#[tokio::test]
async fn local_services_start_and_stop_cleanly() {
    let (services, mut events) = spawn(
        ServicesBuilder::new(ServicesConfig { audio: true, backlight: true, ..disabled() })
            .session_bus(BusAddress::Disabled)
            .system_bus(BusAddress::Disabled),
    );
    assert!(matches!(next_event(&mut events).await, ServiceEvent::State(_)));
    services.send(ServiceCommand::SetVolume(0.3));
    services.send(ServiceCommand::ToggleMute);
    services.send(ServiceCommand::SetBrightness(0.5));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    drop(services);
    assert!(started.elapsed() < Duration::from_secs(2));
}
