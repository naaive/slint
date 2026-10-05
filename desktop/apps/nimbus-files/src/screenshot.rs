// SPDX-License-Identifier: MIT

//! Renders the main window with deterministic sample data, using the software renderer.
//!
//! Slint's platform can be set only once per thread, so call [`render`] at most once per thread.

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use nimbus_theme::headless::{Frame, Headless};
use slint::{ComponentHandle as _, Model as _};

use crate::app::{Controller, Env};
use crate::{AppWindow, Theme};

pub const WIDTH: u32 = 1100;
pub const HEIGHT: u32 = 720;

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub light: bool,
    pub list: bool,
    pub scene: Scene,
}

/// What the window shows besides the folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scene {
    #[default]
    Main,
    /// The context menu of a picture.
    Menu,
    Properties,
    Rename,
    Search,
    Toast,
    /// The trash with two items.
    Trash,
}

impl std::str::FromStr for Scene {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(match name {
            "main" => Scene::Main,
            "menu" => Scene::Menu,
            "properties" => Scene::Properties,
            "rename" => Scene::Rename,
            "search" => Scene::Search,
            "toast" => Scene::Toast,
            "trash" => Scene::Trash,
            other => return Err(format!("unknown scene {other}")),
        })
    }
}

/// Handles worker results and redraws until nothing arrives for a while; drawing requests thumbnails.
fn settle(controller: &Rc<Controller>, headless: &Headless, limit: Duration) {
    let deadline = Instant::now() + limit;
    let mut quiet_rounds = 0;
    while Instant::now() < deadline && quiet_rounds < 8 {
        let handled = controller.pump();
        headless.draw();
        let busy =
            handled > 0 || controller.ui().get_loading() || controller.ui().get_props_computing();
        quiet_rounds = if busy { 0 } else { quiet_rounds + 1 };
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Renders the window and writes it to `path` as PNG.
pub fn render_to_file(path: &Path, options: Options) -> anyhow::Result<()> {
    Ok(render(options)?.write_png(path)?)
}

/// Renders the window showing a sample home folder.
pub fn render(options: Options) -> anyhow::Result<Frame> {
    let headless = Headless::install(WIDTH, HEIGHT)?;

    let sample = tempfile::Builder::new().prefix("nimbus-files-sample").tempdir()?;
    let home = sample.path().join("ada");
    create_sample_home(&home)?;
    let env = Env {
        home: home.clone(),
        config_dir: home.join(".config"),
        data_dir: home.join(".local/share"),
        cache_dir: sample.path().join("cache"),
        prefs_path: None,
        mountinfo: sample.path().join("mountinfo"),
        live_updates: false,
    };

    let ui = AppWindow::new()?;
    let settings = nimbus_theme::ThemeSettings {
        dark: !options.light,
        animations: false,
        ..Default::default()
    };
    nimbus_theme::apply_theme!(ui, settings);
    let controller = Controller::new(ui.clone_strong(), env, Some(home.clone()));
    if options.list {
        ui.invoke_set_list_mode(true);
    }
    ui.show()?;

    settle(&controller, &headless, Duration::from_secs(10));
    for name in ["Mountains.png", "Holiday.jpg"] {
        if let Some(index) = (0..ui.get_files().row_count())
            .find(|&i| ui.get_files().row_data(i).is_some_and(|f| f.name == name))
        {
            ui.invoke_item_pressed(index as i32, true, false);
        }
    }
    ui.set_status_detail("48.2 GB free".into());
    let find = |name: &str| {
        (0..ui.get_files().row_count())
            .find(|&i| ui.get_files().row_data(i).is_some_and(|f| f.name == name))
    };
    match options.scene {
        Scene::Main => {}
        Scene::Menu => {
            if let Some(index) = find("Mountains.png") {
                ui.invoke_item_context(index as i32, 760.0, 250.0);
            }
        }
        Scene::Properties => {
            if let Some(index) = find("Mountains.png") {
                ui.invoke_item_pressed(index as i32, false, false);
                ui.invoke_window_key(
                    char::from(slint::platform::Key::Return).to_string().into(),
                    false,
                    false,
                    true,
                );
            }
        }
        Scene::Rename => {
            if let Some(index) = find("Quarterly Report.pdf") {
                ui.invoke_item_pressed(index as i32, false, false);
                ui.invoke_view_key(
                    char::from(slint::platform::Key::F2).to_string().into(),
                    false,
                    false,
                    false,
                );
            }
        }
        Scene::Search => {
            ui.invoke_toggle_search();
            ui.set_search_text("o".into());
            ui.invoke_search_edited("o".into());
        }
        Scene::Toast => {
            ui.set_toast_text("“Notes.md” moved to the Trash".into());
            ui.set_toast_action("Undo".into());
            ui.set_toast_visible(true);
        }
        Scene::Trash => {
            let delete = char::from(slint::platform::Key::Delete).to_string();
            for name in ["Notes.md", "Trailer.mp4"] {
                if let Some(index) = find(name) {
                    ui.invoke_item_pressed(index as i32, false, false);
                    ui.invoke_view_key(delete.as_str().into(), false, false, false);
                    settle(&controller, &headless, Duration::from_secs(5));
                }
            }
            let places = ui.get_places();
            if let Some(trash) = (0..places.row_count())
                .find(|&i| places.row_data(i).is_some_and(|p| p.label == "Trash"))
            {
                ui.invoke_place_clicked(trash as i32);
            }
            ui.invoke_toast_dismissed();
        }
    }
    settle(&controller, &headless, Duration::from_secs(5));
    ui.set_status_detail("48.2 GB free".into());
    let frame = headless.draw();
    ui.hide()?;
    drop(controller);
    Ok(frame)
}

/// A fixed time, so dates in the screenshot don't depend on when it's taken.
fn sample_time(days_ago: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_251_200 - days_ago * 86_400)
}

fn write(path: &Path, content: &[u8], days_ago: u64) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    std::fs::File::options().write(true).open(path)?.set_modified(sample_time(days_ago))?;
    Ok(())
}

