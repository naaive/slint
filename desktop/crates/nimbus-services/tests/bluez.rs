// SPDX-License-Identifier: MIT

//! The BlueZ client and its pairing agent against a fake BlueZ on a private `dbus-daemon`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nimbus_services::BusAddress;
use nimbus_services::bluez::{
    Answer, BluetoothState, Client, Command, Device, Event, PairingKind, PairingRequest,
};
use nimbus_test_support::PrivateBus;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zbus::{Connection, ObjectServer, interface};

const WAIT: Duration = Duration::from_secs(10);
const ADAPTER: &str = "/org/bluez/hci0";
const KEYBOARD: &str = "/org/bluez/hci0/dev_00_00_00_00_00_01";
const HEADPHONES: &str = "/org/bluez/hci0/dev_00_00_00_00_00_02";
const PHONE: &str = "/org/bluez/hci0/dev_00_00_00_00_00_03";
const NAMELESS: &str = "/org/bluez/hci0/dev_00_00_00_00_00_04";

/// The registered agent: its owner, path, and capability.
type Registration = Arc<Mutex<Option<(String, OwnedObjectPath, String)>>>;

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum BluezError {
    #[zbus(error)]
    ZBus(zbus::Error),
    AuthenticationRejected(String),
}

struct FakeAgentManager {
    agent: Registration,
    default: Arc<Mutex<bool>>,
}

#[interface(name = "org.bluez.AgentManager1")]
impl FakeAgentManager {
    fn register_agent(
        &self,
        agent: OwnedObjectPath,
        capability: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        let sender = header.sender().unwrap().to_string();
        *self.agent.lock().unwrap() = Some((sender, agent, capability));
    }

    fn request_default_agent(&self, _agent: OwnedObjectPath) {
        *self.default.lock().unwrap() = true;
    }
}

struct FakeAdapter {
    powered: bool,
    discoverable: bool,
    discovering: bool,
    agent: Registration,
}

#[interface(name = "org.bluez.Adapter1")]
impl FakeAdapter {
    #[zbus(property)]
    fn alias(&self) -> String {
        "workstation".into()
    }
    #[zbus(property)]
    fn address(&self) -> String {
        "AA:AA:AA:AA:AA:AA".into()
    }
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.powered
    }
    #[zbus(property)]
    fn set_powered(&mut self, powered: bool) {
        self.powered = powered;
    }
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        self.discoverable
    }
    #[zbus(property)]
    fn set_discoverable(&mut self, discoverable: bool) {
        self.discoverable = discoverable;
    }
    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.discovering
    }

    async fn start_discovery(
        &mut self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        self.discovering = true;
        self.discovering_changed(&emitter).await.unwrap();
        for (path, name) in
            [(HEADPHONES, Some("Headphones")), (PHONE, Some("Phone")), (NAMELESS, None)]
        {
            let device = FakeDevice::new(name, false, self.agent.clone());
            server.at(path, device).await.unwrap();
        }
    }

    async fn stop_discovery(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.discovering = false;
        self.discovering_changed(&emitter).await.unwrap();
    }

    async fn remove_device(
        &self,
        device: OwnedObjectPath,
        #[zbus(object_server)] server: &ObjectServer,
    ) {
        server.remove::<FakeDevice, _>(device.as_str()).await.unwrap();
    }
}

struct FakeDevice {
    name: Option<&'static str>,
    /// Atomic, since pairing reads it while BlueZ answers other calls.
    paired: AtomicBool,
    trusted: bool,
    connected: bool,
    agent: Registration,
}

impl FakeDevice {
    fn new(name: Option<&'static str>, paired: bool, agent: Registration) -> Self {
        Self { name, paired: AtomicBool::new(paired), trusted: paired, connected: false, agent }
    }
}

