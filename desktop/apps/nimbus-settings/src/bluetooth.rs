// SPDX-License-Identifier: MIT

//! The Bluetooth page's logic: describing devices and pairing requests,
//! and sample devices for screenshots and tests.

use std::sync::Mutex;

use nimbus_services::bluez::{
    Adapter, Answer, BluetoothState, Command, Device, Event, PairingKind, PairingRequest,
};

use crate::sources::{Control, Events};

/// What a device is, for its icon; the UI matches the numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Other = 0,
    Audio = 1,
    Keyboard = 2,
    Mouse = 3,
    Computer = 4,
}

impl DeviceKind {
    /// Classifies a device by its freedesktop icon name, as BlueZ reports it.
    pub fn from_icon(icon: &str) -> Self {
        match icon {
            "input-keyboard" => DeviceKind::Keyboard,
            "input-mouse" | "input-tablet" => DeviceKind::Mouse,
            "computer" => DeviceKind::Computer,
            icon if icon.starts_with("audio-") => DeviceKind::Audio,
            _ => DeviceKind::Other,
        }
    }
}

/// The subtitle of a device in the list.
pub fn device_status(device: &Device) -> String {
    let status = match (device.busy, device.connected, device.paired) {
        (true, true, _) => "Disconnecting…",
        (true, false, true) => "Connecting…",
        (true, false, false) => "Pairing…",
        (false, true, _) => "Connected",
        (false, false, true) => "Not connected",
        (false, false, false) => "Not set up",
    };
    match device.battery {
        Some(battery) if device.connected => format!("{status} · Battery {battery}%"),
        _ => status.to_owned(),
    }
}

/// What the Bluetooth switch's row says.
pub fn adapter_status(adapter: &Adapter) -> String {
    match (adapter.powered, adapter.discoverable) {
        (false, _) => "Off".into(),
        (true, true) => format!("Visible as “{}”", adapter.name),
        (true, false) => "On".into(),
    }
}

/// A name for a Bluetooth service, for asking whether a device may use it.
pub fn service_name(uuid: &str) -> &'static str {
    // The 16-bit assigned number inside the Bluetooth base UUID, `0000xxxx-0000-1000-8000-00805f9b34fb`.
    let short = uuid
        .strip_prefix("0000")
        .and_then(|rest| rest.get(..4))
        .filter(|_| uuid.to_ascii_lowercase().ends_with("-0000-1000-8000-00805f9b34fb"))
        .and_then(|short| u16::from_str_radix(short, 16).ok());
    match short {
        Some(0x110a..=0x110e) => "audio",
        Some(0x1108 | 0x1112 | 0x111e | 0x111f) => "calls",
        Some(0x1124 | 0x1812) => "input",
        Some(0x1105 | 0x1106) => "file transfer",
        Some(0x112f) => "the phone book",
        Some(0x1132..=0x1134) => "messages",
        Some(0x1115 | 0x1116) => "network access",
        _ => "a service",
    }
}

/// The pairing dialog for a request; the UI matches the `kind` numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingPrompt {
    /// 0: confirm a code, 1: type a code on the device, 2: enter a passkey, 3: enter a PIN, 4: allow.
    pub kind: i32,
    pub title: String,
    pub message: String,
    pub code: String,
}

impl PairingPrompt {
    pub fn new(request: &PairingRequest) -> Self {
        let device = &request.device;
        let (kind, title, message, code) = match &request.kind {
            PairingKind::Confirm { passkey } => (
                0,
                format!("Pair with {device}?"),
                format!("Make sure {device} shows this code."),
                format!("{passkey:06}"),
            ),
            PairingKind::DisplayPasskey { passkey, entered } => (
                1,
                format!("Pair with {device}"),
                match entered {
                    0 => format!("Type this code on {device}, then press Enter."),
                    n => format!(
                        "Type this code on {device}, then press Enter. {n} of 6 digits typed."
                    ),
                },
                format!("{passkey:06}"),
            ),
            PairingKind::DisplayPin { pin } => (
                1,
                format!("Pair with {device}"),
                format!("Type this PIN on {device}, then press Enter."),
                pin.clone(),
            ),
            PairingKind::EnterPasskey => (
                2,
                format!("Pair with {device}"),
                format!("Enter the six-digit code that {device} shows."),
                String::new(),
            ),
            PairingKind::EnterPin => (
                3,
                format!("Pair with {device}"),
                format!("Enter the PIN of {device}. Many devices use 0000 or 1234."),
                String::new(),
            ),
            PairingKind::Authorize => (
                4,
                format!("Pair with {device}?"),
                format!("{device} wants to pair with this computer."),
                String::new(),
            ),
            PairingKind::AuthorizeService { uuid } => (
                4,
                format!("Allow {device}?"),
                format!("{device} wants to use {}.", service_name(uuid)),
                String::new(),
            ),
        };
        Self { kind, title, message, code }
    }
}

