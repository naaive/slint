// SPDX-License-Identifier: MIT

//! The udisks client's D-Bus side: snapshots of the volumes, and the commands, each on a task of its own.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;
use zbus::Connection;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::Value;

use super::parse::{self, DRIVE, FILESYSTEM, ManagedObjects};
use super::{Command, Event, Volume};
use crate::bus::{self, BusService, Wake};

const ROOT: &str = "/org/freedesktop/UDisks2";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
/// The polkit dialog stays open until the user answers, and unmounting waits for writes to reach the drive.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);
const DISMISSED: &str = "org.freedesktop.UDisks2.Error.NotAuthorizedDismissed";
const NEEDS_AUTHORIZATION: &str = "org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain";

pub(crate) type Emit = Arc<dyn Fn(Event) + Send + Sync>;

pub(crate) struct Udisks {
    emit: Emit,
    /// The last snapshot, which commands act on.
    volumes: Option<Vec<Volume>>,
    /// The volumes since udisks appeared, so a new one counts as added; `None` before its first snapshot.
    known: Option<HashSet<String>>,
}

impl Udisks {
    pub(crate) fn new(emit: Emit) -> Self {
        Self { emit, volumes: None, known: None }
    }

    fn publish(&mut self, volumes: Vec<Volume>) {
        if self.volumes.as_ref() != Some(&volumes) {
            (self.emit)(Event::Volumes(volumes.clone()));
            for volume in &volumes {
                if self.known.as_ref().is_some_and(|known| !known.contains(&volume.id)) {
                    (self.emit)(Event::Added(volume.clone()));
                }
            }
        }
        self.known = Some(volumes.iter().map(|v| v.id.clone()).collect());
        self.volumes = Some(volumes);
    }

    fn handle(&self, conn: &Connection, owner: &OwnedUniqueName, command: Command) {
        let volumes = self.volumes.as_deref().unwrap_or_default();
        let Some(volume) = volumes.iter().find(|v| v.id == command.volume()) else {
            (self.emit)(Event::Failed {
                command,
                message: "The volume is no longer there.".into(),
            });
            return;
        };
        let steps = steps(volumes, volume, &command);
        let (conn, owner, emit) = (conn.clone(), owner.to_string(), self.emit.clone());
        tokio::spawn(async move {
            for step in steps {
                match step.run(&conn, &owner).await {
                    Ok(Some(event)) => emit(event),
                    Ok(None) => {}
                    Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == DISMISSED => {
                        tracing::debug!("the user dismissed authorizing {command:?}");
                        emit(Event::Dismissed(command));
                        return;
                    }
                    Err(zbus::Error::MethodError(name, _, _))
                        if name.as_str() == NEEDS_AUTHORIZATION
                            && matches!(command, Command::Automount(_)) =>
                    {
                        tracing::info!("not mounting {} without authorization", command.volume());
                        return;
                    }
                    Err(err) => {
                        tracing::info!("udisks refused {command:?}: {err}");
                        emit(Event::Failed { command, message: reason(err) });
                        return;
                    }
                }
            }
        });
    }
}

/// One udisks call of a [`Command`].
#[derive(Debug, PartialEq)]
enum Step {
    /// Reports a mount that's already there.
    Mounted {
        id: String,
        mount_point: std::path::PathBuf,
    },
    Mount {
        id: String,
        /// Whether udisks may ask the user for authorization.
        interactive: bool,
    },
    Unmount(String),
    /// Calls `Eject` or `PowerOff` on a drive.
    Drive {
        id: String,
        method: &'static str,
    },
}

/// The calls that carry out `command` on `volume`, given every volume udisks reported.
fn steps(volumes: &[Volume], volume: &Volume, command: &Command) -> Vec<Step> {
    let method = match command {
        Command::Mount(_) | Command::Automount(_) => {
            let id = volume.id.clone();
            return vec![match volume.mount_point() {
                Some(mount_point) => Step::Mounted { id, mount_point: mount_point.to_owned() },
                None => Step::Mount { id, interactive: matches!(command, Command::Mount(_)) },
            }];
        }
        Command::Unmount(_) => "",
        Command::Eject(_) => "Eject",
        Command::PowerOff(_) => "PowerOff",
    };
    let drive = volume.drive.as_ref().filter(|_| !method.is_empty());
    let mut steps: Vec<Step> = volumes
        .iter()
        .filter(|v| match drive {
            Some(drive) => v.drive.as_ref().is_some_and(|d| d.id == drive.id),
            None => v.id == volume.id,
        })
        .filter(|v| v.mount_point().is_some())
        .map(|v| Step::Unmount(v.id.clone()))
        .collect();
    if let Some(drive) = drive {
        steps.push(Step::Drive { id: drive.id.clone(), method });
    }
    steps
}

impl Step {
    async fn run(self, conn: &Connection, owner: &str) -> zbus::Result<Option<Event>> {
        let mut options = HashMap::<&str, Value<'_>>::new();
        match self {
            Step::Mounted { id, mount_point } => Ok(Some(Event::Mounted { id, mount_point })),
            Step::Mount { id, interactive } => {
                if !interactive {
                    options.insert("auth.no_user_interaction", true.into());
                }
                let reply = call(conn, owner, &id, FILESYSTEM, "Mount", &(options,)).await?;
                let mount_point: String = reply.body().deserialize()?;
                Ok(Some(Event::Mounted { id, mount_point: mount_point.into() }))
            }
            Step::Unmount(id) => {
                call(conn, owner, &id, FILESYSTEM, "Unmount", &(options,)).await.map(|_| None)
            }
            Step::Drive { id, method } => {
                call(conn, owner, &id, DRIVE, method, &(options,)).await.map(|_| None)
            }
        }
    }
}

