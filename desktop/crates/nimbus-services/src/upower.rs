// SPDX-License-Identifier: MIT

//! Battery state from UPower's display device on the system bus.

use std::convert::Infallible;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use zbus::Connection;
use zbus::names::OwnedUniqueName;

use crate::bus::{self, BusService, Props, Wake};
use crate::hub::{Update, Updates};
use crate::{Battery, BatteryWarning};

const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE_INTERFACE: &str = "org.freedesktop.UPower.Device";

// Values of the UPower.Device `Type`, `State`, and `WarningLevel` properties.
const TYPE_BATTERY: u32 = 2;
const TYPE_UPS: u32 = 3;
const STATE_CHARGING: u32 = 1;
const STATE_FULLY_CHARGED: u32 = 4;
const STATE_PENDING_CHARGE: u32 = 5;
const WARNING_LOW: u32 = 3;
const WARNING_CRITICAL: u32 = 4;
const WARNING_ACTION: u32 = 5;

pub(crate) fn parse_battery(mut props: Props) -> Option<Battery> {
    let present = props.take::<bool>("IsPresent").unwrap_or(false);
    let kind = props.take::<u32>("Type").unwrap_or(0);
    if !present || !matches!(kind, TYPE_BATTERY | TYPE_UPS) {
        return None;
    }
    let percentage = props.take::<f64>("Percentage")?;
    let state = props.take::<u32>("State").unwrap_or(0);
    let charging = matches!(state, STATE_CHARGING | STATE_FULLY_CHARGED | STATE_PENDING_CHARGE);
    let time_to_empty = props
        .take::<i64>("TimeToEmpty")
        .filter(|_| !charging)
        .and_then(|seconds| u64::try_from(seconds).ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs);
    let warning = match props.take::<u32>("WarningLevel") {
        _ if charging => BatteryWarning::None,
        Some(WARNING_LOW) => BatteryWarning::Low,
        Some(WARNING_CRITICAL | WARNING_ACTION) => BatteryWarning::Critical,
        _ => BatteryWarning::None,
    };
    let level = (percentage / 100.0).clamp(0.0, 1.0) as f32;
    Some(Battery { level, charging, time_to_empty, warning })
}

pub(crate) struct UPower {
    updates: Updates,
}

impl UPower {
    pub(crate) fn new(updates: Updates) -> Self {
        Self { updates }
    }
}

impl BusService for UPower {
    type Command = Infallible;
    const NAME: &'static str = "org.freedesktop.UPower";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<Infallible>,
    ) -> zbus::Result<()> {
        let mut signals =
            bus::signals(conn, [bus::properties_changed_rule(owner, DISPLAY_DEVICE)?]).await?;
        loop {
            let props = bus::get_all(conn, owner, DISPLAY_DEVICE, DEVICE_INTERFACE).await?;
            self.updates.send(Update::Battery(parse_battery(props)));
            match bus::next_wake(&mut signals, commands).await? {
                Some(Wake::Signal) => {}
                Some(Wake::Command(never)) => match never {},
                None => return Ok(()),
            }
        }
    }

    fn unavailable(&mut self) {
        self.updates.send(Update::Battery(None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedValue, Value};

    fn device(present: bool, kind: u32, percentage: f64, state: u32, tte: i64) -> Props {
        Props::from_pairs([
            ("IsPresent", Value::Bool(present)),
            ("Type", Value::U32(kind)),
            ("Percentage", Value::F64(percentage)),
            ("State", Value::U32(state)),
            ("TimeToEmpty", Value::I64(tte)),
        ])
    }

    fn warning_level(mut props: Props, level: u32) -> Props {
        props.0.insert("WarningLevel".into(), OwnedValue::from(level));
        props
    }

    fn empty() -> Battery {
        Battery { level: 0.0, charging: false, time_to_empty: None, warning: BatteryWarning::None }
    }

    #[test]
    fn discharging_battery() {
        let battery = parse_battery(device(true, TYPE_BATTERY, 42.0, 2, 3600));
        assert_eq!(
            battery,
            Some(Battery {
                level: 0.42,
                charging: false,
                time_to_empty: Some(Duration::from_secs(3600)),
                warning: BatteryWarning::None,
            })
        );
    }

    #[test]
    fn charging_and_full_hide_time_to_empty() {
        for state in [STATE_CHARGING, STATE_FULLY_CHARGED, STATE_PENDING_CHARGE] {
            let battery = parse_battery(device(true, TYPE_BATTERY, 100.0, state, 3600));
            assert_eq!(battery, Some(Battery { level: 1.0, charging: true, ..empty() }));
        }
    }

    #[test]
    fn unknown_time_and_out_of_range_level() {
        let battery = parse_battery(device(true, TYPE_UPS, 140.0, 2, 0));
        assert_eq!(battery, Some(Battery { level: 1.0, ..empty() }));
    }

    #[test]
    fn warning_levels() {
        let warning = |level, state| {
            let props = warning_level(device(true, TYPE_BATTERY, 5.0, state, 0), level);
            parse_battery(props).unwrap().warning
        };
        assert_eq!(warning(1, 2), BatteryWarning::None);
        assert_eq!(warning(WARNING_LOW, 2), BatteryWarning::Low);
        assert_eq!(warning(WARNING_CRITICAL, 2), BatteryWarning::Critical);
        assert_eq!(warning(WARNING_ACTION, 2), BatteryWarning::Critical);
        assert_eq!(warning(WARNING_CRITICAL, STATE_CHARGING), BatteryWarning::None);
        assert_eq!(
            parse_battery(device(true, TYPE_BATTERY, 5.0, 2, 0)).unwrap().warning,
            BatteryWarning::None
        );
    }

    #[test]
    fn no_battery() {
        assert_eq!(parse_battery(device(false, TYPE_BATTERY, 50.0, 2, 0)), None);
        assert_eq!(parse_battery(device(true, 0, 0.0, 0, 0)), None);
        assert_eq!(parse_battery(Props::default()), None);
    }
}
