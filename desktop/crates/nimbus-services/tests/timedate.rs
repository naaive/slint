// SPDX-License-Identifier: MIT

//! The timedated client against a fake timedated on a private `dbus-daemon`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nimbus_services::BusAddress;
use nimbus_services::timedate::{Client, Command, Event, TimeState};
use nimbus_test_support::PrivateBus;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, interface};

const WAIT: Duration = Duration::from_secs(10);
const PATH: &str = "/org/freedesktop/timedate1";

#[derive(Default)]
struct World {
    timezone: String,
    ntp: bool,
    /// Whether every change asked to be interactive, so polkit may ask the user.
    interactive: bool,
}

struct FakeTimedated(Arc<Mutex<World>>);

#[interface(name = "org.freedesktop.timedate1")]
impl FakeTimedated {
    #[zbus(property)]
    fn timezone(&self) -> String {
        self.0.lock().unwrap().timezone.clone()
    }
    #[zbus(property, name = "CanNTP")]
    fn can_ntp(&self) -> bool {
        true
    }
    #[zbus(property, name = "NTP")]
    fn ntp(&self) -> bool {
        self.0.lock().unwrap().ntp
    }
    #[zbus(property(emits_changed_signal = "false"), name = "NTPSynchronized")]
    fn ntp_synchronized(&self) -> bool {
        self.0.lock().unwrap().ntp
    }

    async fn set_timezone(
        &self,
        timezone: String,
        interactive: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        if !timezone.contains('/') {
            return Err(zbus::fdo::Error::InvalidArgs("Invalid or unknown time zone".into()));
        }
        {
            let mut world = self.0.lock().unwrap();
            world.timezone = timezone;
            world.interactive &= interactive;
        }
        self.timezone_changed(&emitter).await?;
        Ok(())
    }

    #[zbus(name = "SetNTP")]
    async fn set_ntp(
        &self,
        on: bool,
        interactive: bool,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        {
            let mut world = self.0.lock().unwrap();
            world.ntp = on;
            world.interactive &= interactive;
        }
        self.n_t_p_changed(&emitter).await?;
        Ok(())
    }

    fn list_timezones(&self) -> Vec<String> {
        ["America/New_York", "Europe/Berlin", "UTC"].map(String::from).to_vec()
    }
}

async fn next(events: &mut UnboundedReceiver<Event>) -> Event {
    timeout(WAIT, events.recv()).await.expect("an event in time").expect("the client runs")
}

async fn state_where(
    events: &mut UnboundedReceiver<Event>,
    matches: impl Fn(&TimeState) -> bool,
) -> TimeState {
    loop {
        if let Event::State(state) = next(events).await
            && matches(&state)
        {
            return state;
        }
    }
}

async fn start_daemon(bus: &PrivateBus, world: &Arc<Mutex<World>>) -> Connection {
    let daemon = bus.connect().await;
    daemon.object_server().at(PATH, FakeTimedated(world.clone())).await.unwrap();
    daemon.request_name("org.freedesktop.timedate1").await.unwrap();
    daemon
}

#[tokio::test]
async fn reads_and_changes_the_time_zone_and_synchronization() {
    let Some(bus) = PrivateBus::start() else { return };
    let (sender, mut events) = unbounded_channel();
    let client = Client::spawn(BusAddress::Address(bus.address.clone()), move |event| {
        let _ = sender.send(event);
    });
    assert_eq!(next(&mut events).await, Event::State(TimeState::default()), "nothing answers yet");

    // timedated usually starts on demand, so a refresh reaches it without waiting for its name.
    let world = Arc::new(Mutex::new(World {
        timezone: "Europe/Berlin".into(),
        ntp: true,
        interactive: true,
    }));
    let daemon = start_daemon(&bus, &world).await;
    client.send(Command::Refresh);
    let state = state_where(&mut events, |s| s.available).await;
    assert_eq!(
        state,
        TimeState {
            available: true,
            timezone: "Europe/Berlin".into(),
            can_ntp: true,
            ntp: true,
            synchronized: true,
        }
    );
    assert_eq!(
        next(&mut events).await,
        Event::Timezones(["America/New_York", "Europe/Berlin", "UTC"].map(String::from).to_vec())
    );

    client.send(Command::SetTimezone("America/New_York".into()));
    state_where(&mut events, |s| s.timezone == "America/New_York").await;
    client.send(Command::SetNtp(false));
    state_where(&mut events, |s| !s.ntp && !s.synchronized).await;
    assert!(world.lock().unwrap().interactive, "changes let polkit ask the user");

    client.send(Command::SetTimezone("Atlantis".into()));
    loop {
        if let Event::Failed(reason) = next(&mut events).await {
            assert_eq!(reason, "Invalid or unknown time zone");
            break;
        }
    }

    // A change made elsewhere arrives through PropertiesChanged.
    world.lock().unwrap().timezone = "UTC".into();
    let iface = daemon.object_server().interface::<_, FakeTimedated>(PATH).await.unwrap();
    iface.get().await.timezone_changed(iface.signal_emitter()).await.unwrap();
    state_where(&mut events, |s| s.timezone == "UTC").await;

    drop(iface);
    drop(daemon);
    client.send(Command::Refresh);
    state_where(&mut events, |s| !s.available).await;
}
