// SPDX-License-Identifier: MIT

//! Headless rendering with sample data, for `--screenshot` and the screenshot test.
//!
//! Slint's platform can be set once per thread, so [`install_platform`] must run before any window exists.

use std::path::Path;
use std::rc::Rc;

use nimbus_config::{Appearance, ColorScheme};
use nimbus_theme::headless::Headless;
use slint::ComponentHandle;

use crate::app::Controller;
use crate::core::prefs::{Page, Prefs};
use crate::core::rates::RateTracker;
use crate::sampler::{SampleSource, SamplerEvent, Source};
use crate::{AppWindow, Theme};

pub const WIDTH: u32 = 1100;
pub const HEIGHT: u32 = 720;

/// Makes Slint render into an off-screen window of the screenshot size.
pub fn install_platform() -> anyhow::Result<Headless> {
    Ok(Headless::install(WIDTH, HEIGHT)?)
}

/// The window on `page` after a minute of sample data, with a process selected.
pub fn sample_app(page: Page, light: bool) -> anyhow::Result<(AppWindow, Rc<Controller>)> {
    let ui = AppWindow::new()?;
    let appearance = Appearance {
        color_scheme: if light { ColorScheme::Light } else { ColorScheme::Dark },
        animations: false,
        ..Appearance::default()
    };
    nimbus_theme::apply_theme!(ui, nimbus_theme::ThemeSettings::from_config(&appearance));
    let prefs = Prefs { page, all_users: true, ..Prefs::default() };
    let controller = Controller::new(&ui, prefs, None, Some("ada".into()), |_| {});
    let mut source = SampleSource::new();
    let mut rates = RateTracker::new();
    for _ in 0..60 {
        controller.handle(SamplerEvent::Snapshot(Box::new(rates.update(source.sample()))));
    }
    ui.invoke_select(2350);
    ui.show()?;
    Ok((ui, controller))
}

/// Renders the sample window into the PNG file at `path`; for `--screenshot`.
pub fn run(path: &Path, page: Page, light: bool) -> anyhow::Result<()> {
    let headless = install_platform()?;
    let (ui, _controller) = sample_app(page, light)?;
    // The first pass lays out; the second draws with the final geometry.
    headless.render();
    let frame = headless.render();
    ui.hide()?;
    Ok(frame.write_png(path)?)
}
