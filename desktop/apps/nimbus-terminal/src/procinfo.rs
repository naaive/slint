// SPDX-License-Identifier: MIT

//! What runs inside a terminal: the foreground process and the working directory, read from `/proc`.
//! These calls touch the file system, so run them off the UI thread.

use std::os::fd::AsFd;
use std::path::PathBuf;

/// A process in the foreground of a terminal other than its shell, such as an editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: i32,
    pub name: String,
}

/// The process the user is interacting with in the terminal on `pty`, unless it's the shell `shell_pid` itself.
pub fn foreground_process(pty: impl AsFd, shell_pid: u32) -> Option<ForegroundProcess> {
    let group = rustix::termios::tcgetpgrp(pty).ok()?.as_raw_nonzero().get();
    if u32::try_from(group).ok() == Some(shell_pid) {
        return None;
    }
    let name = process_name(group).unwrap_or_else(|| format!("process {group}"));
    Some(ForegroundProcess { pid: group, name })
}

/// The command name of `pid`, such as `vim`.
pub fn process_name(pid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim_end_matches('\n');
    (!name.is_empty()).then(|| name.to_string())
}

/// The current directory of `pid`.
pub fn working_directory(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().filter(|path| path.is_dir())
}

/// The directory a new tab opened next to a terminal starts in: that of its foreground process, or its shell.
pub fn terminal_directory(pty: impl AsFd, shell_pid: u32) -> Option<PathBuf> {
    let shell = i32::try_from(shell_pid).ok()?;
    foreground_process(pty, shell_pid)
        .and_then(|process| working_directory(process.pid))
        .or_else(|| working_directory(shell))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_pid() -> i32 {
        i32::try_from(std::process::id()).unwrap_or(i32::MAX)
    }

    #[test]
    fn reads_own_process() {
        let name = process_name(own_pid()).expect("our own name is readable");
        assert!(!name.is_empty());
        let cwd = std::env::current_dir().expect("a current directory");
        assert_eq!(working_directory(own_pid()), Some(cwd));
    }

    #[test]
    fn missing_processes_are_none() {
        assert_eq!(process_name(i32::MAX), None);
        assert_eq!(working_directory(i32::MAX), None);
    }

    #[test]
    fn files_that_are_not_terminals_have_no_foreground() {
        let file = std::fs::File::open("/proc/self/comm").expect("procfs is readable");
        assert_eq!(foreground_process(&file, std::process::id()), None);
        assert_eq!(terminal_directory(&file, std::process::id()), std::env::current_dir().ok());
    }
}
