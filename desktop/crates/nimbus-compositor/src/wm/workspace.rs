// SPDX-License-Identifier: MIT

use nimbus_ipc::WorkspaceId;

/// Global workspaces: every output shows the active one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workspaces {
    count: u32,
    active: WorkspaceId,
}

impl Workspaces {
    pub fn new(count: u32) -> Self {
        Self { count: count.max(1), active: 0 }
    }

    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn active(&self) -> WorkspaceId {
        self.active
    }

    pub fn contains(&self, workspace: WorkspaceId) -> bool {
        workspace < self.count
    }

    /// Activates `workspace`; returns whether the active workspace changed.
    pub fn activate(&mut self, workspace: WorkspaceId) -> bool {
        let workspace = workspace.min(self.count - 1);
        std::mem::replace(&mut self.active, workspace) != workspace
    }

    pub fn next(&self) -> WorkspaceId {
        (self.active + 1) % self.count
    }

    pub fn previous(&self) -> WorkspaceId {
        (self.active + self.count - 1) % self.count
    }

    /// Changes the number of workspaces, keeping the active one in range.
    pub fn set_count(&mut self, count: u32) {
        self.count = count.max(1);
        self.active = self.active.min(self.count - 1);
    }

    /// Maps a window's workspace into range after the count shrank.
    pub fn clamp(&self, workspace: WorkspaceId) -> WorkspaceId {
        workspace.min(self.count - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_and_previous_wrap() {
        let mut ws = Workspaces::new(4);
        assert_eq!(ws.previous(), 3);
        assert_eq!(ws.next(), 1);
        assert!(ws.activate(3));
        assert_eq!(ws.next(), 0);
        assert!(!ws.activate(3));
    }

    #[test]
    fn shrinking_clamps_active_and_windows() {
        let mut ws = Workspaces::new(6);
        ws.activate(5);
        ws.set_count(2);
        assert_eq!(ws.active(), 1);
        assert_eq!(ws.clamp(4), 1);
        assert!(!ws.contains(2));
    }

    #[test]
    fn zero_count_means_one_workspace() {
        let mut ws = Workspaces::new(0);
        assert_eq!(ws.count(), 1);
        ws.activate(7);
        assert_eq!(ws.active(), 0);
        assert_eq!(ws.next(), 0);
    }
}
