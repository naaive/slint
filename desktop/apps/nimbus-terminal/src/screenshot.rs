// SPDX-License-Identifier: MIT

//! Headless rendering of the window with sample content, for `--screenshot` and the screenshot test.
//!
//! Slint's platform can be set once per thread, so [`install_platform`] must run before any window exists.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use nimbus_config::{Appearance, ColorScheme};
use nimbus_theme::headless::Headless;
use slint::ComponentHandle;

use crate::app::{self, Controller, Options};
use crate::fonts::FontSet;
use crate::prefs::Prefs;
use crate::{AppWindow, sample};

pub const WIDTH: u32 = 1100;
pub const HEIGHT: u32 = 720;

/// Makes Slint render into an off-screen window of the screenshot size.
pub fn install_platform() -> anyhow::Result<Headless> {
    Ok(Headless::install(WIDTH, HEIGHT)?)
}

/// The window with three sample tabs, the last one current, in the dark or light scheme.
pub fn sample_app(light: bool) -> anyhow::Result<(AppWindow, Rc<Controller>)> {
    let window = AppWindow::new()?;
    // Events from sample sessions have nowhere to go.
    let (sender, _) = app::channel();
    let appearance = Appearance {
        color_scheme: if light { ColorScheme::Light } else { ColorScheme::Dark },
        animations: false,
        ..Appearance::default()
    };
    let controller = Controller::new(
        &window,
        sender,
        Options { prefs: Prefs::default(), prefs_path: None, appearance, title: None },
    );
    controller.set_fonts(Arc::new(FontSet::discover("")?));
    window.show()?;
    controller.update_geometry();

    controller.open_detached_tab(
        "vim README.md",
        b"\x1b[1;34m# Nimbus\x1b[0m\r\n\r\nA desktop for Wayland.\r\n",
    );
    controller.open_detached_tab("htop", b"\x1b[32m  1\x1b[0m[|||||   23.1%]\r\n");
    controller.open_detached_tab("ada@nimbus: ~/projects/nimbus", &sample::session());
    window.invoke_focus_terminal();
    controller.render_now();
    Ok((window, controller))
}

/// Renders the sample window into the PNG file at `path`; for `--screenshot`.
pub fn run(path: &Path, light: bool) -> anyhow::Result<()> {
    let headless = install_platform()?;
    let (window, controller) = sample_app(light)?;
    // The first pass lays out; the second draws with the final geometry.
    headless.render();
    controller.update_geometry();
    controller.render_now();
    let frame = headless.render();
    window.hide()?;
    Ok(frame.write_png(path)?)
}
