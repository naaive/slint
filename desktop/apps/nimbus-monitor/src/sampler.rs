// SPDX-License-Identifier: MIT

//! Samples the system on a worker thread and sends signals to processes.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sysinfo::{
    Components, CpuRefreshKind, DiskRefreshKind, Disks, MemoryRefreshKind, Networks, Pid,
    ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, ThreadKind, UpdateKind, Users,
};

use crate::core::rates::RateTracker;
use crate::core::snapshot::{
    CpuInfo, FileSystem, HostInfo, LoadAverage, MemoryInfo, ProcessKey, ProcessKind, ProcessState,
    RawDisk, RawInterface, RawProcess, RawSample, Snapshot, Temperature,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcessSignal {
    /// Ask the process to exit (SIGTERM).
    End,
    /// Stop the process immediately (SIGKILL).
    Kill,
    /// Pause the process (SIGSTOP).
    Stop,
    /// Resume a paused process (SIGCONT).
    Continue,
}

impl ProcessSignal {
    fn to_rustix(self) -> rustix::process::Signal {
        use rustix::process::Signal;
        match self {
            Self::End => Signal::TERM,
            Self::Kill => Signal::KILL,
            Self::Stop => Signal::STOP,
            Self::Continue => Signal::CONT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignalError {
    #[error("the process has already exited")]
    Gone,
    #[error("you don't have permission to control this process")]
    PermissionDenied,
    #[error("cannot signal the process: {0}")]
    Other(String),
}

/// Where samples come from.
pub trait Source: Send {
    fn sample(&mut self) -> RawSample;
    /// Sends `signal` to the process `key` names, if it still runs.
    fn signal(&mut self, key: ProcessKey, signal: ProcessSignal) -> Result<(), SignalError>;
}

#[derive(Debug)]
pub enum SamplerEvent {
    Snapshot(Box<Snapshot>),
    Signalled { key: ProcessKey, signal: ProcessSignal, result: Result<(), SignalError> },
}

enum Command {
    SetInterval(Duration),
    Refresh,
    Signal(ProcessKey, ProcessSignal),
}

/// The sampling thread; dropping it stops the thread.
pub struct Sampler {
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Starts sampling every `interval`, sending each snapshot to `sink`; the first one comes right away.
    ///
    /// `make_source` runs on the sampling thread, since reading the whole system the first time can take a while.
    pub fn spawn(
        make_source: impl FnOnce() -> Box<dyn Source> + Send + 'static,
        interval: Duration,
        sink: impl Fn(SamplerEvent) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let thread =
            std::thread::Builder::new().name("nimbus-monitor-sampler".into()).spawn(move || {
                let mut source = make_source();
                let mut rates = RateTracker::new();
                let mut interval = interval;
                let mut next = Instant::now();
                loop {
                    let now = Instant::now();
                    if now >= next {
                        sink(SamplerEvent::Snapshot(Box::new(rates.update(source.sample()))));
                        next = Instant::now() + interval;
                        continue;
                    }
                    match receiver.recv_timeout(next - now) {
                        Ok(Command::SetInterval(new)) => {
                            next = next.checked_sub(interval).unwrap_or(next) + new;
                            interval = new;
                        }
                        Ok(Command::Refresh) => next = Instant::now(),
                        Ok(Command::Signal(key, signal)) => {
                            let result = source.signal(key, signal);
                            sink(SamplerEvent::Signalled { key, signal, result });
                            next = Instant::now();
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self { commands: Some(commands), thread: Some(thread) })
    }

    fn send(&self, command: Command) {
        if let Some(commands) = &self.commands
            && commands.send(command).is_err()
        {
            tracing::warn!("the sampler thread has stopped");
        }
    }

    pub fn set_interval(&self, interval: Duration) {
        self.send(Command::SetInterval(interval));
    }

    /// Takes a sample now instead of at the next tick.
    pub fn refresh(&self) {
        self.send(Command::Refresh);
    }

    pub fn signal(&self, key: ProcessKey, signal: ProcessSignal) {
        self.send(Command::Signal(key, signal));
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.commands = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the sampler thread panicked");
        }
    }
}

const TERMINALS: &[&str] = &[
    "bash",
    "zsh",
    "fish",
    "sh",
    "dash",
    "ksh",
    "tcsh",
    "nu",
    "nimbus-terminal",
    "foot",
    "alacritty",
    "kitty",
    "wezterm",
    "gnome-terminal-",
    "konsole",
    "xterm",
    "tmux",
    "screen",
];

/// The kernel cuts process names to 15 bytes; this restores the full name from the program path when it was cut.
fn display_name(comm: &str, argv0: Option<&str>) -> String {
    const COMM_LEN: usize = 15;
    if comm.len() >= COMM_LEN
        && let Some(program) =
            argv0.and_then(|a| a.split_whitespace().next()).and_then(|a| a.rsplit('/').next())
        && program.starts_with(comm)
    {
        return program.to_owned();
    }
    comm.to_owned()
}

fn classify(name: &str, thread: Option<ThreadKind>, parent: Option<u32>) -> ProcessKind {
    if thread == Some(ThreadKind::Kernel) {
        ProcessKind::Kernel
    } else if TERMINALS.contains(&name) {
        ProcessKind::Terminal
    } else if name == "systemd" || (parent == Some(1) && name.len() > 3 && name.ends_with('d')) {
        ProcessKind::Service
    } else {
        ProcessKind::Application
    }
}

fn state(status: ProcessStatus) -> ProcessState {
    match status {
        ProcessStatus::Run => ProcessState::Running,
        ProcessStatus::Sleep => ProcessState::Sleeping,
        ProcessStatus::UninterruptibleDiskSleep => ProcessState::DiskSleep,
        ProcessStatus::Stop | ProcessStatus::Tracing => ProcessState::Stopped,
        ProcessStatus::Zombie | ProcessStatus::Dead => ProcessState::Zombie,
        ProcessStatus::Idle | ProcessStatus::Parked => ProcessState::Idle,
        _ => ProcessState::Other,
    }
}

/// Pseudo file systems that the File Systems page leaves out.
fn is_pseudo_file_system(fs_type: &str, total: u64) -> bool {
    total == 0
        || matches!(
            fs_type,
            "tmpfs"
                | "devtmpfs"
                | "proc"
                | "sysfs"
                | "cgroup"
                | "cgroup2"
                | "overlay"
                | "squashfs"
                | "efivarfs"
                | "ramfs"
        )
}

/// The running system, read through `sysinfo`.
pub struct SystemSource {
    system: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    users: Users,
    start: Instant,
    host: HostInfo,
}

impl SystemSource {
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_list(CpuRefreshKind::nothing().with_cpu_usage().with_frequency());
        let host = HostInfo {
            host_name: System::host_name().unwrap_or_default(),
            os: System::long_os_version().unwrap_or_else(|| "Linux".into()),
            kernel: System::kernel_version().unwrap_or_default(),
        };
        Self {
            system,
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            start: Instant::now(),
            host,
        }
    }

    fn user_name(&mut self, uid: Option<&sysinfo::Uid>) -> String {
        let Some(uid) = uid else { return String::new() };
        if self.users.get_user_by_id(uid).is_none() {
            self.users.refresh();
        }
        self.users
            .get_user_by_id(uid)
            .map(|u| u.name().to_owned())
            .unwrap_or_else(|| uid.to_string())
    }
}

impl Default for SystemSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for SystemSource {
    fn sample(&mut self) -> RawSample {
        self.system
            .refresh_cpu_specifics(CpuRefreshKind::nothing().with_cpu_usage().with_frequency());
        self.system.refresh_memory_specifics(MemoryRefreshKind::everything());
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        self.networks.refresh(true);
        self.disks.refresh_specifics(true, DiskRefreshKind::everything());
        self.components.refresh(true);

        let cores: Vec<f32> = self.system.cpus().iter().map(|c| c.cpu_usage()).collect();
        let frequency = match self.system.cpus().len() {
            0 => 0,
            n => self.system.cpus().iter().map(|c| c.frequency()).sum::<u64>() / n as u64,
        };
        let cpu = CpuInfo {
            brand: self
                .system
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_owned())
                .unwrap_or_default(),
            total: self.system.global_cpu_usage(),
            cores,
            frequency_mhz: frequency,
        };
        let core_count = cpu.cores.len().max(1) as f32;
        let memory = MemoryInfo {
            total: self.system.total_memory(),
            used: self.system.used_memory(),
            available: self.system.available_memory(),
            swap_total: self.system.total_swap(),
            swap_used: self.system.used_swap(),
        };

        let mut uids = Vec::new();
        let mut processes: Vec<RawProcess> = self
            .system
            .processes()
            .values()
            .filter(|p| p.thread_kind() != Some(ThreadKind::Userland))
            .map(|p| {
                let name = display_name(
                    &p.name().to_string_lossy(),
                    p.cmd().first().map(|a| a.to_string_lossy()).as_deref(),
                );
                let parent = p.parent().map(Pid::as_u32);
                uids.push(p.user_id().cloned());
                let usage = p.disk_usage();
                RawProcess {
                    key: ProcessKey { pid: p.pid().as_u32(), start_time: p.start_time() },
                    parent,
                    command: p
                        .cmd()
                        .iter()
                        .map(|arg| arg.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                    user: String::new(),
                    cpu: p.cpu_usage() / core_count,
                    memory: p.memory(),
                    disk_read_total: usage.total_read_bytes,
                    disk_written_total: usage.total_written_bytes,
                    state: state(p.status()),
                    kind: classify(&name, p.thread_kind(), parent),
                    name,
                }
            })
            .collect();
        for (process, uid) in processes.iter_mut().zip(uids) {
            process.user = self.user_name(uid.as_ref());
        }

        let load = System::load_average();
        RawSample {
            at: self.start.elapsed(),
            cpu,
            memory,
            load: LoadAverage { one: load.one, five: load.five, fifteen: load.fifteen },
            uptime: Duration::from_secs(System::uptime()),
            processes,
            interfaces: self
                .networks
                .list()
                .iter()
                .filter(|(name, _)| name.as_str() != "lo")
                .map(|(name, data)| RawInterface {
                    name: name.clone(),
                    received_total: data.total_received(),
                    transmitted_total: data.total_transmitted(),
                })
                .collect(),
            disks: self
                .disks
                .list()
                .iter()
                .map(|d| {
                    let usage = d.usage();
                    RawDisk {
                        device: d.name().to_string_lossy().into_owned(),
                        read_total: usage.total_read_bytes,
                        written_total: usage.total_written_bytes,
                    }
                })
                .collect(),
            temperatures: self
                .components
                .list()
                .iter()
                .filter_map(|c| {
                    Some(Temperature {
                        label: c.label().to_owned(),
                        celsius: c.temperature().filter(|t| t.is_finite())?,
                        critical: c.critical().filter(|t| t.is_finite()),
                    })
                })
                .collect(),
            file_systems: self
                .disks
                .list()
                .iter()
                .map(|d| FileSystem {
                    device: d.name().to_string_lossy().into_owned(),
                    mount_point: d.mount_point().to_string_lossy().into_owned(),
                    fs_type: d.file_system().to_string_lossy().into_owned(),
                    total: d.total_space(),
                    available: d.available_space(),
                    removable: d.is_removable(),
                    read_only: d.is_read_only(),
                })
                .filter(|fs| !is_pseudo_file_system(&fs.fs_type, fs.total))
                .collect(),
            host: self.host.clone(),
        }
    }

    fn signal(&mut self, key: ProcessKey, signal: ProcessSignal) -> Result<(), SignalError> {
        let raw = i32::try_from(key.pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or(SignalError::Gone)?;
        let errno = |err: rustix::io::Errno| match err {
            rustix::io::Errno::SRCH => SignalError::Gone,
            rustix::io::Errno::PERM => SignalError::PermissionDenied,
            other => SignalError::Other(other.to_string()),
        };
        // A pidfd pins the process, so the PID can't be reused between the check below and the signal.
        // Kernels before 5.3 lack `pidfd_open`; they fall back to `kill`.
        let pidfd = match rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty()) {
            Ok(fd) => Some(fd),
            Err(rustix::io::Errno::SRCH) => return Err(SignalError::Gone),
            Err(_) => None,
        };
        let pid = Pid::from_u32(key.pid);
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        // A different start time means the PID now belongs to another process.
        if self.system.process(pid).is_none_or(|p| p.start_time() != key.start_time) {
            return Err(SignalError::Gone);
        }
        match pidfd {
            Some(fd) => rustix::process::pidfd_send_signal(fd, signal.to_rustix()),
            None => rustix::process::kill_process(raw, signal.to_rustix()),
        }
        .map_err(errno)
    }
}

/// Deterministic made-up data, for screenshots and tests.
#[derive(Debug, Default)]
pub struct SampleSource {
    tick: u64,
    /// Signals received, for tests.
    pub signals: Vec<(ProcessKey, ProcessSignal)>,
    stopped: Vec<u32>,
    ended: Vec<u32>,
}

impl SampleSource {
    pub fn new() -> Self {
        Self::default()
    }

    fn wave(&self, phase: f64, low: f64, high: f64) -> f64 {
        wave_at(self.tick, phase, low, high)
    }
}

/// A smooth, repeatable curve between `low` and `high`.
fn wave_at(tick: u64, phase: f64, low: f64, high: f64) -> f64 {
    let t = tick as f64 * 0.35 + phase;
    low + (high - low) * (0.5 + 0.5 * (t.sin() * 0.7 + (t * 2.3).sin() * 0.3))
}

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

impl Source for SampleSource {
    fn sample(&mut self) -> RawSample {
        self.tick += 1;
        let t = self.tick;
        let cores: Vec<f32> =
            (0..8).map(|i| self.wave(i as f64 * 0.9, 4.0, 70.0 - i as f64 * 5.0) as f32).collect();
        let total = cores.iter().sum::<f32>() / cores.len() as f32;
        let apps: [(&str, &str, &str, f64, u64, ProcessKind); 14] = [
            ("systemd", "/usr/lib/systemd/systemd --user", "ada", 0.1, 12, ProcessKind::Service),
            (
                "nimbus-compositor",
                "nimbus-compositor --backend udev",
                "ada",
                6.0,
                310,
                ProcessKind::Application,
            ),
            ("nimbus-terminal", "nimbus-terminal", "ada", 1.2, 96, ProcessKind::Terminal),
            ("bash", "-bash", "ada", 0.0, 6, ProcessKind::Terminal),
            ("cargo", "cargo build --release", "ada", 22.0, 640, ProcessKind::Application),
            ("rustc", "rustc --crate-name slint", "ada", 61.0, 1900, ProcessKind::Application),
            ("firefox", "/usr/lib/firefox/firefox", "ada", 8.5, 1450, ProcessKind::Application),
            (
                "Web Content",
                "/usr/lib/firefox/firefox -contentproc",
                "ada",
                3.1,
                520,
                ProcessKind::Application,
            ),
            ("nimbus-files", "nimbus-files", "ada", 0.4, 140, ProcessKind::Application),
            ("pipewire", "/usr/bin/pipewire", "ada", 0.8, 28, ProcessKind::Service),
            ("dbus-broker", "dbus-broker --scope user", "ada", 0.1, 9, ProcessKind::Service),
            (
                "NetworkManager",
                "/usr/sbin/NetworkManager --no-daemon",
                "root",
                0.2,
                22,
                ProcessKind::Service,
            ),
            ("sshd", "sshd: /usr/sbin/sshd -D", "root", 0.0, 8, ProcessKind::Service),
            ("kworker/3:1", "", "root", 0.3, 0, ProcessKind::Kernel),
        ];
        let parents = [
            None,
            Some(1),
            Some(1),
            Some(3),
            Some(4),
            Some(5),
            Some(1),
            Some(7),
            Some(1),
            Some(1),
            Some(1),
            None,
            None,
            Some(2),
        ];
        let processes = apps
            .iter()
            .zip(parents)
            .enumerate()
            .filter(|(i, _)| !self.ended.contains(&(*i as u32 + 1)))
            .map(|(i, ((name, command, user, cpu, mib, kind), parent))| {
                let pid = i as u32 + 1;
                let busy = if self.stopped.contains(&pid) {
                    0.0
                } else {
                    self.wave(i as f64, 0.6, 1.4) * cpu
                };
                RawProcess {
                    key: ProcessKey {
                        pid: [
                            1, 812, 1204, 1210, 2301, 2350, 1530, 1588, 1402, 920, 640, 711, 733,
                            97,
                        ][i],
                        start_time: 1000 + i as u64,
                    },
                    parent: parent.map(|p: u32| {
                        [1, 812, 1204, 1210, 2301, 2350, 1530, 1588, 1402, 920, 640, 711, 733, 97]
                            [p as usize - 1]
                    }),
                    name: (*name).into(),
                    command: (*command).into(),
                    user: (*user).into(),
                    cpu: busy as f32,
                    memory: mib * MIB,
                    disk_read_total: t * (i as u64 % 4) * 180_000,
                    disk_written_total: t * (i as u64 % 3) * 90_000,
                    state: if self.stopped.contains(&pid) {
                        ProcessState::Stopped
                    } else if busy > 5.0 {
                        ProcessState::Running
                    } else {
                        ProcessState::Sleeping
                    },
                    kind: *kind,
                }
            })
            .collect();
        RawSample {
            at: Duration::from_secs(t),
            cpu: CpuInfo {
                brand: "AMD Ryzen 7 7840U w/ Radeon 780M Graphics".into(),
                total,
                cores,
                frequency_mhz: 3800,
            },
            memory: MemoryInfo {
                total: 32 * GIB,
                used: (self.wave(0.2, 9.0, 11.0) * GIB as f64) as u64,
                available: 20 * GIB,
                swap_total: 8 * GIB,
                swap_used: 300 * MIB,
            },
            load: LoadAverage { one: 2.31, five: 1.87, fifteen: 1.52 },
            uptime: Duration::from_secs(3 * 86400 + 4 * 3600 + t),
            processes,
            interfaces: vec![RawInterface {
                name: "wlan0".into(),
                received_total: (0..t)
                    .map(|i| (wave_at(i, 1.0, 0.2, 3.5) * MIB as f64) as u64)
                    .sum(),
                transmitted_total: t * 120_000,
            }],
            disks: vec![RawDisk {
                device: "nvme0n1p2".into(),
                read_total: (0..t).map(|i| (wave_at(i, 2.5, 0.0, 12.0) * MIB as f64) as u64).sum(),
                written_total: (0..t)
                    .map(|i| (wave_at(i, 4.0, 0.5, 4.0) * MIB as f64) as u64)
                    .sum(),
            }],
            temperatures: vec![
                Temperature { label: "CPU".into(), celsius: 54.0, critical: Some(100.0) },
                Temperature { label: "NVMe".into(), celsius: 41.0, critical: Some(84.0) },
            ],
            file_systems: vec![
                FileSystem {
                    device: "/dev/nvme0n1p2".into(),
                    mount_point: "/".into(),
                    fs_type: "btrfs".into(),
                    total: 930 * GIB,
                    available: 412 * GIB,
                    removable: false,
                    read_only: false,
                },
                FileSystem {
                    device: "/dev/nvme0n1p1".into(),
                    mount_point: "/boot/efi".into(),
                    fs_type: "vfat".into(),
                    total: GIB,
                    available: 870 * MIB,
                    removable: false,
                    read_only: false,
                },
                FileSystem {
                    device: "/dev/sda1".into(),
                    mount_point: "/run/media/ada/USB".into(),
                    fs_type: "exfat".into(),
                    total: 58 * GIB,
                    available: 6 * GIB,
                    removable: true,
                    read_only: false,
                },
            ],
            host: HostInfo {
                host_name: "nimbus".into(),
                os: "Linux (Fedora 44)".into(),
                kernel: "6.18.4".into(),
            },
        }
    }

    fn signal(&mut self, key: ProcessKey, signal: ProcessSignal) -> Result<(), SignalError> {
        self.signals.push((key, signal));
        let index = (key.start_time.checked_sub(1000).ok_or(SignalError::Gone)? as u32) + 1;
        if self.ended.contains(&index) {
            return Err(SignalError::Gone);
        }
        if key.pid == 1 {
            return Err(SignalError::PermissionDenied);
        }
        match signal {
            ProcessSignal::End | ProcessSignal::Kill => self.ended.push(index),
            ProcessSignal::Stop => self.stopped.push(index),
            ProcessSignal::Continue => self.stopped.retain(|p| *p != index),
        }
        Ok(())
    }
}

/// The name of the user running the monitor.
pub fn current_user() -> Option<String> {
    let uid = rustix::process::getuid().as_raw();
    let users = Users::new_with_refreshed_list();
    users.list().iter().find(|u| **u.id() == uid).map(|u| u.name().to_owned())
}

/// Seconds since the Unix epoch, for comparing process start times.
pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn classification() {
        assert_eq!(classify("kworker/0:1", Some(ThreadKind::Kernel), Some(2)), ProcessKind::Kernel);
        assert_eq!(classify("zsh", None, Some(100)), ProcessKind::Terminal);
        assert_eq!(classify("systemd", None, None), ProcessKind::Service);
        assert_eq!(classify("sshd", None, Some(1)), ProcessKind::Service);
        assert_eq!(classify("sshd", None, Some(500)), ProcessKind::Application);
        assert_eq!(classify("firefox", None, Some(1)), ProcessKind::Application);
    }

    #[test]
    fn current_user_is_known() {
        let mut source = SystemSource::new();
        let me = std::process::id();
        let sample = source.sample();
        let process = sample.processes.iter().find(|p| p.key.pid == me).unwrap();
        if let Some(user) = current_user() {
            assert_eq!(process.user, user);
        }
    }

    #[test]
    fn truncated_names_are_restored() {
        assert_eq!(
            display_name("nimbus-composit", Some("/usr/bin/nimbus-compositor")),
            "nimbus-compositor"
        );
        assert_eq!(display_name("nimbus-composit", Some("python3")), "nimbus-composit");
        assert_eq!(display_name("bash", Some("/bin/bash-other")), "bash");
        assert_eq!(display_name("dbus-run-sessio", None), "dbus-run-sessio");
    }

    #[test]
    fn pseudo_file_systems_are_skipped() {
        assert!(is_pseudo_file_system("tmpfs", 100));
        assert!(is_pseudo_file_system("ext4", 0));
        assert!(!is_pseudo_file_system("ext4", 100));
    }

    #[test]
    fn real_system_sample_is_sane() {
        let mut source = SystemSource::new();
        let sample = source.sample();
        assert!(sample.memory.total > 0);
        assert!(!sample.cpu.cores.is_empty());
        let me = std::process::id();
        let process =
            sample.processes.iter().find(|p| p.key.pid == me).expect("own process listed");
        assert!(process.memory > 0);
        assert!(process.key.start_time <= now_unix());
    }

    #[test]
    fn signals_check_identity() {
        let mut source = SystemSource::new();
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let sample = source.sample();
        let key = sample.processes.iter().find(|p| p.key.pid == pid).unwrap().key;
        let stale = ProcessKey { start_time: key.start_time + 1000, ..key };
        for signal in
            [ProcessSignal::Stop, ProcessSignal::Continue, ProcessSignal::End, ProcessSignal::Kill]
        {
            assert_eq!(source.signal(stale, signal), Err(SignalError::Gone), "{signal:?}");
        }
        assert_eq!(source.signal(key, ProcessSignal::Stop), Ok(()));
        assert_eq!(source.signal(key, ProcessSignal::Continue), Ok(()));
        assert_eq!(source.signal(key, ProcessSignal::End), Ok(()));
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert_eq!(source.signal(key, ProcessSignal::End), Err(SignalError::Gone));
    }

    #[test]
    fn sampler_delivers_snapshots_and_signal_results() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let sampler = Sampler::spawn(
            || Box::new(SampleSource::new()),
            Duration::from_secs(3600),
            move |event| {
                sink.lock().unwrap().push(event);
            },
        )
        .unwrap();
        let key = ProcessKey { pid: 2301, start_time: 1004 };
        sampler.signal(key, ProcessSignal::End);
        sampler.signal(ProcessKey { pid: 1, start_time: 1000 }, ProcessSignal::Kill);
        let deadline = Instant::now() + Duration::from_secs(5);
        while events.lock().unwrap().len() < 5 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(sampler);
        let events = events.lock().unwrap();
        assert!(matches!(&events[0], SamplerEvent::Snapshot(s) if s.processes.len() == 14));
        assert!(events.iter().any(|e| matches!(e, SamplerEvent::Signalled { result: Ok(()), .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            SamplerEvent::Signalled { result: Err(SignalError::PermissionDenied), .. }
        )));
        let last = events.iter().rev().find_map(|e| match e {
            SamplerEvent::Snapshot(s) => Some(s),
            _ => None,
        });
        assert!(last.is_some_and(|s| s.processes.iter().all(|p| p.key.pid != 2301)));
    }
}