fn create_sample_home(home: &Path) -> anyhow::Result<()> {
    for dir in [
        "Desktop",
        "Documents",
        "Downloads",
        "Music",
        "Pictures",
        "Videos",
        "Projects",
        ".config/gtk-3.0",
    ] {
        std::fs::create_dir_all(home.join(dir))?;
    }
    for (dir, count) in [
        ("Documents", 12),
        ("Downloads", 5),
        ("Music", 31),
        ("Pictures", 48),
        ("Videos", 3),
        ("Projects", 7),
    ] {
        for i in 0..count {
            write(&home.join(dir).join(format!("item-{i:02}")), b"", 3)?;
        }
    }
    write(
        &home.join(".config/gtk-3.0/bookmarks"),
        format!("file://{}/Projects\n", home.display()).as_bytes(),
        1,
    )?;
    let mountinfo = "22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw\n\
                     90 22 8:17 / /run/media/ada/Backup\\040Drive rw,nosuid shared:300 - exfat /dev/sdb1 rw\n";
    if let Some(parent) = home.parent() {
        std::fs::write(parent.join("mountinfo"), mountinfo)?;
    }
    write(&home.join("Mountains.png"), &landscape_png(640, 400, false)?, 2)?;
    write(&home.join("Holiday.jpg"), &landscape_jpeg(640, 420)?, 9)?;
    write(&home.join("Budget 2024.ods"), &[0u8; 18_432], 12)?;
    write(&home.join("Notes.md"), b"# Notes\n\nCall the plumber.\n", 0)?;
    write(&home.join("Quarterly Report.pdf"), &vec![0u8; 1_284_000], 30)?;
    write(&home.join("photos-backup.tar.gz"), &vec![0u8; 52_000_000 / 16], 45)?;
    write(&home.join("Theme Song.flac"), &vec![0u8; 3_400_000], 60)?;
    write(&home.join("Trailer.mp4"), &vec![0u8; 8_800_000], 61)?;
    write(&home.join("install.sh"), b"#!/bin/sh\necho hi\n", 90)?;
    write(&home.join("logo.svg"), LOGO_SVG.as_bytes(), 5)?;
    write(&home.join(".bashrc"), b"", 400)?;
    let names = ["Desktop", "Documents", "Downloads", "Music", "Pictures", "Videos", "Projects"];
    for (i, name) in names.iter().enumerate() {
        let file = std::fs::File::open(home.join(name))?;
        file.set_modified(sample_time(i as u64 + 1))?;
    }
    Ok(())
}

const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#62a0ea"/><stop offset="1" stop-color="#1c71d8"/></linearGradient></defs>
<rect x="16" y="16" width="224" height="224" rx="56" fill="url(#g)"/>
<path d="M72 176c0-40 24-72 56-72s56 32 56 72" fill="none" stroke="#fff" stroke-width="20" stroke-linecap="round"/>
<circle cx="128" cy="84" r="20" fill="#fff"/></svg>"##;

/// A procedural mountain scene, so the screenshot has real thumbnails.
fn landscape(width: u32, height: u32, sunset: bool) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        let (fx, fy) = (x as f32 / width as f32, y as f32 / height as f32);
        let sky = if sunset {
            lerp([255.0, 170.0, 90.0], [120.0, 60.0, 140.0], 1.0 - fy * 1.6)
        } else {
            lerp([190.0, 222.0, 255.0], [60.0, 120.0, 200.0], 1.0 - fy * 1.4)
        };
        let ridge_far = 0.45 + 0.12 * ((fx * 9.0).sin() * 0.5 + (fx * 23.0).sin() * 0.2);
        let ridge_near = 0.62 + 0.1 * ((fx * 5.0 + 1.0).sin() * 0.6 + (fx * 17.0).cos() * 0.15);
        let color = if fy > 0.85 && sunset {
            lerp([40.0, 60.0, 110.0], [20.0, 30.0, 70.0], (fy - 0.85) / 0.15)
        } else if fy > ridge_near {
            if sunset { [70.0, 40.0, 80.0] } else { [46.0, 110.0, 70.0] }
        } else if fy > ridge_far {
            if sunset { [130.0, 70.0, 110.0] } else { [92.0, 120.0, 160.0] }
        } else if sunset && ((fx - 0.7).powi(2) + (fy - 0.42).powi(2)).sqrt() < 0.09 {
            [255.0, 230.0, 160.0]
        } else {
            sky
        };
        image::Rgb(color.map(|c| c.clamp(0.0, 255.0) as u8))
    })
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn landscape_png(width: u32, height: u32, sunset: bool) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    landscape(width, height, sunset)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

fn landscape_jpeg(width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    landscape(width, height, true)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)?;
    Ok(bytes)
}
