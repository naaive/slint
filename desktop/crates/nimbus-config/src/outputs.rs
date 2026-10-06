// SPDX-License-Identifier: MIT

//! Per-display settings: the `[[outputs]]` entries of the configuration.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Config;

/// The settings of one display, as the compositor last applied them.
///
/// An entry matches a display by make, model, and serial number when both have a serial number,
/// and by connector name otherwise.
/// Fields left out keep the display's default:
/// its preferred mode, its native orientation, `appearance.scale`, and a place to the right of the other displays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// The connector, such as `DP-1`.
    pub connector: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub enabled: bool,
    pub mode: Option<OutputMode>,
    /// The top-left corner in the global layout, in logical pixels.
    pub position: Option<[i32; 2]>,
    pub scale: Option<f64>,
    pub transform: Option<Transform>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            connector: String::new(),
            make: String::new(),
            model: String::new(),
            serial: String::new(),
            enabled: true,
            mode: None,
            position: None,
            scale: None,
            transform: None,
        }
    }
}

/// How a display identifies itself; empty fields are unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputId<'a> {
    pub connector: &'a str,
    pub make: &'a str,
    pub model: &'a str,
    pub serial: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    Connector,
    Identity,
}

impl OutputConfig {
    /// An entry with the defaults for the display `id`.
    pub fn new(id: OutputId<'_>) -> Self {
        Self {
            connector: id.connector.into(),
            make: id.make.into(),
            model: id.model.into(),
            serial: id.serial.into(),
            ..Self::default()
        }
    }

    pub fn id(&self) -> OutputId<'_> {
        OutputId {
            connector: &self.connector,
            make: &self.make,
            model: &self.model,
            serial: &self.serial,
        }
    }

    fn matches(&self, id: OutputId<'_>) -> Option<Match> {
        if !self.serial.is_empty() && !id.serial.is_empty() {
            let same = self.make == id.make && self.model == id.model && self.serial == id.serial;
            same.then_some(Match::Identity)
        } else {
            (self.connector == id.connector).then_some(Match::Connector)
        }
    }
}

impl Config {
    /// The entry for the display `id`; see [`OutputConfig`] for how entries match.
    pub fn output(&self, id: OutputId<'_>) -> Option<&OutputConfig> {
        self.output_index(id).map(|index| &self.outputs[index])
    }

    /// Replaces the entry that matches the display `entry` describes, or adds `entry`.
    pub fn set_output(&mut self, entry: OutputConfig) {
        match self.output_index(entry.id()) {
            Some(index) => self.outputs[index] = entry,
            None => self.outputs.push(entry),
        }
    }

    fn output_index(&self, id: OutputId<'_>) -> Option<usize> {
        self.outputs
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| Some((entry.matches(id)?, index)))
            // The best match, and the first entry among equally good ones.
            .min_by_key(|&(kind, index)| (std::cmp::Reverse(kind), index))
            .map(|(_, index)| index)
    }
}

/// A display mode, written as `2560x1440` or `2560x1440@59.951`, with the refresh rate in hertz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OutputMode {
    pub width: u32,
    pub height: u32,
    /// The refresh rate in millihertz; 0 picks the fastest rate at this size.
    pub refresh_mhz: u32,
}

impl fmt::Display for OutputMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)?;
        if self.refresh_mhz > 0 {
            write!(f, "@{}", format_hz(self.refresh_mhz, 3))?;
        }
        Ok(())
    }
}

