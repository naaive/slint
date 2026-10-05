// SPDX-License-Identifier: MIT

//! Off-screen rendering with Slint's software renderer, for `--screenshot` options and screenshot tests.
//!
//! Enabled by the `headless` feature.

use std::io;
use std::path::Path;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, TargetPixel};
use slint::platform::{Platform, WindowAdapter};
use slint::{PhysicalSize, PlatformError, Rgb8Pixel};

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

/// A Slint platform that shows every component in one off-screen window.
pub struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Headless {
    /// Makes Slint render into an off-screen window of `width` by `height` physical pixels.
    ///
    /// Slint's platform can be set once per thread, before the first component is created.
    /// Call this once, at the start of the program or test, and keep the result for rendering.
    /// It fails when the thread already has a platform.
    pub fn install(width: u32, height: u32) -> Result<Self, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(HeadlessPlatform { window: window.clone() }))
            .map_err(|err| {
                PlatformError::Other(format!("cannot set the headless platform: {err}"))
            })?;
        window.set_size(PhysicalSize::new(width, height));
        Ok(Self { window })
    }

    /// The window that components are shown in.
    pub fn window(&self) -> &Rc<MinimalSoftwareWindow> {
        &self.window
    }

    /// Advances Slint's timers and animations, then renders the window.
    pub fn render(&self) -> Frame {
        slint::platform::update_timers_and_animations();
        self.draw()
    }

    /// Renders the window without advancing timers and animations.
    ///
    /// Use it when timers would change what's rendered, such as a delayed search.
    pub fn draw(&self) -> Frame {
        let size = self.window.size();
        Frame { width: size.width, height: size.height, pixels: self.draw_pixels() }
    }

    /// Like [`Headless::render`], into pixels of type `P`, row by row.
    ///
    /// Use [`slint::platform::software_renderer::PremultipliedRgbaColor`] to keep transparency.
    pub fn render_pixels<P: TargetPixel + Default>(&self) -> Vec<P> {
        slint::platform::update_timers_and_animations();
        self.draw_pixels()
    }

    fn draw_pixels<P: TargetPixel + Default>(&self) -> Vec<P> {
        let size = self.window.size();
        let mut pixels = vec![P::default(); size.width as usize * size.height as usize];
        self.window.request_redraw();
        self.window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, size.width as usize);
        });
        pixels
    }
}

/// An RGB image, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgb8Pixel>,
}

impl Frame {
    /// The color at `(x, y)` as `(red, green, blue)`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<(u8, u8, u8)> {
        (x < self.width && y < self.height).then(|| {
            let p = self.pixels[y as usize * self.width as usize + x as usize];
            (p.r, p.g, p.b)
        })
    }

    /// How many different colors the image has; a low count means it looks blank.
    pub fn distinct_colors(&self) -> usize {
        self.pixels.iter().map(|p| (p.r, p.g, p.b)).collect::<std::collections::HashSet<_>>().len()
    }

    /// Writes the image to `path` as an 8-bit RGB PNG, creating missing parent directories.
    pub fn write_png(&self, path: &Path) -> io::Result<()> {
        let with_path =
            |err: io::Error| io::Error::new(err.kind(), format!("{}: {err}", path.display()));
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(with_path)?;
        }
        let file = std::fs::File::create(path).map_err(with_path)?;
        let mut encoder = png::Encoder::new(io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = self.pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let mut writer = encoder.write_header().map_err(|err| with_path(err.into()))?;
        writer.write_image_data(&bytes).map_err(|err| with_path(err.into()))?;
        writer.finish().map_err(|err| with_path(err.into()))
    }
}
