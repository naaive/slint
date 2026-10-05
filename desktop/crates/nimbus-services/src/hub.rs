// SPDX-License-Identifier: MIT

//! The central task: owns the aggregated [`SystemState`], starts the services, and routes commands.

use std::convert::Infallible;
use std::time::Duration;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep_until};
use zbus::Connection;

use crate::audio::AudioCommand;
use crate::backlight::BacklightCommand;
use crate::bluetooth::BluetoothCommand;
use crate::bus::{self, BusKind, BusService};
use crate::logind::LoginCommand;
use crate::mpris::MediaCommand;
use crate::network::NetworkCommand;
use crate::notifications::NotificationCommand;
use crate::{
    Audio, Battery, Bluetooth, BusAddress, Media, Network, ServiceCommand, ServiceEvent,
    ServicesConfig, SystemState,
};

/// State changes within this window are emitted as one [`ServiceEvent::State`].
const COALESCE: Duration = Duration::from_millis(16);

/// A partial update pushed by a service to the central task.
#[derive(Debug)]
pub(crate) enum Update {
    Battery(Option<Battery>),
    Network(Network),
    Audio(Option<Audio>),
    Brightness(Option<f32>),
    Media(Option<Media>),
    Bluetooth(Option<Bluetooth>),
    /// Forwarded to the client immediately, ahead of any pending state change.
    Event(ServiceEvent),
}

#[derive(Clone, Debug)]
pub(crate) struct Updates(UnboundedSender<Update>);

impl Updates {
    pub(crate) fn send(&self, update: Update) {
        // Failing means the central task has stopped, and the runtime is shutting down.
        let _ = self.0.send(update);
    }

    pub(crate) fn event(&self, event: ServiceEvent) {
        self.send(Update::Event(event));
    }

    #[cfg(test)]
    pub(crate) fn channel() -> (Self, UnboundedReceiver<Update>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }
}