/// A frequency in millihertz as hertz, rounded to `decimals` and without trailing zeros, such as `59.95`.
pub fn format_hz(mhz: u32, decimals: usize) -> String {
    let text = format!("{:.decimals$}", f64::from(mhz) / 1000.0);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid display mode '{0}'; expected a size such as '1920x1080' or '1920x1080@60'")]
pub struct InvalidOutputMode(String);

impl FromStr for OutputMode {
    type Err = InvalidOutputMode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidOutputMode(s.into());
        let (size, refresh) = match s.trim().split_once('@') {
            Some((size, refresh)) => (size, Some(refresh)),
            None => (s.trim(), None),
        };
        let (width, height) = size.split_once('x').ok_or_else(invalid)?;
        let dimension = |text: &str| text.trim().parse::<u32>().ok().filter(|&n| n > 0);
        let refresh_mhz = match refresh {
            Some(hz) => {
                let hz: f64 = hz.trim().parse().map_err(|_| invalid())?;
                if !hz.is_finite() || hz <= 0.0 || hz > 1000.0 {
                    return Err(invalid());
                }
                (hz * 1000.0).round() as u32
            }
            None => 0,
        };
        Ok(Self {
            width: dimension(width).ok_or_else(invalid)?,
            height: dimension(height).ok_or_else(invalid)?,
            refresh_mhz,
        })
    }
}

impl TryFrom<String> for OutputMode {
    type Error = InvalidOutputMode;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<OutputMode> for String {
    fn from(mode: OutputMode) -> Self {
        mode.to_string()
    }
}

/// A display's rotation, clockwise, and whether it's flipped, with the names and values of `wl_output.transform`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Transform {
    #[default]
    #[serde(rename = "normal")]
    Normal = 0,
    #[serde(rename = "90")]
    Rotate90 = 1,
    #[serde(rename = "180")]
    Rotate180 = 2,
    #[serde(rename = "270")]
    Rotate270 = 3,
    #[serde(rename = "flipped")]
    Flipped = 4,
    #[serde(rename = "flipped-90")]
    Flipped90 = 5,
    #[serde(rename = "flipped-180")]
    Flipped180 = 6,
    #[serde(rename = "flipped-270")]
    Flipped270 = 7,
}

impl Transform {
    const ALL: [Self; 8] = [
        Self::Normal,
        Self::Rotate90,
        Self::Rotate180,
        Self::Rotate270,
        Self::Flipped,
        Self::Flipped90,
        Self::Flipped180,
        Self::Flipped270,
    ];

    /// The clockwise rotation in quarter turns, `0..4`.
    pub fn rotation(self) -> u32 {
        self as u32 % 4
    }

    /// This transform turned to `quarter_turns` clockwise, flipped if it was.
    pub fn with_rotation(self, quarter_turns: u32) -> Self {
        let flip = self as u32 / 4 * 4;
        Self::ALL[(flip + quarter_turns % 4) as usize]
    }
}

impl TryFrom<u32> for Transform {
    type Error = u32;

    fn try_from(value: u32) -> Result<Self, u32> {
        Self::ALL.get(value as usize).copied().ok_or(value)
    }
}

