// SPDX-License-Identifier: MIT

//! The Network page's logic: describing Wi-Fi networks and connections, checking passwords,
//! and sample networks for screenshots and tests.

use std::sync::Mutex;

use nimbus_services::nm::{
    Command, ConnectionDetails, Device, Event, Link, NetworkState, Security, WifiNetwork,
};

use crate::sources::{Control, Events};

/// The bars of the Wi-Fi icon, from 0 to 3, as the panel shows them.
pub fn strength_bars(strength: u8) -> i32 {
    match strength {
        76.. => 3,
        51..=75 => 2,
        26..=50 => 1,
        _ => 0,
    }
}

pub fn security_label(security: Security) -> &'static str {
    match security {
        Security::Open => "Open",
        Security::Owe => "Enhanced Open",
        Security::Wep => "WEP",
        Security::Psk => "WPA2 Personal",
        Security::Sae => "WPA3 Personal",
        Security::Enterprise => "Enterprise",
    }
}

/// The frequency band, such as `5 GHz`.
pub fn band(frequency_mhz: u32) -> Option<&'static str> {
    match frequency_mhz {
        2400..=2500 => Some("2.4 GHz"),
        4900..=5900 => Some("5 GHz"),
        5925..=7125 => Some("6 GHz"),
        _ => None,
    }
}

/// Whether NetworkManager takes `password` for a network with this security.
pub fn password_acceptable(security: Security, password: &str) -> bool {
    let hex = |len: usize| password.len() == len && password.bytes().all(|b| b.is_ascii_hexdigit());
    match security {
        // A passphrase of 8 to 63 characters, or a raw key of 64 hex digits.
        Security::Psk => (8..=63).contains(&password.len()) || hex(64),
        Security::Sae => !password.is_empty(),
        // A passphrase of 5 or 13 characters, or a key of 10 or 26 hex digits.
        Security::Wep => matches!(password.len(), 5 | 13) || hex(10) || hex(26),
        Security::Open | Security::Owe | Security::Enterprise => true,
    }
}

/// The subtitle of a network in the list.
pub fn network_status(network: &WifiNetwork, link: Link) -> String {
    let security = match network.security {
        Security::Open => "Open",
        Security::Owe => "Encrypted",
        Security::Enterprise => "Enterprise",
        Security::Wep | Security::Psk | Security::Sae => "Secured",
    };
    let state = match (network.active, link) {
        (true, Link::Connecting) => Some("Connecting…"),
        (true, Link::Connected) => Some("Connected"),
        (true, Link::Disconnecting) => Some("Disconnecting…"),
        (_, _) if network.known => Some("Saved"),
        _ => None,
    };
    match state {
        Some(state) => format!("{state} · {security}"),
        None => security.to_owned(),
    }
}

/// What the Wi-Fi switch's row says.
pub fn wifi_status(state: &NetworkState) -> String {
    let Some(wifi) = &state.wifi else { return "No Wi-Fi adapter".into() };
    if !state.wifi_hardware_enabled {
        return "Turned off with a hardware switch".into();
    }
    if !state.wifi_enabled {
        return "Off".into();
    }
    match (wifi.link, &wifi.connection) {
        (Link::Connected, Some(connection)) => format!("Connected to {}", connection.name),
        (Link::Connecting, Some(connection)) => format!("Connecting to {}…", connection.name),
        (Link::Connecting, None) => "Connecting…".into(),
        (Link::Failed, _) => "Couldn't connect".into(),
        _ => "Not connected".into(),
    }
}

/// What a wired device's row says.
pub fn wired_status(device: &Device) -> String {
    match device.link {
        Link::Connected if device.speed_mbps > 0 => {
            format!("Connected · {} Mb/s", device.speed_mbps)
        }
        Link::Connected => "Connected".into(),
        Link::Connecting => "Connecting…".into(),
        Link::Disconnecting => "Disconnecting…".into(),
        Link::Failed => "Couldn't connect".into(),
        _ if !device.carrier => "Cable unplugged".into(),
        _ => "Not connected".into(),
    }
}

fn address_rows(connection: &ConnectionDetails, rows: &mut Vec<(String, String)>) {
    let mut row = |label: &str, value: String| {
        if !value.is_empty() {
            rows.push((label.to_owned(), value));
        }
    };
    row("IPv4 address", connection.ipv4.join(", "));
    row("IPv6 address", connection.ipv6.join(", "));
    row("Gateway", connection.gateway.clone().unwrap_or_default());
    row("DNS", connection.dns.join(", "));
}