#[interface(name = "org.bluez.Device1")]
impl FakeDevice {
    #[zbus(property)]
    fn address(&self) -> String {
        "00:00:00:00:00:00".into()
    }
    #[zbus(property)]
    fn alias(&self) -> String {
        self.name.unwrap_or("00-00-00-00-00-00").into()
    }
    #[zbus(property)]
    fn name(&self) -> zbus::fdo::Result<String> {
        self.name.map(str::to_owned).ok_or_else(|| zbus::fdo::Error::InvalidArgs("no name".into()))
    }
    #[zbus(property)]
    fn icon(&self) -> String {
        "audio-headphones".into()
    }
    #[zbus(property)]
    fn adapter(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(ADAPTER).unwrap()
    }
    #[zbus(property)]
    fn paired(&self) -> bool {
        self.paired.load(Ordering::Relaxed)
    }
    #[zbus(property)]
    fn trusted(&self) -> bool {
        self.trusted
    }
    #[zbus(property)]
    fn set_trusted(&mut self, trusted: bool) {
        self.trusted = trusted;
    }
    #[zbus(property)]
    fn connected(&self) -> bool {
        self.connected
    }

    /// Asks the agent to confirm a passkey, as a device with a display would.
    async fn pair(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), BluezError> {
        let (owner, agent, _) = self.agent.lock().unwrap().clone().expect("a registered agent");
        let device = header.path().unwrap().to_owned();
        let confirmed = conn
            .call_method(
                Some(owner.as_str()),
                agent.as_str(),
                Some("org.bluez.Agent1"),
                "RequestConfirmation",
                &(device, 123_456u32),
            )
            .await;
        if confirmed.is_err() {
            return Err(BluezError::AuthenticationRejected("the agent declined".into()));
        }
        self.paired.store(true, Ordering::Relaxed);
        self.paired_changed(&emitter).await?;
        Ok(())
    }

    async fn connect(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.connected = true;
        Ok(self.connected_changed(&emitter).await?)
    }

    async fn disconnect(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.connected = false;
        Ok(self.connected_changed(&emitter).await?)
    }
}

struct Fake {
    _daemon: Connection,
    agent: Registration,
    default: Arc<Mutex<bool>>,
}

