// SPDX-License-Identifier: MIT

//! Renders every page with sample data on the software renderer and checks that each draws.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/settings-*.png`.
//!
//! Slint's platform can be set once per thread, so a single test renders every page.

use std::path::PathBuf;

use nimbus_settings::page::Page;
use nimbus_settings::screenshot::{self, HEIGHT, WIDTH};
use nimbus_settings::{Nav, Prefs};
use slint::ComponentHandle;

#[test]
fn pages_render() {
    let headless = screenshot::install_platform().expect("the platform is set once");
    let dir = tempfile::tempdir().expect("temporary directory");
    let app = screenshot::sample_app(dir.path(), Page::Appearance, false).expect("the app starts");
    app.window().show().expect("the window shows");
    let output = std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS")
        .map(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots"));

    for page in Page::ALL {
        app.window().global::<Nav>().set_page(page.index() as i32);
        headless.render();
        let frame = headless.render();
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        // The sidebar is `Theme.surface-sunken` in the dark scheme.
        assert_eq!(frame.pixel(4, HEIGHT - 4), Some((0x16, 0x16, 0x19)), "{page} sidebar");
        assert!(frame.distinct_colors() > 150, "{page} looks blank");
        if let Some(dir) = &output {
            let name =
                if page == Page::Appearance { "main".to_string() } else { page.id().to_string() };
            frame
                .write_png(&dir.join(format!("settings-{name}.png")))
                .expect("the screenshot is written");
        }
    }

    // Switching the scheme through the UI applies the light theme right away.
    app.window().global::<Nav>().set_page(Page::Shortcuts.index() as i32);
    app.window().global::<Prefs>().invoke_set_int("appearance.color-scheme".into(), 0);
    headless.render();
    let frame = headless.render();
    assert_eq!(frame.pixel(4, HEIGHT - 4), Some((0xe9, 0xe9, 0xed)), "light sidebar");
    if let Some(dir) = &output {
        frame.write_png(&dir.join("settings-light.png")).expect("the screenshot is written");
    }
    app.window().hide().expect("the window hides");
}
