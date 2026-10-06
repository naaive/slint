// SPDX-License-Identifier: MIT

//! NetworkManager's values turned into the client's types, and the settings of new Wi-Fi connections.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::{Link, WifiNetwork};
use crate::network::ssid_to_string;

// NM80211ApFlags and NM80211ApSecurityFlags.
const AP_PRIVACY: u32 = 0x1;
const KEY_MGMT_PSK: u32 = 0x100;
const KEY_MGMT_802_1X: u32 = 0x200;
const KEY_MGMT_SAE: u32 = 0x400;
const KEY_MGMT_OWE: u32 = 0x800 | 0x1000;
const KEY_MGMT_SUITE_B: u32 = 0x2000;

// NMActiveConnectionState and NMActiveConnectionStateReason.
const ACTIVATED: u32 = 2;
const DEACTIVATED: u32 = 4;
const REASON_USER_DISCONNECTED: u32 = 2;
const REASON_NO_SECRETS: u32 = 9;
const REASON_LOGIN_FAILED: u32 = 10;

/// How a Wi-Fi network is secured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Security {
    #[default]
    Open,
    /// Opportunistic Wireless Encryption ("Enhanced Open"): encrypted, without a password.
    Owe,
    Wep,
    /// WPA or WPA2 Personal, including WPA3 transition networks.
    Psk,
    /// WPA3 Personal.
    Sae,
    /// WPA Enterprise, which needs more than a password.
    Enterprise,
}

impl Security {
    /// Classifies an access point by its `Flags`, `WpaFlags`, and `RsnFlags`.
    pub(crate) fn classify(flags: u32, wpa_flags: u32, rsn_flags: u32) -> Self {
        let key_mgmt = wpa_flags | rsn_flags;
        if key_mgmt & (KEY_MGMT_802_1X | KEY_MGMT_SUITE_B) != 0 {
            Security::Enterprise
        } else if key_mgmt & KEY_MGMT_PSK != 0 {
            Security::Psk
        } else if key_mgmt & KEY_MGMT_SAE != 0 {
            Security::Sae
        } else if key_mgmt & KEY_MGMT_OWE != 0 {
            Security::Owe
        } else if flags & AP_PRIVACY != 0 {
            Security::Wep
        } else {
            Security::Open
        }
    }

    /// Whether joining needs a password.
    pub fn needs_password(self) -> bool {
        matches!(self, Security::Wep | Security::Psk | Security::Sae)
    }

    /// Whether [`super::Command::Connect`] can add a connection to such a network.
    pub fn can_connect(self) -> bool {
        self != Security::Enterprise
    }

    /// The `key-mgmt` of the `802-11-wireless-security` setting; `None` for open networks.
    fn key_mgmt(self) -> Option<&'static str> {
        match self {
            Security::Open | Security::Enterprise => None,
            Security::Owe => Some("owe"),
            Security::Wep => Some("none"),
            Security::Psk => Some("wpa-psk"),
            Security::Sae => Some("sae"),
        }
    }
}

/// Maps an `NMDeviceState`.
pub(crate) fn link(state: u32) -> Link {
    match state {
        30 => Link::Disconnected,
        40..=90 => Link::Connecting,
        100 => Link::Connected,
        110 => Link::Disconnecting,
        120 => Link::Failed,
        _ => Link::Unavailable,
    }
}

/// One access point, read from NetworkManager.
#[derive(Clone, Debug)]
pub(crate) struct AccessPoint {
    pub path: OwnedObjectPath,
    pub ssid: Vec<u8>,
    pub strength: u8,
    pub security: Security,
    pub frequency_mhz: u32,
}

/// Groups access points by SSID, strongest first; hidden networks, which have no SSID, are left out.
pub(crate) fn networks(
    access_points: &[AccessPoint],
    active: Option<&OwnedObjectPath>,
    known: &HashSet<Vec<u8>>,
) -> Vec<WifiNetwork> {
    let mut by_ssid: HashMap<&[u8], WifiNetwork> = HashMap::new();
    for ap in access_points {
        let Some(name) = ssid_to_string(&ap.ssid) else { continue };
        let is_active = active == Some(&ap.path);
        let network = by_ssid.entry(&ap.ssid).or_insert_with(|| WifiNetwork {
            ssid: ap.ssid.clone(),
            name,
            known: known.contains(&ap.ssid),
            ..WifiNetwork::default()
        });
        network.active |= is_active;
        if ap.strength >= network.strength {
            network.strength = ap.strength;
            network.security = ap.security;
            network.frequency_mhz = ap.frequency_mhz;
        }
    }
    let mut networks: Vec<WifiNetwork> = by_ssid.into_values().collect();
    networks.sort_by(|a, b| {
        b.active.cmp(&a.active).then(b.strength.cmp(&a.strength)).then(a.name.cmp(&b.name))
    });
    networks
}

