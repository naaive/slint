// SPDX-License-Identifier: MIT

//! The window: wiring between the Slint UI, the sessions, and the worker threads.
//!
//! Threads report to the UI thread through one channel of [`AppEvent`]s,
//! which an async task on Slint's event loop hands to the [`Controller`].

mod controller;
mod input;
mod models;
mod workers;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures_util::StreamExt as _;
use slint::ComponentHandle as _;

pub use controller::{Controller, Options};

use crate::cli;
use crate::engine::{SessionEvent, SessionId};
use crate::fonts::FontSet;
use crate::prefs::Prefs;

/// Everything that reaches the UI thread from elsewhere.
pub enum AppEvent {
    Session(SessionId, SessionEvent),
    FontsLoaded(Result<Arc<FontSet>, String>),
    Theme(nimbus_theme::ThemeSettings),
    /// Text to paste into the current tab, from the clipboard or the primary selection.
    Paste(Option<String>),
    CloseChecked {
        session: SessionId,
        running: Option<String>,
    },
    QuitChecked {
        running: Vec<String>,
    },
    OpenTab(Option<PathBuf>),
    OpenWindow(Option<PathBuf>),
}

/// Creates the channel that carries [`AppEvent`]s to the UI thread.
pub fn channel() -> (UnboundedSender<AppEvent>, UnboundedReceiver<AppEvent>) {
    futures_channel::mpsc::unbounded()
}

/// Hands every event from `events` to `controller` on Slint's event loop.
pub fn pump(
    controller: &Rc<Controller>,
    mut events: UnboundedReceiver<AppEvent>,
) -> Result<(), slint::EventLoopError> {
    let weak = Rc::downgrade(controller);
    slint::spawn_local(async move {
        while let Some(event) = events.next().await {
            let Some(controller) = weak.upgrade() else { break };
            controller.handle(event);
        }
    })
    .map(|_| ())
}

/// Opens the terminal window and runs until it closes.
pub fn run(options: cli::Options) -> anyhow::Result<()> {
    let config_path = nimbus_config::default_path().ok();
    let config = match config_path.as_deref().map(nimbus_config::Config::load_from) {
        Some(Ok(config)) => config,
        Some(Err(error)) => {
            tracing::warn!("{error}");
            nimbus_config::Config::default()
        }
        None => nimbus_config::Config::default(),
    };
    let prefs_path = crate::prefs::default_path().ok();
    let prefs = prefs_path.as_deref().map_or_else(Prefs::default, Prefs::load_or_default);

    let window = crate::AppWindow::new()?;
    if let Err(error) = slint::set_xdg_app_id(crate::APP_ID) {
        tracing::warn!("cannot set the application id: {error}");
    }
    let (sender, receiver) = channel();
    let controller = Controller::new(
        &window,
        sender.clone(),
        Options { prefs, prefs_path, appearance: config.appearance.clone(), title: options.title },
    );
    pump(&controller, receiver)?;
    workers::load_fonts(sender.clone(), controller.font_family());
    workers::resolve_theme(sender.clone(), config.appearance);

    let watcher = config_path.and_then(|path| {
        let sender = sender.clone();
        // The callback runs on the watcher's thread, where waiting for the desktop portal is harmless.
        nimbus_config::watch(&path, move |config| {
            let settings = nimbus_theme::ThemeSettings::from_config(&config.appearance);
            workers::send(&sender, AppEvent::Theme(settings));
        })
        .inspect_err(|error| tracing::warn!("not following configuration changes: {error}"))
        .ok()
    });

    controller.open_tab(crate::engine::SpawnOptions {
        command: options.command,
        working_directory: options.working_directory,
    });
    let weak = Rc::downgrade(&controller);
    window.window().on_close_requested(move || match weak.upgrade() {
        Some(controller) => controller.close_requested(),
        None => slint::CloseRequestResponse::HideWindow,
    });

    window.show()?;
    window.invoke_focus_terminal();
    slint::run_event_loop()?;
    window.hide()?;
    controller.shutdown();
    drop(watcher);
    Ok(())
}
