// SPDX-License-Identifier: MIT

//! The Network page: Wi-Fi networks and wired connections from NetworkManager, and the password dialog.

use std::cell::Cell;
use std::rc::Rc;

use nimbus_services::nm::{Command, Event, NetworkState, WifiNetwork};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Inner, Message, With, deliver, index_of, post};
use crate::dispatch::Dispatch;
use crate::network::{self, details, network_status, strength_bars, wifi_status, wired_status};
use crate::sources::{Control, Events};
use crate::{AppWindow, InfoRow, NetworkModel, WifiNetworkItem, WiredItem};

/// What the page knows about the network.
#[derive(Default)]
pub(crate) struct Network {
    control: Option<Rc<dyn Control<Command>>>,
    state: NetworkState,
    /// The network the password dialog asks for.
    asking: Option<WifiNetwork>,
    /// The network a password was typed for last, so a failure says the password was wrong.
    typed_for: Option<Vec<u8>>,
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let model = ui.global::<NetworkModel>();
    let h = with.clone();
    model.on_set_wifi_enabled(move |enabled| {
        h(&|i| i.send_network(Command::SetWifiEnabled(enabled)))
    });
    let h = with.clone();
    model.on_scan(move || h(&|i| i.send_network(Command::Scan)));
    let h = with.clone();
    model.on_activate(move |index| h(&|i| i.activate_network(index_of(index))));
    let h = with.clone();
    model.on_disconnect(move || h(&|i| i.send_network(Command::Disconnect)));
    let h = with.clone();
    model.on_forget(move |index| {
        h(&|i| {
            let ssid = i.network_at(index_of(index)).map(|network| network.ssid);
            if let Some(ssid) = ssid {
                i.send_network(Command::Forget { ssid });
            }
        })
    });
    let h = with.clone();
    model.on_password_acceptable(move |text| {
        let acceptable = Cell::new(false);
        h(&|i| {
            let asking = &i.state.borrow().network.asking;
            acceptable.set(
                asking.as_ref().is_some_and(|n| network::password_acceptable(n.security, &text)),
            );
        });
        acceptable.get()
    });
    let h = with.clone();
    model.on_password_submit(move || h(&|i| i.submit_password()));
    let h = with.clone();
    model.on_password_cancel(move || h(&|i| i.close_password()));
}