/// The details of an active connection on `device`, as label and value; empty without a connection.
/// `network` adds what's particular to Wi-Fi.
pub fn details(device: &Device, network: Option<&WifiNetwork>) -> Vec<(String, String)> {
    let Some(connection) = &device.connection else { return Vec::new() };
    let mut rows = Vec::new();
    if let Some(network) = network {
        rows.push(("Security".to_owned(), security_label(network.security).to_owned()));
        let signal = match band(network.frequency_mhz) {
            Some(band) => format!("{}% · {band}", network.strength),
            None => format!("{}%", network.strength),
        };
        rows.push(("Signal".to_owned(), signal));
    }
    if device.speed_mbps > 0 {
        rows.push(("Link speed".to_owned(), format!("{} Mb/s", device.speed_mbps)));
    }
    address_rows(connection, &mut rows);
    if !device.hw_address.is_empty() {
        rows.push(("Hardware address".to_owned(), device.hw_address.clone()));
    }
    rows
}

/// Networks that behave like a NetworkManager would, reporting synchronously.
pub struct SampleNetwork {
    state: Mutex<NetworkState>,
    events: Events<Event>,
}

/// The sample network whose saved password is missing, so connecting asks for it.
const NEEDS_PASSWORD: &[u8] = b"Library";

impl SampleNetwork {
    pub fn new(events: Events<Event>) -> Self {
        let network = |name: &str, strength, security, frequency_mhz, known, active| WifiNetwork {
            ssid: name.as_bytes().to_vec(),
            name: name.into(),
            strength,
            security,
            frequency_mhz,
            known,
            active,
        };
        let state = NetworkState {
            running: true,
            wifi_enabled: true,
            wifi_hardware_enabled: true,
            wifi: Some(Device {
                interface: "wlp2s0".into(),
                link: Link::Connected,
                hw_address: "3C:21:9C:5A:0E:71".into(),
                carrier: false,
                speed_mbps: 866,
                connection: Some(ConnectionDetails {
                    name: "Nimbus HQ".into(),
                    ipv4: vec!["192.168.1.42/24".into()],
                    ipv6: vec!["fd00::5a0e:71/64".into()],
                    gateway: Some("192.168.1.1".into()),
                    dns: vec!["192.168.1.1".into()],
                }),
            }),
            networks: vec![
                network("Nimbus HQ", 86, Security::Sae, 5180, true, true),
                network("Nimbus Guest", 72, Security::Open, 2437, false, false),
                network("Library", 58, Security::Psk, 2412, true, false),
                network("Corner Café", 44, Security::Owe, 5240, false, false),
                network("eduroam", 38, Security::Enterprise, 5500, false, false),
                network("Neighbor 5G", 21, Security::Psk, 5745, false, false),
            ],
            wired: vec![Device {
                interface: "enp0s31f6".into(),
                link: Link::Unavailable,
                hw_address: "8C:16:45:2D:19:B0".into(),
                ..Device::default()
            }],
        };
        let sample = Self { state: Mutex::new(state), events };
        sample.report();
        sample
    }

    fn report(&self) {
        let state = self.state.lock().map(|state| state.clone()).unwrap_or_default();
        (self.events)(Event::State(Box::new(state)));
    }

    fn connect(state: &mut NetworkState, ssid: &[u8], password: Option<&str>) -> Option<Event> {
        let index = state.networks.iter().position(|n| n.ssid == ssid)?;
        let network = &state.networks[index];
        let asks = ssid == NEEDS_PASSWORD || !network.known && network.security.needs_password();
        if asks && password.is_none() {
            return Some(Event::ConnectFailed { ssid: ssid.to_vec(), needs_password: true });
        }
        for (i, network) in state.networks.iter_mut().enumerate() {
            network.active = i == index;
        }
        let network = &mut state.networks[index];
        network.known = true;
        let name = network.name.clone();
        if let Some(wifi) = &mut state.wifi {
            wifi.link = Link::Connected;
            let connection = wifi.connection.get_or_insert_with(ConnectionDetails::default);
            connection.name = name;
        }
        None
    }
}

