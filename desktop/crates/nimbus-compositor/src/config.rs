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
use std::sync::mpsc;
use std::thread::JoinHandle;

pub struct ConfigManager {
    path: Option<PathBuf>,
    current: Config,
    watcher: Option<ConfigWatcher>,
    writer: Option<Writer>,
}

type Edit = Box<dyn FnOnce(&mut Config) + Send>;

/// Saves edits in order on a thread of its own, because [`nimbus_config::update`] waits for other programs' lock on the file.
struct Writer {
    edits: mpsc::Sender<Edit>,
    thread: JoinHandle<()>,
}

impl Writer {
    fn spawn(path: PathBuf) -> std::io::Result<Self> {
        let (edits, receiver) = mpsc::channel::<Edit>();
        let thread = std::thread::Builder::new().name("nimbus-config".into()).spawn(move || {
            for edit in receiver {
                if let Err(err) = nimbus_config::update(&path, edit) {
                    tracing::warn!("cannot save the configuration: {err}");
                }
            }
        })?;
        Ok(Self { edits, thread })
    }
}

impl ConfigManager {
    pub fn load(path: Option<PathBuf>) -> Self {
        let (path, current) = Config::load_or_default(path);
        Self { path, current, watcher: None, writer: None }
    }

    pub fn current(&self) -> &Config {
        &self.current
    }

    pub fn reload(&self) -> Result<Config, nimbus_config::Error> {
        self.path.as_deref().map_or_else(|| Ok(Config::default()), Config::load_from)
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
            // The receiver only goes away when the compositor exits.
            let _ = sender.send(config);
        }) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(err) => tracing::warn!("cannot watch the configuration: {err}"),
        }
    }

    /// Applies `edit` to the configuration, and saves it in the background without losing other programs' changes to the file.
    pub fn update(&mut self, edit: impl Fn(&mut Config) + Send + 'static) {
        edit(&mut self.current);
        let Some(path) = &self.path else {
            return;
        };
        if self.writer.is_none() {
            match Writer::spawn(path.clone()) {
                Ok(writer) => self.writer = Some(writer),
                Err(err) => {
                    tracing::warn!("cannot save the configuration: {err}");
                    return;
                }
            }
        }
        if let Some(writer) = &self.writer {
            // The thread only ends when the writer is dropped.
            let _ = writer.edits.send(Box::new(move |config| edit(config)));
        }
    }

    fn replace(&mut self, config: Config) -> Config {
        std::mem::replace(&mut self.current, config)
    }
}

impl Drop for ConfigManager {
    /// Waits for the edits that aren't saved yet.
    fn drop(&mut self) {
        if let Some(Writer { edits, thread }) = self.writer.take() {
            drop(edits);
            let _ = thread.join();
        }
    }
}

impl State {
    /// Applies a new configuration: input, keybindings, workspaces, gaps, layout, wallpaper, decorations, and displays.
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
        if old.appearance != config.appearance {
            self.nimbus.decorations.set_appearance(&config.appearance);
            self.nimbus.wm.set_decoration_metrics(self.nimbus.decorations.metrics());
        }
        if old.outputs != config.outputs || old.appearance.scale != config.appearance.scale {
            self.nimbus.reconfigure_outputs(&mut self.backend);
        }
        self.nimbus.arrange();
    }
}
