// SPDX-License-Identifier: MIT

//! The sidebar's places: XDG user folders, bookmarks, mounted drives, and the volumes udisks can mount.

use std::path::{Path, PathBuf};

use nimbus_services::udisks::{Command, Volume};

use super::uri;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlaceIcon {
    Home,
    Desktop,
    Documents,
    Downloads,
    Music,
    Pictures,
    Videos,
    Trash,
    Computer,
    Drive,
    Bookmark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Places,
    Devices,
    Bookmarks,
}

/// Where a place leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Dir(PathBuf),
    Trash,
    /// A volume that isn't mounted, by its udisks id; opening it mounts it.
    Volume(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub label: String,
    pub target: Target,
    pub icon: PlaceIcon,
    pub section: Section,
    /// The udisks volume behind the place.
    pub volume: Option<Volume>,
}

/// The XDG user folder keys shown in the sidebar, with their fallback names and icons.
const USER_DIRS: &[(&str, &str, PlaceIcon)] = &[
    ("DESKTOP", "Desktop", PlaceIcon::Desktop),
    ("DOCUMENTS", "Documents", PlaceIcon::Documents),
    ("DOWNLOAD", "Downloads", PlaceIcon::Downloads),
    ("MUSIC", "Music", PlaceIcon::Music),
    ("PICTURES", "Pictures", PlaceIcon::Pictures),
    ("VIDEOS", "Videos", PlaceIcon::Videos),
];

/// Parses `user-dirs.dirs`, whose lines look like `XDG_MUSIC_DIR="$HOME/Music"`.
///
/// Returns `(key, path)` pairs such as `("MUSIC", "/home/ada/Music")`.
/// Paths must be absolute or relative to `$HOME`, as `xdg-user-dirs` writes them.
pub fn parse_user_dirs(text: &str, home: &Path) -> Vec<(String, PathBuf)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim().strip_prefix("XDG_")?.strip_suffix("_DIR")?;
            let value = value.trim();
            let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
            let value = value.replace("\\\"", "\"");
            let path = if let Some(rest) = value.strip_prefix("$HOME") {
                let rest = rest.trim_start_matches('/');
                if rest.is_empty() { home.to_path_buf() } else { home.join(rest) }
            } else if value.starts_with('/') {
                PathBuf::from(value)
            } else {
                return None;
            };
            Some((key.to_string(), path))
        })
        .collect()
}

/// The path of an XDG user folder from parsed `user-dirs.dirs` entries, or its conventional default.
///
/// A folder set to the home folder itself counts as disabled, as `xdg-user-dirs` defines it.
pub fn user_dir(entries: &[(String, PathBuf)], key: &str, home: &Path) -> Option<PathBuf> {
    match entries.iter().rev().find(|(k, _)| k == key) {
        Some((_, path)) if path == home => None,
        Some((_, path)) => Some(path.clone()),
        None => USER_DIRS.iter().find(|(k, _, _)| *k == key).map(|(_, name, _)| home.join(name)),
    }
}

/// Home, the XDG user folders that exist, and Trash.
pub fn standard_places(home: &Path, user_dirs_text: Option<&str>) -> Vec<Place> {
    let entries = user_dirs_text.map(|t| parse_user_dirs(t, home)).unwrap_or_default();
    let mut places = vec![Place {
        label: "Home".into(),
        target: Target::Dir(home.to_path_buf()),
        icon: PlaceIcon::Home,
        section: Section::Places,
        volume: None,
    }];
    for (key, _, icon) in USER_DIRS {
        if let Some(path) = user_dir(&entries, key, home).filter(|p| p.is_dir()) {
            let label =
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            places.push(Place {
                label,
                target: Target::Dir(path),
                icon: *icon,
                section: Section::Places,
                volume: None,
            });
        }
    }
    places.push(Place {
        label: "Trash".into(),
        target: Target::Trash,
        icon: PlaceIcon::Trash,
        section: Section::Places,
        volume: None,
    });
    places
}

