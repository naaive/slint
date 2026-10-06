// SPDX-License-Identifier: MIT

//! The udisks client against a fake udisks on a private `dbus-daemon`.

use std::time::Duration;

use nimbus_services::udisks::{Client, Command, Event, Volume};
use nimbus_services::{BusAddress, ServiceCommand, ServiceEvent, ServicesBuilder, ServicesConfig};
use nimbus_test_support::{FakeUdisks, MountAnswer, PrivateBus};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);
const ROOT: &str = "/org/freedesktop/UDisks2";
const SDB1: &str = "/org/freedesktop/UDisks2/block_devices/sdb1";
const SDB2: &str = "/org/freedesktop/UDisks2/block_devices/sdb2";

async fn next(events: &mut UnboundedReceiver<Event>) -> Event {
    timeout(WAIT, events.recv()).await.expect("an event in time").expect("the client runs")
}

/// Skips events until `pick` accepts one.
async fn wait_for<T>(
    events: &mut UnboundedReceiver<Event>,
    pick: impl Fn(Event) -> Option<T>,
) -> T {
    loop {
        if let Some(found) = pick(next(events).await) {
            return found;
        }
    }
}

async fn volumes_where(
    events: &mut UnboundedReceiver<Event>,
    matches: impl Fn(&[Volume]) -> bool,
) -> Vec<Volume> {
    wait_for(events, |event| match event {
        Event::Volumes(volumes) if matches(&volumes) => Some(volumes),
        _ => None,
    })
    .await
}

fn client(bus: &PrivateBus) -> (Client, UnboundedReceiver<Event>) {
    let (sender, events) = unbounded_channel();
    let client = Client::spawn(BusAddress::Address(bus.address.clone()), move |event| {
        let _ = sender.send(event);
    });
    (client, events)
}

