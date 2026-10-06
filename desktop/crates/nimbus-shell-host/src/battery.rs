// SPDX-License-Identifier: MIT

//! Low battery toasts, at the warning levels UPower reports.

use crate::state::State;
use nimbus_services::{Battery, BatteryWarning, CloseReason, Notification, ServiceEvent, Urgency};
use nimbus_shell::short_duration;
use std::time::SystemTime;

/// The toast for the lowest warning level the battery reached since it last charged.
#[derive(Default)]
pub struct BatteryAlert {
    announced: BatteryWarning,
    toast: Option<u32>,
}

enum Change<'a> {
    Announce(&'a Battery),
    Clear,
    Keep,
}

impl BatteryAlert {
    fn change<'a>(&self, battery: Option<&'a Battery>) -> Change<'a> {
        match battery {
            Some(battery) if battery.warning > self.announced => Change::Announce(battery),
            Some(battery) if battery.warning > BatteryWarning::None => Change::Keep,
            _ if self.announced > BatteryWarning::None => Change::Clear,
            _ => Change::Keep,
        }
    }
}

fn toast(id: u32, battery: &Battery) -> Notification {
    let percent = (battery.level * 100.0).round();
    let left = match battery.time_to_empty {
        Some(time) => format!("About {} left ({percent}%)", short_duration(time)),
        None => format!("{percent}% left"),
    };
    let (summary, body, icon, urgency) = match battery.warning {
        BatteryWarning::Critical => (
            "Battery critically low",
            format!("Connect the charger now. {left}"),
            "battery-caution",
            Urgency::Critical,
        ),
        _ => ("Battery low", left, "battery-low", Urgency::Normal),
    };
    Notification {
        id,
        app_name: "Power".into(),
        app_icon: icon.into(),
        summary: summary.into(),
        body,
        actions: Vec::new(),
        urgency,
        expire_timeout: None,
        received: SystemTime::now(),
        transient: false,
        resident: false,
    }
}

impl State {
    /// Shows a toast when the battery reaches a lower warning level, and takes it back once the battery charges.
    pub fn battery_changed(&mut self, battery: Option<&Battery>) {
        match self.services.battery.change(battery) {
            Change::Keep => {}
            Change::Clear => {
                tracing::info!("battery warning cleared");
                let alert = std::mem::take(&mut self.services.battery);
                if let Some(id) = alert.toast {
                    let reason = CloseReason::Closed;
                    self.model
                        .handle_service_event(&ServiceEvent::NotificationClosed { id, reason });
                }
            }
            Change::Announce(battery) => {
                tracing::info!(level = ?battery.warning, "battery warning");
                // A critical toast replaces the low one.
                let id = match self.services.battery.toast {
                    Some(id) => id,
                    None => self.services.next_local_id(),
                };
                self.services.battery =
                    BatteryAlert { announced: battery.warning, toast: Some(id) };
                self.model.handle_service_event(&ServiceEvent::Notification(toast(id, battery)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn battery(level: f32, warning: BatteryWarning) -> Battery {
        let charging = false;
        Battery { level, charging, time_to_empty: Some(Duration::from_secs(45 * 60)), warning }
    }

    /// Feeds `batteries` in turn, and lists the warning levels announced and the clears.
    fn run(batteries: &[Option<Battery>]) -> Vec<String> {
        let mut alert = BatteryAlert::default();
        let mut changes = Vec::new();
        for battery in batteries {
            match alert.change(battery.as_ref()) {
                Change::Announce(battery) => {
                    changes.push(format!("{:?}", battery.warning));
                    alert.announced = battery.warning;
                }
                Change::Clear => {
                    changes.push("clear".into());
                    alert.announced = BatteryWarning::None;
                }
                Change::Keep => {}
            }
        }
        changes
    }

    #[test]
    fn announces_each_level_once_per_crossing() {
        use BatteryWarning::{Critical, Low, None as Fine};
        let charged = Battery { charging: true, ..battery(0.06, Fine) };
        let changes = run(&[
            Some(battery(0.5, Fine)),
            Some(battery(0.1, Low)),
            Some(battery(0.09, Low)),
            Some(battery(0.05, Critical)),
            // Back to low, as after recalibrating, isn't a new crossing.
            Some(battery(0.06, Low)),
            Some(battery(0.04, Critical)),
            Some(charged),
            Some(battery(0.08, Low)),
            None,
        ]);
        assert_eq!(changes, ["Low", "Critical", "clear", "Low", "clear"]);
    }

    #[test]
    fn critical_right_away_skips_low() {
        let changes = run(&[None, Some(battery(0.03, BatteryWarning::Critical))]);
        assert_eq!(changes, ["Critical"]);
    }

    #[test]
    fn toasts_say_how_long_the_battery_lasts() {
        let low = toast(3, &battery(0.1, BatteryWarning::Low));
        assert_eq!((low.id, low.summary.as_str()), (3, "Battery low"));
        assert_eq!(low.body, "About 45 min left (10%)");
        assert_eq!((low.app_icon.as_str(), low.urgency), ("battery-low", Urgency::Normal));

        let critical = Battery { time_to_empty: None, ..battery(0.04, BatteryWarning::Critical) };
        let critical = toast(3, &critical);
        assert_eq!(critical.summary, "Battery critically low");
        assert_eq!(critical.body, "Connect the charger now. 4% left");
        assert_eq!(critical.urgency, Urgency::Critical);
    }
}
