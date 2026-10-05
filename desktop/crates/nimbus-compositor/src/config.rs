// SPDX-License-Identifier: MIT

//! Loading the configuration, watching it, and applying changes live.

use crate::keybindings::Bindings;
use crate::render::Wallpaper;
use crate::state::{State, xkb_config};
use crate::wm::layout_from_config;
use nimbus_config::{Config, ConfigWatcher};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::channel::{self, Event as ChannelEvent};
use std::path::PathBuf;

pub struct ConfigManager {
    path: PathBuf,
    current: Config,
    watcher: Option<ConfigWatcher>,
}

impl ConfigManager {
    /// Loads `path` or the default location; an invalid file is reported and replaced by the defaults.
    pub fn load(path: Option<PathBuf>) -> Self {
        let path = match path.map(Ok).unwrap_or_else(nimbus_config::default_path) {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("{err}; using the default configuration");
                return Self { path: PathBuf::new(), current: Config::default(), watcher: None };
            }
        };
        let current = Config::load_from(&path).unwrap_or_else(|err| {
            tracing::error!("{err}; using the default configuration");
            Config::default()
        });
        Self { path, current, watcher: None }
    }

    /// The configuration file, unless no location could be determined.
    pub fn path(&self) -> Option<&std::path::Path> {
        (!self.path.as_os_str().is_empty()).then_some(self.path.as_path())
    }

    pub fn current(&self) -> &Config {
        &self.current
    }

    pub fn reload(&self) -> Result<Config, nimbus_config::Error> {
        Config::load_from(&self.path)
    }

    /// Delivers changes of the file to [`State::apply_config`] on the event loop.
    pub fn watch(&mut self, handle: &LoopHandle<'static, State>) {
        if self.path.as_os_str().is_empty() {
            return;
        }
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
        match nimbus_config::watch(&self.path, move |config| {
            // The receiver only goes away when the compositor exits.
            let _ = sender.send(config);
        }) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(err) => tracing::warn!("cannot watch the configuration: {err}"),
        }
    }

    fn replace(&mut self, config: Config) -> Config {
        std::mem::replace(&mut self.current, config)
    }
}

impl State {
    /// Applies a new configuration: input, keybindings, workspaces, gaps, layout, scale, wallpaper, and the shell.
    pub fn apply_config(&mut self, config: Config) {
        let old = self.nimbus.config.replace(config.clone());
        if old.input != config.input {
            if let Some(keyboard) = self.nimbus.keyboard.clone() {
                if let Err(err) = keyboard.set_xkb_config(self, xkb_config(&config.input)) {
                    tracing::warn!(
                        "invalid keyboard layout '{}': {err}",
                        config.input.keyboard_layout
                    );
                }
                keyboard.change_repeat_info(
                    i32::try_from(config.input.repeat_rate).unwrap_or(30),
                    i32::try_from(config.input.repeat_delay_ms).unwrap_or(300),
                );
            }
            self.backend.apply_input_config(&config.input);
        }
        if old.keybindings != config.keybindings {
            self.nimbus.bindings = Bindings::from_config(&config.keybindings);
        }
        if old.workspaces.count != config.workspaces.count {
            self.nimbus.wm.set_workspace_count(config.workspaces.count);
        }
        if old.workspaces.gaps != config.workspaces.gaps {
            self.nimbus.wm.set_gaps(config.workspaces.gaps);
        }
        if old.workspaces.layout != config.workspaces.layout {
            self.nimbus.wm.set_layout(layout_from_config(config.workspaces.layout));
        }
        if old.appearance.wallpaper != config.appearance.wallpaper {
            self.nimbus.wallpaper = Wallpaper::new(config.appearance.wallpaper.clone());
        }
        if old.appearance.scale != config.appearance.scale
            && config.appearance.scale.is_finite()
            && config.appearance.scale > 0.0
        {
            for output in self.nimbus.outputs().cloned().collect::<Vec<_>>() {
                output.change_current_state(
                    None,
                    None,
                    Some(smithay::output::Scale::Fractional(config.appearance.scale)),
                    None,
                );
            }
            self.nimbus.outputs_changed();
        }
        if let Some(shell) = self.nimbus.shell.as_ref() {
            shell.set_config(&config);
        }
        self.nimbus.arrange();
    }
}
