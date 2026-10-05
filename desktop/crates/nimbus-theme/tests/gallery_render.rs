// SPDX-License-Identifier: MIT

//! Renders `ui/gallery.slint` in both schemes with the software renderer.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/theme-gallery-{dark,light}.png`.

use std::path::Path;

use nimbus_theme::headless::Headless;
use slint_interpreter::{ComponentHandle, Value};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 1460;

#[test]
fn gallery_renders_in_both_schemes() {
    let headless = Headless::install(WIDTH, HEIGHT).expect("no platform was set on this thread");

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
        let frame = headless.draw();
        // Below the header bar, in the sidebar under the last navigation item.
        assert_eq!(frame.pixel(4, HEIGHT - 4), Some(sidebar), "{name} sidebar color");
        let colors = frame.distinct_colors();
        assert!(colors > 200, "{name} gallery looks blank: {colors} colors");
        if let Some(dir) = &screenshots {
            let path = dir.join(format!("theme-gallery-{name}.png"));
            frame.write_png(&path).expect("the screenshot is written");
        }
    }
    gallery.hide().expect("the window hides");
}
