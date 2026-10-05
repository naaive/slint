// SPDX-License-Identifier: MIT

//! Fixed-length histories of the values the Resources page graphs.

use std::collections::VecDeque;

use super::snapshot::Snapshot;

/// Number of samples each graph shows.
pub const HISTORY_LEN: usize = 60;

/// A ring buffer of the most recent values, oldest first.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    values: VecDeque<f32>,
    capacity: usize,
}

impl Series {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self { values: VecDeque::with_capacity(capacity), capacity }
    }

    pub fn push(&mut self, value: f32) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(if value.is_finite() { value } else { 0.0 });
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.values.iter().copied()
    }

    pub fn to_vec(&self) -> Vec<f32> {
        self.values.iter().copied().collect()
    }

    pub fn latest(&self) -> Option<f32> {
        self.values.back().copied()
    }

    pub fn max(&self) -> f32 {
        self.values.iter().copied().fold(0.0, f32::max)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Everything the Resources page graphs.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// Total CPU usage, 0 to 100.
    pub cpu: Series,
    /// Usage of each logical core, 0 to 100.
    pub cores: Vec<Series>,
    /// Used memory, 0 to 100.
    pub memory: Series,
    /// Used swap, 0 to 100.
    pub swap: Series,
    /// Bytes per second.
    pub receive: Series,
    pub transmit: Series,
    pub disk_read: Series,
    pub disk_write: Series,
}

impl Default for History {
    fn default() -> Self {
        let series = || Series::new(HISTORY_LEN);
        Self {
            cpu: series(),
            cores: Vec::new(),
            memory: series(),
            swap: series(),
            receive: series(),
            transmit: series(),
            disk_read: series(),
            disk_write: series(),
        }
    }
}

impl History {
    pub fn push(&mut self, snapshot: &Snapshot) {
        self.cpu.push(snapshot.cpu.total);
        if self.cores.len() != snapshot.cpu.cores.len() {
            self.cores = vec![Series::new(HISTORY_LEN); snapshot.cpu.cores.len()];
        }
        for (series, usage) in self.cores.iter_mut().zip(&snapshot.cpu.cores) {
            series.push(*usage);
        }
        self.memory.push(snapshot.memory.used_fraction() * 100.0);
        self.swap.push(snapshot.memory.swap_fraction() * 100.0);
        self.receive.push(snapshot.total_receive_rate() as f32);
        self.transmit.push(snapshot.total_transmit_rate() as f32);
        self.disk_read.push(snapshot.total_disk_read_rate() as f32);
        self.disk_write.push(snapshot.total_disk_write_rate() as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::snapshot::{CpuInfo, MemoryInfo};

    #[test]
    fn series_drops_oldest() {
        let mut series = Series::new(3);
        for v in [1.0, 2.0, 3.0, 4.0] {
            series.push(v);
        }
        assert_eq!(series.to_vec(), vec![2.0, 3.0, 4.0]);
        assert_eq!(series.latest(), Some(4.0));
        assert_eq!(series.max(), 4.0);
        series.push(f32::NAN);
        assert_eq!(series.latest(), Some(0.0));
    }

    #[test]
    fn history_follows_core_count() {
        let mut history = History::default();
        let snapshot = Snapshot {
            cpu: CpuInfo { total: 50.0, cores: vec![10.0, 90.0], ..CpuInfo::default() },
            memory: MemoryInfo { total: 4, used: 1, ..MemoryInfo::default() },
            ..Snapshot::default()
        };
        history.push(&snapshot);
        assert_eq!(history.cores.len(), 2);
        assert_eq!(history.cores[1].latest(), Some(90.0));
        assert_eq!(history.memory.latest(), Some(25.0));
        let snapshot =
            Snapshot { cpu: CpuInfo { cores: vec![1.0; 4], ..CpuInfo::default() }, ..snapshot };
        history.push(&snapshot);
        assert_eq!(history.cores.len(), 4);
        assert_eq!(history.cpu.len(), 2);
    }
}
