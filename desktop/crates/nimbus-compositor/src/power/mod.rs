// SPDX-License-Identifier: MIT

//! Output power: blanking after `power.blank_after_minutes` of inactivity, and wlr-output-power-management.
//!
//! An output that's off isn't rendered; on udev, its CRTC is off (DPMS).
//! Input turns blanked outputs back on, but not those a client turned off.

mod management;

pub use management::OutputPowerState;

use crate::state::Nimbus;
use nimbus_ipc::{Event, PowerState};
use smithay::output::Output;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Why an output is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerOff {
    /// Blanked after inactivity or by `Request::Blank`.
    Blank,
    /// Turned off through wlr-output-power-management.
    Client,
}

pub struct OutputPower {
    /// The enabled outputs that are off, by name.
    off: BTreeMap<String, PowerOff>,
    blank_after_minutes: u32,
    /// The length of a minute of `blank_after_minutes`; shorter on the headless backend in tests.
    minute: Duration,
    /// The last input, or the last time an idle inhibitor was visible.
    last_activity: Instant,
    reported: PowerState,
}

impl OutputPower {
    pub fn new(blank_after_minutes: u32) -> Self {
        Self {
            off: BTreeMap::new(),
            blank_after_minutes,
            minute: Duration::from_secs(60),
            last_activity: Instant::now(),
            reported: PowerState::default(),
        }
    }

    pub fn set_blank_after_minutes(&mut self, minutes: u32) {
        self.blank_after_minutes = minutes;
        self.last_activity = Instant::now();
    }

    pub fn set_minute(&mut self, minute: Duration) {
        self.minute = minute;
    }

    pub fn is_off(&self, name: &str) -> bool {
        self.off.contains_key(name)
    }

    /// Restarts the inactivity timeout without turning anything on.
    pub fn restart_timeout(&mut self, now: Instant) {
        self.last_activity = now;
    }

    /// Whether the inactivity timeout ran out at `now`.
    fn timed_out(&self, now: Instant) -> bool {
        self.blank_after_minutes > 0
            && now.duration_since(self.last_activity) >= self.minute * self.blank_after_minutes
    }

    fn state(&self) -> PowerState {
        PowerState {
            blanked: self.off.values().any(|&why| why == PowerOff::Blank),
            off: self.off.keys().cloned().collect(),
        }
    }
}

impl Nimbus {
    /// Whether `output` is on; outputs that are off show nothing and get no frames.
    pub fn output_powered(&self, output: &Output) -> bool {
        !self.power.is_off(&output.name())
    }

    /// Turns every output that's on off until the next input, and restarts the inactivity timeout,
    /// so an output a client turns on meanwhile stays on for a whole timeout.
    pub fn blank(&mut self) {
        self.power.restart_timeout(Instant::now());
        let names: Vec<String> = self.outputs().map(Output::name).collect();
        for name in names {
            self.power.off.entry(name).or_insert(PowerOff::Blank);
        }
    }

    /// Turns `output` on or off for a wlr-output-power-management client.
    pub fn set_output_power(&mut self, output: &str, on: bool) {
        if on {
            self.power.off.remove(output);
        } else {
            self.power.off.insert(output.to_owned(), PowerOff::Client);
        }
    }

    /// Restarts the inactivity timeout and turns blanked outputs back on, for user input.
    pub fn wake(&mut self) {
        self.power.restart_timeout(Instant::now());
        self.power.off.retain(|_, why| *why != PowerOff::Blank);
    }

    /// Blanks the outputs once the inactivity timeout runs out, unless a visible surface inhibits idling.
    pub fn blank_when_idle(&mut self) {
        let now = Instant::now();
        if self.idle_inhibited() {
            self.power.restart_timeout(now);
        } else if self.power.timed_out(now) {
            tracing::info!("blanking the screens after inactivity");
            self.blank();
        }
    }

    pub fn power_state(&self) -> PowerState {
        self.power.state()
    }

    /// Brings output power objects, redraws, and control socket subscribers in line with the power state.
    pub fn sync_power_state(&mut self) {
        let enabled: Vec<String> = self.outputs().map(Output::name).collect();
        self.power.off.retain(|name, _| enabled.contains(name));
        self.output_power.retain_outputs(&enabled);
        let state = self.power.state();
        if state == self.power.reported {
            return;
        }
        let old = std::mem::replace(&mut self.power.reported, state.clone());
        let switched = enabled.into_iter().filter(|n| old.off.contains(n) != state.off.contains(n));
        self.pending_redraws.extend(switched);
        self.output_power.send_modes(&old.off, &state.off);
        self.push_event(Event::PowerState(state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timeout_runs_out_after_the_configured_minutes() {
        let mut power = OutputPower::new(2);
        power.set_minute(Duration::from_millis(10));
        let start = Instant::now();
        power.restart_timeout(start);
        assert!(!power.timed_out(start + Duration::from_millis(19)));
        assert!(power.timed_out(start + Duration::from_millis(20)));
        power.set_blank_after_minutes(0);
        assert!(!power.timed_out(start + Duration::from_secs(3600)), "0 never blanks");
    }

    #[test]
    fn the_state_lists_outputs_that_are_off() {
        let mut power = OutputPower::new(5);
        assert_eq!(power.state(), PowerState::default());
        power.off.insert("B-1".into(), PowerOff::Client);
        assert_eq!(power.state(), PowerState { blanked: false, off: vec!["B-1".into()] });
        power.off.insert("A-1".into(), PowerOff::Blank);
        assert_eq!(
            power.state(),
            PowerState { blanked: true, off: vec!["A-1".into(), "B-1".into()] }
        );
    }
}
