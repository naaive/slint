// SPDX-License-Identifier: MIT

//! System integration for the Nimbus shell.
//!
//! [`Services::spawn`] starts a Tokio runtime on a background thread that talks to D-Bus
//! (UPower, NetworkManager, MPRIS, logind, udisks, the session's `org.freedesktop.Notifications` server, and its polkit agent)
//! and to PipeWire or PulseAudio through `wpctl`/`pactl`.
//! Every service degrades gracefully: a missing bus or daemon leaves its part of [`SystemState`] at `None` or default.
//!
//! [`nm`], [`bluez`], [`sound`], and [`timedate`] are clients for network, Bluetooth, sound, and date and time settings,
//! each on a thread of its own.

use std::time::{Duration, Instant, SystemTime};

use tokio::sync::{mpsc, oneshot};

mod audio;
mod backlight;
mod bluetooth;
mod bus;
mod hub;
mod logind;
mod mpris;
mod network;
mod notifications;
mod polkit;
mod upower;
mod worker;

pub mod bluez;
pub mod nm;
pub mod sound;
pub mod timedate;
pub mod udisks;

pub use notifications::DEFAULT_TIMEOUT as DEFAULT_NOTIFICATION_TIMEOUT;
pub use polkit::{AuthenticationCommand, AuthenticationEvent, AuthenticationRequest, Secret};

/// How long dropping [`Services`] waits for the background thread to finish.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemState {
    pub battery: Option<Battery>,
    pub network: Network,
    pub audio: Option<Audio>,
    /// Backlight brightness in `0.0..=1.0`, or `None` without a backlight.
    pub brightness: Option<f32>,
    pub media: Option<Media>,
    pub bluetooth: Option<Bluetooth>,
    pub do_not_disturb: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    /// Charge in `0.0..=1.0`.
    pub level: f32,
    /// On external power: charging, fully charged, or about to charge.
    pub charging: bool,
    pub time_to_empty: Option<std::time::Duration>,
    pub warning: BatteryWarning,
}

