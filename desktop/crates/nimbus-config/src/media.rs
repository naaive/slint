// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};

/// Removable media, such as USB sticks and memory cards.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Media {
    /// Mount removable media when they're inserted while the session is unlocked.
    pub automount: bool,
}

impl Default for Media {
    fn default() -> Self {
        Self { automount: true }
    }
}

#[cfg(test)]
mod tests {
    use crate::Config;

    #[test]
    fn automount_is_on_by_default() {
        assert!(Config::default().media.automount);
        let config: Config = toml::from_str("[media]\nautomount = false\n").unwrap();
        assert!(!config.media.automount);
    }
}
