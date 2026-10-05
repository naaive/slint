// SPDX-License-Identifier: MIT

//! Work that runs off the UI thread: font discovery, process inspection, saving preferences, starting windows.
//! Results come back to the UI thread as [`AppEvent`]s.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use futures_channel::mpsc::UnboundedSender;

use super::AppEvent;
use crate::engine::SessionId;
use crate::fonts::FontSet;
use crate::prefs::Prefs;
use crate::procinfo;

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(error) = std::thread::Builder::new().name(name.into()).spawn(work) {
        tracing::error!("cannot start the {name} thread: {error}");
    }
}

/// Sends an event to the UI thread; the receiver only goes away while the app quits.
pub(super) fn send(events: &UnboundedSender<AppEvent>, event: AppEvent) {
    let _ = events.unbounded_send(event);
}

/// Finds the fonts for `family`.
pub(super) fn load_fonts(events: UnboundedSender<AppEvent>, family: String) {
    spawn("fonts", move || {
        let result = FontSet::discover(&family).map(Arc::new).map_err(|e| e.to_string());
        send(&events, AppEvent::FontsLoaded(result));
    });
}

/// Resolves the user's theme, which may ask the desktop portal and wait for it.
pub(super) fn resolve_theme(
    events: UnboundedSender<AppEvent>,
    appearance: nimbus_config::Appearance,
) {
    spawn("theme", move || {
        let settings = nimbus_theme::ThemeSettings::from_config(&appearance);
        send(&events, AppEvent::Theme(settings));
    });
}

/// A process handle of a session: its child's pid and its PTY.
pub(super) type ProcessHandle = (u32, OwnedFd);

/// Finds whether a program other than the shell runs in a session, before closing it.
pub(super) fn check_close(
    events: UnboundedSender<AppEvent>,
    session: SessionId,
    handle: Option<ProcessHandle>,
) {
    spawn("close-check", move || {
        let running =
            handle.and_then(|(pid, fd)| procinfo::foreground_process(&fd, pid)).map(|p| p.name);
        send(&events, AppEvent::CloseChecked { session, running });
    });
}

/// Finds the programs running in any of the sessions, before closing the window.
pub(super) fn check_quit(events: UnboundedSender<AppEvent>, handles: Vec<ProcessHandle>) {
    spawn("quit-check", move || {
        let running = handles
            .iter()
            .filter_map(|(pid, fd)| procinfo::foreground_process(fd, *pid))
            .map(|p| p.name)
            .collect();
        send(&events, AppEvent::QuitChecked { running });
    });
}

/// Finds the directory of the current tab, so a new tab or window opens there.
pub(super) fn find_directory(
    events: UnboundedSender<AppEvent>,
    handle: Option<ProcessHandle>,
    window: bool,
) {
    spawn("directory", move || {
        let directory = handle.and_then(|(pid, fd)| procinfo::terminal_directory(&fd, pid));
        send(
            &events,
            if window { AppEvent::OpenWindow(directory) } else { AppEvent::OpenTab(directory) },
        );
    });
}

/// Starts another terminal window as a separate process.
pub(super) fn open_window(directory: Option<PathBuf>) {
    spawn("new-window", move || {
        let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nimbus-terminal"));
        let mut command = std::process::Command::new(program);
        if let Some(directory) = directory {
            command.arg("--working-directory").arg(directory);
        }
        match command.spawn() {
            // Reap the child when it exits, so it doesn't linger as a zombie.
            Ok(mut child) => {
                let _ = child.wait();
            }
            Err(error) => tracing::warn!("cannot open a new window: {error}"),
        }
    });
}

/// Saves preferences in order on one thread, skipping versions that a newer one replaced.
pub(super) struct PrefsWriter {
    requests: Option<mpsc::Sender<Prefs>>,
}

impl PrefsWriter {
    pub(super) fn start(path: Option<PathBuf>) -> Self {
        let Some(path) = path else {
            return Self { requests: None };
        };
        let (tx, rx) = mpsc::channel::<Prefs>();
        spawn("prefs-writer", move || {
            while let Ok(mut prefs) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    prefs = newer;
                }
                write_prefs(&path, &prefs);
            }
        });
        Self { requests: Some(tx) }
    }

    pub(super) fn save(&self, prefs: &Prefs) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(prefs.clone());
        }
    }
}

fn write_prefs(path: &Path, prefs: &Prefs) {
    if let Err(error) = prefs.save_to(path) {
        tracing::warn!("{error}");
    }
}
