// SPDX-License-Identifier: MIT

//! Child processes started by the compositor.

use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// Children started through `sh -c`, each in its own process group, reaped periodically.
#[derive(Default)]
pub struct Children {
    children: Vec<Child>,
}

impl Children {
    /// Runs `command` through `sh -c` with the compositor's environment; returns the child's process id.
    pub fn spawn(&mut self, command: &str) -> std::io::Result<u32> {
        self.spawn_args("/bin/sh", &["-c", command])
    }

    /// Runs `program` with `args` directly, without a shell.
    pub fn spawn_args(&mut self, program: &str, args: &[&str]) -> std::io::Result<u32> {
        // Standard output carries the readiness line, so children write to standard error instead.
        let stdout = std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .map_or_else(|_| Stdio::null(), Stdio::from);
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .process_group(0)
            .spawn()?;
        let pid = child.id();
        tracing::info!(pid, program, ?args, "started child");
        self.children.push(child);
        Ok(pid)
    }

    /// Collects exited children so they don't linger as zombies.
    pub fn reap(&mut self) {
        self.children.retain_mut(|child| match child.try_wait() {
            Ok(Some(status)) => {
                tracing::debug!(pid = child.id(), %status, "child exited");
                false
            }
            Ok(None) => true,
            Err(err) => {
                tracing::debug!(pid = child.id(), "cannot query child: {err}");
                false
            }
        });
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.children.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn spawned_children_run_and_are_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let mut children = Children::default();
        children.spawn(&format!("echo hi > '{}'", marker.display())).unwrap();
        assert_eq!(children.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(10);
        while children.len() > 0 && Instant::now() < deadline {
            children.reap();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(children.len(), 0);
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "hi\n");
    }

    #[test]
    fn missing_programs_report_errors() {
        let mut children = Children::default();
        assert!(children.spawn_args("/nonexistent/nimbus-test-program", &[]).is_err());
        assert_eq!(children.len(), 0);
    }
}
