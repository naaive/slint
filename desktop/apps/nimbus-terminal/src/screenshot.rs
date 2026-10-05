// SPDX-License-Identifier: MIT

//! Headless rendering of the window with sample content, for `--screenshot` and the screenshot test.
//!
//! Slint's platform can be set once per process, so [`install_platform`] must run before any window exists.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use nimbus_config::{Appearance, ColorScheme};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError, Rgb8Pixel};

use crate::app::{self, Controller, Options};
use crate::fonts::FontSet;
use crate::prefs::Prefs;
use crate::{AppWindow, sample};

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
    let platform_window = install_platform()?;
    let (window, controller) = sample_app(light)?;
    // The first pass lays out; the second draws with the final geometry.
    render(&platform_window);
    controller.update_geometry();
    controller.render_now();
    let frame = render(&platform_window);
    window.hide()?;
    frame.write_png(path)
}