async fn fake_bluez(bus: &PrivateBus) -> Fake {
    let agent = Registration::default();
    let default = Arc::new(Mutex::new(false));
    let daemon = bus.connect().await;
    let server = daemon.object_server();
    server.at("/", zbus::fdo::ObjectManager).await.unwrap();
    let manager = FakeAgentManager { agent: agent.clone(), default: default.clone() };
    server.at("/org/bluez", manager).await.unwrap();
    let adapter = FakeAdapter {
        powered: false,
        discoverable: false,
        discovering: false,
        agent: agent.clone(),
    };
    server.at(ADAPTER, adapter).await.unwrap();
    server.at(KEYBOARD, FakeDevice::new(Some("Keyboard"), true, agent.clone())).await.unwrap();
    daemon.request_name("org.bluez").await.unwrap();
    Fake { _daemon: daemon, agent, default }
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
    predicate: impl Fn(&BluetoothState) -> bool,
) -> BluetoothState {
    loop {
        if let Event::State(state) = next_event(events).await
            && predicate(&state)
        {
            return state;
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

fn device<'a>(state: &'a BluetoothState, id: &str) -> Option<&'a Device> {
    state.devices.iter().find(|device| device.id == id)
}

#[tokio::test]
async fn powers_discovers_pairs_and_removes() {
    let Some(bus) = PrivateBus::start() else { return };
    let fake = fake_bluez(&bus).await;
    let (client, mut events) = spawn(BusAddress::Address(bus.address.clone()));

    let state = state_where(&mut events, |state| state.adapter.is_some()).await;
    let adapter = state.adapter.as_ref().unwrap();
    assert_eq!((adapter.name.as_str(), adapter.powered), ("workstation", false));
    let names: Vec<&str> = state.devices.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["Keyboard"]);
    let registration = timeout(WAIT, async {
        loop {
            if let Some(registration) = fake.agent.lock().unwrap().clone() {
                return registration;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent registers");
    assert_eq!(registration.2, "KeyboardDisplay");
    assert!(*fake.default.lock().unwrap(), "the agent is the default one");

    client.send(Command::SetPowered(true));
    client.send(Command::SetDiscoverable(true));
    state_where(&mut events, |state| {
        state.adapter.as_ref().is_some_and(|a| a.powered && a.discoverable)
    })
    .await;

    client.send(Command::SetDiscovering(true));
    let state = state_where(&mut events, |state| state.devices.len() == 3).await;
    assert!(state.adapter.as_ref().unwrap().discovering);
    let names: Vec<&str> = state.devices.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["Keyboard", "Headphones", "Phone"],
        "paired first; nameless devices are left out"
    );

    client.send(Command::Pair(HEADPHONES.into()));
    let Event::Pairing(PairingRequest { id, device_id, device: name, kind }) =
        next_non_state(&mut events).await
    else {
        panic!("expected a pairing request");
    };
    assert_eq!((name.as_str(), kind), ("Headphones", PairingKind::Confirm { passkey: 123_456 }));
    assert_eq!(device_id, HEADPHONES);
    client.send(Command::Answer { id, answer: Answer::Accept });
    assert_eq!(next_non_state(&mut events).await, Event::PairingEnded { id });
    let state = state_where(&mut events, |state| {
        device(state, HEADPHONES).is_some_and(|d| d.paired && d.trusted && d.connected && !d.busy)
    })
    .await;
    assert_eq!(state.devices[0].id, HEADPHONES, "connected devices come first");

    client.send(Command::Pair(PHONE.into()));
    let Event::Pairing(PairingRequest { id, .. }) = next_non_state(&mut events).await else {
        panic!("expected a pairing request");
    };
    client.send(Command::Answer { id, answer: Answer::Reject });
    assert_eq!(next_non_state(&mut events).await, Event::PairingEnded { id });
    assert_eq!(
        next_non_state(&mut events).await,
        Event::Failed { device: "Phone".into(), message: "the agent declined".into() }
    );

    client.send(Command::SetDiscovering(false));
    client.send(Command::Disconnect(HEADPHONES.into()));
    client.send(Command::Remove(KEYBOARD.into()));
    state_where(&mut events, |state| {
        device(state, KEYBOARD).is_none()
            && device(state, HEADPHONES).is_some_and(|d| !d.connected)
            && state.adapter.as_ref().is_some_and(|a| !a.discovering)
    })
    .await;
}

#[tokio::test]
async fn the_agent_answers_only_bluez() {
    let Some(bus) = PrivateBus::start() else { return };
    let fake = fake_bluez(&bus).await;
    let (_client, mut events) = spawn(BusAddress::Address(bus.address.clone()));
    state_where(&mut events, |state| state.adapter.is_some()).await;
    let (owner, agent, _) = timeout(WAIT, async {
        loop {
            if let Some(registration) = fake.agent.lock().unwrap().clone() {
                return registration;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent registers");

    let stranger = bus.connect().await;
    let device = ObjectPath::try_from(KEYBOARD).unwrap();
    let reply = stranger
        .call_method(
            Some(owner.as_str()),
            agent.as_str(),
            Some("org.bluez.Agent1"),
            "RequestConfirmation",
            &(device, 1u32),
        )
        .await;
    match reply {
        Err(zbus::Error::MethodError(name, _, _)) => {
            assert_eq!(name.as_str(), "org.bluez.Error.Rejected")
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
    let quiet = timeout(Duration::from_millis(200), next_non_state(&mut events)).await;
    assert!(quiet.is_err(), "no pairing request reaches the user: {quiet:?}");
}

#[tokio::test]
async fn without_bluez_commands_fail() {
    let Some(bus) = PrivateBus::start() else { return };
    let (client, mut events) = spawn(BusAddress::Address(bus.address.clone()));
    assert_eq!(next_event(&mut events).await, Event::State(BluetoothState::default()));
    client.send(Command::SetPowered(true));
    assert_eq!(
        next_non_state(&mut events).await,
        Event::Failed { device: String::new(), message: "Bluetooth isn't available".into() }
    );
}
