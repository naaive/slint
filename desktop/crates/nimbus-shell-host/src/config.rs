// SPDX-License-Identifier: MIT

//! The configuration file: loading it, watching it, and applying changes.

use crate::state::State;
use nimbus_config::{Config, ConfigWatcher};
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event as ChannelEvent};
use std::path::{Path, PathBuf};

pub struct Settings {
    path: Option<PathBuf>,
    pub current: Config,
    watcher: Option<ConfigWatcher>,
}

impl Settings {
    /// Loads `path` or the default location; an invalid file is reported and replaced by the defaults.
    pub fn load(path: Option<PathBuf>) -> Self {
        let path = match path.map(Ok).unwrap_or_else(nimbus_config::default_path) {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("{err}; using the default configuration");
                return Self { path: None, current: Config::default(), watcher: None };
            }
        };
        let current = Config::load_from(&path).unwrap_or_else(|err| {
            tracing::error!("{err}; using the default configuration");
            Config::default()
        });
        Self { path: Some(path), current, watcher: None }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Delivers changes of the file to [`State::apply_config`] on the event loop.
    pub fn watch(&mut self, handle: &LoopHandle<'static, State>) {
        let Some(path) = &self.path else {
            return;
        };
        let (sender, receiver) = channel::channel::<Config>();
        if let Err(err) = handle.insert_source(receiver, |event, _, state| {
            if let ChannelEvent::Msg(config) = event {
                tracing::info!("configuration changed");
                state.apply_config(config);
            }
        }) {
            tracing::warn!("cannot watch the configuration: {err}");
            return;
        }
        match nimbus_config::watch(path, move |config| {
            // The receiver only goes away when the shell exits.
            let _ = sender.send(config);
        }) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(err) => tracing::warn!("cannot watch the configuration: {err}"),
        }
    }
}

impl State {
    pub fn apply_config(&mut self, config: Config) {
        let old = std::mem::replace(&mut self.settings.current, config);
        let config = &self.settings.current;
        self.model.set_config(config);
        if old.appearance.icon_theme != config.appearance.icon_theme {
            self.reload_apps();
        }
        if old.power.lock_after_minutes != config.power.lock_after_minutes {
            self.watch_idle();
        }
    }
}
