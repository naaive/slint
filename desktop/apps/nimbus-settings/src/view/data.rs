// SPDX-License-Identifier: MIT

//! Loading data from the [`Sources`](crate::sources::Sources) in the background, and showing it.

use std::rc::Rc;
use std::sync::mpsc;

use image::RgbaImage;
use nimbus_config::Appearance;
use nimbus_theme::ThemeSettings;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{Inner, Message, PickerKind, deliver, post};
use crate::about::SystemInfo;
use crate::{AboutModel, InfoRow, Prefs, WallpaperItem};

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
        self.start_displays();
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
            Message::Outputs(result) => self.handle_outputs(result),
            Message::Display(event) => self.handle_display(event),
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
}
