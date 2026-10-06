// SPDX-License-Identifier: MIT

//! The Bluetooth page: the adapter and devices from BlueZ, discovering while the page shows, and the pairing dialog.

use std::cell::Cell;
use std::rc::Rc;

use nimbus_services::bluez::{Answer, BluetoothState, Command, DeviceId, Event, PairingRequest};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Inner, Message, With, deliver, index_of, post};
use crate::bluetooth::{DeviceKind, PairingPrompt, adapter_status, device_status, typed_answer};
use crate::dispatch::Dispatch;
use crate::sources::{Control, Events};
use crate::{AppWindow, BluetoothDeviceItem, BluetoothModel};

/// What the page knows about Bluetooth.
#[derive(Default)]
pub(crate) struct Bluetooth {
    control: Option<Rc<dyn Control<Command>>>,
    state: BluetoothState,
    /// The request the pairing dialog shows.
    pairing: Option<PairingRequest>,
    /// Whether the page shows, which is while the adapter looks for devices.
    shown: bool,
    discovery_requested: bool,
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let model = ui.global::<BluetoothModel>();
    let h = with.clone();
    model.on_set_powered(move |on| h(&|i| i.send_bluetooth(Command::SetPowered(on))));
    let h = with.clone();
    model.on_set_discoverable(move |on| h(&|i| i.send_bluetooth(Command::SetDiscoverable(on))));
    let device_command = |command: fn(DeviceId) -> Command| {
        let h = with.clone();
        move |index: i32| {
            h(&|i| {
                if let Some(id) = i.device_id(index_of(index)) {
                    i.send_bluetooth(command(id));
                }
            })
        }
    };
    model.on_pair(device_command(Command::Pair));
    model.on_connect(device_command(Command::Connect));
    model.on_disconnect(device_command(Command::Disconnect));
    model.on_remove(device_command(Command::Remove));
    let h = with.clone();
    model.on_pairing_input_valid(move |text| {
        let valid = Cell::new(false);
        h(&|i| {
            let pairing = &i.state.borrow().bluetooth.pairing;
            valid.set(pairing.as_ref().is_some_and(|p| typed_answer(&p.kind, &text).is_some()));
        });
        valid.get()
    });
    let h = with.clone();
    model.on_pairing_accept(move || h(&|i| i.answer_pairing(true)));
    let h = with.clone();
    model.on_pairing_cancel(move || h(&|i| i.answer_pairing(false)));
}

impl Inner {
    /// Starts the BlueZ client, which is also the pairing agent while the app runs.
    pub fn start_bluetooth(&self) {
        let events: Events<Event> = match self.dispatch {
            Dispatch::Threaded => Box::new(|event| post(Message::Bluetooth(event))),
            Dispatch::Inline => Box::new(|event| deliver(Message::Bluetooth(event))),
        };
        // Inline sources report right away, so no borrow may be held here.
        let control = self.sources.bluetooth(events);
        self.state.borrow_mut().bluetooth.control = Some(Rc::from(control));
    }

    fn send_bluetooth(&self, command: Command) {
        // Inline sources report right away, so no borrow may be held while sending.
        let control = self.state.borrow().bluetooth.control.clone();
        if let Some(control) = control {
            control.send(command);
        }
    }

    fn device_id(&self, index: usize) -> Option<DeviceId> {
        self.state.borrow().bluetooth.state.devices.get(index).map(|device| device.id.clone())
    }

    /// Looks for devices while the page shows and the adapter is on, and stops once it's left.
    pub(super) fn show_bluetooth_page(&self, shown: bool) {
        let stop = {
            let mut state = self.state.borrow_mut();
            let bluetooth = &mut state.bluetooth;
            let stop = bluetooth.shown && !shown && bluetooth.discovery_requested;
            bluetooth.shown = shown;
            if stop {
                bluetooth.discovery_requested = false;
            }
            stop
        };
        if stop {
            self.send_bluetooth(Command::SetDiscovering(false));
        }
        self.discover_if_shown();
    }

