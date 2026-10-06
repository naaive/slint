// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Running X11 apps through `xwayland-satellite`, which the compositor starts when the first X11 client connects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Xwayland {
    pub enabled: bool,
    /// The `xwayland-satellite` executable; found in `PATH` when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Default for Xwayland {
    fn default() -> Self {
        Self { enabled: true, path: None }
    }
}

#[cfg(test)]
mod tests {
    use crate::Config;

    #[test]
    fn xwayland_is_enabled_by_default_and_takes_a_path() {
        assert!(Config::default().xwayland.enabled);
        let config: Config =
            toml::from_str("[xwayland]\nenabled = false\npath = \"/opt/satellite\"\n").unwrap();
        assert!(!config.xwayland.enabled);
        assert_eq!(config.xwayland.path.as_deref(), Some(std::path::Path::new("/opt/satellite")));
    }
}
