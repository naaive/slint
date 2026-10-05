// SPDX-License-Identifier: MIT

//! Renders the window with sample tabs on the software renderer and checks that it draws.
//! Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/terminal-{main,light,preferences}.png`.
//!
//! Slint's platform can be set once per process, so this binary has a single test.

use std::collections::HashSet;
use std::path::PathBuf;

use nimbus_terminal::screenshot::{self, Frame, HEIGHT, WIDTH};
use slint::ComponentHandle;

fn distinct_colors(frame: &Frame) -> usize {
    frame.pixels.iter().map(|p| (p.r, p.g, p.b)).collect::<HashSet<_>>().len()
}

#[test]
fn window_renders() {
    let platform = screenshot::install_platform().expect("the platform is set once");
    let output = std::env::var_os("NIMBUS_UPDATE_SCREENSHOTS")
        .map(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots"));

    for (light, name, background) in
        [(false, "main", (0x1c, 0x1c, 0x20)), (true, "light", (0xfc, 0xfc, 0xfd))]
    {
        let Ok((window, controller)) = screenshot::sample_app(light) else {
            // Without any installed font there's no text to draw.
            return;
        };
        assert_eq!(controller.tab_count(), 3);
        screenshot::render(&platform);
        controller.update_geometry();
        controller.render_now();
        let frame = screenshot::render(&platform);
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        // The terminal's background fills the bottom-left corner, below the header bar.
        assert_eq!(frame.pixel(2, HEIGHT - 2), Some(background), "{name} terminal background");
        assert!(distinct_colors(&frame) > 300, "{name} looks blank");
        assert_eq!(window.get_window_title(), "ada@nimbus: ~/projects/nimbus");
        if let Some(dir) = &output {
            frame
                .write_png(&dir.join(format!("terminal-{name}.png")))
                .expect("the screenshot is written");
        }

        if !light {
            window.set_preferences_open(true);
            screenshot::render(&platform);
            let frame = screenshot::render(&platform);
            assert_ne!(
                frame.pixel(WIDTH - 60, 200),
                Some(background),
                "the preferences cover the terminal"
            );
            if let Some(dir) = &output {
                frame
                    .write_png(&dir.join("terminal-preferences.png"))
                    .expect("the screenshot is written");
            }
            window.set_preferences_open(false);
        }
        window.hide().expect("the window hides");
    }
}
