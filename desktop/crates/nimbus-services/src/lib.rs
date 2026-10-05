// SPDX-License-Identifier: MIT

//! System integration for the Nimbus shell.
//!
//! [`Services::spawn`] starts a Tokio runtime on a background thread that talks to D-Bus
//! (UPower, NetworkManager, MPRIS, logind, and the session's `org.freedesktop.Notifications` server)
//! and to PipeWire or PulseAudio through `wpctl`/`pactl`.
//! Every service degrades gracefully: a missing bus or daemon leaves its part of [`SystemState`] at `None` or default.

use std::time::SystemTime;

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
    pub charging: bool,
    pub time_to_empty: Option<std::time::Duration>,
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
    pub kind: ConnectionKind,
    pub ssid: Option<String>,
    /// Wi-Fi signal strength in `0.0..=1.0`.
    pub strength: f32,
    pub wifi_enabled: bool,
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
    Notification(Notification),
    NotificationClosed { id: u32, reason: CloseReason },
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
    InvokeNotificationAction { id: u32, action: String },
    CloseNotification { id: u32, reason: CloseReason },
    LockSession,
    Suspend,
    Reboot,
    PowerOff,
    Logout,
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
        }
    }
}

/// A handle to the running services; dropping it shuts them down.
pub struct Services {
    _private: (),
}

impl Services {
    /// Starts the services on a background thread and calls `on_event` from that thread for every event.
    /// The first event is always a [`ServiceEvent::State`].
    pub fn spawn(config: ServicesConfig, on_event: impl Fn(ServiceEvent) + Send + 'static) -> Self {
        let _ = (config, on_event);
        todo!()
    }

    /// Queues `command`; it never blocks, and failures are logged.
    pub fn send(&self, command: ServiceCommand) {
        let _ = command;
        todo!()
    }
}
