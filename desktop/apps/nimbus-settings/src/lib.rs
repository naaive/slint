// SPDX-License-Identifier: MIT

//! Nimbus Settings: the system settings app, editing the Nimbus configuration.
//!
//! The modules other than `view` and `screenshot` have no UI dependency and hold the app's logic.

slint::include_modules!();

pub mod about;
pub mod bluetooth;
pub mod cli;
pub mod clock;
pub mod dispatch;
pub mod displays;
pub mod fonts;
pub mod imaging;
pub mod network;
pub mod page;
pub mod screenshot;
pub mod search;
pub mod settings;
pub mod shortcuts;
pub mod sources;
pub mod store;
pub mod view;
pub mod wallpapers;
pub mod xkb;
