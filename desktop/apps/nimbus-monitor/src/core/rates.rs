// SPDX-License-Identifier: MIT

//! Turns cumulative I/O counters into per-second rates.

use std::collections::HashMap;
use std::time::Duration;

use super::snapshot::{DiskRates, InterfaceRates, ProcessInfo, ProcessKey, RawSample, Snapshot};

#[derive(Debug, Default)]
struct Previous {
    at: Duration,
    processes: HashMap<ProcessKey, (u64, u64)>,
    interfaces: HashMap<String, (u64, u64)>,
    disks: HashMap<String, (u64, u64)>,
}

/// Remembers the previous sample's counters and computes rates against them.
///
/// Processes are matched by [`ProcessKey`], so a reused PID starts from zero instead of showing a bogus rate.
#[derive(Debug, Default)]
pub struct RateTracker {
    previous: Option<Previous>,
}

impl RateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts `raw` into a snapshot, with rates of zero for anything not seen in the previous sample.
    pub fn update(&mut self, raw: RawSample) -> Snapshot {
        let previous = self.previous.take().unwrap_or_default();
        let seconds = raw.at.saturating_sub(previous.at).as_secs_f64();
        let rate = |new: u64, old: Option<u64>| match old {
            Some(old) if seconds > 0.0 && new >= old => (new - old) as f64 / seconds,
            _ => 0.0,
        };

        let mut next = Previous { at: raw.at, ..Previous::default() };
        let processes = raw
            .processes
            .into_iter()
            .map(|p| {
                let old = previous.processes.get(&p.key);
                next.processes.insert(p.key, (p.disk_read_total, p.disk_written_total));
                ProcessInfo {
                    key: p.key,
                    parent: p.parent,
                    disk_read_rate: rate(p.disk_read_total, old.map(|o| o.0)),
                    disk_write_rate: rate(p.disk_written_total, old.map(|o| o.1)),
                    name: p.name,
                    command: p.command,
                    user: p.user,
                    cpu: p.cpu,
                    memory: p.memory,
                    state: p.state,
                    kind: p.kind,
                }
            })
            .collect();
        let interfaces = raw
            .interfaces
            .into_iter()
            .map(|i| {
                let old = previous.interfaces.get(&i.name);
                next.interfaces.insert(i.name.clone(), (i.received_total, i.transmitted_total));
                InterfaceRates {
                    receive_rate: rate(i.received_total, old.map(|o| o.0)),
                    transmit_rate: rate(i.transmitted_total, old.map(|o| o.1)),
                    received_total: i.received_total,
                    transmitted_total: i.transmitted_total,
                    name: i.name,
                }
            })
            .collect();
        let disks = raw
            .disks
            .into_iter()
            .map(|d| {
                let old = previous.disks.get(&d.device);
                next.disks.insert(d.device.clone(), (d.read_total, d.written_total));
                DiskRates {
                    read_rate: rate(d.read_total, old.map(|o| o.0)),
                    write_rate: rate(d.written_total, old.map(|o| o.1)),
                    device: d.device,
                }
            })
            .collect();
        self.previous = Some(next);

        Snapshot {
            cpu: raw.cpu,
            memory: raw.memory,
            load: raw.load,
            uptime: raw.uptime,
            processes,
            interfaces,
            disks,
            temperatures: raw.temperatures,
            file_systems: raw.file_systems,
            host: raw.host,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::snapshot::{RawDisk, RawInterface, RawProcess};

    fn sample(at: u64, pid_start: u64, read: u64, received: u64, disk: u64) -> RawSample {
        RawSample {
            at: Duration::from_secs(at),
            processes: vec![RawProcess {
                key: ProcessKey { pid: 7, start_time: pid_start },
                disk_read_total: read,
                ..RawProcess::default()
            }],
            interfaces: vec![RawInterface {
                name: "eth0".into(),
                received_total: received,
                transmitted_total: 0,
            }],
            disks: vec![RawDisk { device: "sda".into(), read_total: disk, written_total: 0 }],
            ..RawSample::default()
        }
    }

    #[test]
    fn first_sample_has_zero_rates() {
        let snapshot = RateTracker::new().update(sample(10, 1, 500, 500, 500));
        assert_eq!(snapshot.processes[0].disk_read_rate, 0.0);
        assert_eq!(snapshot.interfaces[0].receive_rate, 0.0);
        assert_eq!(snapshot.disks[0].read_rate, 0.0);
        assert_eq!(snapshot.interfaces[0].received_total, 500);
    }

    #[test]
    fn rates_are_per_second() {
        let mut tracker = RateTracker::new();
        tracker.update(sample(10, 1, 0, 1000, 0));
        let snapshot = tracker.update(sample(12, 1, 4000, 3000, 800));
        assert_eq!(snapshot.processes[0].disk_read_rate, 2000.0);
        assert_eq!(snapshot.interfaces[0].receive_rate, 1000.0);
        assert_eq!(snapshot.disks[0].read_rate, 400.0);
    }

    #[test]
    fn reused_pid_and_counter_reset_give_zero() {
        let mut tracker = RateTracker::new();
        tracker.update(sample(10, 1, 100, 1000, 100));
        let snapshot = tracker.update(sample(11, 2, 5000, 10, 50));
        assert_eq!(snapshot.processes[0].disk_read_rate, 0.0);
        assert_eq!(snapshot.interfaces[0].receive_rate, 0.0);
        assert_eq!(snapshot.disks[0].read_rate, 0.0);
    }

    #[test]
    fn same_timestamp_gives_zero() {
        let mut tracker = RateTracker::new();
        tracker.update(sample(10, 1, 0, 0, 0));
        let snapshot = tracker.update(sample(10, 1, 100, 100, 100));
        assert_eq!(snapshot.interfaces[0].receive_rate, 0.0);
    }
}
