// SPDX-License-Identifier: MIT

//! The Sound page's logic: describing devices, and a sample sound server for screenshots and tests.

use std::sync::Mutex;

use nimbus_services::sound::{Command, Device, Direction, Event, SoundState};

use crate::sources::{Control, Events};

/// A volume in `0.0..=1.5` as a whole percentage, as the sliders show it.
pub fn percent(volume: f32) -> f32 {
    (volume * 100.0).round()
}

/// The bars of the volume icon: 0 when muted or silent, then 1 to 3, as the panel shows them.
pub fn level(device: &Device) -> i32 {
    match device.volume {
        _ if device.muted => 0,
        v if v <= 0.0 => 0,
        v if v < 0.34 => 1,
        v if v < 0.67 => 2,
        _ => 3,
    }
}

/// A sound server that behaves like PipeWire would, reporting synchronously.
pub struct SampleSound {
    state: Mutex<SoundState>,
    events: Events<Event>,
}

impl SampleSound {
    pub fn new(events: Events<Event>) -> Self {
        let device = |name: &str, description: &str, volume, default| Device {
            name: name.into(),
            description: description.into(),
            volume,
            muted: false,
            default,
        };
        let state = SoundState {
            available: true,
            outputs: vec![
                device("alsa_output.pci-0000_00_1f.3.analog-stereo", "Speakers", 0.62, true),
                device("bluez_output.AC_80_0A_2E_51_17.1", "WH-1000XM5", 0.4, false),
                device(
                    "alsa_output.pci-0000_00_1f.3.hdmi-stereo",
                    "DELL U2720Q (HDMI)",
                    1.0,
                    false,
                ),
            ],
            inputs: vec![
                device(
                    "alsa_input.pci-0000_00_1f.3.analog-stereo",
                    "Built-in Microphone",
                    0.8,
                    true,
                ),
                device("alsa_input.usb-Blue_Yeti", "Yeti Stereo Microphone", 0.55, false),
            ],
        };
        let sample = Self { state: Mutex::new(state), events };
        sample.report();
        sample
    }

    fn report(&self) {
        let state = self.state.lock().map(|state| state.clone()).unwrap_or_default();
        (self.events)(Event::State(state));
    }

    /// Applies `command`, or returns why it can't.
    fn apply(state: &mut SoundState, command: Command) -> Result<(), String> {
        let (direction, name) = match &command {
            Command::SetDefault { direction, name }
            | Command::SetVolume { direction, name, .. }
            | Command::SetMute { direction, name, .. } => (*direction, name.clone()),
            Command::Refresh => return Ok(()),
        };
        let devices = state.devices_mut(direction);
        let Some(index) = devices.iter().position(|d| d.name == name) else {
            return Err("Failure: No such entity".into());
        };
        match command {
            Command::SetDefault { .. } => {
                devices.iter_mut().enumerate().for_each(|(i, d)| d.default = i == index);
            }
            Command::SetVolume { volume, .. } => devices[index].volume = volume.clamp(0.0, 1.5),
            Command::SetMute { muted, .. } => devices[index].muted = muted,
            Command::Refresh => {}
        }
        Ok(())
    }
}

impl Control<Command> for SampleSound {
    fn send(&self, command: Command) {
        let result = match self.state.lock() {
            Ok(mut state) => Self::apply(&mut state, command),
            Err(_) => return,
        };
        if let Err(reason) = result {
            (self.events)(Event::Failed(reason));
        }
        self.report();
    }
}

/// The direction of the UI's `input` flag.
pub fn direction(input: bool) -> Direction {
    if input { Direction::Input } else { Direction::Output }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_percentages() {
        let device = |volume, muted| Device { volume, muted, ..Device::default() };
        let levels = [(0.0, false), (0.2, false), (0.5, false), (0.9, false), (0.9, true)]
            .map(|(volume, muted)| level(&device(volume, muted)));
        assert_eq!(levels, [0, 1, 2, 3, 0]);
        assert_eq!(percent(0.555), 56.0);
        assert_eq!(percent(1.5), 150.0);
    }
}
