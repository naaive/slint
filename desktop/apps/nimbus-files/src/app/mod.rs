// SPDX-License-Identifier: MIT

//! The window: wiring between the UI and the model.

mod actions;
mod bindings;
mod controller;
pub(crate) mod convert;
mod workers;

pub use controller::{Controller, Env};

use std::path::PathBuf;

use slint::ComponentHandle as _;

use crate::{AppWindow, Theme};

/// Runs the file manager until its window closes.
pub fn run(start: Option<PathBuf>) -> anyhow::Result<()> {
    let config = nimbus_config::Config::load().unwrap_or_else(|error| {
        tracing::warn!("using the default configuration: {error}");
        nimbus_config::Config::default()
    });
    let ui = AppWindow::new()?;
    if let Err(error) = slint::set_xdg_app_id("org.nimbus.Files") {
        tracing::warn!("cannot set the application id: {error}");
    }
    nimbus_theme::apply_theme!(ui, nimbus_theme::ThemeSettings::from_config(&config.appearance));
    let _config_watch = watch_config(&ui);
    let controller = Controller::new(ui.clone_strong(), Env::from_system(), start);
    controller.start_message_loop();
    ui.run()?;
    Ok(())
}

/// Re-applies the theme when the Nimbus configuration changes.
fn watch_config(ui: &AppWindow) -> Option<nimbus_config::ConfigWatcher> {
    let path = nimbus_config::default_path().ok()?;
    let weak = ui.as_weak();
    nimbus_config::watch(&path, move |config| {
        // Resolving the system scheme may query the portal, so it happens here on the watcher thread.
        let settings = nimbus_theme::ThemeSettings::from_config(&config.appearance);
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                nimbus_theme::apply_theme!(ui, settings);
            }
        });
    })
    .map_err(|error| tracing::warn!("not watching the configuration: {error}"))
    .ok()
}