/// Calls a udisks method that may wait for the user, unlike [`bus::call`].
async fn call(
    conn: &Connection,
    owner: &str,
    path: &str,
    interface: &str,
    method: &str,
    body: &(HashMap<&str, Value<'_>>,),
) -> zbus::Result<zbus::Message> {
    let call = conn.call_method(Some(owner), path, Some(interface), method, body);
    timeout(COMMAND_TIMEOUT, call).await.unwrap_or_else(|_| Err(bus::timed_out()))
}

/// udisks's own message, such as "Error unmounting /dev/sdb1: target is busy".
fn reason(err: zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(message), _) => message,
        err => err.to_string(),
    }
}

impl BusService for Udisks {
    type Command = Command;
    const NAME: &'static str = "org.freedesktop.UDisks2";

    async fn run(
        &mut self,
        conn: &Connection,
        owner: &OwnedUniqueName,
        commands: &mut UnboundedReceiver<Command>,
    ) -> zbus::Result<()> {
        let objects_rule = bus::signal_rule()
            .sender(owner.to_owned().into_inner())?
            .path(ROOT)?
            .interface(OBJECT_MANAGER)?
            .build();
        let rules = [objects_rule, bus::properties_changed_rule(owner, ROOT)?];
        let mut signals = bus::signals(conn, rules).await?;
        tracing::info!("Watching volumes from udisks");
        loop {
            let objects: ManagedObjects =
                bus::call_for(conn, owner, ROOT, OBJECT_MANAGER, "GetManagedObjects", &()).await?;
            self.publish(parse::volumes(&objects));
            match bus::next_wake(&mut signals, commands).await? {
                Some(Wake::Signal) => {}
                Some(Wake::Command(command)) => self.handle(conn, owner, command),
                None => return Ok(()),
            }
        }
    }

    fn unavailable(&mut self) {
        self.publish(Vec::new());
        self.known = None;
    }

    fn command_unavailable(&mut self, command: Command) {
        let message = "udisks isn't running.".into();
        (self.emit)(Event::Failed { command, message });
    }
}

#[cfg(test)]
mod tests {
    use super::super::Drive;
    use super::*;

    fn volume(id: &str, drive: Option<&str>, mounted: bool) -> Volume {
        Volume {
            id: id.into(),
            mount_points: if mounted { vec![format!("/media/{id}").into()] } else { Vec::new() },
            drive: drive.map(|drive| Drive { id: drive.into(), ..Drive::default() }),
            ..Volume::default()
        }
    }

    #[test]
    fn mounting_a_mounted_volume_reports_it() {
        let volumes = [volume("a", None, true), volume("b", None, false)];
        assert_eq!(
            steps(&volumes, &volumes[0], &Command::Mount("a".into())),
            [Step::Mounted { id: "a".into(), mount_point: "/media/a".into() }]
        );
        assert_eq!(
            steps(&volumes, &volumes[1], &Command::Mount("b".into())),
            [Step::Mount { id: "b".into(), interactive: true }]
        );
        assert_eq!(
            steps(&volumes, &volumes[1], &Command::Automount("b".into())),
            [Step::Mount { id: "b".into(), interactive: false }]
        );
        assert_eq!(steps(&volumes, &volumes[1], &Command::Unmount("b".into())), []);
    }

    #[test]
    fn removing_a_drive_unmounts_all_of_its_volumes_first() {
        let volumes = [
            volume("a", Some("stick"), true),
            volume("b", Some("stick"), false),
            volume("c", Some("stick"), true),
            volume("d", Some("disk"), true),
        ];
        assert_eq!(
            steps(&volumes, &volumes[1], &Command::PowerOff("b".into())),
            [
                Step::Unmount("a".into()),
                Step::Unmount("c".into()),
                Step::Drive { id: "stick".into(), method: "PowerOff" },
            ]
        );
        assert_eq!(
            steps(&volumes, &volumes[0], &Command::Unmount("a".into())),
            [Step::Unmount("a".into())]
        );
        let loop_device = volume("e", None, true);
        assert_eq!(
            steps(std::slice::from_ref(&loop_device), &loop_device, &Command::Eject("e".into())),
            [Step::Unmount("e".into())]
        );
    }

    #[test]
    fn volumes_after_the_first_snapshot_count_as_added() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = events.clone();
        let mut udisks = Udisks::new(Arc::new(move |event| sink.lock().unwrap().push(event)));
        udisks.unavailable();
        udisks.publish(Vec::new());
        udisks.publish(vec![volume("a", None, false)]);
        udisks.publish(vec![volume("a", None, false), volume("b", None, false)]);
        udisks.publish(vec![volume("a", None, false), volume("b", None, false)]);
        udisks.unavailable();
        udisks.publish(vec![volume("a", None, false)]);
        let events = events.lock().unwrap();
        let summary: Vec<String> = events
            .iter()
            .map(|event| match event {
                Event::Volumes(volumes) => format!("volumes {}", volumes.len()),
                Event::Added(volume) => format!("added {}", volume.id),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            summary,
            ["volumes 0", "volumes 1", "added a", "volumes 2", "added b", "volumes 0", "volumes 1"]
        );
    }
}