/// Parses a GTK bookmarks file: one `file://` URI per line, optionally followed by a label.
///
/// Remote bookmarks are skipped since only local folders can be browsed.
pub fn parse_bookmarks(text: &str) -> Vec<(PathBuf, Option<String>)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (uri, label) = match line.split_once(' ') {
                Some((uri, label)) => {
                    (uri, Some(label.trim().to_string()).filter(|l| !l.is_empty()))
                }
                None => (line, None),
            };
            Some((uri::uri_to_path(uri)?, label))
        })
        .collect()
}

pub fn bookmark_places(text: &str) -> Vec<Place> {
    parse_bookmarks(text)
        .into_iter()
        .map(|(path, label)| Place {
            label: label.unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned())
            }),
            target: Target::Dir(path),
            icon: PlaceIcon::Bookmark,
            section: Section::Bookmarks,
            volume: None,
        })
        .collect()
}

/// The bookmarks file with `path` appended, or `None` when it's already bookmarked.
pub fn add_bookmark(text: &str, path: &Path) -> Option<String> {
    if parse_bookmarks(text).iter().any(|(p, _)| p == path) {
        return None;
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&uri::path_to_uri(path));
    out.push('\n');
    Some(out)
}

/// The bookmarks file without the lines for `path`.
pub fn remove_bookmark(text: &str, path: &Path) -> String {
    text.lines()
        .filter(|line| {
            let uri = line.trim().split(' ').next().unwrap_or_default();
            uri::uri_to_path(uri).as_deref() != Some(path)
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub source: String,
}

/// Decodes the octal escapes the kernel uses for spaces and other special bytes, such as `\040`.
fn unescape_octal(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
        {
            let value =
                bytes[i + 1..i + 4].iter().fold(0u32, |acc, b| acc * 8 + u32::from(b - b'0'));
            out.push(u8::try_from(value).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses `/proc/self/mountinfo`; malformed lines are skipped.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let mount_point = before.split(' ').nth(4)?;
            let mut after = after.split(' ');
            let fs_type = after.next()?;
            let source = after.next()?;
            Some(Mount {
                mount_point: PathBuf::from(unescape_octal(mount_point)),
                fs_type: fs_type.to_string(),
                source: unescape_octal(source),
            })
        })
        .collect()
}

/// File systems that hold no user files.
const PSEUDO_FS: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "securityfs",
    "cgroup",
    "cgroup2",
    "pstore",
    "efivarfs",
    "bpf",
    "debugfs",
    "tracefs",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "autofs",
    "binfmt_misc",
    "rpc_pipefs",
    "nsfs",
    "ramfs",
    "overlay",
    "squashfs",
    "fuse.portal",
    "fuse.gvfsd-fuse",
    "selinuxfs",
    "rootfs",
];

/// Whether a mount is a drive the user would browse: a real file system under a removable-media folder.
pub fn is_user_mount(mount: &Mount) -> bool {
    if PSEUDO_FS.contains(&mount.fs_type.as_str()) {
        return false;
    }
    let point = &mount.mount_point;
    let under_media = ["/media", "/run/media", "/mnt"]
        .iter()
        .any(|base| point.starts_with(base) && point != Path::new(base));
    let real_source = mount.source.starts_with("/dev/")
        || mount.fs_type.starts_with("fuse")
        || mount.fs_type.contains("nfs")
        || mount.fs_type == "cifs";
    under_media && real_source
}

/// "Computer" for the root file system, then each user mount, deduplicated by mount point.
pub fn device_places(mounts: &[Mount]) -> Vec<Place> {
    let mut places = vec![Place {
        label: "Computer".into(),
        target: Target::Dir(PathBuf::from("/")),
        icon: PlaceIcon::Computer,
        section: Section::Devices,
        volume: None,
    }];
    for mount in mounts.iter().filter(|m| is_user_mount(m)) {
        let target = Target::Dir(mount.mount_point.clone());
        if places.iter().any(|p| p.target == target) {
            continue;
        }
        places.push(Place {
            label: mount
                .mount_point
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| mount.source.clone()),
            target,
            icon: PlaceIcon::Drive,
            section: Section::Devices,
            volume: None,
        });
    }
    places
}

