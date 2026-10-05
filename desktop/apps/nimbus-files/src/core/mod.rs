// SPDX-License-Identifier: MIT

//! The file manager's model, free of any UI dependency.

pub mod apps;
pub mod diff;
pub mod entry;
pub mod format;
pub mod history;
pub mod keymap;
pub mod menu;
pub mod mime;
pub mod names;
pub mod ops;
pub mod pathbar;
pub mod places;
pub mod prefs;
pub mod properties;
pub mod rubberband;
pub mod search;
pub mod selection;
pub mod sort;
pub mod thumbnail;
pub mod trash;
pub mod uri;

#[cfg(test)]
pub(crate) mod testutil;