/// The answer to a request from what the user typed, or `None` if it isn't valid.
pub fn typed_answer(kind: &PairingKind, input: &str) -> Option<Answer> {
    let input = input.trim();
    match kind {
        PairingKind::EnterPasskey => {
            let valid = (1..=6).contains(&input.len()) && input.bytes().all(|b| b.is_ascii_digit());
            valid.then(|| input.parse().ok().map(Answer::Passkey)).flatten()
        }
        // BlueZ takes PINs of 1 to 16 characters.
        PairingKind::EnterPin => {
            (1..=16).contains(&input.len()).then(|| Answer::Pin(input.to_owned()))
        }
        _ => Some(Answer::Accept),
    }
}

/// Devices that behave like BlueZ would, reporting synchronously.
pub struct SampleBluetooth {
    state: Mutex<BluetoothState>,
    pending: Mutex<Option<PairingRequest>>,
    events: Events<Event>,
}

/// The passkey the sample phone shows when it pairs.
pub const SAMPLE_PASSKEY: u32 = 482_913;

impl SampleBluetooth {
    pub fn new(events: Events<Event>) -> Self {
        let device = |n: u8, name: &str, icon: &str, paired, connected, battery| Device {
            id: format!("/org/bluez/hci0/dev_00_1A_7D_DA_71_{n:02X}"),
            address: format!("00:1A:7D:DA:71:{n:02X}"),
            name: name.into(),
            icon: icon.into(),
            paired,
            trusted: paired,
            connected,
            rssi: None,
            battery,
            busy: false,
        };
        let state = BluetoothState {
            adapter: Some(Adapter {
                name: "workstation".into(),
                address: "00:1A:7D:DA:71:00".into(),
                powered: true,
                discoverable: false,
                discovering: false,
            }),
            devices: vec![
                device(1, "WH-1000XM5", "audio-headphones", true, true, Some(70)),
                device(2, "MX Keys", "input-keyboard", true, false, None),
                device(3, "MX Master 3S", "input-mouse", false, false, None),
                device(4, "Pixel 8", "phone", false, false, None),
            ],
        };
        let sample = Self { state: Mutex::new(state), pending: Mutex::new(None), events };
        sample.report();
        sample
    }

    fn report(&self) {
        let state = self.state.lock().map(|state| state.clone()).unwrap_or_default();
        (self.events)(Event::State(state));
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut Device)) {
        if let Ok(mut state) = self.state.lock()
            && let Some(device) = state.devices.iter_mut().find(|device| device.id == id)
        {
            change(device);
        }
    }

    fn adapter(&self, change: impl FnOnce(&mut Adapter)) {
        if let Ok(mut state) = self.state.lock()
            && let Some(adapter) = &mut state.adapter
        {
            change(adapter);
        }
    }
}

