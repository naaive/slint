// SPDX-License-Identifier: MIT

//! A fake udisks: one removable drive, whose file systems tests insert and remove, and a log of the calls it gets.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{Connection, interface};

use crate::PrivateBus;

const ROOT: &str = "/org/freedesktop/UDisks2";
/// The object path of the fake drive, "Acme Stick".
pub const FAKE_DRIVE: &str = "/org/freedesktop/UDisks2/drives/Acme_Stick";

type Calls = Arc<Mutex<Vec<String>>>;
type Options = HashMap<String, OwnedValue>;

/// How a fake file system answers `Mount`.
#[derive(Clone, Copy, Debug)]
pub enum MountAnswer {
    /// Mounts at `/run/media/ada/<label>`.
    Mount,
    /// Fails with `org.freedesktop.UDisks2.Error.Failed` and "Unknown file system".
    Fail,
    /// Fails as if the user dismissed the polkit dialog.
    Dismiss,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.UDisks2.Error")]
enum UdisksError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    NotAuthorizedDismissed(String),
}

fn nul_terminated(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

struct Block {
    device: String,
    label: String,
}

#[interface(name = "org.freedesktop.UDisks2.Block")]
impl Block {
    #[zbus(property)]
    fn device(&self) -> Vec<u8> {
        nul_terminated(&self.device)
    }

    #[zbus(property)]
    fn size(&self) -> u64 {
        8_000_000_000
    }

    #[zbus(property)]
    fn id_label(&self) -> String {
        self.label.clone()
    }

    #[zbus(property)]
    fn id_type(&self) -> String {
        "vfat".into()
    }

    #[zbus(property)]
    fn id_usage(&self) -> String {
        "filesystem".into()
    }

    #[zbus(property)]
    fn drive(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(FAKE_DRIVE).expect("a valid object path")
    }

    #[zbus(property)]
    fn hint_system(&self) -> bool {
        false
    }
}

struct Filesystem {
    label: String,
    mount_point: Option<String>,
    answer: MountAnswer,
    calls: Calls,
}

#[interface(name = "org.freedesktop.UDisks2.Filesystem")]
impl Filesystem {
    #[zbus(property)]
    fn mount_points(&self) -> Vec<Vec<u8>> {
        self.mount_point.iter().map(|point| nul_terminated(point)).collect()
    }

    async fn mount(
        &mut self,
        _options: Options,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<String, UdisksError> {
        self.calls.lock().unwrap().push(format!("mount {}", self.label));
        match self.answer {
            MountAnswer::Mount => {}
            MountAnswer::Fail => return Err(UdisksError::Failed("Unknown file system".into())),
            MountAnswer::Dismiss => {
                return Err(UdisksError::NotAuthorizedDismissed("Dismissed".into()));
            }
        }
        let point = format!("/run/media/ada/{}", self.label);
        self.mount_point = Some(point.clone());
        self.mount_points_changed(&emitter).await?;
        Ok(point)
    }

    async fn unmount(
        &mut self,
        _options: Options,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> Result<(), UdisksError> {
        self.calls.lock().unwrap().push(format!("unmount {}", self.label));
        self.mount_point = None;
        self.mount_points_changed(&emitter).await?;
        Ok(())
    }
}

struct Drive {
    calls: Calls,
}

#[interface(name = "org.freedesktop.UDisks2.Drive")]
impl Drive {
    #[zbus(property)]
    fn vendor(&self) -> String {
        "Acme".into()
    }

    #[zbus(property)]
    fn model(&self) -> String {
        "Stick".into()
    }

    #[zbus(property)]
    fn removable(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_power_off(&self) -> bool {
        true
    }

    async fn power_off(&self, _options: Options) {
        self.calls.lock().unwrap().push("power off".into());
    }
}

/// `org.freedesktop.UDisks2` on a private bus, with the drive [`FAKE_DRIVE`] and no file systems yet.
pub struct FakeUdisks {
    conn: Connection,
    calls: Calls,
}

impl FakeUdisks {
    pub async fn start(bus: &PrivateBus) -> Self {
        let conn = bus.connect().await;
        let calls = Calls::default();
        let server = conn.object_server();
        server.at(ROOT, zbus::fdo::ObjectManager).await.expect("serve the object manager");
        server.at(FAKE_DRIVE, Drive { calls: calls.clone() }).await.expect("serve the drive");
        conn.request_name("org.freedesktop.UDisks2").await.expect("own the udisks name");
        Self { conn, calls }
    }

    /// Adds an unmounted file system on the drive, such as `block_devices/sdb1` for `/dev/sdb1`.
    pub async fn insert(&self, path: &str, device: &str, label: &str, answer: MountAnswer) {
        let server = self.conn.object_server();
        let block = Block { device: device.into(), label: label.into() };
        server.at(path, block).await.expect("serve the block device");
        let calls = self.calls.clone();
        let filesystem = Filesystem { label: label.into(), mount_point: None, answer, calls };
        server.at(path, filesystem).await.expect("serve the file system");
    }

    /// The calls so far, such as `mount STICK`, `unmount STICK`, and `power off`.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
