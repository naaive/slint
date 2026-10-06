// SPDX-License-Identifier: MIT

//! Freedesktop.org integration: desktop entries, icon themes, application search, and launching.
//!
//! Implements the Desktop Entry Specification 1.5, the Icon Theme Specification 0.13,
//! and the MIME Applications Associations Specification 1.0.1,
//! and chooses default applications, including the terminal of the xdg-terminal-exec proposal.

mod defaults;
mod entry;
mod exec;
mod icons;
mod index;
mod keyfile;
mod launch;
mod mime;
mod search;

pub use entry::{
    DESKTOP_NAME, DesktopAction, DesktopEntry, ParseOptions, Rejection, current_desktops,
};
pub use exec::ExecError;
pub use icons::IconResolver;
pub use index::{AppIndex, data_dirs, data_home, system_data_dirs};
pub use keyfile::locale_from_env;
pub use launch::{LaunchError, launch};
pub use mime::{MimeLookup, default_handler_for_mime};
