// SPDX-License-Identifier: MIT

//! The Nimbus design system.
//!
//! The Slint sources in `ui/` are a library imported as `@nimbus`, for example
//! `import { Theme, Button } from "@nimbus/theme.slint";`.
//! Consumers call [`library_paths`] from their `build.rs` and [`apply_theme!`] at runtime.

use std::collections::HashMap;
use std::path::PathBuf;

/// Library paths for `slint_build::CompilerConfiguration::with_library_paths`.
pub fn library_paths() -> HashMap<String, PathBuf> {
    HashMap::from([("nimbus".to_string(), PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"))])
}

/// Runtime theme values resolved from the user's configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeSettings {
    pub dark: bool,
    /// Accent as `(red, green, blue)`.
    pub accent: (u8, u8, u8),
    pub corner_radius: f32,
    pub font_family: String,
    pub font_size: f32,
    pub animations: bool,
}

impl ThemeSettings {
    /// Resolves `ColorScheme::System` through the XDG desktop portal's `color-scheme` setting, defaulting to dark.
    pub fn from_config(appearance: &nimbus_config::Appearance) -> Self {
        let _ = appearance;
        todo!()
    }
}

/// Sets the `Theme` global of a generated component from a [`ThemeSettings`].
///
/// Each crate compiles its own copy of the `Theme` global, so this is a macro rather than a function.
#[macro_export]
macro_rules! apply_theme {
    ($component:expr, $settings:expr) => {{
        let _ = (&$component, &$settings);
        todo!()
    }};
}