    fn discover_if_shown(&self) {
        let start = {
            let mut state = self.state.borrow_mut();
            let bluetooth = &mut state.bluetooth;
            let powered = bluetooth.state.adapter.as_ref().is_some_and(|adapter| adapter.powered);
            if !powered {
                bluetooth.discovery_requested = false;
            }
            let start = bluetooth.shown && powered && !bluetooth.discovery_requested;
            bluetooth.discovery_requested |= start;
            start
        };
        if start {
            self.send_bluetooth(Command::SetDiscovering(true));
        }
    }

    fn answer_pairing(&self, accept: bool) {
        let Some(request) = self.state.borrow_mut().bluetooth.pairing.take() else { return };
        let mut input = String::new();
        self.with_ui(|ui| {
            let model = ui.global::<BluetoothModel>();
            input = model.get_pairing_input().to_string();
            model.set_pairing_input("".into());
            model.set_pairing_open(false);
        });
        let command = match (accept, request.kind.needs_answer()) {
            (_, false) => Command::CancelPairing(request.device_id),
            (true, true) => match typed_answer(&request.kind, &input) {
                Some(answer) => Command::Answer { id: request.id, answer },
                None => return,
            },
            (false, true) => Command::Answer { id: request.id, answer: Answer::Reject },
        };
        self.send_bluetooth(command);
    }

    pub(super) fn handle_bluetooth(&self, event: Event) {
        match event {
            Event::State(state) => {
                self.show_bluetooth(&state);
                self.state.borrow_mut().bluetooth.state = state;
                self.discover_if_shown();
            }
            Event::Pairing(request) => {
                let prompt = PairingPrompt::new(&request);
                let replaces = {
                    let pairing = &self.state.borrow().bluetooth.pairing;
                    pairing.as_ref().is_some_and(|shown| shown.id == request.id)
                };
                self.with_ui(|ui| {
                    let model = ui.global::<BluetoothModel>();
                    model.set_pairing_kind(prompt.kind);
                    model.set_pairing_title(prompt.title.into());
                    model.set_pairing_message(prompt.message.into());
                    model.set_pairing_code(prompt.code.into());
                    if !replaces {
                        model.set_pairing_input("".into());
                    }
                    model.set_pairing_open(true);
                });
                self.state.borrow_mut().bluetooth.pairing = Some(request);
            }
            Event::PairingEnded { id } => {
                let ended = {
                    let pairing = &mut self.state.borrow_mut().bluetooth.pairing;
                    let ended = pairing.as_ref().is_some_and(|shown| shown.id == id);
                    if ended {
                        *pairing = None;
                    }
                    ended
                };
                if ended {
                    self.with_ui(|ui| ui.global::<BluetoothModel>().set_pairing_open(false));
                }
            }
            Event::Failed { device, message } => {
                let text = if device.is_empty() {
                    format!("Bluetooth: {message}")
                } else {
                    format!("Bluetooth couldn't finish with {device}: {message}")
                };
                self.show_banner(&text, true);
            }
        }
    }

    fn show_bluetooth(&self, state: &BluetoothState) {
        let devices: Vec<BluetoothDeviceItem> = state
            .devices
            .iter()
            .map(|device| BluetoothDeviceItem {
                name: device.name.as_str().into(),
                status: device_status(device).into(),
                kind: DeviceKind::from_icon(&device.icon) as i32,
                paired: device.paired,
                connected: device.connected,
                busy: device.busy,
            })
            .collect();
        self.with_ui(|ui| {
            let model = ui.global::<BluetoothModel>();
            match &state.adapter {
                Some(adapter) => {
                    model.set_state(1);
                    model.set_adapter_name(adapter.name.as_str().into());
                    model.set_powered(adapter.powered);
                    model.set_discoverable(adapter.discoverable);
                    model.set_discovering(adapter.discovering);
                    model.set_status(adapter_status(adapter).into());
                }
                None => model.set_state(2),
            }
            model.set_devices(ModelRc::new(VecModel::from(devices)));
        });
    }
}
