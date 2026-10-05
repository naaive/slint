// SPDX-License-Identifier: MIT

//! Filtering, sorting, and tree arrangement of the process list.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::snapshot::ProcessInfo;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortColumn {
    Name,
    User,
    #[default]
    Cpu,
    Memory,
    Disk,
    Pid,
}

impl SortColumn {
    pub const ALL: [SortColumn; 6] =
        [Self::Name, Self::User, Self::Cpu, Self::Memory, Self::Disk, Self::Pid];

    /// Text columns read naturally ascending, numeric ones descending.
    pub fn default_descending(self) -> bool {
        !matches!(self, Self::Name | Self::User | Self::Pid)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortOrder {
    pub column: SortColumn,
    pub descending: bool,
}

impl Default for SortOrder {
    fn default() -> Self {
        Self { column: SortColumn::Cpu, descending: true }
    }
}

impl SortOrder {
    /// The order after clicking `column`'s header: the same column flips, another starts in its natural direction.
    pub fn toggled(self, column: SortColumn) -> Self {
        if self.column == column {
            Self { column, descending: !self.descending }
        } else {
            Self { column, descending: column.default_descending() }
        }
    }

    pub fn compare(self, a: &ProcessInfo, b: &ProcessInfo) -> Ordering {
        let primary = match self.column {
            SortColumn::Name => natural_lowercase(&a.name).cmp(&natural_lowercase(&b.name)),
            SortColumn::User => a.user.cmp(&b.user),
            SortColumn::Cpu => a.cpu.total_cmp(&b.cpu),
            SortColumn::Memory => a.memory.cmp(&b.memory),
            SortColumn::Disk => (a.disk_read_rate + a.disk_write_rate)
                .total_cmp(&(b.disk_read_rate + b.disk_write_rate)),
            SortColumn::Pid => a.key.pid.cmp(&b.key.pid),
        };
        let primary = if self.descending { primary.reverse() } else { primary };
        primary.then_with(|| a.key.pid.cmp(&b.key.pid))
    }
}

fn natural_lowercase(text: &str) -> String {
    text.to_lowercase()
}

/// Which processes to list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Case-insensitive text matched against the name, command, user, and PID.
    pub query: String,
    /// Only processes of this user, when set.
    pub user: Option<String>,
    pub show_kernel_threads: bool,
}

impl Filter {
    pub fn matches(&self, process: &ProcessInfo) -> bool {
        if !self.show_kernel_threads && process.kind == super::snapshot::ProcessKind::Kernel {
            return false;
        }
        if self.user.as_ref().is_some_and(|user| *user != process.user) {
            return false;
        }
        let query = self.query.trim();
        if query.is_empty() {
            return true;
        }
        let query = query.to_lowercase();
        process.name.to_lowercase().contains(&query)
            || process.command.to_lowercase().contains(&query)
            || process.user.to_lowercase().contains(&query)
            || process.key.pid.to_string() == query
    }
}

/// A process in display order, with its depth in the tree view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// Index into the slice passed to [`arrange`].
    pub index: usize,
    pub depth: usize,
}

