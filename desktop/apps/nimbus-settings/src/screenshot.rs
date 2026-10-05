// SPDX-License-Identifier: MIT

//! Headless rendering of the window with sample data, for `--screenshot` and the screenshot test.
//!
//! Slint's platform can be set once per process, so [`install_platform`] must run before any window exists.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use nimbus_config::{ColorScheme, Config};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError, Rgb8Pixel};

use crate::dispatch::Dispatch;
use crate::page::Page;
use crate::sources::SampleSources;
use crate::view::{App, AppOptions, SystemScheme};

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

/// Makes Slint render into an off-screen window, and returns it.
pub fn install_platform() -> anyhow::Result<Rc<MinimalSoftwareWindow>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform { window: window.clone() }))
        .map_err(|e| anyhow::anyhow!("cannot set the headless platform: {e}"))?;
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    Ok(window)
}

/// The sample configuration shown in screenshots.
pub fn sample_config(light: bool) -> Config {
    let mut config = Config::default();
    config.appearance.color_scheme = if light { ColorScheme::Light } else { ColorScheme::Dark };
    config.appearance.wallpaper = Some(PathBuf::from("/usr/share/backgrounds/nimbus/aurora.jpg"));
    config.input.keyboard_layout = "us,de".into();
    config.input.keyboard_variant = ",nodeadkeys".into();
    config.input.keyboard_options = "compose:ralt".into();
    config
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

/// Creates the app with sample data on `page`, editing a configuration file in `dir`.
pub fn sample_app(dir: &Path, page: Page, light: bool) -> anyhow::Result<App> {
    let config_path = dir.join("config.toml");
    sample_config(light).save_to(&config_path)?;
    let app = App::new(AppOptions {
        config_path,
        page,
        sources: Arc::new(SampleSources),
        dispatch: Dispatch::Inline,
        system_scheme: SystemScheme::Fixed { dark: !light },
        watch: false,
        debounce: Duration::from_millis(50),
    })?;
    Ok(app)
}

/// Renders `page` with sample data into the PNG file at `path`; for `--screenshot`.
pub fn run(path: &Path, page: Page, light: bool) -> anyhow::Result<()> {
    let window = install_platform()?;
    let dir =
        std::env::temp_dir().join(format!("nimbus-settings-screenshot-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = (|| {
        let app = sample_app(&dir, page, light)?;
        app.window().show()?;
        // The first pass lays out; the second draws images that arrived during it.
        render(&window);
        let frame = render(&window);
        app.window().hide()?;
        frame.write_png(path)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}
