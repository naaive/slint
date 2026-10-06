// SPDX-License-Identifier: MIT

//! A client for sound settings: the output and input devices of PipeWire or PulseAudio,
//! their volume and mute state, and the default device of each direction.
//!
//! It runs `pactl`, which PipeWire serves through `pipewire-pulse`, and follows `pactl subscribe`.
//! [`Client::spawn`] runs it on a thread of its own, apart from [`crate::Services`].

mod parse;
mod service;

use std::path::PathBuf;

use crate::worker::Worker;

/// Whether a device plays or records sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// A sink, such as speakers or headphones.
    Output,
    /// A source, such as a microphone; monitors of sinks aren't listed.
    Input,
}

/// A request to the sound server; failures arrive as [`Event::Failed`].
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Makes the device named `name` the default of its direction, which moves playing or recording streams to it.
    SetDefault {
        direction: Direction,
        name: String,
    },
    /// Sets every channel of the device to `volume`, in `0.0..=1.5` where `1.0` is 100 %.
    SetVolume {
        direction: Direction,
        name: String,
        volume: f32,
    },
    SetMute {
        direction: Direction,
        name: String,
        muted: bool,
    },
    /// Reads every device again.
    Refresh,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    State(SoundState),
    /// The sound server refused a [`Command`], with its reason.
    Failed(String),
}

/// Everything the sound settings show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundState {
    /// Whether `pactl` reaches a sound server; the device lists are empty without one.
    pub available: bool,
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
}

impl SoundState {
    pub fn devices(&self, direction: Direction) -> &[Device] {
        match direction {
            Direction::Output => &self.outputs,
            Direction::Input => &self.inputs,
        }
    }

    pub fn devices_mut(&mut self, direction: Direction) -> &mut Vec<Device> {
        match direction {
            Direction::Output => &mut self.outputs,
            Direction::Input => &mut self.inputs,
        }
    }
}

/// A sink or source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    /// The sound server's name for it, which commands take.
    pub name: String,
    /// The name for people, such as "Built-in Audio Analog Stereo".
    pub description: String,
    /// The average of its channels, in `0.0..=1.5` where `1.0` is 100 %.
    pub volume: f32,
    pub muted: bool,
    /// Whether it's the default device of its direction.
    pub default: bool,
}

/// A sound settings client on its own thread; dropping it stops the client.
pub struct Client {
    worker: Worker<Command>,
}

impl Client {
    /// Runs `pactl` from `PATH` and calls `on_event` from the client's thread.
    /// The first event is an [`Event::State`], whose `available` is false without a sound server or `pactl`.
    pub fn spawn(on_event: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self::spawn_with("pactl", on_event)
    }

    /// Like [`Client::spawn`], running `program` in place of `pactl`.
    pub fn spawn_with(
        program: impl Into<PathBuf>,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> Self {
        let program = program.into();
        let worker = Worker::run("nimbus-sound", move |commands| {
            service::run(program, Box::new(on_event), commands)
        });
        Self { worker }
    }

    /// Queues `command`; it never blocks.
    pub fn send(&self, command: Command) {
        self.worker.send(command);
    }
}