/// A connection's settings, as `org.freedesktop.NetworkManager.Settings.Connection` takes them.
pub(crate) type Settings = HashMap<String, HashMap<String, OwnedValue>>;

fn owned(value: Value<'_>) -> OwnedValue {
    // Only fails for file descriptors, which settings never hold.
    value.try_to_owned().unwrap_or_else(|_| OwnedValue::from(false))
}

/// The settings of a new connection to a Wi-Fi network.
pub(crate) fn new_connection(
    ssid: &[u8],
    name: &str,
    security: Security,
    password: Option<&str>,
) -> Settings {
    let connection = HashMap::from([
        ("type".to_owned(), owned(Value::from("802-11-wireless"))),
        ("id".to_owned(), owned(Value::from(name))),
    ]);
    let mut wireless = HashMap::from([
        ("ssid".to_owned(), owned(Value::from(ssid))),
        ("mode".to_owned(), owned(Value::from("infrastructure"))),
    ]);
    let mut settings = Settings::from([("connection".to_owned(), connection)]);
    if let Some(key_mgmt) = security.key_mgmt() {
        wireless.insert("security".to_owned(), owned(Value::from("802-11-wireless-security")));
        settings.insert(
            "802-11-wireless-security".to_owned(),
            HashMap::from([("key-mgmt".to_owned(), owned(Value::from(key_mgmt)))]),
        );
        if let Some(password) = password {
            set_password(&mut settings, security, password);
        }
    }
    settings.insert("802-11-wireless".to_owned(), wireless);
    settings
}

/// Stores `password` in `settings`, for NetworkManager to keep with the connection.
pub(crate) fn set_password(settings: &mut Settings, security: Security, password: &str) {
    let section = settings.entry("802-11-wireless-security".to_owned()).or_default();
    if security == Security::Wep {
        section.insert("wep-key0".to_owned(), owned(Value::from(password)));
        // A passphrase of 5 or 13 characters, or a hex key of 10 or 26 digits.
        let key_type = if matches!(password.len(), 10 | 26) { 1u32 } else { 2u32 };
        section.insert("wep-key-type".to_owned(), owned(Value::from(key_type)));
        section.insert("wep-key-flags".to_owned(), owned(Value::from(0u32)));
    } else {
        section.insert("psk".to_owned(), owned(Value::from(password)));
        section.insert("psk-flags".to_owned(), owned(Value::from(0u32)));
    }
}

/// The SSID of a saved Wi-Fi connection, or `None` for any other connection.
pub(crate) fn saved_ssid(settings: &Settings) -> Option<Vec<u8>> {
    let kind = settings.get("connection")?.get("type")?;
    if !matches!(&**kind, Value::Str(kind) if kind.as_str() == "802-11-wireless") {
        return None;
    }
    let ssid = settings.get("802-11-wireless")?.get("ssid")?;
    Vec::<u8>::try_from(ssid.try_clone().ok()?).ok()
}

/// Addresses from an `AddressData` property, as `address/prefix`.
pub(crate) fn addresses(data: &[HashMap<String, OwnedValue>]) -> Vec<String> {
    data.iter()
        .filter_map(|entry| {
            let address = <&str>::try_from(&**entry.get("address")?).ok()?;
            match entry.get("prefix").and_then(|prefix| u32::try_from(&**prefix).ok()) {
                Some(prefix) => Some(format!("{address}/{prefix}")),
                None => Some(address.to_owned()),
            }
        })
        .collect()
}

/// Name servers from a `NameserverData` property.
pub(crate) fn nameservers(data: &[HashMap<String, OwnedValue>]) -> Vec<String> {
    data.iter()
        .filter_map(|entry| <&str>::try_from(&**entry.get("address")?).ok().map(str::to_owned))
        .collect()
}

/// Name servers from the older `Nameservers` property, IPv4 addresses in network byte order.
pub(crate) fn legacy_nameservers(addresses: &[u32]) -> Vec<String> {
    addresses.iter().map(|address| Ipv4Addr::from(address.to_ne_bytes()).to_string()).collect()
}

/// What an active connection's `StateChanged` means for a connection attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attempt {
    Pending,
    /// Connected, or the user cancelled.
    Finished,
    Failed {
        needs_password: bool,
    },
}

