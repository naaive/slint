// SPDX-License-Identifier: MIT

//! Writes changes the shell makes to the configuration file, such as the dark style toggle and dock pins.
//!
//! Edits run in order on one background thread, so the UI never waits for the disk
//! and a later edit never loses to an earlier one.
//! The compositor watches the file and pushes the new configuration back through `Shell::set_config`.

use std::path::PathBuf;
use std::sync::mpsc;

use nimbus_config::Config;

type Edit = Box<dyn FnOnce(&mut Config) + Send>;

pub struct ConfigWriter {
    path: Option<PathBuf>,
    sender: Option<mpsc::Sender<(Option<PathBuf>, Edit)>>,
}

impl ConfigWriter {
    pub fn new() -> Self {
        Self { path: None, sender: None }
    }

    /// Sets the file to edit; `None` means `nimbus_config::default_path()`.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }

    /// Loads the file, applies `edit`, and saves it, on the writer thread.
    pub fn edit(&mut self, edit: impl FnOnce(&mut Config) + Send + 'static) {
        let job = (self.path.clone(), Box::new(edit) as Edit);
        let job = match &self.sender {
            Some(sender) => match sender.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        match spawn_writer() {
            Some(sender) => {
                if sender.send(job).is_err() {
                    tracing::warn!("The configuration writer stopped; the change isn't saved");
                }
                self.sender = Some(sender);
            }
            None => tracing::warn!("Can't start the configuration writer; the change isn't saved"),
        }
    }
}

fn spawn_writer() -> Option<mpsc::Sender<(Option<PathBuf>, Edit)>> {
    let (sender, receiver) = mpsc::channel::<(Option<PathBuf>, Edit)>();
    let spawned = std::thread::Builder::new().name("nimbus-shell-config".into()).spawn(move || {
        for (path, edit) in receiver {
            let path = match path.map_or_else(nimbus_config::default_path, Ok) {
                Ok(path) => path,
                Err(err) => {
                    tracing::warn!("Can't save the configuration: {err}");
                    continue;
                }
            };
            let mut config = match Config::load_from(&path) {
                Ok(config) => config,
                Err(err) => {
                    // Saving over a file that doesn't parse would discard the user's other settings.
                    tracing::warn!("Not saving the configuration: {err}");
                    continue;
                }
            };
            edit(&mut config);
            if let Err(err) = config.save_to(&path) {
                tracing::warn!("Can't save the configuration: {err}");
            }
        }
    });
    match spawned {
        Ok(_) => Some(sender),
        Err(err) => {
            tracing::warn!("Can't start the configuration writer thread: {err}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_config::ColorScheme;
    use std::time::{Duration, Instant};

    fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn edits_apply_in_order() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("nimbus/config.toml");
        let mut writer = ConfigWriter::new();
        writer.set_path(Some(path.clone()));
        writer.edit(|c| c.appearance.color_scheme = ColorScheme::Light);
        writer.edit(|c| c.favorites = vec!["firefox".into()]);
        writer.edit(|c| c.favorites.push("foot".into()));
        writer.edit(|c| c.appearance.color_scheme = ColorScheme::Dark);
        assert!(wait_for(|| Config::load_from(&path).is_ok_and(|c| {
            c.appearance.color_scheme == ColorScheme::Dark && c.favorites == ["firefox", "foot"]
        })));
    }

    #[test]
    fn invalid_files_are_left_alone() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "panel = [not toml").expect("fixture writes");
        let mut writer = ConfigWriter::new();
        writer.set_path(Some(path.clone()));
        writer.edit(|c| c.favorites.push("firefox".into()));
        let marker = dir.path().join("marker.toml");
        writer.set_path(Some(marker.clone()));
        writer.edit(|_| {});
        assert!(wait_for(|| marker.exists()));
        assert_eq!(std::fs::read_to_string(&path).expect("fixture reads"), "panel = [not toml");
    }
}
