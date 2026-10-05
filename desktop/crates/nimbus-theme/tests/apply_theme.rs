// SPDX-License-Identifier: MIT

use nimbus_theme::headless::Headless;
use slint::Color;

// Compiling the gallery here also checks that the whole library works with the Rust code generator.
slint::slint! {
    // Relative to the workspace root, where Cargo runs rustc.
    #[library_path(nimbus) = "crates/nimbus-theme/ui"]
    import { Theme } from "@nimbus/theme.slint";
    import { Gallery } from "@nimbus/gallery.slint";

    export { Theme }

    export component Probe inherits Gallery { }
}

mod scoped {
    slint::slint! {
        export global Theme {
            in-out property <bool> dark: true;
            in-out property <color> accent;
            in-out property <length> corner-radius;
            in-out property <string> font-family;
            in-out property <length> font-size;
            in-out property <bool> animations;
        }

        export component Other inherits Window { }
    }
}

fn settings() -> nimbus_theme::ThemeSettings {
    nimbus_theme::ThemeSettings {
        dark: false,
        accent: (0xe0, 0x1b, 0x24),
        corner_radius: 6.0,
        font_family: "Cantarell".into(),
        font_size: 15.0,
        animations: false,
    }
}

#[test]
fn apply_theme_sets_every_input() {
    let _headless = Headless::install(1, 1).expect("no platform was set on this thread");

    let probe = Probe::new().expect("Probe instantiates");
    nimbus_theme::apply_theme!(probe, settings());
    let theme = slint::ComponentHandle::global::<Theme>(&probe);
    assert!(!theme.get_dark());
    assert_eq!(theme.get_accent(), Color::from_rgb_u8(0xe0, 0x1b, 0x24));
    assert_eq!(theme.get_corner_radius(), 6.0);
    assert_eq!(theme.get_font_family(), "Cantarell");
    assert_eq!(theme.get_font_size(), 15.0);
    assert!(!theme.get_animations());

    // The explicit form names a `Theme` that isn't in scope, and accepts a reference to the settings.
    let other = scoped::Other::new().expect("Other instantiates");
    let settings = settings();
    nimbus_theme::apply_theme!(other, &settings, scoped::Theme);
    let theme = slint::ComponentHandle::global::<scoped::Theme>(&other);
    assert!(!theme.get_dark());
    assert_eq!(theme.get_font_family(), "Cantarell");
}
