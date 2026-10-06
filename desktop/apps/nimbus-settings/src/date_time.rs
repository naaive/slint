// SPDX-License-Identifier: MIT

//! The Date & Time page's logic: describing time zones and synchronization,
//! and a sample timedated for screenshots and tests.

use std::sync::Mutex;

use chrono::NaiveDateTime;
use nimbus_services::timedate::{Command, Event, TimeState};

use crate::sources::{Control, Events};

/// The place a time zone is named after, such as `Buenos Aires` for `America/Argentina/Buenos_Aires`.
pub fn city(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// What the automatic time switch's row says.
pub fn sync_status(state: &TimeState) -> &'static str {
    match state {
        TimeState { can_ntp: false, .. } => "No time synchronization service is installed",
        TimeState { ntp: true, synchronized: true, .. } => "Synchronized with network time servers",
        TimeState { ntp: true, .. } => "Waiting for a network time server…",
        _ => "Set by hand",
    }
}

/// The date and time, such as `Monday, October 5, 09:41`.
pub fn now_text(now: NaiveDateTime, twenty_four_hour: bool) -> String {
    let time = if twenty_four_hour { "%H:%M" } else { "%-I:%M %p" };
    now.format(&format!("%A, %B %-d, {time}")).to_string()
}

/// A timedated that takes every change, reporting synchronously.
pub struct SampleTime {
    state: Mutex<TimeState>,
    events: Events<Event>,
}

/// The sample time zones.
pub const SAMPLE_ZONES: [&str; 12] = [
    "Africa/Nairobi",
    "America/Argentina/Buenos_Aires",
    "America/Los_Angeles",
    "America/New_York",
    "Asia/Kolkata",
    "Asia/Tokyo",
    "Australia/Sydney",
    "Europe/Berlin",
    "Europe/London",
    "Europe/Paris",
    "Pacific/Auckland",
    "UTC",
];

impl SampleTime {
    pub fn new(events: Events<Event>) -> Self {
        let state = TimeState {
            available: true,
            timezone: "Europe/Berlin".into(),
            can_ntp: true,
            ntp: true,
            synchronized: true,
        };
        let sample = Self { state: Mutex::new(state), events };
        sample.report();
        (sample.events)(Event::Timezones(SAMPLE_ZONES.map(String::from).to_vec()));
        sample
    }

    fn report(&self) {
        let state = self.state.lock().map(|state| state.clone()).unwrap_or_default();
        (self.events)(Event::State(state));
    }
}

impl Control<Command> for SampleTime {
    fn send(&self, command: Command) {
        let failure = {
            let Ok(mut state) = self.state.lock() else { return };
            match command {
                Command::SetTimezone(zone) if SAMPLE_ZONES.contains(&zone.as_str()) => {
                    state.timezone = zone;
                    None
                }
                Command::SetTimezone(_) => Some("Invalid or unknown time zone"),
                Command::SetNtp(on) => {
                    state.ntp = on;
                    state.synchronized = on;
                    None
                }
                Command::Refresh => None,
            }
        };
        if let Some(reason) = failure {
            (self.events)(Event::Failed(reason.into()));
        }
        self.report();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn describes_zones_and_synchronization() {
        assert_eq!(city("America/Argentina/Buenos_Aires"), "Buenos Aires");
        assert_eq!(city("UTC"), "UTC");
        let mut state = TimeState { can_ntp: true, ntp: true, ..TimeState::default() };
        assert_eq!(sync_status(&state), "Waiting for a network time server…");
        state.synchronized = true;
        assert_eq!(sync_status(&state), "Synchronized with network time servers");
        state.ntp = false;
        assert_eq!(sync_status(&state), "Set by hand");
        state.can_ntp = false;
        assert_eq!(sync_status(&state), "No time synchronization service is installed");

        let now =
            NaiveDate::from_ymd_opt(2026, 10, 5).and_then(|d| d.and_hms_opt(14, 7, 0)).unwrap();
        assert_eq!(now_text(now, true), "Monday, October 5, 14:07");
        assert_eq!(now_text(now, false), "Monday, October 5, 2:07 PM");
    }
}
