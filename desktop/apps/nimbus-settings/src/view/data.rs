// SPDX-License-Identifier: MIT

//! Loading data from the [`Sources`](crate::sources::Sources) in the background, and showing it.

use std::rc::Rc;
use std::sync::mpsc;

use image::RgbaImage;
use nimbus_config::Appearance;
use nimbus_theme::ThemeSettings;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{Inner, Message, PickerKind, With, deliver, post};
use crate::about::SystemInfo;
use crate::{
    AboutModel, AppWindow, DisplayItem, DisplaysModel, InfoRow, Prefs, WallpaperItem, displays,
};

fn pixels(image: RgbaImage) -> SharedPixelBuffer<Rgba8Pixel> {
    SharedPixelBuffer::clone_from_slice(image.as_raw(), image.width(), image.height())
}

/// Starts the thread that resolves themes, which may wait on the desktop portal.
///
/// Requests that queue up while one resolves are coalesced into the newest.
pub(super) fn start_theme_worker() -> Option<mpsc::Sender<Appearance>> {
    let (sender, receiver) = mpsc::channel::<Appearance>();
    let spawned =
        std::thread::Builder::new().name("nimbus-settings-theme".into()).spawn(move || {
            while let Ok(mut appearance) = receiver.recv() {
                while let Ok(newer) = receiver.try_recv() {
                    appearance = newer;
                }
                post(Message::Theme(ThemeSettings::from_config(&appearance)));
            }
        });
    match spawned {
        Ok(_) => Some(sender),
        Err(error) => {
            tracing::error!("cannot start the theme thread: {error}");
            None
        }
    }
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let h = with.clone();
    ui.global::<DisplaysModel>().on_refresh(move || h(&|i| i.load_displays()));
}

/// The arrangement preview's rectangles: outputs side by side, vertically centered, with small gaps.
pub(crate) fn arrange(rows: &[displays::DisplayRow]) -> (Vec<(f32, f32, f32, f32)>, f32) {
    let tallest = rows.iter().map(|r| r.logical_height).fold(0.0_f32, f32::max);
    if rows.is_empty() || tallest <= 0.0 {
        return (vec![(0.0, 0.0, 0.0, 0.0); rows.len()], 1.0);
    }
    let gap = tallest * 0.04;
    let total = rows.iter().map(|r| r.logical_width).sum::<f32>() + gap * (rows.len() - 1) as f32;
    let mut x = 0.0;
    let rects = rows
        .iter()
        .map(|r| {
            let rect = (
                x / total,
                (tallest - r.logical_height) / 2.0 / tallest,
                r.logical_width / total,
                r.logical_height / tallest,
            );
            x += r.logical_width + gap;
            rect
        })
        .collect();
    (rects, total / tallest)
}

impl Inner {
    /// Starts every background load.
    pub fn load_data(&self) {
        let sources = self.sources.clone();
        self.dispatch.run(
            "nimbus-settings-fonts",
            move || sources.font_families(),
            |f| deliver(Message::Fonts(f)),
        );
        let sources = self.sources.clone();
        self.dispatch.run(
            "nimbus-settings-xkb",
            move || sources.xkb_rules(),
            |r| deliver(Message::Rules(r)),
        );
        let sources = self.sources.clone();
        self.dispatch.run(
            "nimbus-settings-about",
            move || {
                let info = sources.system_info();
                let logo = sources.logo(&info).map(pixels);
                (info, logo)
            },
            |(info, logo)| deliver(Message::About(Box::new(info), logo)),
        );
        let sources = self.sources.clone();
        self.dispatch.stream(
            "nimbus-settings-wallpapers",
            move |emit: &dyn Fn(Message)| {
                let wallpapers = sources.wallpapers();
                let paths: Vec<_> = wallpapers.iter().map(|w| w.path.clone()).collect();
                emit(Message::Wallpapers(wallpapers));
                for path in paths {
                    if let Some(thumbnail) = sources.thumbnail(&path) {
                        emit(Message::Thumbnail(
                            path.to_string_lossy().into_owned(),
                            pixels(thumbnail),
                        ));
                    }
                }
            },
            deliver,
        );
        self.load_displays();
    }

    pub fn load_displays(&self) {
        {
            let mut state = self.state.borrow_mut();
            if state.displays_loading {
                return;
            }
            state.displays_loading = true;
        }
        self.with_ui(|ui| ui.global::<DisplaysModel>().set_state(0));
        let sources = self.sources.clone();
        self.dispatch.run(
            "nimbus-settings-displays",
            move || sources.outputs(),
            |o| deliver(Message::Outputs(o)),
        );
    }

