// SPDX-License-Identifier: MIT

//! Headless rendering of the window with sample data, for `--screenshot` and the screenshot test.
//!
//! Slint's platform can be set once per thread, so [`install_platform`] must run before any window exists.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nimbus_config::{ColorScheme, Config};
use nimbus_theme::headless::Headless;
use slint::ComponentHandle;

use crate::dispatch::Dispatch;
use crate::page::Page;
use crate::sources::SampleSources;
use crate::view::{App, AppOptions, SystemScheme};

pub const WIDTH: u32 = 1100;
pub const HEIGHT: u32 = 720;

/// Makes Slint render into an off-screen window of the screenshot size.
pub fn install_platform() -> anyhow::Result<Headless> {
    Ok(Headless::install(WIDTH, HEIGHT)?)
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

/// Creates the app with sample data on `page`, editing a configuration file in `dir`.
pub fn sample_app(dir: &Path, page: Page, light: bool) -> anyhow::Result<App> {
    let config_path = dir.join("config.toml");
    sample_config(light).save_to(&config_path)?;
    let app = App::new(AppOptions {
        config_path,
        page,
        sources: Arc::new(SampleSources::default()),
        dispatch: Dispatch::Inline,
        system_scheme: SystemScheme::Fixed { dark: !light },
        watch: false,
        debounce: Duration::from_millis(50),
    })?;
    Ok(app)
}

/// Renders `page` with sample data into the PNG file at `path`; for `--screenshot`.
pub fn run(path: &Path, page: Page, light: bool) -> anyhow::Result<()> {
    let headless = install_platform()?;
    let dir =
        std::env::temp_dir().join(format!("nimbus-settings-screenshot-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = (|| {
        let app = sample_app(&dir, page, light)?;
        app.window().show()?;
        // The first pass lays out; the second draws images that arrived during it.
        headless.render();
        let frame = headless.render();
        app.window().hide()?;
        Ok(frame.write_png(path)?)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}