#[tokio::test]
async fn lists_mounts_and_powers_off_a_stick() {
    let Some(bus) = PrivateBus::start() else { return };
    let fake = FakeUdisks::start(&bus).await;
    fake.insert(SDB1, "/dev/sdb1", "STICK", MountAnswer::Mount).await;
    let (client, mut events) = client(&bus);

    assert_eq!(next(&mut events).await, Event::Volumes(Vec::new()));
    let volumes = volumes_where(&mut events, |v| !v.is_empty()).await;
    assert_eq!(volumes.len(), 1);
    let stick = &volumes[0];
    assert_eq!((stick.id.as_str(), stick.name.as_str()), (SDB1, "STICK"));
    assert_eq!(stick.device, std::path::Path::new("/dev/sdb1"));
    assert_eq!(stick.drive.as_ref().map(|d| d.name.as_str()), Some("Acme Stick"));
    assert!(stick.removable());
    assert_eq!(stick.removal(), Command::PowerOff(SDB1.into()));

    client.send(Command::Mount(SDB1.into()));
    let mounted = wait_for(&mut events, |event| match event {
        Event::Mounted { id, mount_point } => Some((id, mount_point)),
        _ => None,
    })
    .await;
    assert_eq!(mounted, (SDB1.into(), "/run/media/ada/STICK".into()));
    volumes_where(&mut events, |v| v.first().is_some_and(|v| v.mount_point().is_some())).await;

    fake.insert(SDB2, "/dev/sdb2", "DATA", MountAnswer::Fail).await;
    let added = wait_for(&mut events, |event| match event {
        Event::Added(volume) => Some(volume),
        _ => None,
    })
    .await;
    assert_eq!(added.id, SDB2);

    client.send(Command::PowerOff(SDB2.into()));
    volumes_where(&mut events, |v| v.iter().all(|v| v.mount_point().is_none())).await;
    timeout(WAIT, async {
        while !fake.calls().contains(&"power off".to_string()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the drive powers off");
    assert_eq!(fake.calls(), ["mount STICK", "unmount STICK", "power off"]);
}

#[tokio::test]
async fn reports_failures_and_dismissed_dialogs() {
    let Some(bus) = PrivateBus::start() else { return };
    let fake = FakeUdisks::start(&bus).await;
    fake.insert(SDB1, "/dev/sdb1", "STICK", MountAnswer::Mount).await;
    fake.insert(SDB2, "/dev/sdb2", "DATA", MountAnswer::Dismiss).await;
    let sdc1 = "/org/freedesktop/UDisks2/block_devices/sdc1";
    fake.insert(sdc1, "/dev/sdc1", "BROKEN", MountAnswer::Fail).await;
    let (client, mut events) = client(&bus);
    volumes_where(&mut events, |v| v.len() == 3).await;

    client.send(Command::Mount(SDB2.into()));
    client.send(Command::Mount(sdc1.into()));
    client.send(Command::Unmount("/org/freedesktop/UDisks2/block_devices/gone".into()));
    let (mut failures, mut dismissed) = (Vec::new(), Vec::new());
    while failures.len() < 2 || dismissed.is_empty() {
        match next(&mut events).await {
            Event::Failed { command, message } => failures.push((command, message)),
            Event::Dismissed(command) => dismissed.push(command),
            _ => {}
        }
    }
    assert_eq!(dismissed, [Command::Mount(SDB2.into())]);
    failures.sort_by(|a, b| a.0.volume().cmp(b.0.volume()));
    assert_eq!(
        failures,
        [
            (
                Command::Unmount(format!("{ROOT}/block_devices/gone")),
                "The volume is no longer there.".into()
            ),
            (Command::Mount(sdc1.into()), "Unknown file system".into()),
        ]
    );
    assert!(fake.calls().contains(&"mount DATA".to_string()));
    assert!(
        timeout(Duration::from_millis(300), async {
            loop {
                if let Event::Failed { .. } = next(&mut events).await {
                    return;
                }
            }
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn automounting_never_asks_for_authorization() {
    let Some(bus) = PrivateBus::start() else { return };
    let fake = FakeUdisks::start(&bus).await;
    fake.insert(SDB1, "/dev/sdb1", "STICK", MountAnswer::Authorize).await;
    let (client, mut events) = client(&bus);
    volumes_where(&mut events, |v| v.len() == 1).await;

    client.send(Command::Automount(SDB1.into()));
    let quiet = async {
        while !fake.calls().contains(&"mount STICK quietly".to_string()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    timeout(WAIT, quiet).await.expect("a quiet mount in time");
    assert!(
        timeout(Duration::from_millis(300), async {
            loop {
                if let Event::Failed { .. } | Event::Mounted { .. } = next(&mut events).await {
                    return;
                }
            }
        })
        .await
        .is_err(),
        "an automount that needs authorization reported back"
    );

    client.send(Command::Mount(SDB1.into()));
    let mounted = wait_for(&mut events, |event| match event {
        Event::Mounted { mount_point, .. } => Some(mount_point.clone()),
        _ => None,
    })
    .await;
    assert_eq!(mounted, std::path::Path::new("/run/media/ada/STICK"));
    assert_eq!(fake.calls(), ["mount STICK quietly", "mount STICK"]);
}

async fn service_event(events: &mut UnboundedReceiver<ServiceEvent>) -> ServiceEvent {
    timeout(WAIT, events.recv()).await.expect("an event in time").expect("the services run")
}

#[tokio::test]
async fn reaches_the_shell_through_the_services() {
    let Some(bus) = PrivateBus::start() else { return };
    let fake = FakeUdisks::start(&bus).await;
    fake.insert(SDB1, "/dev/sdb1", "STICK", MountAnswer::Mount).await;
    let (sender, mut events) = unbounded_channel();
    let config = ServicesConfig {
        notifications: false,
        upower: false,
        network_manager: false,
        audio: false,
        backlight: false,
        mpris: false,
        bluetooth: false,
        logind: false,
        polkit: false,
        udisks: true,
    };
    let services = ServicesBuilder::new(config)
        .session_bus(BusAddress::Disabled)
        .system_bus(BusAddress::Address(bus.address.clone()))
        .spawn(move |event| {
            let _ = sender.send(event);
        });
    loop {
        if let ServiceEvent::Disks(Event::Volumes(volumes)) = service_event(&mut events).await
            && volumes.len() == 1
        {
            break;
        }
    }
    services.send(ServiceCommand::Disks(Command::Mount(SDB1.into())));
    loop {
        if let ServiceEvent::Disks(Event::Mounted { id, .. }) = service_event(&mut events).await {
            assert_eq!(id, SDB1);
            break;
        }
    }
    assert_eq!(fake.calls(), ["mount STICK"]);
}
