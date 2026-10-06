// SPDX-License-Identifier: MIT

//! Volumes from udisks's `GetManagedObjects`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{Drive, Volume};

pub(super) const BLOCK: &str = "org.freedesktop.UDisks2.Block";
pub(super) const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
pub(super) const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

type Properties = HashMap<String, OwnedValue>;
pub(super) type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, Properties>>;

fn take<T: TryFrom<OwnedValue>>(properties: &Properties, key: &str) -> Option<T> {
    properties.get(key)?.try_clone().ok().and_then(|value| T::try_from(value).ok())
}

fn string(properties: &Properties, key: &str) -> String {
    take::<String>(properties, key).unwrap_or_default()
}

fn flag(properties: &Properties, key: &str) -> bool {
    take::<bool>(properties, key).unwrap_or(false)
}

/// Decodes udisks's NUL-terminated byte strings, such as device files and mount points.
fn path_from_bytes(mut bytes: Vec<u8>) -> PathBuf {
    if let Some(end) = bytes.iter().position(|&b| b == 0) {
        bytes.truncate(end);
    }
    PathBuf::from(OsString::from_vec(bytes))
}

/// A size in decimal units, as storage is sold: "512 MB", "15.9 GB".
pub(super) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 || value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit]).replace(".0 ", " ")
    }
}

