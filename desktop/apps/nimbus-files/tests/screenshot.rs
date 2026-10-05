// SPDX-License-Identifier: MIT

//! Renders the main window with sample data on the software renderer.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/files-main.png`.
//!
//! Slint's platform can be set once per process, so this binary has a single test.

use std::collections::HashSet;
use std::path::Path;

use nimbus_files::screenshot::{self, HEIGHT, Options, WIDTH};

#[test]
fn main_window_renders() {
    let frame = screenshot::render(Options::default()).expect("the window renders");
    assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
    // The sidebar uses `Theme.surface-sunken` and the view `Theme.surface`, in the dark scheme.
    assert_eq!(frame.pixel(4, HEIGHT - 4), Some((0x16, 0x16, 0x19)), "sidebar");
    assert_eq!(frame.pixel(WIDTH - 40, HEIGHT - 80), Some((0x25, 0x25, 0x29)), "view background");
    let colors: HashSet<_> = frame.pixels.iter().map(|p| (p.r, p.g, p.b)).collect();
    assert!(colors.len() > 500, "the window looks blank: {} colors", colors.len());
    if std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS").is_some() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots/files-main.png");
        frame.write_png(&path).expect("the screenshot is written");
    }
}
