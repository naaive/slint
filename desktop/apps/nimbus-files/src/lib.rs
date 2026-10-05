// SPDX-License-Identifier: MIT

//! Nimbus Files, the file manager of the Nimbus desktop.
//!
//! `core` holds the UI-free model: listing, sorting, operations, trash, places, and thumbnails.
//! `app` connects it to the Slint window, and `screenshot` renders the window with sample data.

pub mod app;
pub mod cli;
pub mod core;
pub mod screenshot;

slint::include_modules!();