/// Whether the sidebar offers to eject `volume`: it's mounted, or its drive can be ejected or powered off.
pub fn can_eject(volume: &Volume) -> bool {
    volume.mount_point().is_some() || !matches!(volume.removal(), Command::Unmount(_))
}

/// `places` with the udisks volumes among the devices.
/// A mounted volume takes over the place of its mount point;
/// the others follow the devices, and opening one mounts it.
pub fn with_volumes(places: &[Place], volumes: &[Volume]) -> Vec<Place> {
    let mut places = places.to_vec();
    let mut end =
        places.iter().rposition(|p| p.section == Section::Devices).map_or(places.len(), |i| i + 1);
    for volume in volumes {
        let target = match volume.mount_point() {
            Some(point) => Target::Dir(point.to_path_buf()),
            None => Target::Volume(volume.id.clone()),
        };
        let place = Place {
            label: volume.name.clone(),
            target,
            icon: PlaceIcon::Drive,
            section: Section::Devices,
            volume: Some(volume.clone()),
        };
        match places.iter_mut().find(|p| p.section == Section::Devices && p.target == place.target)
        {
            Some(existing) => *existing = place,
            None => {
                places.insert(end, place);
                end += 1;
            }
        }
    }
    places
}

/// The mount points of real file systems, deepest first, for finding a file's top directory.
pub fn mount_points(mounts: &[Mount]) -> Vec<PathBuf> {
    let mut points: Vec<PathBuf> = mounts
        .iter()
        .filter(|m| !PSEUDO_FS.contains(&m.fs_type.as_str()) || m.mount_point == Path::new("/"))
        .map(|m| m.mount_point.clone())
        .collect();
    points
        .sort_by(|a, b| b.components().count().cmp(&a.components().count()).then_with(|| a.cmp(b)));
    points.dedup();
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
23 22 0:21 / /proc rw,nosuid shared:12 - proc proc rw
24 22 0:22 / /sys rw shared:2 - sysfs sysfs rw
25 22 0:5 / /dev rw shared:3 - devtmpfs devtmpfs rw
40 22 259:1 / /boot/efi rw shared:30 - vfat /dev/nvme0n1p1 rw
41 22 0:40 / /run/user/1000 rw shared:200 - tmpfs tmpfs rw
90 22 8:17 / /run/media/ada/My\\040Stick rw,nosuid shared:300 - exfat /dev/sdb1 rw
91 22 8:33 / /media/backup rw shared:301 - fuseblk /dev/sdc1 rw
92 22 8:33 / /media/backup rw shared:301 - fuseblk /dev/sdc1 rw
93 22 0:50 / /mnt rw shared:302 - ext4 /dev/sdd1 rw
94 22 0:51 / /mnt/nas rw shared:303 - nfs4 nas:/export rw
garbage line
";

    #[test]
    fn mountinfo() {
        let mounts = parse_mountinfo(MOUNTINFO);
        assert_eq!(mounts.len(), 11);
        assert_eq!(mounts[0].mount_point, PathBuf::from("/"));
        assert_eq!(mounts[6].mount_point, PathBuf::from("/run/media/ada/My Stick"));
        assert_eq!(mounts[6].fs_type, "exfat");
        let places = device_places(&mounts);
        let labels: Vec<&str> = places.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["Computer", "My Stick", "backup", "nas"]);
        assert_eq!(places[1].icon, PlaceIcon::Drive);
        let points = mount_points(&mounts);
        assert_eq!(points.last(), Some(&PathBuf::from("/")));
        assert!(!points.contains(&PathBuf::from("/proc")));
        assert!(points.contains(&PathBuf::from("/boot/efi")));
    }

    #[test]
    fn volumes_join_the_devices() {
        let mounts = parse_mountinfo(MOUNTINFO);
        let mut places = device_places(&mounts);
        places.extend(bookmark_places("file:///home/ada/Projects\n"));
        let stick = Volume {
            id: "/org/freedesktop/UDisks2/block_devices/sdb1".into(),
            name: "My Stick".into(),
            mount_points: vec!["/run/media/ada/My Stick".into()],
            ..Volume::default()
        };
        let card = Volume {
            id: "/org/freedesktop/UDisks2/block_devices/mmcblk0p1".into(),
            name: "Camera".into(),
            ..Volume::default()
        };
        let joined = with_volumes(&places, &[stick.clone(), card.clone()]);
        let labels: Vec<&str> = joined.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["Computer", "My Stick", "backup", "nas", "Camera", "Projects"]);
        assert_eq!(joined[1].volume.as_ref(), Some(&stick));
        assert_eq!(joined[4].target, Target::Volume(card.id.clone()));
        assert_eq!(joined[4].section, Section::Devices);
        assert_eq!(with_volumes(&places, &[]), places);
    }

    #[test]
    fn octal_escapes() {
        assert_eq!(unescape_octal("a\\040b\\011c"), "a b\tc");
        assert_eq!(unescape_octal("trail\\04"), "trail\\04");
        assert_eq!(unescape_octal("\\777"), "?");
        assert_eq!(unescape_octal("\\"), "\\");
    }

    #[test]
    fn user_dirs() {
        let home = Path::new("/home/ada");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\nXDG_MUSIC_DIR=\"$HOME\"\n\
                    XDG_VIDEOS_DIR=\"/data/Videos\"\nXDG_BROKEN_DIR=relative\nnonsense\n";
        let entries = parse_user_dirs(text, home);
        assert_eq!(
            entries,
            [
                ("DESKTOP".to_string(), PathBuf::from("/home/ada/Schreibtisch")),
                ("MUSIC".to_string(), PathBuf::from("/home/ada")),
                ("VIDEOS".to_string(), PathBuf::from("/data/Videos")),
            ]
        );
        assert_eq!(
            user_dir(&entries, "DESKTOP", home),
            Some(PathBuf::from("/home/ada/Schreibtisch"))
        );
        assert_eq!(user_dir(&entries, "MUSIC", home), None);
        assert_eq!(
            user_dir(&entries, "DOWNLOAD", home),
            Some(PathBuf::from("/home/ada/Downloads"))
        );
        assert_eq!(user_dir(&entries, "UNKNOWN", home), None);
    }

    #[test]
    fn standard_places_only_list_existing_folders() {
        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path();
        std::fs::create_dir(home.join("Documents")).expect("mkdir");
        std::fs::create_dir(home.join("Bilder")).expect("mkdir");
        let text = "XDG_PICTURES_DIR=\"$HOME/Bilder\"\n";
        let places = standard_places(home, Some(text));
        let labels: Vec<&str> = places.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["Home", "Documents", "Bilder", "Trash"]);
        assert_eq!(places[2].icon, PlaceIcon::Pictures);
        assert_eq!(places[3].target, Target::Trash);
        assert_eq!(standard_places(home, None).len(), 3);
    }

    #[test]
    fn bookmarks() {
        let text =
            "file:///home/ada/Projects\nfile:///home/ada/a%20b Work stuff\nsftp://host/x\n\n";
        let places = bookmark_places(text);
        assert_eq!(places.len(), 2);
        assert_eq!(places[0].label, "Projects");
        assert_eq!(places[1].label, "Work stuff");
        assert_eq!(places[1].target, Target::Dir(PathBuf::from("/home/ada/a b")));
        assert_eq!(bookmark_places("file:///")[0].label, "/");

        assert_eq!(add_bookmark(text, Path::new("/home/ada/Projects")), None);
        let added = add_bookmark("file:///a", Path::new("/b c")).expect("added");
        assert_eq!(added, "file:///a\nfile:///b%20c\n");
        assert_eq!(add_bookmark("", Path::new("/x")).as_deref(), Some("file:///x\n"));
        assert_eq!(remove_bookmark(&added, Path::new("/a")), "file:///b%20c\n");
    }
}
