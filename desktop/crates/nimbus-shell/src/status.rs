// SPDX-License-Identifier: MIT

//! Maps the services' system state onto the shell's status model.

use nimbus_services::{ConnectionKind, SystemState};

use crate::clock::short_duration;
use crate::{NetworkKind, SystemStatus};

/// The status shown in the panel and quick settings, with the slider levels in percent.
pub struct Status {
    pub status: SystemStatus,
    pub volume_percent: f32,
    pub brightness_percent: f32,
}

pub fn status(state: &SystemState) -> Status {
    let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
    let battery = state.battery.as_ref();
    let battery_level = battery.map_or(0.0, |b| finite(b.level).clamp(0.0, 1.0));
    let battery_detail = match battery {
        Some(b) if b.charging && battery_level >= 0.99 => "Fully charged".to_owned(),
        Some(b) if b.charging => "Charging".to_owned(),
        Some(b) => {
            b.time_to_empty.map(|t| format!("{} left", short_duration(t))).unwrap_or_default()
        }
        None => String::new(),
    };
    let network = &state.network;
    let bluetooth = state.bluetooth.as_ref();
    let bluetooth_detail = match bluetooth {
        Some(b) if !b.powered => "Off".to_owned(),
        Some(b) if b.connected_devices == 1 => "1 device".to_owned(),
        Some(b) if b.connected_devices > 1 => format!("{} devices", b.connected_devices),
        Some(_) => "On".to_owned(),
        None => String::new(),
    };
    let media = state.media.as_ref();
    let status = SystemStatus {
        has_battery: battery.is_some(),
        battery_level,
        charging: battery.is_some_and(|b| b.charging),
        battery_detail: battery_detail.into(),
        network: match network.kind {
            ConnectionKind::None => NetworkKind::None,
            ConnectionKind::Ethernet => NetworkKind::Ethernet,
            ConnectionKind::Wifi => NetworkKind::Wifi,
        },
        network_name: network.ssid.clone().unwrap_or_default().into(),
        wifi_strength: finite(network.strength).clamp(0.0, 1.0),
        wifi_enabled: network.wifi_enabled,
        online: network.available,
        has_audio: state.audio.is_some(),
        muted: state.audio.as_ref().is_some_and(|a| a.muted),
        has_brightness: state.brightness.is_some(),
        has_bluetooth: bluetooth.is_some(),
        bluetooth_powered: bluetooth.is_some_and(|b| b.powered),
        bluetooth_detail: bluetooth_detail.into(),
        has_media: media.is_some(),
        media_title: media.map(|m| m.title.clone()).unwrap_or_default().into(),
        media_artist: media.map(|m| m.artist.clone()).unwrap_or_default().into(),
        media_player: media.map(|m| player_name(&m.player)).unwrap_or_default().into(),
        media_playing: media.is_some_and(|m| m.playing),
    };
    Status {
        status,
        volume_percent: state
            .audio
            .as_ref()
            .map_or(0.0, |a| finite(a.volume).clamp(0.0, 1.5) * 100.0),
        brightness_percent: state.brightness.map_or(0.0, |b| finite(b).clamp(0.0, 1.0) * 100.0),
    }
}

/// A readable player name from an MPRIS bus name, such as "Spotify" for `org.mpris.MediaPlayer2.spotify`.
fn player_name(bus_name: &str) -> String {
    let name = bus_name.strip_prefix("org.mpris.MediaPlayer2.").unwrap_or(bus_name);
    // Players append `.instance123` when several run at once.
    let name = name.split('.').next().unwrap_or(name);
    let mut chars = name.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_services::{Audio, Battery, BatteryWarning, Bluetooth, Media, Network};
    use std::time::Duration;

    #[test]
    fn empty_state_hides_everything() {
        let s = status(&SystemState::default());
        assert!(!s.status.has_battery && !s.status.has_audio && !s.status.has_brightness);
        assert!(!s.status.has_bluetooth && !s.status.has_media);
        assert_eq!(s.status.network, NetworkKind::None);
        assert_eq!(s.volume_percent, 0.0);
    }

    #[test]
    fn full_state() {
        let state = SystemState {
            battery: Some(Battery {
                level: 0.42,
                charging: false,
                time_to_empty: Some(Duration::from_secs(7500)),
                warning: BatteryWarning::None,
            }),
            network: Network {
                kind: ConnectionKind::Wifi,
                ssid: Some("Home".into()),
                strength: 0.8,
                wifi_enabled: true,
                available: true,
            },
            audio: Some(Audio { volume: 2.0, muted: true }),
            brightness: Some(f32::NAN),
            media: Some(Media {
                player: "org.mpris.MediaPlayer2.spotify.instance42".into(),
                title: "Song".into(),
                artist: "Band".into(),
                art_url: None,
                playing: true,
            }),
            bluetooth: Some(Bluetooth { powered: true, connected_devices: 2 }),
            do_not_disturb: false,
        };
        let s = status(&state);
        assert_eq!(s.status.battery_detail, "2 h 5 min left");
        assert_eq!(s.status.network, NetworkKind::Wifi);
        assert_eq!(s.status.network_name, "Home");
        assert_eq!(s.volume_percent, 150.0);
        assert!(s.status.muted);
        assert_eq!(s.brightness_percent, 0.0);
        assert_eq!(s.status.media_player, "Spotify");
        assert_eq!(s.status.bluetooth_detail, "2 devices");
    }

    #[test]
    fn battery_details() {
        let battery = |level, charging| {
            Some(Battery { level, charging, time_to_empty: None, warning: BatteryWarning::None })
        };
        let mut state = SystemState { battery: battery(1.0, true), ..Default::default() };
        assert_eq!(status(&state).status.battery_detail, "Fully charged");
        state.battery = battery(0.5, true);
        assert_eq!(status(&state).status.battery_detail, "Charging");
        state.battery = battery(0.5, false);
        assert_eq!(status(&state).status.battery_detail, "");
    }
}