/// Lists the processes that pass `filter`, sorted; in a tree, children follow their parents and siblings are sorted.
///
/// A process whose parent isn't listed becomes a root.
pub fn arrange(
    processes: &[ProcessInfo],
    filter: &Filter,
    order: SortOrder,
    tree: bool,
) -> Vec<Row> {
    let mut visible: Vec<usize> =
        (0..processes.len()).filter(|&i| filter.matches(&processes[i])).collect();
    visible.sort_by(|&a, &b| order.compare(&processes[a], &processes[b]));
    if !tree {
        return visible.into_iter().map(|index| Row { index, depth: 0 }).collect();
    }

    let listed: HashSet<u32> = visible.iter().map(|&i| processes[i].key.pid).collect();
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for &index in &visible {
        match processes[index]
            .parent
            .filter(|p| listed.contains(p) && *p != processes[index].key.pid)
        {
            Some(parent) => children.entry(parent).or_default().push(index),
            None => roots.push(index),
        }
    }

    let mut rows = Vec::with_capacity(visible.len());
    let mut seen = HashSet::new();
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((index, depth)) = stack.pop() {
        if !seen.insert(index) {
            continue;
        }
        rows.push(Row { index, depth });
        if let Some(kids) = children.get(&processes[index].key.pid) {
            stack.extend(kids.iter().rev().map(|&kid| (kid, depth + 1)));
        }
    }
    // Parent links that form a cycle leave processes unreachable from any root; list them flat at the end.
    for index in visible {
        if !seen.contains(&index) {
            rows.push(Row { index, depth: 0 });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::snapshot::{ProcessKey, ProcessKind};

    fn process(pid: u32, parent: Option<u32>, name: &str, cpu: f32) -> ProcessInfo {
        ProcessInfo {
            key: ProcessKey { pid, start_time: 0 },
            parent,
            name: name.into(),
            command: format!("/usr/bin/{name}"),
            user: if pid == 3 { "root".into() } else { "ada".into() },
            cpu,
            memory: u64::from(pid) * 10,
            ..ProcessInfo::default()
        }
    }

    fn sample() -> Vec<ProcessInfo> {
        vec![
            process(1, None, "systemd", 0.1),
            process(2, Some(1), "bash", 2.0),
            process(3, Some(1), "Xorg", 9.0),
            process(4, Some(2), "cargo", 50.0),
            process(5, Some(2), "vim", 1.0),
        ]
    }

    fn names(processes: &[ProcessInfo], rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| format!("{}{}", " ".repeat(r.depth), processes[r.index].name)).collect()
    }

    #[test]
    fn flat_sort_by_cpu_descending() {
        let p = sample();
        let rows = arrange(&p, &Filter::default(), SortOrder::default(), false);
        assert_eq!(names(&p, &rows), ["cargo", "Xorg", "bash", "vim", "systemd"]);
    }

    #[test]
    fn name_sort_ignores_case() {
        let p = sample();
        let order = SortOrder { column: SortColumn::Name, descending: false };
        let rows = arrange(&p, &Filter::default(), order, false);
        assert_eq!(names(&p, &rows), ["bash", "cargo", "systemd", "vim", "Xorg"]);
    }

    #[test]
    fn tree_nests_and_sorts_siblings() {
        let p = sample();
        let rows = arrange(&p, &Filter::default(), SortOrder::default(), true);
        assert_eq!(names(&p, &rows), ["systemd", " Xorg", " bash", "  cargo", "  vim"]);
    }

    #[test]
    fn filtered_tree_promotes_orphans() {
        let p = sample();
        let filter = Filter { user: Some("ada".into()), ..Filter::default() };
        let rows = arrange(&p, &filter, SortOrder::default(), true);
        assert_eq!(names(&p, &rows), ["systemd", " bash", "  cargo", "  vim"]);
        let filter = Filter { query: "CARGO".into(), ..Filter::default() };
        assert_eq!(names(&p, &arrange(&p, &filter, SortOrder::default(), true)), ["cargo"]);
        let filter = Filter { query: "5".into(), ..Filter::default() };
        assert_eq!(names(&p, &arrange(&p, &filter, SortOrder::default(), false)), ["vim"]);
    }

    #[test]
    fn cycles_do_not_hang() {
        let p = vec![process(1, Some(2), "a", 0.0), process(2, Some(1), "b", 0.0)];
        let rows = arrange(&p, &Filter::default(), SortOrder::default(), true);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn kernel_threads_are_hidden_by_default() {
        let mut p = sample();
        p[0].kind = ProcessKind::Kernel;
        assert_eq!(arrange(&p, &Filter::default(), SortOrder::default(), false).len(), 4);
        let filter = Filter { show_kernel_threads: true, ..Filter::default() };
        assert_eq!(arrange(&p, &filter, SortOrder::default(), false).len(), 5);
    }

    #[test]
    fn toggling_sort() {
        let order = SortOrder::default().toggled(SortColumn::Cpu);
        assert!(!order.descending);
        let order = order.toggled(SortColumn::Name);
        assert_eq!(order, SortOrder { column: SortColumn::Name, descending: false });
        assert_eq!(SortColumn::from_index(SortColumn::Memory.index()), Some(SortColumn::Memory));
    }
}
