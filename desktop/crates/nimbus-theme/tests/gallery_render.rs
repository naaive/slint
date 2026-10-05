// SPDX-License-Identifier: MIT

//! Renders `ui/gallery.slint` in both schemes with the software renderer.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/theme-gallery-{dark,light}.png`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::Rgb8Pixel;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{PhysicalSize, PlatformError};
use slint_interpreter::{ComponentHandle, Value};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 1460;

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

fn render(window: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    let mut buffer = vec![Rgb8Pixel::default(); (WIDTH * HEIGHT) as usize];
    window.request_redraw();
    let drawn = window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, WIDTH as usize);
    });
    assert!(drawn, "the window didn't redraw");
    buffer
}

fn pixel(buffer: &[Rgb8Pixel], x: u32, y: u32) -> (u8, u8, u8) {
    let p = buffer[(y * WIDTH + x) as usize];
    (p.r, p.g, p.b)
}

fn write_png(path: &Path, buffer: &[Rgb8Pixel]) {
    let file = std::fs::File::create(path).expect("screenshot file can be created");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let bytes: Vec<u8> = buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    encoder.write_header().and_then(|mut w| w.write_image_data(&bytes)).expect("PNG encodes");
}

#[test]
fn gallery_renders_in_both_schemes() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform { window: window.clone() }))
        .expect("no platform was set on this thread");
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));

    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths(nimbus_theme::library_paths());
    let result = spin_on::spin_on(compiler.build_from_path(ui.join("gallery.slint")));
    assert!(!result.has_errors(), "{:#?}", result.diagnostics().collect::<Vec<_>>());
    let gallery = result
        .component("Gallery")
        .expect("Gallery is exported")
        .create()
        .expect("Gallery instantiates");
    let set_theme = |name: &str, value: Value| {
        gallery.set_global_property("Theme", name, value).expect("Theme property exists");
    };
    // Snapshots must not catch a color transition halfway.
    set_theme("animations", Value::Bool(false));
    gallery.show().expect("the window shows");

    let screenshots = std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS")
        .map(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots"));
    for (dark, name, sidebar) in
        [(false, "light", (0xe9, 0xe9, 0xed)), (true, "dark", (0x16, 0x16, 0x19))]
    {
        set_theme("dark", Value::Bool(dark));
        let buffer = render(&window);
        // Below the header bar, in the sidebar under the last navigation item.
        assert_eq!(pixel(&buffer, 4, HEIGHT - 4), sidebar, "{name} sidebar color");
        let distinct: std::collections::HashSet<_> =
            buffer.iter().map(|p| (p.r, p.g, p.b)).collect();
        assert!(distinct.len() > 200, "{name} gallery looks blank: {} colors", distinct.len());
        if let Some(dir) = &screenshots {
            std::fs::create_dir_all(dir).expect("screenshot directory can be created");
            let path: PathBuf = dir.join(format!("theme-gallery-{name}.png"));
            write_png(&path, &buffer);
        }
    }
    gallery.hide().expect("the window hides");
}