impl From<Transform> for u32 {
    fn from(transform: Transform) -> Self {
        transform as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<'a>(connector: &'a str, serial: &'a str) -> OutputId<'a> {
        OutputId { connector, make: "DEL", model: "U2720Q", serial }
    }

    #[test]
    fn modes_parse_and_print() {
        let mode: OutputMode = "2560x1440@59.951".parse().unwrap();
        assert_eq!(mode, OutputMode { width: 2560, height: 1440, refresh_mhz: 59_951 });
        assert_eq!(mode.to_string(), "2560x1440@59.951");
        assert_eq!("1920x1080@60".parse::<OutputMode>().unwrap().to_string(), "1920x1080@60");
        assert_eq!(" 800x600 ".parse::<OutputMode>().unwrap().refresh_mhz, 0);
        assert_eq!("800x600".parse::<OutputMode>().unwrap().to_string(), "800x600");
        for bad in ["", "1920", "0x1080", "1920x1080@", "1920x1080@-5", "axb", "1920x1080@nan"] {
            assert!(bad.parse::<OutputMode>().is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn hertz_round_and_drop_trailing_zeros() {
        assert_eq!(format_hz(60_000, 2), "60");
        assert_eq!(format_hz(59_951, 2), "59.95");
        assert_eq!(format_hz(59_951, 3), "59.951");
        assert_eq!(format_hz(143_900, 2), "143.9");
        assert_eq!(format_hz(59_996, 2), "60");
        assert_eq!(format_hz(120_000, 0), "120");
    }

    #[test]
    fn transforms_have_wire_values() {
        for (value, transform) in Transform::ALL.into_iter().enumerate() {
            assert_eq!(u32::from(transform), value as u32);
            assert_eq!(Transform::try_from(value as u32), Ok(transform));
        }
        assert_eq!(Transform::try_from(8), Err(8));
    }

    #[test]
    fn rotation_keeps_flip() {
        assert_eq!(Transform::Normal.with_rotation(1), Transform::Rotate90);
        assert_eq!(Transform::Flipped90.with_rotation(2), Transform::Flipped180);
        assert_eq!(Transform::Rotate270.with_rotation(4), Transform::Normal);
        assert_eq!(Transform::Flipped270.rotation(), 3);
        assert_eq!(Transform::Rotate180.rotation(), 2);
    }

    #[test]
    fn entries_match_by_identity_before_connector() {
        let mut config = Config::default();
        config.set_output(OutputConfig { scale: Some(2.0), ..OutputConfig::new(id("DP-1", "A")) });
        config.set_output(OutputConfig { scale: Some(1.5), ..OutputConfig::new(id("DP-2", "")) });

        // The same monitor on another connector keeps its settings.
        assert_eq!(config.output(id("DP-2", "A")).and_then(|o| o.scale), Some(2.0));
        // Another monitor with a serial number doesn't take over a connector's entry that has one.
        assert!(config.output(id("DP-1", "B")).is_none());
        // Without a serial number, the connector decides.
        assert_eq!(config.output(id("DP-2", "")).and_then(|o| o.scale), Some(1.5));
        assert_eq!(config.output(id("DP-1", "")).and_then(|o| o.scale), Some(2.0));

        config.set_output(OutputConfig { scale: Some(1.0), ..OutputConfig::new(id("DP-3", "A")) });
        assert_eq!(config.outputs.len(), 2, "the entry with the same identity was replaced");
        assert_eq!(config.outputs[0].connector, "DP-3");
    }

    #[test]
    fn entries_round_trip_through_toml() {
        let config = Config {
            outputs: vec![
                OutputConfig {
                    mode: Some("3840x2160@60".parse().unwrap()),
                    position: Some([1920, -200]),
                    scale: Some(1.5),
                    transform: Some(Transform::Rotate90),
                    ..OutputConfig::new(id("DP-1", "A"))
                },
                OutputConfig { enabled: false, ..OutputConfig::new(id("HDMI-A-1", "")) },
            ],
            ..Config::default()
        };
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("mode = \"3840x2160@60\""), "{text}");
        assert!(text.contains("transform = \"90\""), "{text}");
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);

        let parsed: Config =
            toml::from_str("[[outputs]]\nconnector = \"eDP-1\"\nscale = 2.0\n").unwrap();
        assert_eq!(parsed.outputs[0].scale, Some(2.0));
        assert!(parsed.outputs[0].enabled);
        assert!(toml::from_str::<Config>("[[outputs]]\nmode = \"big\"\n").is_err());
    }

    #[test]
    fn saving_over_a_file_updates_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# Mine\n[panel]\nheight = 40\n\n[[outputs]]\nconnector = \"DP-1\"\n",
        )
        .unwrap();
        let mut config = Config::load_from(&path).unwrap();
        config.set_output(OutputConfig {
            position: Some([0, 0]),
            ..OutputConfig::new(id("DP-1", ""))
        });
        config.set_output(OutputConfig::new(id("DP-2", "")));
        config.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# Mine\n[panel]"), "{text}");
        assert_eq!(Config::load_from(&path).unwrap(), config);
    }
}