    pub(super) fn handle_data(&self, message: Message) {
        match message {
            Message::Fonts(fonts) => {
                self.state.borrow_mut().fonts = fonts;
                self.refresh_picker(PickerKind::Font);
            }
            Message::Rules(rules) => {
                self.state.borrow_mut().rules = Some(Rc::new(rules));
                self.state.borrow_mut().pushed_sources.clear();
                self.sync_prefs();
                self.refresh_picker(PickerKind::Layout);
            }
            Message::Wallpapers(wallpapers) => {
                let items: Vec<WallpaperItem> = wallpapers
                    .iter()
                    .map(|w| WallpaperItem {
                        path: w.path.to_string_lossy().as_ref().into(),
                        name: w.name.as_str().into(),
                        thumbnail: Image::default(),
                    })
                    .collect();
                self.wallpaper_model.set_vec(items);
                self.state.borrow_mut().wallpapers = wallpapers;
                self.with_ui(|ui| ui.global::<Prefs>().set_wallpapers_loading(false));
            }
            Message::Thumbnail(path, buffer) => {
                let model = &self.wallpaper_model;
                if let Some(row) = (0..model.row_count())
                    .find(|&r| model.row_data(r).is_some_and(|w| w.path == path.as_str()))
                    && let Some(mut item) = model.row_data(row)
                {
                    item.thumbnail = Image::from_rgba8(buffer);
                    model.set_row_data(row, item);
                }
            }
            Message::About(info, logo) => self.show_about(&info, logo),
            Message::Outputs(result) => {
                self.state.borrow_mut().displays_loading = false;
                self.show_outputs(result);
            }
            other @ (Message::ExternalConfig(_) | Message::Saved(_) | Message::Theme(_)) => {
                self.handle(other)
            }
        }
    }

    fn show_about(&self, info: &SystemInfo, logo: Option<SharedPixelBuffer<Rgba8Pixel>>) {
        let rows = |pairs: &[(&str, &str)]| {
            let rows: Vec<InfoRow> = pairs
                .iter()
                .map(|(label, value)| InfoRow { label: (*label).into(), value: (*value).into() })
                .collect();
            ModelRc::new(VecModel::from(rows))
        };
        self.with_ui(|ui| {
            let about = ui.global::<AboutModel>();
            about.set_os_name(info.os_name.as_str().into());
            if let Some(brand) = info.os_color.as_deref().and_then(nimbus_theme::parse_hex_color) {
                about.set_brand(slint::Color::from_rgb_u8(brand.0, brand.1, brand.2));
            }
            if let Some(logo) = logo {
                about.set_logo(Image::from_rgba8(logo));
            }
            about.set_hardware(rows(&[
                ("Device name", &info.hostname),
                ("Processor", &info.cpu),
                ("Memory", &info.memory),
                ("Graphics", &info.graphics),
                ("Disk capacity", &info.disk),
            ]));
            about.set_software(rows(&[
                ("Operating system", &info.os_name),
                ("Desktop", &info.desktop),
                ("Windowing system", &info.windowing),
                ("Kernel", &info.kernel),
            ]));
        });
    }

    fn show_outputs(&self, result: Result<Vec<nimbus_ipc::OutputInfo>, String>) {
        self.with_ui(|ui| {
            let model = ui.global::<DisplaysModel>();
            match result {
                Ok(outputs) if !outputs.is_empty() => {
                    let rows = displays::rows(&outputs);
                    let (rects, aspect) = arrange(&rows);
                    let items: Vec<DisplayItem> = rows
                        .iter()
                        .zip(rects)
                        .map(|(r, (x, y, w, h))| DisplayItem {
                            name: r.name.as_str().into(),
                            resolution: r.resolution.as_str().into(),
                            refresh: r.refresh.as_str().into(),
                            scale: r.scale.as_str().into(),
                            frac_x: x,
                            frac_y: y,
                            frac_width: w,
                            frac_height: h,
                        })
                        .collect();
                    model.set_items(ModelRc::new(VecModel::from(items)));
                    model.set_arrangement_aspect(aspect);
                    model.set_state(1);
                }
                Ok(_) => {
                    model.set_message("The compositor reports no connected displays.".into());
                    model.set_state(2);
                }
                Err(error) => {
                    let mut message = error;
                    if let Some(first) = message.get(..1) {
                        message = first.to_uppercase() + &message[1..];
                    }
                    model.set_message(
                        format!(
                            "{message}. Open Settings from a Nimbus session to see your displays."
                        )
                        .into(),
                    );
                    model.set_state(2);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::DisplayRow;

    fn row(w: f32, h: f32) -> DisplayRow {
        DisplayRow {
            name: String::new(),
            resolution: String::new(),
            refresh: String::new(),
            scale: String::new(),
            logical_width: w,
            logical_height: h,
        }
    }

    #[test]
    fn arrangement_is_normalized() {
        let (rects, aspect) = arrange(&[row(1440.0, 900.0), row(2560.0, 1440.0)]);
        let gap = 1440.0 * 0.04;
        let total = 1440.0 + 2560.0 + gap;
        assert!((aspect - total / 1440.0).abs() < 1e-4);
        assert_eq!(rects[0].0, 0.0);
        assert!((rects[0].1 - 270.0 / 1440.0).abs() < 1e-5, "smaller output is centered");
        assert!((rects[1].0 + rects[1].2 - 1.0).abs() < 1e-5, "last output ends at the right edge");
        assert_eq!(rects[1].3, 1.0);
        assert_eq!(arrange(&[]).1, 1.0);
        assert_eq!(arrange(&[row(0.0, 0.0)]).0, [(0.0, 0.0, 0.0, 0.0)]);
    }
}