pub(crate) fn attempt(state: u32, reason: u32) -> Attempt {
    match state {
        ACTIVATED => Attempt::Finished,
        DEACTIVATED if reason == REASON_USER_DISCONNECTED => Attempt::Finished,
        DEACTIVATED => Attempt::Failed {
            needs_password: matches!(reason, REASON_NO_SECRETS | REASON_LOGIN_FAILED),
        },
        _ => Attempt::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(path: &str, ssid: &str, strength: u8, security: Security) -> AccessPoint {
        AccessPoint {
            path: OwnedObjectPath::try_from(path).unwrap(),
            ssid: ssid.as_bytes().to_vec(),
            strength,
            security,
            frequency_mhz: 2412 + u32::from(strength),
        }
    }

    #[test]
    fn classifies_security() {
        assert_eq!(Security::classify(0, 0, 0), Security::Open);
        assert_eq!(Security::classify(AP_PRIVACY, 0, 0), Security::Wep);
        assert_eq!(Security::classify(AP_PRIVACY, 0, 0x188), Security::Psk);
        assert_eq!(Security::classify(AP_PRIVACY, 0, 0x588), Security::Psk, "transition mode");
        assert_eq!(Security::classify(AP_PRIVACY, 0, 0x488), Security::Sae);
        assert_eq!(Security::classify(0, 0, 0x888), Security::Owe);
        assert_eq!(Security::classify(AP_PRIVACY, 0x248, 0x288), Security::Enterprise);
        assert!(Security::Sae.needs_password() && !Security::Owe.needs_password());
        assert!(!Security::Enterprise.can_connect() && Security::Wep.can_connect());
    }

    #[test]
    fn maps_device_states() {
        assert_eq!(link(10), Link::Unavailable);
        assert_eq!(link(20), Link::Unavailable);
        assert_eq!(link(30), Link::Disconnected);
        assert_eq!(link(60), Link::Connecting);
        assert_eq!(link(100), Link::Connected);
        assert_eq!(link(110), Link::Disconnecting);
        assert_eq!(link(120), Link::Failed);
    }

    #[test]
    fn groups_access_points_by_ssid() {
        let aps = [
            ap("/ap/1", "home", 40, Security::Psk),
            ap("/ap/2", "home", 80, Security::Sae),
            ap("/ap/3", "cafe", 90, Security::Open),
            ap("/ap/4", "", 99, Security::Open),
            ap("/ap/5", "office", 20, Security::Enterprise),
        ];
        let active = OwnedObjectPath::try_from("/ap/1").unwrap();
        let known = HashSet::from([b"home".to_vec()]);
        let networks = networks(&aps, Some(&active), &known);
        let names: Vec<&str> = networks.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            ["home", "cafe", "office"],
            "the active network leads, hidden ones are left out"
        );
        assert_eq!(networks[0].strength, 80);
        assert_eq!(networks[0].security, Security::Sae);
        assert_eq!(networks[0].frequency_mhz, 2492);
        assert!(networks[0].known && networks[0].active);
        assert!(!networks[1].known && !networks[1].active);
    }

    #[test]
    fn new_connections() {
        let open = new_connection(b"cafe", "cafe", Security::Open, None);
        assert!(!open.contains_key("802-11-wireless-security"));
        assert!(!open["802-11-wireless"].contains_key("security"));
        assert_eq!(saved_ssid(&open), Some(b"cafe".to_vec()));

        let psk = new_connection(b"home", "home", Security::Psk, Some("hunter22"));
        let security = &psk["802-11-wireless-security"];
        assert_eq!(<&str>::try_from(&*security["key-mgmt"]).unwrap(), "wpa-psk");
        assert_eq!(<&str>::try_from(&*security["psk"]).unwrap(), "hunter22");

        let mut wep = new_connection(b"old", "old", Security::Wep, None);
        assert!(!wep["802-11-wireless-security"].contains_key("wep-key0"));
        set_password(&mut wep, Security::Wep, "0123456789");
        assert_eq!(u32::try_from(&*wep["802-11-wireless-security"]["wep-key-type"]).unwrap(), 1);

        let mut wired = Settings::new();
        wired.insert(
            "connection".into(),
            HashMap::from([("type".into(), owned(Value::from("802-3-ethernet")))]),
        );
        assert_eq!(saved_ssid(&wired), None);
    }

    #[test]
    fn formats_addresses() {
        let entry = |address: &str, prefix: Option<u32>| {
            let mut entry = HashMap::from([("address".to_owned(), owned(Value::from(address)))]);
            if let Some(prefix) = prefix {
                entry.insert("prefix".to_owned(), owned(Value::from(prefix)));
            }
            entry
        };
        let data = [entry("192.168.1.20", Some(24)), entry("10.0.0.1", None)];
        assert_eq!(addresses(&data), ["192.168.1.20/24", "10.0.0.1"]);
        assert_eq!(nameservers(&data), ["192.168.1.20", "10.0.0.1"]);
        let one_one = u32::from_ne_bytes([1, 1, 1, 1]);
        let local = u32::from_ne_bytes([192, 168, 1, 1]);
        assert_eq!(legacy_nameservers(&[one_one, local]), ["1.1.1.1", "192.168.1.1"]);
    }

    #[test]
    fn connection_attempts() {
        assert_eq!(attempt(1, 0), Attempt::Pending);
        assert_eq!(attempt(2, 0), Attempt::Finished);
        assert_eq!(attempt(4, 9), Attempt::Failed { needs_password: true });
        assert_eq!(attempt(4, 6), Attempt::Failed { needs_password: false });
        assert_eq!(attempt(4, 2), Attempt::Finished, "the user cancelled it");
    }
}