fn drive(path: &str, properties: &Properties) -> Drive {
    let vendor = string(properties, "Vendor");
    let model = string(properties, "Model");
    let name = [vendor.trim(), model.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Drive {
        id: path.to_owned(),
        name,
        removable: flag(properties, "Removable") || flag(properties, "MediaRemovable"),
        ejectable: flag(properties, "Ejectable"),
        can_power_off: flag(properties, "CanPowerOff"),
    }
}

fn volume(
    path: &str,
    block: &Properties,
    filesystem: &Properties,
    objects: &ManagedObjects,
) -> Volume {
    let drive = take::<OwnedObjectPath>(block, "Drive")
        .filter(|drive| drive.as_str() != "/")
        .and_then(|drive| {
            let properties = objects.get(&drive)?.get(DRIVE)?;
            Some(self::drive(drive.as_str(), properties))
        });
    let size = take::<u64>(block, "Size").unwrap_or(0);
    let name = [string(block, "HintName"), string(block, "IdLabel")]
        .into_iter()
        .find(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("{} Volume", format_size(size)));
    let device = take::<Vec<u8>>(block, "PreferredDevice")
        .or_else(|| take::<Vec<u8>>(block, "Device"))
        .map(path_from_bytes)
        .unwrap_or_default();
    let mount_points = take::<Vec<Vec<u8>>>(filesystem, "MountPoints")
        .unwrap_or_default()
        .into_iter()
        .map(path_from_bytes)
        .collect();
    let icon = string(block, "HintIconName");
    let icon = if !icon.is_empty() {
        icon
    } else if drive.as_ref().is_some_and(|drive| drive.removable) {
        "media-removable".into()
    } else {
        "drive-harddisk".into()
    };
    Volume { id: path.to_owned(), name, device, size, mount_points, icon, drive }
}

/// Whether the user would look for this file system, as a file manager's sidebar shows them.
/// udisks sets `HintSystem` for internal disks, and `HintIgnore` for what no one should see.
fn shown(block: &Properties) -> bool {
    !flag(block, "HintIgnore")
        && !flag(block, "HintSystem")
        && string(block, "IdUsage") == "filesystem"
}

/// The volumes worth showing, by drive and then device.
pub(super) fn volumes(objects: &ManagedObjects) -> Vec<Volume> {
    let mut volumes: Vec<Volume> = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let block = interfaces.get(BLOCK)?;
            let filesystem = interfaces.get(FILESYSTEM)?;
            shown(block).then(|| volume(path.as_str(), block, filesystem, objects))
        })
        .collect();
    volumes.sort_by(|a, b| {
        let drive = |v: &Volume| v.drive.as_ref().map(|d| d.id.clone());
        drive(a).cmp(&drive(b)).then_with(|| a.device.cmp(&b.device)).then_with(|| a.id.cmp(&b.id))
    });
    volumes
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{ObjectPath, Value};

    fn props(pairs: Vec<(&str, Value<'_>)>) -> Properties {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v.try_to_owned().unwrap())).collect()
    }

    fn bytes(text: &str) -> Value<'static> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        Value::from(bytes)
    }

    fn path(text: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(text).unwrap()
    }

    fn block(drive: &str, label: &str, system: bool) -> Properties {
        props(vec![
            ("Device", bytes("/dev/sdb1")),
            ("Size", Value::U64(15_931_539_456)),
            ("IdLabel", Value::from(label)),
            ("IdType", Value::from("vfat")),
            ("IdUsage", Value::from("filesystem")),
            ("Drive", Value::from(ObjectPath::try_from(drive).unwrap())),
            ("HintSystem", Value::Bool(system)),
            ("HintIgnore", Value::Bool(false)),
            ("HintName", Value::from("")),
            ("HintIconName", Value::from("")),
        ])
    }

    fn objects() -> ManagedObjects {
        let stick = "/org/freedesktop/UDisks2/drives/SanDisk_Cruzer";
        let mut objects = ManagedObjects::new();
        objects.insert(
            path(stick),
            HashMap::from([(
                DRIVE.to_owned(),
                props(vec![
                    ("Vendor", Value::from("SanDisk ")),
                    ("Model", Value::from("Cruzer Blade")),
                    ("Removable", Value::Bool(true)),
                    ("Ejectable", Value::Bool(false)),
                    ("CanPowerOff", Value::Bool(true)),
                ]),
            )]),
        );
        let mounted =
            props(vec![("MountPoints", Value::from(vec![bytes("/run/media/ada/STICK")]))]);
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/sdb1"),
            HashMap::from([
                (BLOCK.to_owned(), block(stick, "STICK", false)),
                (FILESYSTEM.to_owned(), mounted),
            ]),
        );
        let unlabeled = block(stick, "", false);
        let empty = props(vec![("MountPoints", Value::from(Vec::<Value<'_>>::new()))]);
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/sdb2"),
            HashMap::from([(BLOCK.to_owned(), unlabeled), (FILESYSTEM.to_owned(), empty.clone())]),
        );
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/nvme0n1p2"),
            HashMap::from([
                (BLOCK.to_owned(), block("/", "root", true)),
                (FILESYSTEM.to_owned(), empty),
            ]),
        );
        // The whole stick, a partition table without a file system.
        objects.insert(
            path("/org/freedesktop/UDisks2/block_devices/sdb"),
            HashMap::from([(BLOCK.to_owned(), block(stick, "", false))]),
        );
        objects
    }

    #[test]
    fn lists_user_file_systems_with_their_drive() {
        let volumes = volumes(&objects());
        let ids: Vec<&str> = volumes.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "/org/freedesktop/UDisks2/block_devices/sdb1",
                "/org/freedesktop/UDisks2/block_devices/sdb2"
            ]
        );
        let stick = &volumes[0];
        assert_eq!(stick.name, "STICK");
        assert_eq!(stick.device, PathBuf::from("/dev/sdb1"));
        assert_eq!(stick.mount_point(), Some(std::path::Path::new("/run/media/ada/STICK")));
        assert_eq!(stick.icon, "media-removable");
        let drive = stick.drive.as_ref().unwrap();
        assert_eq!(drive.name, "SanDisk Cruzer Blade");
        assert!(stick.removable());
        assert_eq!(stick.removal(), super::super::Command::PowerOff(stick.id.clone()));
        assert_eq!(volumes[1].name, "15.9 GB Volume");
        assert_eq!(volumes[1].mount_point(), None);
    }

    #[test]
    fn sizes_read_like_storage_labels() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1_000), "1 kB");
        assert_eq!(format_size(512_000_000), "512 MB");
        assert_eq!(format_size(15_931_539_456), "15.9 GB");
        assert_eq!(format_size(2_000_000_000_000), "2 TB");
    }

    #[test]
    fn removal_depends_on_the_drive() {
        let mut volume = Volume { id: "v".into(), ..Volume::default() };
        assert_eq!(volume.removal(), super::super::Command::Unmount("v".into()));
        volume.drive = Some(Drive { ejectable: true, can_power_off: true, ..Drive::default() });
        assert_eq!(volume.removal(), super::super::Command::Eject("v".into()));
    }
}
