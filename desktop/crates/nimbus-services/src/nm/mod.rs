// SPDX-License-Identifier: MIT

//! A NetworkManager client for network settings: Wi-Fi networks, connecting and forgetting them,
//! and the details of wired and wireless connections.
//!
//! [`Client::spawn`] runs it on a thread of its own, apart from [`crate::Services`].

mod parse;
mod service;

use crate::BusAddress;
use crate::worker::Worker;

pub use parse::Security;

/// A request to NetworkManager; failures arrive as [`Event::Failed`].
#[derive(Clone, PartialEq, Eq)]
pub enum Command {
    SetWifiEnabled(bool),
    /// Asks the Wi-Fi device to scan for networks.
    Scan,
    /// Activates the saved connection to the network with this SSID, or adds one.
    /// `password` replaces the saved one; a new connection to a secured network needs it.
    Connect {
        ssid: Vec<u8>,
        password: Option<String>,
    },
    /// Disconnects the Wi-Fi device until the user connects again.
    Disconnect,
    /// Deletes every saved connection to the network with this SSID.
    Forget {
        ssid: Vec<u8>,
    },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SetWifiEnabled(enabled) => {
                f.debug_tuple("SetWifiEnabled").field(enabled).finish()
            }
            Command::Scan => f.write_str("Scan"),
            Command::Connect { ssid, password } => f
                .debug_struct("Connect")
                .field("ssid", &String::from_utf8_lossy(ssid))
                .field("password", &password.as_ref().map(|_| ".."))
                .finish(),
            Command::Disconnect => f.write_str("Disconnect"),
            Command::Forget { ssid } => {
                f.debug_struct("Forget").field("ssid", &String::from_utf8_lossy(ssid)).finish()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    State(Box<NetworkState>),
    /// Connecting to the network with this SSID failed after NetworkManager accepted the request.
    /// `needs_password` means the password was missing or wrong.
    ConnectFailed {
        ssid: Vec<u8>,
        needs_password: bool,
    },
    /// NetworkManager refused a [`Command`], with its reason.
    Failed(String),
}

/// Everything the network settings show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkState {
    /// Whether NetworkManager is running; everything else is empty without it.
    pub running: bool,
    pub wifi_enabled: bool,
    /// Whether a hardware switch allows Wi-Fi.
    pub wifi_hardware_enabled: bool,
    /// The first Wi-Fi device, if there's one.
    pub wifi: Option<Device>,
    /// The networks in range, one per SSID, strongest first.
    pub networks: Vec<WifiNetwork>,
    /// The Ethernet devices.
    pub wired: Vec<Device>,
}

/// A network interface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    /// The interface name, such as `wlan0`.
    pub interface: String,
    pub link: Link,
    pub hw_address: String,
    /// For Ethernet, whether a cable is plugged in.
    pub carrier: bool,
    /// The link speed in Mbit/s, or 0 when unknown.
    pub speed_mbps: u32,
    pub connection: Option<ConnectionDetails>,
}

/// A device's state, from NetworkManager's `NMDeviceState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Link {
    /// Unmanaged, or not ready, such as Wi-Fi switched off or Ethernet without a cable.
    #[default]
    Unavailable,
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Failed,
}

/// The active connection on a device.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectionDetails {
    /// The connection's name, which for Wi-Fi is usually the SSID.
    pub name: String,
    /// Addresses with their prefix length, such as `192.168.1.20/24`.
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
}

/// A Wi-Fi network in range: the access points that share an SSID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiNetwork {
    pub ssid: Vec<u8>,
    /// The SSID for display.
    pub name: String,
    /// Signal strength in percent, of the strongest access point.
    pub strength: u8,
    pub security: Security,
    /// The strongest access point's frequency in MHz.
    pub frequency_mhz: u32,
    /// Whether a saved connection exists for it.
    pub known: bool,
    /// Whether the Wi-Fi device is connected, or connecting, to it.
    pub active: bool,
}

/// A NetworkManager client on its own thread; dropping it stops the client.
pub struct Client {
    worker: Worker<Command>,
}

impl Client {
    /// Connects to NetworkManager on `system_bus` and calls `on_event` from the client's thread.
    /// The first event is an [`Event::State`], whose `running` is false while NetworkManager isn't available.
    pub fn spawn(system_bus: BusAddress, on_event: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let worker = Worker::spawn("nimbus-nm", system_bus, move || {
            service::NetworkSettings::new(Box::new(on_event))
        });
        Self { worker }
    }

    /// Queues `command`; it never blocks.
    pub fn send(&self, command: Command) {
        self.worker.send(command);
    }
}
