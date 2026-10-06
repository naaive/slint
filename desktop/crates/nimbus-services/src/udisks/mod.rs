// SPDX-License-Identifier: MIT

//! A udisks2 client for removable media: the volumes worth showing, mounting, unmounting, ejecting, and powering off.
//!
//! [`Client::spawn`] runs it on a thread of its own, for apps;
//! the shell gets the same events as [`crate::ServiceEvent::Disks`].
//! udisks asks polkit before it mounts what the active session may not mount freely,
//! and the shell's polkit agent answers.

mod parse;
mod service;

use std::path::{Path, PathBuf};

use crate::BusAddress;
use crate::bus::BusService;
use crate::worker::Worker;

pub(crate) use service::Udisks;

/// A request to udisks, naming the volume by [`Volume::id`]; failures arrive as [`Event::Failed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Mount(String),
    Unmount(String),
    /// Unmounts every volume on the volume's drive, then ejects its medium, as for an optical disc or SD card.
    Eject(String),
    /// Unmounts every volume on the volume's drive, then powers the drive off, so it can be unplugged safely.
    PowerOff(String),
}

impl Command {
    /// The volume the command acts on.
    pub fn volume(&self) -> &str {
        match self {
            Command::Mount(id)
            | Command::Unmount(id)
            | Command::Eject(id)
            | Command::PowerOff(id) => id,
        }
    }

    /// What the command does, for messages such as "Couldn't eject".
    pub fn verb(&self) -> &'static str {
        match self {
            Command::Mount(_) => "mount",
            Command::Unmount(_) => "unmount",
            Command::Eject(_) => "eject",
            Command::PowerOff(_) => "power off",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Every volume worth showing, whenever one changes; empty while udisks isn't available.
    Volumes(Vec<Volume>),
    /// A volume that appeared after the first [`Event::Volumes`], such as on an inserted USB stick.
    /// It follows the [`Event::Volumes`] that lists it.
    Added(Volume),
    /// A [`Command::Mount`] succeeded, or found the volume mounted already.
    Mounted { id: String, mount_point: PathBuf },
    /// udisks refused a command, with its reason; a user dismissing the polkit dialog isn't a failure.
    Failed { command: Command, message: String },
}

/// A file system that udisks can mount, on a drive or a loop device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volume {
    /// The block device's udisks object path, which commands take.
    pub id: String,
    /// The file system's label, or its size and kind, such as "16 GB Volume".
    pub name: String,
    /// The device file, such as `/dev/sdb1`.
    pub device: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Where it's mounted; empty while it isn't.
    pub mount_points: Vec<PathBuf>,
    /// The icon name udisks suggests, such as `media-removable`.
    pub icon: String,
    pub drive: Option<Drive>,
}

impl Volume {
    pub fn mount_point(&self) -> Option<&Path> {
        self.mount_points.first().map(PathBuf::as_path)
    }

    /// Whether its drive takes removable media or can be unplugged, such as a USB stick or a card reader.
    pub fn removable(&self) -> bool {
        self.drive.as_ref().is_some_and(|drive| drive.removable)
    }

    /// How to remove the volume safely: eject an ejectable medium, power off a drive that can be,
    /// and otherwise just unmount.
    pub fn removal(&self) -> Command {
        let id = self.id.clone();
        match &self.drive {
            Some(drive) if drive.ejectable => Command::Eject(id),
            Some(drive) if drive.can_power_off => Command::PowerOff(id),
            _ => Command::Unmount(id),
        }
    }
}

/// The drive a [`Volume`] is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drive {
    /// The drive's udisks object path.
    pub id: String,
    /// The vendor and model, such as "SanDisk Cruzer Blade".
    pub name: String,
    /// Whether the drive or its medium can be removed.
    pub removable: bool,
    pub ejectable: bool,
    pub can_power_off: bool,
}

/// A udisks2 client on its own thread; dropping it stops the client.
pub struct Client {
    worker: Worker<Command>,
}

impl Client {
    /// Connects to udisks on `system_bus` and calls `on_event` from the client's thread.
    /// The first event is an empty [`Event::Volumes`], before udisks answers.
    pub fn spawn(system_bus: BusAddress, on_event: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let worker = Worker::spawn("nimbus-udisks", system_bus, move || {
            let mut udisks = Udisks::new(std::sync::Arc::new(on_event));
            udisks.unavailable();
            udisks
        });
        Self { worker }
    }

    /// Queues `command`; it never blocks.
    pub fn send(&self, command: Command) {
        self.worker.send(command);
    }
}
