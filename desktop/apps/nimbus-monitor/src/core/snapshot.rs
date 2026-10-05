// SPDX-License-Identifier: MIT

//! Plain data describing the system at one moment.
//!
//! [`RawSample`] holds cumulative counters as the kernel reports them;
//! [`Snapshot`] holds per-second rates computed from two raw samples by [`crate::core::rates::RateTracker`].

use std::time::Duration;

/// Identifies a process across samples; the start time tells a reused PID apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessKey {
    pub pid: u32,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProcessState {
    Running,
    #[default]
    Sleeping,
    /// Uninterruptible sleep, usually waiting for disk I/O.
    DiskSleep,
    Stopped,
    Zombie,
    Idle,
    Other,
}

impl ProcessState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Sleeping => "Sleeping",
            Self::DiskSleep => "Waiting",
            Self::Stopped => "Stopped",
            Self::Zombie => "Zombie",
            Self::Idle => "Idle",
            Self::Other => "Unknown",
        }
    }
}

/// A coarse classification that picks the icon of a process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProcessKind {
    #[default]
    Application,
    /// A kernel thread, which has no executable.
    Kernel,
    /// A shell or terminal emulator.
    Terminal,
    /// A system or session daemon.
    Service,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawProcess {
    pub key: ProcessKey,
    pub parent: Option<u32>,
    pub name: String,
    pub command: String,
    pub user: String,
    /// Share of the whole machine, from 0 to 100.
    pub cpu: f32,
    /// Resident memory in bytes.
    pub memory: u64,
    pub disk_read_total: u64,
    pub disk_written_total: u64,
    pub state: ProcessState,
    pub kind: ProcessKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuInfo {
    pub brand: String,
    /// Total usage from 0 to 100.
    pub total: f32,
    /// Usage of each logical core from 0 to 100.
    pub cores: Vec<f32>,
    /// Average frequency in MHz, 0 when unknown.
    pub frequency_mhz: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryInfo {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

impl MemoryInfo {
    /// Used memory from 0 to 1.
    pub fn used_fraction(&self) -> f32 {
        fraction(self.used, self.total)
    }

    /// Used swap from 0 to 1.
    pub fn swap_fraction(&self) -> f32 {
        fraction(self.swap_used, self.swap_total)
    }
}

/// `part / whole`, clamped to 0..=1, and 0 when `whole` is 0.
pub fn fraction(part: u64, whole: u64) -> f32 {
    if whole == 0 { 0.0 } else { (part as f64 / whole as f64).clamp(0.0, 1.0) as f32 }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawInterface {
    pub name: String,
    pub received_total: u64,
    pub transmitted_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawDisk {
    /// Block device name, such as `nvme0n1p2`.
    pub device: String,
    pub read_total: u64,
    pub written_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Temperature {
    pub label: String,
    pub celsius: f32,
    pub critical: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSystem {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
    pub read_only: bool,
}

impl FileSystem {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn used_fraction(&self) -> f32 {
        fraction(self.used(), self.total)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostInfo {
    pub host_name: String,
    pub os: String,
    pub kernel: String,
}

/// Everything a source reports at one moment, with cumulative I/O counters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawSample {
    /// Monotonic time of the sample.
    pub at: Duration,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub load: LoadAverage,
    pub uptime: Duration,
    pub processes: Vec<RawProcess>,
    pub interfaces: Vec<RawInterface>,
    pub disks: Vec<RawDisk>,
    pub temperatures: Vec<Temperature>,
    pub file_systems: Vec<FileSystem>,
    pub host: HostInfo,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessInfo {
    pub key: ProcessKey,
    pub parent: Option<u32>,
    pub name: String,
    pub command: String,
    pub user: String,
    /// Share of the whole machine, from 0 to 100.
    pub cpu: f32,
    pub memory: u64,
    /// Bytes per second.
    pub disk_read_rate: f64,
    /// Bytes per second.
    pub disk_write_rate: f64,
    pub state: ProcessState,
    pub kind: ProcessKind,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InterfaceRates {
    pub name: String,
    /// Bytes per second.
    pub receive_rate: f64,
    /// Bytes per second.
    pub transmit_rate: f64,
    pub received_total: u64,
    pub transmitted_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiskRates {
    pub device: String,
    /// Bytes per second.
    pub read_rate: f64,
    /// Bytes per second.
    pub write_rate: f64,
}

/// The system at one moment, with I/O as rates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub load: LoadAverage,
    pub uptime: Duration,
    pub processes: Vec<ProcessInfo>,
    pub interfaces: Vec<InterfaceRates>,
    pub disks: Vec<DiskRates>,
    pub temperatures: Vec<Temperature>,
    pub file_systems: Vec<FileSystem>,
    pub host: HostInfo,
}

impl Snapshot {
    pub fn total_receive_rate(&self) -> f64 {
        self.interfaces.iter().map(|i| i.receive_rate).sum()
    }

    pub fn total_transmit_rate(&self) -> f64 {
        self.interfaces.iter().map(|i| i.transmit_rate).sum()
    }

    pub fn total_disk_read_rate(&self) -> f64 {
        self.disks.iter().map(|d| d.read_rate).sum()
    }

    pub fn total_disk_write_rate(&self) -> f64 {
        self.disks.iter().map(|d| d.write_rate).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_are_clamped() {
        assert_eq!(fraction(1, 0), 0.0);
        assert_eq!(fraction(5, 10), 0.5);
        assert_eq!(fraction(20, 10), 1.0);
        let memory = MemoryInfo { total: 8, used: 2, available: 6, swap_total: 0, swap_used: 0 };
        assert_eq!(memory.used_fraction(), 0.25);
        assert_eq!(memory.swap_fraction(), 0.0);
    }

    #[test]
    fn file_system_usage() {
        let fs = FileSystem { total: 100, available: 120, ..FileSystem::default() };
        assert_eq!(fs.used(), 0);
        let fs = FileSystem { total: 100, available: 25, ..FileSystem::default() };
        assert_eq!(fs.used(), 75);
        assert_eq!(fs.used_fraction(), 0.75);
    }

    #[test]
    fn totals_add_up() {
        let snapshot = Snapshot {
            interfaces: vec![
                InterfaceRates { receive_rate: 1.0, transmit_rate: 2.0, ..Default::default() },
                InterfaceRates { receive_rate: 3.0, transmit_rate: 4.0, ..Default::default() },
            ],
            disks: vec![DiskRates { read_rate: 5.0, write_rate: 6.0, ..Default::default() }],
            ..Snapshot::default()
        };
        assert_eq!(snapshot.total_receive_rate(), 4.0);
        assert_eq!(snapshot.total_transmit_rate(), 6.0);
        assert_eq!(snapshot.total_disk_read_rate(), 5.0);
        assert_eq!(snapshot.total_disk_write_rate(), 6.0);
    }
}