impl Control<Command> for SampleNetwork {
    fn send(&self, command: Command) {
        let event = {
            let Ok(mut state) = self.state.lock() else { return };
            match command {
                Command::SetWifiEnabled(enabled) => {
                    state.wifi_enabled = enabled;
                    None
                }
                Command::Scan => None,
                Command::Connect { ssid, password } => {
                    Self::connect(&mut state, &ssid, password.as_deref())
                }
                Command::Disconnect => {
                    state.networks.iter_mut().for_each(|network| network.active = false);
                    if let Some(wifi) = &mut state.wifi {
                        wifi.link = Link::Disconnected;
                        wifi.connection = None;
                    }
                    None
                }
                Command::Forget { ssid } => {
                    if let Some(network) = state.networks.iter_mut().find(|n| n.ssid == ssid) {
                        network.known = false;
                    }
                    None
                }
            }
        };
        if let Some(event) = event {
            (self.events)(event);
        }
        self.report();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_matches_the_panel() {
        assert_eq!([0, 25, 26, 50, 51, 75, 76, 100].map(strength_bars), [0, 0, 1, 1, 2, 2, 3, 3]);
    }

    #[test]
    fn passwords() {
        assert!(!password_acceptable(Security::Psk, "short"));
        assert!(password_acceptable(Security::Psk, "long enough"));
        assert!(password_acceptable(Security::Psk, &"a".repeat(64)));
        assert!(!password_acceptable(Security::Psk, &"g".repeat(64)));
        assert!(password_acceptable(Security::Sae, "x"));
        assert!(!password_acceptable(Security::Sae, ""));
        assert!(password_acceptable(Security::Wep, "abcde"));
        assert!(password_acceptable(Security::Wep, "0123456789"));
        assert!(!password_acceptable(Security::Wep, "abcdef"));
        assert!(password_acceptable(Security::Open, ""));
    }

    #[test]
    fn describes_networks_and_devices() {
        let network = WifiNetwork {
            name: "home".into(),
            strength: 80,
            security: Security::Psk,
            frequency_mhz: 5180,
            known: true,
            active: true,
            ..WifiNetwork::default()
        };
        assert_eq!(network_status(&network, Link::Connected), "Connected · Secured");
        assert_eq!(network_status(&network, Link::Connecting), "Connecting… · Secured");
        let saved = WifiNetwork { active: false, ..network.clone() };
        assert_eq!(network_status(&saved, Link::Connected), "Saved · Secured");
        let open = WifiNetwork { known: false, security: Security::Open, ..saved };
        assert_eq!(network_status(&open, Link::Connected), "Open");

        let mut device = Device { hw_address: "AA".into(), speed_mbps: 866, ..Device::default() };
        assert!(details(&device, Some(&network)).is_empty(), "no details without a connection");
        device.connection = Some(ConnectionDetails {
            name: "home".into(),
            ipv4: vec!["10.0.0.2/24".into()],
            gateway: Some("10.0.0.1".into()),
            ..ConnectionDetails::default()
        });
        let labels: Vec<String> = details(&device, Some(&network))
            .into_iter()
            .map(|(label, value)| format!("{label}: {value}"))
            .collect();
        assert_eq!(
            labels,
            [
                "Security: WPA2 Personal",
                "Signal: 80% · 5 GHz",
                "Link speed: 866 Mb/s",
                "IPv4 address: 10.0.0.2/24",
                "Gateway: 10.0.0.1",
                "Hardware address: AA",
            ]
        );

        let unplugged = Device::default();
        assert_eq!(wired_status(&unplugged), "Cable unplugged");
        let wired =
            Device { link: Link::Connected, carrier: true, speed_mbps: 1000, ..Device::default() };
        assert_eq!(wired_status(&wired), "Connected · 1000 Mb/s");

        let mut state = NetworkState { wifi_hardware_enabled: true, ..NetworkState::default() };
        assert_eq!(wifi_status(&state), "No Wi-Fi adapter");
        state.wifi = Some(device);
        assert_eq!(wifi_status(&state), "Off");
        state.wifi_enabled = true;
        state.wifi.as_mut().unwrap().link = Link::Connected;
        assert_eq!(wifi_status(&state), "Connected to home");
        state.wifi_hardware_enabled = false;
        assert_eq!(wifi_status(&state), "Turned off with a hardware switch");
    }
}
