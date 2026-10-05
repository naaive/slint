// SPDX-License-Identifier: MIT

//! Renders the window with sample data; set NIMBUS_UPDATE_SCREENSHOTS to write desktop/docs/screenshots/monitor-*.png.

use std::path::PathBuf;

use nimbus_monitor::core::prefs::Page;
use nimbus_monitor::screenshot;
use slint::ComponentHandle;

#[test]
fn renders_every_page() {
    let headless = screenshot::install_platform().expect("platform");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots");
    let update = std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS").is_some();
    for (page, light, name) in [
        (Page::Processes, false, "monitor-main"),
        (Page::Resources, false, "monitor-resources"),
        (Page::FileSystems, false, "monitor-file-systems"),
        (Page::Resources, true, "monitor-light"),
    ] {
        let (ui, _controller) = screenshot::sample_app(page, light).expect("sample window");
        headless.render();
        let frame = headless.render();
        assert_eq!((frame.width, frame.height), (screenshot::WIDTH, screenshot::HEIGHT));
        let colors = frame.distinct_colors();
        assert!(colors > 100, "{name} looks blank: {colors} colors");
        // The sidebar is the sunken surface of the active scheme.
        let (r, _, _) = frame.pixel(10, 300).expect("pixel");
        assert_eq!(r < 100, !light, "{name}: unexpected sidebar color");
        if update {
            frame.write_png(&out.join(format!("{name}.png"))).expect("write png");
        }
        ui.hide().expect("hide");
    }
}
