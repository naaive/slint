// SPDX-License-Identifier: MIT

//! A client of systemd-timedated (`org.freedesktop.timedate1`) for date and time settings:
//! the time zone and network time synchronization.
//!
//! timedated starts by D-Bus activation and exits when idle, so the client calls it whenever it needs it
//! instead of following its name on the bus.
//! [`Client::spawn`] runs it on a thread of its own, apart from [`crate::Services`].

mod service;
mod zones;

use crate::BusAddress;
use crate::worker::Worker;

pub use zones::zone_names;

/// A request to timedated; changes ask polkit first, and failures arrive as [`Event::Failed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Sets the system time zone to a name such as `Europe/Berlin`.
    SetTimezone(String),
    /// Turns network time synchronization on or off.
    SetNtp(bool),
    /// Reads the state again, such as whether the clock is synchronized, which timedated doesn't signal.
    Refresh,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    State(TimeState),
    /// The time zones the system knows, sorted; sent once after timedated first answers.
    Timezones(Vec<String>),
    /// timedated refused a [`Command`], with its reason.
    Failed(String),
}

/// What timedated reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeState {
    /// Whether timedated answers; everything else is empty without it.
    pub available: bool,
    /// The system time zone, such as `Europe/Berlin`.
    pub timezone: String,
    /// Whether a time synchronization service, such as systemd-timesyncd, is installed.
    pub can_ntp: bool,
    /// Whether network time synchronization is on.
    pub ntp: bool,
    /// Whether the clock is synchronized with a time server.
    pub synchronized: bool,
}

/// A timedated client on its own thread; dropping it stops the client.
pub struct Client {
    worker: Worker<Command>,
}

impl Client {
    /// Connects to `system_bus` and calls `on_event` from the client's thread.
    /// The first event is an [`Event::State`], whose `available` is false while timedated can't be reached.
    pub fn spawn(system_bus: BusAddress, on_event: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let worker = Worker::run("nimbus-timedate", move |commands| {
            service::run(system_bus, std::sync::Arc::new(on_event), commands)
        });
        Self { worker }
    }

    /// Queues `command`; it never blocks.
    pub fn send(&self, command: Command) {
        self.worker.send(command);
    }
}
