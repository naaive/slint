// SPDX-License-Identifier: MIT

//! Nimbus System Monitor: processes, resource graphs, and file systems.

pub mod app;
pub mod cli;
pub mod core;
pub mod sampler;
pub mod screenshot;

slint::include_modules!();

/// The desktop entry id and Wayland app id.
pub const APP_ID: &str = "org.nimbus.Monitor";
