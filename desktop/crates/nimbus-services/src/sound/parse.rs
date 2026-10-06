// SPDX-License-Identifier: MIT

//! Parsing `pactl --format=json`, which `pactl` 16 and later print.

use std::collections::HashMap;

use serde::Deserialize;

use super::Device;
use crate::audio::MAX_VOLUME;

/// `PA_VOLUME_NORM`: the raw volume of 100 %.
pub(crate) const VOLUME_NORM: f32 = 65536.0;

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    default_sink_name: Option<String>,
    #[serde(default)]
    default_source_name: Option<String>,
}

#[derive(Deserialize)]
struct Channel {
    value: f32,
}

#[derive(Deserialize)]
struct RawDevice {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: HashMap<String, Channel>,
    /// The sink a source monitors; `pactl` prints `n/a` or `null` for other sources.
    #[serde(default)]
    monitor_of_sink: Option<String>,
    #[serde(default)]
    properties: HashMap<String, serde_json::Value>,
}

impl RawDevice {
    fn is_monitor(&self) -> bool {
        let monitors = self.monitor_of_sink.as_deref().is_some_and(|s| !s.is_empty() && s != "n/a");
        monitors || self.properties.get("device.class").and_then(|c| c.as_str()) == Some("monitor")
    }

    fn into_device(self, default: Option<&str>) -> Device {
        let volume = if self.volume.is_empty() {
            0.0
        } else {
            let sum: f32 = self.volume.values().map(|c| c.value).sum();
            sum / self.volume.len() as f32 / VOLUME_NORM
        };
        let volume = if volume.is_finite() { volume.clamp(0.0, MAX_VOLUME) } else { 0.0 };
        Device {
            default: default == Some(self.name.as_str()),
            description: self
                .description
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| self.name.clone()),
            name: self.name,
            volume,
            muted: self.mute,
        }
    }
}

/// The default sink and source from `pactl --format=json info`.
pub(crate) fn defaults(json: &str) -> serde_json::Result<(Option<String>, Option<String>)> {
    let info: Info = serde_json::from_str(json)?;
    Ok((info.default_sink_name, info.default_source_name))
}

/// The devices from `pactl --format=json list sinks` or `list sources`, leaving out monitors.
pub(crate) fn devices(json: &str, default: Option<&str>) -> serde_json::Result<Vec<Device>> {
    let raw: Vec<RawDevice> = serde_json::from_str(json)?;
    Ok(raw.into_iter().filter(|d| !d.is_monitor()).map(|d| d.into_device(default)).collect())
}

/// Whether a `pactl subscribe` line can change a device or a default.
pub(crate) fn is_device_event(line: &str) -> bool {
    line.starts_with("Event ")
        && [" on sink #", " on source #", " on server", " on card #"]
            .iter()
            .any(|on| line.contains(on))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINKS: &str = r#"[
        {"index":56,"state":"RUNNING","name":"alsa_output.analog-stereo","description":"Built-in Audio Analog Stereo",
         "mute":false,"volume":{"front-left":{"value":32768,"value_percent":"50%","db":"-18.06 dB"},
         "front-right":{"value":49152,"value_percent":"75%","db":"-7.50 dB"}},
         "monitor_source":"alsa_output.analog-stereo.monitor","properties":{"device.class":"sound"}},
        {"index":71,"name":"bluez_output.headphones","description":"","mute":true,
         "volume":{"mono":{"value":131072}}}
    ]"#;

    #[test]
    fn sinks() {
        let sinks = devices(SINKS, Some("bluez_output.headphones")).unwrap();
        assert_eq!(
            sinks,
            [
                Device {
                    name: "alsa_output.analog-stereo".into(),
                    description: "Built-in Audio Analog Stereo".into(),
                    volume: 0.625,
                    muted: false,
                    default: false,
                },
                Device {
                    name: "bluez_output.headphones".into(),
                    description: "bluez_output.headphones".into(),
                    volume: MAX_VOLUME,
                    muted: true,
                    default: true,
                },
            ]
        );
    }

    #[test]
    fn sources_leave_out_monitors() {
        let sources = r#"[
            {"name":"alsa_output.analog-stereo.monitor","description":"Monitor of Built-in Audio","monitor_of_sink":"alsa_output.analog-stereo","volume":{}},
            {"name":"virtual.monitor","properties":{"device.class":"monitor"}},
            {"name":"alsa_input.analog-stereo","description":"Built-in Microphone","monitor_of_sink":"n/a","volume":{"mono":{"value":65536}}},
            {"name":"usb.mic","description":"USB Microphone","monitor_of_sink":null}
        ]"#;
        let names: Vec<String> =
            devices(sources, None).unwrap().into_iter().map(|d| d.description).collect();
        assert_eq!(names, ["Built-in Microphone", "USB Microphone"]);
    }

    #[test]
    fn info_and_errors() {
        let info = r#"{"server_name":"PulseAudio (on PipeWire 1.2.7)","default_sink_name":"a","default_source_name":"b"}"#;
        assert_eq!(defaults(info).unwrap(), (Some("a".into()), Some("b".into())));
        assert_eq!(defaults("{}").unwrap(), (None, None));
        assert!(devices("Connection failure: Connection refused", None).is_err());
    }

    #[test]
    fn subscribe_events() {
        assert!(is_device_event("Event 'change' on sink #56"));
        assert!(is_device_event("Event 'new' on source #80"));
        assert!(is_device_event("Event 'change' on server #-1"));
        assert!(is_device_event("Event 'change' on card #47"));
        assert!(!is_device_event("Event 'change' on sink-input #102"));
        assert!(!is_device_event("Event 'remove' on source-output #9"));
        assert!(!is_device_event("garbage"));
    }
}