/// How low UPower considers the charge, by the thresholds in its `UPower.conf`.
/// It's [`BatteryWarning::None`] while the battery charges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum BatteryWarning {
    #[default]
    None,
    Low,
    /// Critically low, or so low that UPower is about to act, such as by hibernating.
    Critical,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectionKind {
    #[default]
    None,
    Ethernet,
    Wifi,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Network {
    /// The primary connection; anything that isn't Wi-Fi, such as a VPN or modem, reports [`ConnectionKind::Ethernet`].
    pub kind: ConnectionKind,
    pub ssid: Option<String>,
    /// Wi-Fi signal strength in `0.0..=1.0`.
    pub strength: f32,
    pub wifi_enabled: bool,
    /// Whether the internet is reachable, as far as NetworkManager knows.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    /// Output volume in `0.0..=1.5`, where `1.0` is 100 %.
    pub volume: f32,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bluetooth {
    pub powered: bool,
    pub connected_devices: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    /// MPRIS bus name, such as `org.mpris.MediaPlayer2.spotify`.
    pub player: String,
    pub title: String,
    pub artist: String,
    pub art_url: Option<String>,
    pub playing: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

/// A notification received through `org.freedesktop.Notifications.Notify`.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    /// Icon name, absolute path, or `file://` URL.
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    /// `(action key, label)` pairs; the key `default` means activating the notification itself.
    pub actions: Vec<(String, String)>,
    pub urgency: Urgency,
    /// `None` means the server decides; `Some(Duration::ZERO)` means never expire.
    pub expire_timeout: Option<std::time::Duration>,
    pub received: SystemTime,
    /// The sender asked to skip the notification history (the `transient` hint).
    pub transient: bool,
    /// The notification stays open after one of its actions runs (the `resident` hint).
    pub resident: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServiceEvent {
    State(SystemState),
    /// A new notification, or one that replaces the notification with the same `id`.
    /// It arrives even with [`SystemState::do_not_disturb`] set; the shell decides whether to show a toast.
    Notification(Notification),
    /// The notification closed; [`CloseReason::Expired`] means its timeout elapsed.
    /// The shell may keep an expired notification that isn't transient in its history.
    /// Its actions keep working until the shell sends [`ServiceCommand::CloseNotification`] for it.
    NotificationClosed {
        id: u32,
        reason: CloseReason,
    },
    /// logind asked the session to lock, for example before suspend or from `loginctl lock-session`.
    LockRequested,
    /// logind asked the session to unlock, from `loginctl unlock-session`.
    UnlockRequested,
    /// [`ServiceCommand::Logout`] couldn't end the session through logind,
    /// for example in a nested session; the compositor should exit on its own.
    LogoutRequested,
    /// polkit asks the user to authenticate; see [`AuthenticationEvent`].
    Authentication(AuthenticationEvent),
    /// Removable media and other volumes from udisks; see [`udisks::Event`].
    Disks(udisks::Event),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServiceCommand {
    SetVolume(f32),
    ToggleMute,
    SetBrightness(f32),
    SetWifiEnabled(bool),
    SetBluetoothPowered(bool),
    SetDoNotDisturb(bool),
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
    InvokeNotificationAction {
        id: u32,
        action: String,
    },
    /// Like [`ServiceCommand::InvokeNotificationAction`],
    /// with an xdg-activation token the client uses to raise its window.
    InvokeNotificationActionWithToken {
        id: u32,
        action: String,
        activation_token: String,
    },
    CloseNotification {
        id: u32,
        reason: CloseReason,
    },
    LockSession,
    /// Every output has presented a frame of the lock screen after [`ServiceEvent::LockRequested`].
    /// A pending suspend waits for this, or for a short timeout.
    LockPresented,
    Suspend,
    Reboot,
    PowerOff,
    Logout,
    /// Answers the polkit agent's [`ServiceEvent::Authentication`].
    Authentication(AuthenticationCommand),
    Disks(udisks::Command),
}

/// Configures which services to start; tests and the shell preview disable the bus-backed ones.
#[derive(Clone, Debug)]
pub struct ServicesConfig {
    pub notifications: bool,
    pub upower: bool,
    pub network_manager: bool,
    pub audio: bool,
    pub backlight: bool,
    pub mpris: bool,
    pub bluetooth: bool,
    pub logind: bool,
    pub polkit: bool,
    pub udisks: bool,
}

impl Default for ServicesConfig {
    fn default() -> Self {
        Self {
            notifications: true,
            upower: true,
            network_manager: true,
            audio: true,
            backlight: true,
            mpris: true,
            bluetooth: true,
            logind: true,
            polkit: true,
            udisks: true,
        }
    }
}

/// Where to find a message bus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BusAddress {
    /// The standard session or system bus.
    #[default]
    Default,
    /// A D-Bus address, such as `unix:path=/run/user/1000/bus`.
    Address(String),
    /// Don't connect; the services on this bus stay disabled.
    Disabled,
}

/// Configures [`Services`] beyond [`ServicesConfig`], for tests and nested sessions.
#[derive(Clone, Debug)]
pub struct ServicesBuilder {
    options: hub::Options,
}

impl ServicesBuilder {
    /// Starts from `config`, with the standard session and system buses.
    pub fn new(config: ServicesConfig) -> Self {
        Self {
            options: hub::Options {
                config,
                session_bus: BusAddress::Default,
                system_bus: BusAddress::Default,
                polkit_helper: None,
            },
        }
    }

    /// Sets the bus for the notification server and MPRIS.
    #[must_use]
    pub fn session_bus(mut self, address: BusAddress) -> Self {
        self.options.session_bus = address;
        self
    }

    /// Sets the bus for UPower, NetworkManager, BlueZ, logind, logind's brightness control, polkit, and udisks.
    #[must_use]
    pub fn system_bus(mut self, address: BusAddress) -> Self {
        self.options.system_bus = address;
        self
    }

    /// Sets the `polkit-agent-helper-1` that checks responses, instead of the installed one.
    #[must_use]
    pub fn polkit_helper(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.options.polkit_helper = Some(path.into());
        self
    }

    /// Starts the services; see [`Services::spawn`].
    pub fn spawn(self, on_event: impl Fn(ServiceEvent) + Send + 'static) -> Services {
        let (commands, command_receiver) = mpsc::unbounded_channel();
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let options = self.options;
        let spawned = std::thread::Builder::new().name("nimbus-services".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    tracing::error!("Can't start the services runtime: {err}");
                    on_event(ServiceEvent::State(SystemState::default()));
                    return;
                }
            };
            runtime.block_on(hub::run(options, command_receiver, shutdown_receiver, &on_event));
            runtime.shutdown_timeout(SHUTDOWN_TIMEOUT / 4);
        });
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(err) => {
                tracing::error!("Can't start the services thread: {err}");
                None
            }
        };
        Services { commands, shutdown: Some(shutdown), thread }
    }
}

/// A handle to the running services; dropping it shuts them down.
pub struct Services {
    commands: mpsc::UnboundedSender<ServiceCommand>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Services {
    /// Starts the services on a background thread and calls `on_event` from that thread for every event.
    /// The first event is always a [`ServiceEvent::State`].
    pub fn spawn(config: ServicesConfig, on_event: impl Fn(ServiceEvent) + Send + 'static) -> Self {
        ServicesBuilder::new(config).spawn(on_event)
    }

    /// Queues `command`; it never blocks, and failures are logged.
    pub fn send(&self, command: ServiceCommand) {
        if let Err(err) = self.commands.send(command) {
            tracing::debug!("Services stopped; dropping {:?}", err.0);
        }
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(thread) = self.thread.take() else {
            return;
        };
        // Dropped from inside `on_event`: the thread finishes once the callback returns.
        if thread.thread().id() == std::thread::current().id() {
            return;
        }
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!(
                    "Services didn't stop within {SHUTDOWN_TIMEOUT:?}; detaching their thread"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.join().is_err() {
            tracing::warn!("The services thread panicked");
        }
    }
}
