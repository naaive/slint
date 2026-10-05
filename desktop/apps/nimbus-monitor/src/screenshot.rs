// SPDX-License-Identifier: MIT

//! Headless rendering with sample data, for `--screenshot` and the screenshot test.
//!
//! Slint's platform can be set once per process, so [`install_platform`] must run before any window exists.

use std::path::Path;
use std::rc::Rc;

use nimbus_config::{Appearance, ColorScheme};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError, Rgb8Pixel};

use crate::app::Controller;
use crate::core::prefs::{Page, Prefs};
use crate::core::rates::RateTracker;
use crate::sampler::{SampleSource, SamplerEvent, Source};
use crate::{AppWindow, Theme};

pub const WIDTH: u32 = 1100;
pub const HEIGHT: u32 = 720;

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

/// Makes Slint render into an off-screen window of the screenshot size, and returns it.
pub fn install_platform() -> anyhow::Result<Rc<MinimalSoftwareWindow>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform { window: window.clone() }))
        .map_err(|e| anyhow::anyhow!("cannot set the headless platform: {e}"))?;
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    Ok(window)
}

/// An RGB image of the window.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgb8Pixel>,
}

impl Frame {
    pub fn pixel(&self, x: u32, y: u32) -> Option<(u8, u8, u8)> {
        let p = self.pixels.get((y * self.width + x) as usize)?;
        Some((p.r, p.g, p.b))
    }

    pub fn write_png(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::File::create(path)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = self.pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&bytes)?;
        writer.finish()?;
        Ok(())
    }
}

/// Renders the current contents of `window`.
pub fn render(window: &MinimalSoftwareWindow) -> Frame {
    slint::platform::update_timers_and_animations();
    let size = window.size();
    let mut pixels = vec![Rgb8Pixel::default(); (size.width * size.height) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, size.width as usize);
    });
    Frame { width: size.width, height: size.height, pixels }
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
    let platform_window = install_platform()?;
    let (ui, _controller) = sample_app(page, light)?;
    // The first pass lays out; the second draws with the final geometry.
    render(&platform_window);
    let frame = render(&platform_window);
    ui.hide()?;
    frame.write_png(path)
}
