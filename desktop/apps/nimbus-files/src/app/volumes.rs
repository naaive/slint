// SPDX-License-Identifier: MIT

//! Volumes from udisks in the sidebar: mounting them when opened, and unmounting, ejecting, and powering off drives.

use nimbus_services::udisks::{self, Event, Volume};

use super::controller::{Controller, ToastAction};
use super::convert::SpecialFolders;
use crate::core::history::Location;
use crate::core::menu::{self, Command, VolumeContext};
use crate::core::places::{self, Place};

/// What the controller tracks of udisks's volumes.
#[derive(Default)]
pub(super) struct Disks {
    /// The places without the volumes, as the worker read them.
    pub listed: Vec<Place>,
    pub volumes: Vec<Volume>,
    /// The volume the open menu is for.
    pub menu: Option<Volume>,
    /// The volume to show once it's mounted.
    pub opening: Option<String>,
    /// Volumes being ejected or powered off, to say when they can be removed.
    pub removing: Vec<Volume>,
}

impl Disks {
    /// Forgets what was waiting for `command`, which ended without doing anything.
    fn end(&mut self, command: &udisks::Command) {
        let id = command.volume();
        if self.opening.as_deref() == Some(id) {
            self.opening = None;
        }
        self.removing.retain(|v| v.id != id);
    }
}

impl Controller {
    /// Shows `listed` with the volumes among them.
    pub(super) fn set_listed_places(&self, listed: Vec<Place>) {
        self.state.borrow_mut().disks.listed = listed;
        self.join_volumes();
    }

    fn join_volumes(&self) {
        {
            let mut state = self.state.borrow_mut();
            let joined = places::with_volumes(&state.disks.listed, &state.disks.volumes);
            state.special = SpecialFolders::from_places(&joined);
            state.places = joined;
        }
        self.sync_places();
    }

    pub(super) fn on_disks_event(&self, event: Event) {
        match event {
            Event::Volumes(volumes) => {
                let removed: Vec<String> = {
                    let mut state = self.state.borrow_mut();
                    let disks = &mut state.disks;
                    let (gone, still) = std::mem::take(&mut disks.removing)
                        .into_iter()
                        .partition(|v| !volumes.iter().any(|now| now.id == v.id));
                    disks.removing = still;
                    disks.volumes = volumes;
                    gone.into_iter().map(|v: Volume| v.name).collect()
                };
                self.join_volumes();
                for name in removed {
                    self.show_toast(format!("“{name}” can be removed"), ToastAction::None, None);
                }
            }
            // The shell announces inserted media.
            Event::Added(_) => {}
            Event::Mounted { id, mount_point } => {
                let open = {
                    let mut state = self.state.borrow_mut();
                    let open = state.disks.opening.as_ref() == Some(&id);
                    if open {
                        state.disks.opening = None;
                    }
                    open
                };
                if open {
                    self.navigate(Location::Dir(mount_point));
                }
            }
            Event::Dismissed(command) => self.state.borrow_mut().disks.end(&command),
            Event::Failed { command, message } => {
                let name = {
                    let mut state = self.state.borrow_mut();
                    let disks = &mut state.disks;
                    disks.end(&command);
                    let volume = disks.volumes.iter().find(|v| v.id == command.volume());
                    volume.map_or_else(|| "the volume".to_string(), |v| format!("“{}”", v.name))
                };
                let text = format!("Couldn't {} {name}: {message}", command.verb());
                self.show_toast(text, ToastAction::None, None);
            }
        }
    }

    /// Mounts the volume `id` and shows it.
    pub(super) fn open_volume(&self, id: String) {
        if let Some(disks) = &self.disks {
            self.state.borrow_mut().disks.opening = Some(id.clone());
            disks.send(udisks::Command::Mount(id));
        }
    }

    pub(super) fn place_eject(&self, index: usize) {
        let volume = self.state.borrow().places.get(index).and_then(|p| p.volume.clone());
        if let Some(volume) = volume {
            self.release(volume.removal(), volume);
        }
    }

    pub(super) fn place_context(&self, index: usize, x: f32, y: f32) {
        let Some(volume) = self.state.borrow().places.get(index).and_then(|p| p.volume.clone())
        else {
            return;
        };
        let drive = volume.drive.clone().unwrap_or_default();
        let ctx = VolumeContext {
            mounted: volume.mount_point().is_some(),
            ejectable: drive.ejectable,
            can_power_off: drive.can_power_off,
        };
        self.state.borrow_mut().disks.menu = Some(volume);
        self.show_menu(menu::volume_menu(ctx), None, x, y);
    }

    /// Runs a command from a volume's menu.
    pub(super) fn volume_command(&self, command: Command) {
        let Some(volume) = self.state.borrow_mut().disks.menu.take() else { return };
        let id = volume.id.clone();
        match command {
            Command::OpenPlace => match volume.mount_point() {
                Some(point) => self.navigate(Location::Dir(point.to_path_buf())),
                None => self.open_volume(id),
            },
            Command::Mount => {
                if let Some(disks) = &self.disks {
                    disks.send(udisks::Command::Mount(id));
                }
            }
            Command::Unmount => self.release(udisks::Command::Unmount(id), volume),
            Command::Eject => self.release(udisks::Command::Eject(id), volume),
            Command::PowerOff => self.release(udisks::Command::PowerOff(id), volume),
            _ => {}
        }
    }

    /// Unmounts, ejects, or powers off `volume`, leaving it first if it's showing.
    fn release(&self, command: udisks::Command, volume: Volume) {
        let Some(disks) = &self.disks else { return };
        let showing = self
            .current_dir()
            .is_some_and(|dir| volume.mount_points.iter().any(|point| dir.starts_with(point)));
        if showing {
            self.navigate(Location::Dir(self.env.home.clone()));
        }
        if matches!(command, udisks::Command::Eject(_) | udisks::Command::PowerOff(_)) {
            self.state.borrow_mut().disks.removing.push(volume);
        }
        disks.send(command);
    }
}
