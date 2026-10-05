// SPDX-License-Identifier: MIT

//! Nimbus Terminal: a terminal emulator on `alacritty_terminal`, drawn with its own glyph renderer in a Slint window.
//!
//! The modules other than `app` and `screenshot` have no UI dependency.

pub mod boxdraw;
pub mod cli;
pub mod clipboard;
pub mod engine;
pub mod fonts;
pub mod glyphs;
pub mod keys;
pub mod mouse;
pub mod palette;
pub mod prefs;
pub mod procinfo;
pub mod render;
pub mod sample;
pub mod search;
pub mod selection;
pub mod shortcuts;

pub mod app;
pub mod screenshot;

/// The application id, used for the desktop entry and the Wayland app id.
pub const APP_ID: &str = "org.nimbus.Terminal";

mod ui {
    slint::include_modules!();
}

pub use ui::{AppWindow, Preferences, Theme};
