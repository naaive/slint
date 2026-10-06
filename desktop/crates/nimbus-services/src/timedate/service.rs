// SPDX-License-Identifier: MIT

//! The timedated client's D-Bus side.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::timeout;
use zbus::Connection;
use zbus::zvariant::DynamicType;

use super::{Command, Event, TimeState, zone_names};
use crate::BusAddress;
use crate::bus::{self, BusKind, PROPERTIES, Wake};

const NAME: &str = "org.freedesktop.timedate1";
const PATH: &str = "/org/freedesktop/timedate1";
const INTERFACE: &str = "org.freedesktop.timedate1";
/// How long a change may wait for the user to answer polkit.
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(120);

type OnEvent = Arc<dyn Fn(Event) + Send + Sync>;

struct TimeService {
    conn: Connection,
    on_event: OnEvent,
    last: Option<TimeState>,
    listed: bool,
    /// Where changes running in the background report their failure, or `None` for success.
    done: UnboundedSender<Option<String>>,
}

impl TimeService {
    fn publish(&mut self, state: TimeState) {
        if self.last.as_ref() != Some(&state) {
            self.last = Some(state.clone());
            (self.on_event)(Event::State(state));
        }
    }

    async fn refresh(&mut self) {
        let state = match bus::get_all(&self.conn, NAME, PATH, INTERFACE).await {
            Ok(mut props) => TimeState {
                available: true,
                timezone: props.take("Timezone").unwrap_or_default(),
                can_ntp: props.take("CanNTP").unwrap_or(false),
                ntp: props.take("NTP").unwrap_or(false),
                synchronized: props.take("NTPSynchronized").unwrap_or(false),
            },
            Err(err) => {
                if self.last.as_ref().is_none_or(|last| last.available) {
                    tracing::info!("timedated isn't available: {err}");
                }
                TimeState::default()
            }
        };
        let available = state.available;
        self.publish(state);
        if available && !self.listed {
            self.listed = true;
            let zones = match bus::call_for(&self.conn, NAME, PATH, INTERFACE, "ListTimezones", &())
                .await
            {
                Ok(zones) => zones,
                Err(err) => {
                    tracing::debug!("timedated didn't list time zones, reading tzdata: {err}");
                    zone_names(Path::new("/usr/share/zoneinfo"))
                }
            };
            (self.on_event)(Event::Timezones(zones));
        }
    }

    /// Starts a change that may wait for polkit, reporting its outcome through `done`.
    fn change<B>(&self, method: &'static str, body: B)
    where
        B: Serialize + DynamicType + Send + Sync + 'static,
    {
        let conn = self.conn.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            let call = conn.call_method(Some(NAME), PATH, Some(INTERFACE), method, &body);
            let result =
                timeout(INTERACTIVE_TIMEOUT, call).await.unwrap_or_else(|_| Err(bus::timed_out()));
            let failure = result.err().map(|err| {
                tracing::info!("timedated refused {method}: {err}");
                bus::reason(&err)
            });
            let _ = done.send(failure);
        });
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::SetTimezone(zone) => self.change("SetTimezone", (zone, true)),
            Command::SetNtp(on) => self.change("SetNTP", (on, true)),
            Command::Refresh => self.refresh().await,
        }
    }
}

fn unavailable(on_event: &OnEvent) {
    on_event(Event::State(TimeState::default()));
}

/// Signals of any sender on timedated's path, since timedated comes and goes with new unique names;
/// a signal from someone else only causes a needless refresh.
async fn signals(conn: &Connection) -> zbus::Result<bus::Signals> {
    let rule =
        bus::signal_rule().path(PATH)?.interface(PROPERTIES)?.member("PropertiesChanged")?.build();
    bus::signals(conn, [rule]).await
}

pub(super) async fn run(
    system_bus: BusAddress,
    on_event: OnEvent,
    mut commands: UnboundedReceiver<Command>,
) {
    let connected = match bus::connect(&system_bus, BusKind::System).await {
        Some(conn) => signals(&conn).await.map(|signals| (conn, signals)).inspect_err(|err| {
            tracing::info!("Can't follow timedated: {err}");
        }),
        None => Err(zbus::Error::Failure("no system bus".into())),
    };
    let Ok((conn, mut signals)) = connected else {
        unavailable(&on_event);
        while commands.recv().await.is_some() {
            on_event(Event::Failed("The system bus isn't available".into()));
        }
        return;
    };
    let (done, mut finished) = mpsc::unbounded_channel();
    let mut service = TimeService { conn, on_event, last: None, listed: false, done };
    service.refresh().await;
    loop {
        tokio::select! {
            wake = bus::next_wake(&mut signals, &mut commands) => match wake {
                Ok(Some(Wake::Signal)) => service.refresh().await,
                Ok(Some(Wake::Command(command))) => service.handle(command).await,
                Ok(None) => return,
                Err(err) => {
                    tracing::info!("Lost timedated's signals: {err}");
                    unavailable(&service.on_event);
                    return;
                }
            },
            Some(failure) = finished.recv() => {
                if let Some(reason) = failure {
                    (service.on_event)(Event::Failed(reason));
                }
                service.refresh().await;
            }
        }
    }
}
