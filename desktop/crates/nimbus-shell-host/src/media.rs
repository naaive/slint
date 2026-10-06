// SPDX-License-Identifier: MIT

//! Removable media: mounting inserted media, a toast with an "Open" action for each,
//! and opening a volume in the file manager.

use crate::state::State;
use nimbus_services::udisks::{Command, Event, Volume};
use nimbus_services::{CloseReason, Notification, ServiceCommand, ServiceEvent, Urgency};
use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

const OPEN_ACTION: &str = "open";
/// The file manager for folders when `mimeapps.list` names none.
const FILE_MANAGER: &str = "org.nimbus.Files";

#[derive(Default)]
pub struct Media {
    /// The volumes udisks reported last, by id.
    volumes: HashMap<String, Volume>,
    /// The volume of each removable media toast, by notification id.
    toasts: HashMap<u32, String>,
    /// Volumes to open once they're mounted, with the activation token for the file manager.
    opening: HashMap<String, Option<String>>,
}

/// Whether to mount a volume as it's inserted.
fn mounts_on_insertion(volume: &Volume, automount: bool, locked: bool) -> bool {
    automount && !locked && volume.removable() && volume.mount_point().is_none()
}

fn toast(id: u32, volume: &Volume) -> Notification {
    let drive = volume.drive.as_ref().map(|drive| drive.name.clone()).unwrap_or_default();
    Notification {
        id,
        app_name: "Removable Media".into(),
        app_icon: volume.icon.clone(),
        summary: format!("{} connected", volume.name),
        body: drive,
        actions: vec![(OPEN_ACTION.into(), "Open".into())],
        urgency: Urgency::Normal,
        expire_timeout: None,
        received: SystemTime::now(),
        transient: false,
        resident: false,
    }
}

impl State {
    pub fn disks_event(&mut self, event: Event) {
        match event {
            Event::Volumes(volumes) => {
                let media = &mut self.services.media;
                media.volumes = volumes.into_iter().map(|v| (v.id.clone(), v)).collect();
                media.opening.retain(|id, _| media.volumes.contains_key(id));
                let volumes = &media.volumes;
                let gone: Vec<u32> = media
                    .toasts
                    .extract_if(|_, volume| !volumes.contains_key(volume))
                    .map(|(id, _)| id)
                    .collect();
                for id in gone {
                    let reason = CloseReason::Closed;
                    self.model
                        .handle_service_event(&ServiceEvent::NotificationClosed { id, reason });
                }
            }
            Event::Added(volume) => {
                let automount = self.settings.current.media.automount;
                if mounts_on_insertion(&volume, automount, self.model.is_locked()) {
                    tracing::info!(volume = %volume.device.display(), "mounting inserted media");
                    self.services.send(ServiceCommand::Disks(Command::Mount(volume.id.clone())));
                }
                if volume.removable() {
                    let id = self.services.next_local_id();
                    self.services.media.toasts.insert(id, volume.id.clone());
                    self.model
                        .handle_service_event(&ServiceEvent::Notification(toast(id, &volume)));
                }
            }
            Event::Mounted { id, mount_point } => {
                if let Some(token) = self.services.media.opening.remove(&id) {
                    self.open_folder(&mount_point, token.as_deref());
                }
            }
            Event::Failed { command, message } => {
                let media = &mut self.services.media;
                media.opening.remove(command.volume());
                let name = media.volumes.get(command.volume()).map_or("the volume", |v| &v.name);
                let summary = format!("Couldn't {} {name}", command.verb());
                self.show_error(summary, message);
            }
        }
    }

    /// Opens the volume of a removable media toast, mounting it first if needed.
    /// Returns `false` when `id` isn't such a toast.
    pub fn media_action(&mut self, id: u32, action: &str, token: Option<String>) -> bool {
        let media = &mut self.services.media;
        let Some(volume) = media.toasts.remove(&id) else {
            return false;
        };
        if action != OPEN_ACTION {
            return true;
        }
        let mount_point =
            media.volumes.get(&volume).and_then(|v| v.mount_point()).map(Path::to_owned);
        match mount_point {
            Some(mount_point) => self.open_folder(&mount_point, token.as_deref()),
            None => {
                media.opening.insert(volume.clone(), token);
                self.services.send(ServiceCommand::Disks(Command::Mount(volume)));
            }
        }
        true
    }

    fn open_folder(&mut self, path: &Path, token: Option<&str>) {
        let id = nimbus_xdg::default_handler_for_mime("inode/directory");
        let entry = id.and_then(|id| self.apps.get(&id)).or_else(|| self.apps.get(FILE_MANAGER));
        let result = match entry {
            Some(entry) => {
                nimbus_xdg::launch(&entry, &[path.to_owned()], token).map_err(|err| err.to_string())
            }
            None => Err("No file manager is installed.".to_string()),
        };
        if let Err(message) = result {
            tracing::warn!("cannot open {}: {message}", path.display());
            self.show_error(format!("Couldn't open {}", path.display()), message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_services::udisks::Drive;

    fn stick(mounted: bool) -> Volume {
        Volume {
            id: "/org/freedesktop/UDisks2/block_devices/sdb1".into(),
            name: "STICK".into(),
            icon: "media-removable".into(),
            mount_points: if mounted { vec!["/run/media/ada/STICK".into()] } else { Vec::new() },
            drive: Some(Drive { name: "Acme Stick".into(), removable: true, ..Drive::default() }),
            ..Volume::default()
        }
    }

    #[test]
    fn mounts_unmounted_removable_media_while_unlocked() {
        assert!(mounts_on_insertion(&stick(false), true, false));
        assert!(!mounts_on_insertion(&stick(false), false, false));
        assert!(!mounts_on_insertion(&stick(false), true, true));
        assert!(!mounts_on_insertion(&stick(true), true, false));
        let fixed = Volume { drive: None, ..stick(false) };
        assert!(!mounts_on_insertion(&fixed, true, false));
    }

    #[test]
    fn toast_offers_to_open_the_volume() {
        let toast = toast(7, &stick(false));
        assert_eq!(toast.id, 7);
        assert_eq!(toast.summary, "STICK connected");
        assert_eq!(toast.body, "Acme Stick");
        assert_eq!(toast.app_icon, "media-removable");
        assert_eq!(toast.actions, [("open".to_string(), "Open".to_string())]);
    }
}