fn rows(pairs: Vec<(String, String)>) -> ModelRc<InfoRow> {
    let rows: Vec<InfoRow> = pairs
        .into_iter()
        .map(|(label, value)| InfoRow { label: label.into(), value: value.into() })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

impl Inner {
    /// Starts the NetworkManager client.
    pub fn start_network(&self) {
        let events: Events<Event> = match self.dispatch {
            Dispatch::Threaded => Box::new(|event| post(Message::Network(event))),
            Dispatch::Inline => Box::new(|event| deliver(Message::Network(event))),
        };
        // Inline sources report right away, so no borrow may be held here.
        let control = self.sources.network(events);
        self.state.borrow_mut().network.control = Some(Rc::from(control));
    }

    pub(super) fn send_network(&self, command: Command) {
        // Inline sources report right away, so no borrow may be held while sending.
        let control = self.state.borrow().network.control.clone();
        if let Some(control) = control {
            control.send(command);
        }
    }

    fn network_at(&self, index: usize) -> Option<WifiNetwork> {
        self.state.borrow().network.state.networks.get(index).cloned()
    }

    fn activate_network(&self, index: usize) {
        let Some(network) = self.network_at(index) else { return };
        if network.active {
            return;
        }
        if !network.security.can_connect() {
            self.show_banner(
                &format!("Nimbus can't join enterprise networks such as “{}” yet. Use nmcli or nm-connection-editor instead.", network.name),
                true,
            );
        } else if !network.known && network.security.needs_password() {
            self.ask_password(network, "");
        } else {
            self.send_network(Command::Connect { ssid: network.ssid, password: None });
        }
    }

    fn ask_password(&self, network: WifiNetwork, error: &str) {
        self.with_ui(|ui| {
            let model = ui.global::<NetworkModel>();
            model.set_password_network(network.name.as_str().into());
            model.set_password_error(error.into());
            model.set_password("".into());
            model.set_password_open(true);
        });
        self.state.borrow_mut().network.asking = Some(network);
    }

    fn submit_password(&self) {
        let Some(network) = self.state.borrow_mut().network.asking.take() else { return };
        let mut password = String::new();
        self.with_ui(|ui| {
            let model = ui.global::<NetworkModel>();
            password = model.get_password().to_string();
            model.set_password("".into());
            model.set_password_open(false);
        });
        if !network::password_acceptable(network.security, &password) {
            return;
        }
        self.state.borrow_mut().network.typed_for = Some(network.ssid.clone());
        self.send_network(Command::Connect { ssid: network.ssid, password: Some(password) });
    }

    fn close_password(&self) {
        self.state.borrow_mut().network.asking = None;
        self.with_ui(|ui| {
            let model = ui.global::<NetworkModel>();
            model.set_password("".into());
            model.set_password_open(false);
        });
    }

    pub(super) fn handle_network(&self, event: Event) {
        match event {
            Event::State(state) => {
                self.show_network(&state);
                self.state.borrow_mut().network.state = *state;
            }
            Event::ConnectFailed { ssid, needs_password } => {
                let (network, typed) = {
                    let mut state = self.state.borrow_mut();
                    let network = &mut state.network;
                    let typed = network.typed_for.take().is_some_and(|typed| typed == ssid);
                    (network.state.networks.iter().find(|n| n.ssid == ssid).cloned(), typed)
                };
                let name = network
                    .as_ref()
                    .map(|n| n.name.clone())
                    .unwrap_or_else(|| String::from_utf8_lossy(&ssid).into_owned());
                match network {
                    Some(network) if needs_password => {
                        let error = if typed {
                            "The password didn't work. Check it and try again."
                        } else {
                            ""
                        };
                        self.ask_password(network, error);
                    }
                    _ => self.show_banner(&format!("Couldn't connect to “{name}”."), true),
                }
            }
            Event::Failed(reason) => {
                self.show_banner(
                    &format!("The network settings couldn't be changed: {reason}"),
                    true,
                );
            }
        }
    }

    fn show_network(&self, state: &NetworkState) {
        let wifi = state.wifi.as_ref();
        let link = wifi.map(|wifi| wifi.link).unwrap_or_default();
        let networks: Vec<WifiNetworkItem> = state
            .networks
            .iter()
            .map(|network| WifiNetworkItem {
                name: network.name.as_str().into(),
                status: network_status(network, link).into(),
                bars: strength_bars(network.strength),
                secured: network.security.needs_password(),
                active: network.active,
                known: network.known,
            })
            .collect();
        let active = state.networks.iter().find(|network| network.active);
        let connection = wifi.filter(|_| state.wifi_enabled).and_then(|wifi| {
            let name = wifi.connection.as_ref()?.name.clone();
            Some((name, details(wifi, active)))
        });
        let wired: Vec<WiredItem> = state
            .wired
            .iter()
            .map(|device| WiredItem {
                title: format!("Ethernet ({})", device.interface).into(),
                status: wired_status(device).into(),
                connected: device.connection.is_some(),
                details: rows(details(device, None)),
            })
            .collect();
        self.with_ui(|ui| {
            let model = ui.global::<NetworkModel>();
            model.set_state(if state.running { 1 } else { 2 });
            model.set_has_wifi(wifi.is_some());
            model.set_wifi_enabled(state.wifi_enabled && state.wifi_hardware_enabled);
            model.set_wifi_blocked(!state.wifi_hardware_enabled);
            model.set_wifi_status(wifi_status(state).into());
            model.set_networks(ModelRc::new(VecModel::from(networks)));
            let (name, pairs) = connection.unwrap_or_default();
            model.set_connection_name(name.into());
            model.set_connection_details(rows(pairs));
            model.set_wired(ModelRc::new(VecModel::from(wired)));
        });
    }
}
