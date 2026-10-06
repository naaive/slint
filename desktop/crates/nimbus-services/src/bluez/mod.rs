// SPDX-License-Identifier: MIT

//! A BlueZ client for Bluetooth settings: the adapter, discovering devices, pairing, and connecting them.
//!
//! [`Client::spawn`] runs it on a thread of its own, apart from [`crate::Services`].
//! While it runs, it's BlueZ's default pairing agent (`org.bluez.Agent1`),
//! which turns each question BlueZ asks into an [`Event::Pairing`] and waits for the [`Command::Answer`].

mod agent;
mod service;

use crate::BusAddress;
use crate::worker::Worker;

/// A device's object path, such as `/org/bluez/hci0/dev_00_11_22_33_44_55`.
pub type DeviceId = String;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    SetPowered(bool),
    /// Lets other devices find the adapter, for as long as BlueZ's `DiscoverableTimeout`.
    SetDiscoverable(bool),
    /// Starts or stops looking for devices.
    SetDiscovering(bool),
    /// Pairs, trusts, and connects the device.
    Pair(DeviceId),
    Connect(DeviceId),
    Disconnect(DeviceId),
    /// Removes the device and its pairing.
    Remove(DeviceId),
    /// Answers the [`PairingRequest`] with this id.
    Answer {
        id: u32,
        answer: Answer,
    },
    /// Stops pairing the device, such as while it shows a code to type.
    CancelPairing(DeviceId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    State(BluetoothState),
    /// BlueZ asks a question while pairing, or shows a code; the request lasts until [`Event::PairingEnded`].
    /// A request with an id that's already shown replaces it.
    Pairing(PairingRequest),
    PairingEnded {
        id: u32,
    },
    /// A command failed; `device` is the name of the device it was for, or empty.
    Failed {
        device: String,
        message: String,
    },
}

/// Everything the Bluetooth settings show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothState {
    /// The first adapter; `None` without BlueZ or an adapter.
    pub adapter: Option<Adapter>,
    /// The adapter's devices: connected first, then paired ones, then by name.
    /// Devices that are neither paired nor named are left out.
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adapter {
    /// The name other devices see.
    pub name: String,
    pub address: String,
    pub powered: bool,
    pub discoverable: bool,
    pub discovering: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub id: DeviceId,
    pub address: String,
    pub name: String,
    /// A freedesktop icon name, such as `audio-headset` or `input-keyboard`, or empty.
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    /// Signal strength in dBm while discovering.
    pub rssi: Option<i16>,
    /// Battery charge in percent.
    pub battery: Option<u8>,
    /// Whether a pair, connect, or disconnect request for it is in progress.
    pub busy: bool,
}

/// A question or code from BlueZ during pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingRequest {
    pub id: u32,
    pub device_id: DeviceId,
    /// The device's name.
    pub device: String,
    pub kind: PairingKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingKind {
    /// Confirm that the device shows this six-digit passkey; answer [`Answer::Accept`] or [`Answer::Reject`].
    Confirm { passkey: u32 },
    /// Type this passkey on the device; `entered` counts the digits typed so far. Needs no answer.
    DisplayPasskey { passkey: u32, entered: u16 },
    /// Type this PIN on the device. Needs no answer.
    DisplayPin { pin: String },
    /// Enter the passkey the device shows; answer [`Answer::Passkey`].
    EnterPasskey,
    /// Enter the device's PIN; answer [`Answer::Pin`].
    EnterPin,
    /// Allow the device to pair, without a code.
    Authorize,
    /// Allow the device to use a service, named by its UUID.
    AuthorizeService { uuid: String },
}

impl PairingKind {
    /// Whether the request waits for a [`Command::Answer`].
    pub fn needs_answer(&self) -> bool {
        !matches!(self, PairingKind::DisplayPasskey { .. } | PairingKind::DisplayPin { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Accept,
    Reject,
    Passkey(u32),
    Pin(String),
}

/// A BlueZ client and pairing agent on its own thread; dropping it stops the client.
pub struct Client {
    worker: Worker<Command>,
}

impl Client {
    /// Connects to BlueZ on `system_bus` and calls `on_event` from the client's thread.
    /// The first event is an [`Event::State`], whose `adapter` is `None` while BlueZ or an adapter isn't available.
    pub fn spawn(system_bus: BusAddress, on_event: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let worker = Worker::spawn("nimbus-bluez", system_bus, move || {
            service::BluetoothSettings::new(std::sync::Arc::new(on_event))
        });
        Self { worker }
    }

    /// Queues `command`; it never blocks.
    pub fn send(&self, command: Command) {
        self.worker.send(command);
    }
}