/// Applies a state update and returns whether anything changed; events don't change state.
fn apply(state: &mut SystemState, update: Update) -> bool {
    fn set<T: PartialEq>(slot: &mut T, value: T) -> bool {
        let changed = *slot != value;
        *slot = value;
        changed
    }
    match update {
        Update::Battery(value) => set(&mut state.battery, value),
        Update::Network(value) => set(&mut state.network, value),
        Update::Audio(value) => set(&mut state.audio, value),
        Update::Brightness(value) => set(&mut state.brightness, value),
        Update::Media(value) => set(&mut state.media, value),
        Update::Bluetooth(value) => set(&mut state.bluetooth, value),
        Update::Event(_) => false,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub(crate) config: ServicesConfig,
    pub(crate) session_bus: BusAddress,
    pub(crate) system_bus: BusAddress,
}

/// Command senders for the services that are enabled.
struct Routes {
    audio: Option<UnboundedSender<AudioCommand>>,
    backlight: Option<UnboundedSender<BacklightCommand>>,
    network: Option<UnboundedSender<NetworkCommand>>,
    bluetooth: Option<UnboundedSender<BluetoothCommand>>,
    media: Option<UnboundedSender<MediaCommand>>,
    notifications: Option<UnboundedSender<NotificationCommand>>,
    login: Option<UnboundedSender<LoginCommand>>,
    /// UPower takes no commands; holding the sender keeps it running until the central task stops.
    _upower: Option<UnboundedSender<Infallible>>,
}

/// Delivers `command` and returns whether a running service received it.
fn route<C: std::fmt::Debug>(sender: &Option<UnboundedSender<C>>, command: C) -> bool {
    match sender {
        Some(sender) => match sender.send(command) {
            Ok(()) => true,
            Err(mpsc::error::SendError(command)) => {
                tracing::debug!("Service for {command:?} isn't running");
                false
            }
        },
        None => {
            tracing::debug!("Service for {command:?} is disabled");
            false
        }
    }
}

fn channel<C>(enabled: bool) -> (Option<UnboundedSender<C>>, Option<UnboundedReceiver<C>>) {
    if enabled {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    }
}

struct Receivers {
    audio: Option<UnboundedReceiver<AudioCommand>>,
    backlight: Option<UnboundedReceiver<BacklightCommand>>,
    network: Option<UnboundedReceiver<NetworkCommand>>,
    bluetooth: Option<UnboundedReceiver<BluetoothCommand>>,
    media: Option<UnboundedReceiver<MediaCommand>>,
    notifications: Option<UnboundedReceiver<NotificationCommand>>,
    login: Option<UnboundedReceiver<LoginCommand>>,
    upower: Option<UnboundedReceiver<Infallible>>,
}

fn routes(config: &ServicesConfig) -> (Routes, Receivers) {
    let (audio_tx, audio) = channel(config.audio);
    let (backlight_tx, backlight) = channel(config.backlight);
    let (network_tx, network) = channel(config.network_manager);
    let (bluetooth_tx, bluetooth) = channel(config.bluetooth);
    let (media_tx, media) = channel(config.mpris);
    let (notifications_tx, notifications) = channel(config.notifications);
    let (login_tx, login) = channel(config.logind);
    let (upower_tx, upower) = channel(config.upower);
    (
        Routes {
            audio: audio_tx,
            backlight: backlight_tx,
            network: network_tx,
            bluetooth: bluetooth_tx,
            media: media_tx,
            notifications: notifications_tx,
            login: login_tx,
            _upower: upower_tx,
        },
        Receivers { audio, backlight, network, bluetooth, media, notifications, login, upower },
    )
}

async fn start_services(options: Options, receivers: Receivers, updates: Updates) {
    if let Some(commands) = receivers.audio {
        tokio::spawn(crate::audio::run(updates.clone(), commands));
    }

    let needs_session = receivers.notifications.is_some() || receivers.media.is_some();
    let needs_system = receivers.upower.is_some()
        || receivers.network.is_some()
        || receivers.bluetooth.is_some()
        || receivers.login.is_some()
        || receivers.backlight.is_some();
    let (session, system) = tokio::join!(
        async {
            if needs_session {
                bus::connect(&options.session_bus, BusKind::Session).await
            } else {
                None
            }
        },
        async {
            if needs_system {
                bus::connect(&options.system_bus, BusKind::System).await
            } else {
                None
            }
        },
    );

    if let Some(commands) = receivers.backlight {
        tokio::spawn(crate::backlight::run(system.clone(), updates.clone(), commands));
    }
    if let Some(conn) = session {
        if let Some(commands) = receivers.notifications {
            tokio::spawn(crate::notifications::run(conn.clone(), updates.clone(), commands));
        }
        if let Some(commands) = receivers.media {
            tokio::spawn(crate::mpris::run(conn, updates.clone(), commands));
        }
    }
    if let Some(commands) = receivers.upower {
        let service = crate::upower::UPower::new(updates.clone());
        start_bus_service(service, system.clone(), commands);
    }
    if let Some(commands) = receivers.network {
        let service = crate::network::NetworkManager::new(updates.clone());
        start_bus_service(service, system.clone(), commands);
    }
    if let Some(commands) = receivers.bluetooth {
        let service = crate::bluetooth::Bluez::new(updates.clone());
        start_bus_service(service, system.clone(), commands);
    }
    if let Some(commands) = receivers.login {
        let service = crate::logind::Logind::new(updates);
        start_bus_service(service, system, commands);
    }
}

fn start_bus_service<S: BusService>(
    service: S,
    conn: Option<Connection>,
    commands: UnboundedReceiver<S::Command>,
) {
    match conn {
        Some(conn) => tokio::spawn(bus::supervise(service, conn, commands)),
        None => tokio::spawn(bus::reject_all(service, commands)),
    };
}

/// Runs until `shutdown` resolves or the command channel closes, calling `emit` for every event.
pub(crate) async fn run(
    options: Options,
    mut commands: UnboundedReceiver<ServiceCommand>,
    mut shutdown: oneshot::Receiver<()>,
    emit: &dyn Fn(ServiceEvent),
) {
    let mut state = SystemState::default();
    let mut emitted = state.clone();
    emit(ServiceEvent::State(state.clone()));

    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let updates = Updates(updates_tx);
    let (routes, receivers) = routes(&options.config);
    tokio::spawn(start_services(options, receivers, updates));

    let mut flush_at: Option<Instant> = None;
    loop {
        let flush = async {
            match flush_at {
                Some(deadline) => sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let mut dirty = false;
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            command = commands.recv() => match command {
                Some(command) => match dispatch(&routes, command) {
                    Dispatch::Routed => {}
                    Dispatch::DoNotDisturb(enabled) => {
                        dirty = state.do_not_disturb != enabled;
                        state.do_not_disturb = enabled;
                    }
                    Dispatch::Emit(event) => emit(event),
                },
                None => break,
            },
            Some(update) = updates_rx.recv() => match update {
                Update::Event(event) => emit(event),
                update => dirty = apply(&mut state, update),
            },
            () = flush => {
                flush_at = None;
                if state != emitted {
                    emitted.clone_from(&state);
                    emit(ServiceEvent::State(state.clone()));
                }
            }
        }
        if dirty && flush_at.is_none() {
            flush_at = Some(Instant::now() + COALESCE);
        }
    }
}

enum Dispatch {
    Routed,
    DoNotDisturb(bool),
    Emit(ServiceEvent),
}

fn dispatch(routes: &Routes, command: ServiceCommand) -> Dispatch {
    use ServiceCommand as C;
    match command {
        C::SetVolume(volume) => {
            route(&routes.audio, AudioCommand::SetVolume(volume));
        }
        C::ToggleMute => {
            route(&routes.audio, AudioCommand::ToggleMute);
        }
        C::SetBrightness(level) => {
            route(&routes.backlight, BacklightCommand::Set(level));
        }
        C::SetWifiEnabled(enabled) => {
            route(&routes.network, NetworkCommand::SetWifiEnabled(enabled));
        }
        C::SetBluetoothPowered(powered) => {
            route(&routes.bluetooth, BluetoothCommand::SetPowered(powered));
        }
        C::SetDoNotDisturb(enabled) => return Dispatch::DoNotDisturb(enabled),
        C::MediaPlayPause => {
            route(&routes.media, MediaCommand::PlayPause);
        }
        C::MediaNext => {
            route(&routes.media, MediaCommand::Next);
        }
        C::MediaPrevious => {
            route(&routes.media, MediaCommand::Previous);
        }
        C::InvokeNotificationAction { id, action } => {
            let command = NotificationCommand::InvokeAction { id, action, activation_token: None };
            route(&routes.notifications, command);
        }
        C::InvokeNotificationActionWithToken { id, action, activation_token } => {
            let activation_token = Some(activation_token);
            let command = NotificationCommand::InvokeAction { id, action, activation_token };
            route(&routes.notifications, command);
        }
        C::CloseNotification { id, reason } => {
            route(&routes.notifications, NotificationCommand::Close { id, reason });
        }
        C::LockSession => {
            if !route(&routes.login, LoginCommand::Lock) {
                return Dispatch::Emit(ServiceEvent::LockRequested);
            }
        }
        C::LockPresented => {
            route(&routes.login, LoginCommand::LockPresented);
        }
        C::Suspend => {
            route(&routes.login, LoginCommand::Suspend);
        }
        C::Reboot => {
            route(&routes.login, LoginCommand::Reboot);
        }
        C::PowerOff => {
            route(&routes.login, LoginCommand::PowerOff);
        }
        C::Logout => {
            if !route(&routes.login, LoginCommand::Logout) {
                return Dispatch::Emit(ServiceEvent::LogoutRequested);
            }
        }
    }
    Dispatch::Routed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CloseReason, ConnectionKind};

    #[test]
    fn apply_reports_changes_only() {
        let mut state = SystemState::default();
        assert!(!apply(&mut state, Update::Brightness(None)));
        assert!(apply(&mut state, Update::Brightness(Some(0.5))));
        assert_eq!(state.brightness, Some(0.5));
        assert!(!apply(&mut state, Update::Brightness(Some(0.5))));

        let network = Network {
            kind: ConnectionKind::Wifi,
            ssid: Some("home".into()),
            strength: 0.7,
            wifi_enabled: true,
            available: true,
        };
        assert!(apply(&mut state, Update::Network(network.clone())));
        assert_eq!(state.network, network);

        let audio = Audio { volume: 0.4, muted: false };
        assert!(apply(&mut state, Update::Audio(Some(audio.clone()))));
        assert_eq!(state.audio, Some(audio));
    }

    #[test]
    fn events_leave_state_alone() {
        let mut state = SystemState::default();
        let event = ServiceEvent::NotificationClosed { id: 3, reason: CloseReason::Expired };
        assert!(!apply(&mut state, Update::Event(event)));
        assert_eq!(state, SystemState::default());
    }

    #[test]
    fn lock_without_logind_emits_locally() {
        let (routes, _receivers) = routes(&ServicesConfig { logind: false, ..Default::default() });
        assert!(matches!(
            dispatch(&routes, ServiceCommand::LockSession),
            Dispatch::Emit(ServiceEvent::LockRequested)
        ));
    }

    #[test]
    fn logout_without_logind_asks_the_compositor() {
        let (routes, _receivers) = routes(&ServicesConfig { logind: false, ..Default::default() });
        assert!(matches!(
            dispatch(&routes, ServiceCommand::Logout),
            Dispatch::Emit(ServiceEvent::LogoutRequested)
        ));
    }

    #[test]
    fn lock_with_stopped_logind_emits_locally() {
        let (routes, receivers) = routes(&ServicesConfig::default());
        drop(receivers);
        assert!(matches!(
            dispatch(&routes, ServiceCommand::LockSession),
            Dispatch::Emit(ServiceEvent::LockRequested)
        ));
    }

    #[test]
    fn commands_reach_their_service() {
        let (routes, mut receivers) = routes(&ServicesConfig::default());
        assert!(matches!(dispatch(&routes, ServiceCommand::SetVolume(0.3)), Dispatch::Routed));
        assert!(matches!(
            receivers.audio.as_mut().and_then(|r| r.try_recv().ok()),
            Some(AudioCommand::SetVolume(v)) if v == 0.3
        ));
        assert!(matches!(dispatch(&routes, ServiceCommand::LockSession), Dispatch::Routed));
        assert!(matches!(
            receivers.login.as_mut().and_then(|r| r.try_recv().ok()),
            Some(LoginCommand::Lock)
        ));
        assert!(matches!(
            dispatch(&routes, ServiceCommand::SetDoNotDisturb(true)),
            Dispatch::DoNotDisturb(true)
        ));
    }
}