impl Control<Command> for SampleBluetooth {
    fn send(&self, command: Command) {
        match command {
            Command::SetPowered(powered) => self.adapter(|adapter| adapter.powered = powered),
            Command::SetDiscoverable(on) => self.adapter(|adapter| adapter.discoverable = on),
            Command::SetDiscovering(on) => self.adapter(|adapter| adapter.discovering = on),
            Command::Pair(id) => {
                let name = self.state.lock().ok().and_then(|state| {
                    state.devices.iter().find(|device| device.id == id).map(|d| d.name.clone())
                });
                let Some(device) = name else { return };
                self.update(&id, |device| device.busy = true);
                let request = PairingRequest {
                    id: 1,
                    device_id: id,
                    device,
                    kind: PairingKind::Confirm { passkey: SAMPLE_PASSKEY },
                };
                if let Ok(mut pending) = self.pending.lock() {
                    *pending = Some(request.clone());
                }
                self.report();
                (self.events)(Event::Pairing(request));
                return;
            }
            Command::Answer { id, answer } => {
                let Some(request) = self.pending.lock().ok().and_then(|mut p| p.take()) else {
                    return;
                };
                if request.id != id {
                    return;
                }
                (self.events)(Event::PairingEnded { id });
                let accepted = answer == Answer::Accept;
                self.update(&request.device_id, |device| {
                    device.busy = false;
                    device.paired = accepted;
                    device.trusted = accepted;
                    device.connected = accepted;
                });
                if !accepted {
                    let message = "Authentication Rejected".to_owned();
                    (self.events)(Event::Failed { device: request.device, message });
                }
            }
            Command::CancelPairing(id) => {
                if let Some(request) = self.pending.lock().ok().and_then(|mut p| p.take()) {
                    (self.events)(Event::PairingEnded { id: request.id });
                }
                self.update(&id, |device| device.busy = false);
            }
            Command::Connect(id) => self.update(&id, |device| device.connected = true),
            Command::Disconnect(id) => self.update(&id, |device| device.connected = false),
            Command::Remove(id) => {
                if let Ok(mut state) = self.state.lock() {
                    state.devices.retain(|device| device.id != id);
                }
            }
        }
        self.report();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_devices() {
        assert_eq!(DeviceKind::from_icon("audio-headset"), DeviceKind::Audio);
        assert_eq!(DeviceKind::from_icon("input-keyboard"), DeviceKind::Keyboard);
        assert_eq!(DeviceKind::from_icon("input-tablet"), DeviceKind::Mouse);
        assert_eq!(DeviceKind::from_icon("phone"), DeviceKind::Other);
    }

    #[test]
    fn describes_devices() {
        let mut device =
            Device { paired: true, connected: true, battery: Some(55), ..Device::default() };
        assert_eq!(device_status(&device), "Connected · Battery 55%");
        device.connected = false;
        assert_eq!(device_status(&device), "Not connected");
        device.busy = true;
        assert_eq!(device_status(&device), "Connecting…");
        device.paired = false;
        assert_eq!(device_status(&device), "Pairing…");
        let adapter = Adapter {
            name: "laptop".into(),
            powered: true,
            discoverable: true,
            ..Adapter::default()
        };
        assert_eq!(adapter_status(&adapter), "Visible as “laptop”");
    }

    #[test]
    fn names_services() {
        assert_eq!(service_name("0000110b-0000-1000-8000-00805f9b34fb"), "audio");
        assert_eq!(service_name("00001124-0000-1000-8000-00805F9B34FB"), "input");
        assert_eq!(service_name("0000110b-1234-1000-8000-00805f9b34fb"), "a service");
        assert_eq!(service_name("nonsense"), "a service");
    }

    #[test]
    fn prompts_and_answers() {
        let request =
            |kind| PairingRequest { id: 1, device_id: String::new(), device: "Pixel".into(), kind };
        let confirm = PairingPrompt::new(&request(PairingKind::Confirm { passkey: 4321 }));
        assert_eq!((confirm.kind, confirm.code.as_str()), (0, "004321"));
        assert_eq!(confirm.title, "Pair with Pixel?");
        let typing =
            PairingPrompt::new(&request(PairingKind::DisplayPasskey { passkey: 1, entered: 2 }));
        assert!(typing.message.ends_with("2 of 6 digits typed."));
        let service = PairingPrompt::new(&request(PairingKind::AuthorizeService {
            uuid: "0000111f-0000-1000-8000-00805f9b34fb".into(),
        }));
        assert_eq!((service.kind, service.message.as_str()), (4, "Pixel wants to use calls."));

        assert_eq!(
            typed_answer(&PairingKind::EnterPasskey, " 012345 "),
            Some(Answer::Passkey(12345))
        );
        assert_eq!(typed_answer(&PairingKind::EnterPasskey, "1234567"), None);
        assert_eq!(typed_answer(&PairingKind::EnterPasskey, "12a"), None);
        assert_eq!(typed_answer(&PairingKind::EnterPin, "0000"), Some(Answer::Pin("0000".into())));
        assert_eq!(typed_answer(&PairingKind::EnterPin, ""), None);
        assert_eq!(typed_answer(&PairingKind::Authorize, ""), Some(Answer::Accept));
    }
}
