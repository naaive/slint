// SPDX-License-Identifier: MIT

//! The data behind the preferences panel and the shortcuts dialog.

use std::rc::Rc;

use slint::{Color, ComponentHandle, ModelRc, SharedString, VecModel};

use super::controller::Controller;
use crate::palette::{self, Scheme};
use crate::prefs::{self, CursorShape, Prefs, SCROLLBACK_CHOICES};
use crate::ui::{Preferences, SchemeInfo, ShortcutInfo};
use crate::{AppWindow, shortcuts};

fn color(c: alacritty_terminal::vte::ansi::Rgb) -> Color {
    Color::from_rgb_u8(c.r, c.g, c.b)
}

fn scheme_info(name: &str, scheme: &Scheme) -> SchemeInfo {
    let colors: Vec<Color> = [1, 2, 3, 4, 5, 6].iter().map(|&i| color(scheme.ansi[i])).collect();
    SchemeInfo {
        name: name.into(),
        background: color(scheme.background),
        foreground: color(scheme.foreground),
        colors: ModelRc::new(VecModel::from(colors)),
    }
}

/// The scheme previews; the first follows the desktop's light or dark preference.
pub(super) fn update_schemes(window: &AppWindow, desktop_dark: bool) {
    let schemes: Vec<SchemeInfo> = palette::scheme_ids()
        .map(|id| {
            let name = palette::scheme_name(id).unwrap_or(id);
            scheme_info(name, palette::resolve_scheme(id, desktop_dark))
        })
        .collect();
    window.global::<Preferences>().set_schemes(ModelRc::new(VecModel::from(schemes)));
}

fn scrollback_label(lines: usize) -> SharedString {
    if lines >= 1000 { format!("{}K", lines / 1000).into() } else { lines.to_string().into() }
}

/// Fills the preferences panel and the shortcuts dialog.
pub(super) fn init_preferences(window: &AppWindow, prefs: &Prefs) {
    let p = window.global::<Preferences>();
    update_schemes(window, window.global::<crate::ui::Theme>().get_dark());
    let choices: Vec<SharedString> =
        SCROLLBACK_CHOICES.iter().map(|&l| scrollback_label(l)).collect();
    p.set_scrollback_choices(ModelRc::new(VecModel::from(choices)));
    p.set_font_families(ModelRc::new(VecModel::from(vec![SharedString::from("Automatic")])));
    p.set_font_index(0);
    sync_preferences(window, prefs);
    let shortcuts: Vec<ShortcutInfo> = shortcuts::REFERENCE
        .iter()
        .map(|(keys, action)| ShortcutInfo { keys: (*keys).into(), action: (*action).into() })
        .collect();
    window.set_shortcuts(ModelRc::new(VecModel::from(shortcuts)));
}

/// Shows `prefs` in the panel.
pub(super) fn sync_preferences(window: &AppWindow, prefs: &Prefs) {
    let p = window.global::<Preferences>();
    let scheme_index = palette::scheme_ids().position(|id| id == prefs.color_scheme).unwrap_or(0);
    p.set_scheme_index(scheme_index as i32);
    p.set_font_size(prefs.font_size);
    p.set_cursor_shape(prefs.cursor_shape.index() as i32);
    p.set_cursor_blink(prefs.cursor_blink);
    p.set_scrollback_index(prefs::scrollback_choice(prefs.scrollback_lines) as i32);
    p.set_visual_bell(prefs.visual_bell);
    p.set_bold_is_bright(prefs.bold_is_bright);
}

/// Lists the installed monospace families after "Automatic", which shows the family in use.
pub(super) fn set_font_families(
    window: &AppWindow,
    families: &[String],
    in_use: &str,
    preferred: &str,
) {
    let p = window.global::<Preferences>();
    let mut entries = vec![SharedString::from(format!("Automatic ({in_use})"))];
    entries.extend(families.iter().map(SharedString::from));
    let index =
        families.iter().position(|f| f.eq_ignore_ascii_case(preferred)).map_or(0, |i| i + 1);
    p.set_font_families(ModelRc::new(VecModel::from(entries)));
    p.set_font_index(index as i32);
}

/// Routes the panel's changes to the controller.
pub(super) fn connect_preferences(controller: &Rc<Controller>, window: &AppWindow) {
    let p = window.global::<Preferences>();
    let weak = Rc::downgrade(controller);
    p.on_scheme_selected(move |index| {
        let id = usize::try_from(index).ok().and_then(|i| palette::scheme_ids().nth(i));
        if let (Some(c), Some(id)) = (weak.upgrade(), id) {
            c.update_prefs(|prefs| prefs.color_scheme = id.into());
        }
    });
    let weak = Rc::downgrade(controller);
    let window_weak = window.as_weak();
    p.on_font_selected(move |index| {
        let (Some(c), Some(window)) = (weak.upgrade(), window_weak.upgrade()) else { return };
        let family = match usize::try_from(index) {
            Ok(0) | Err(_) => String::new(),
            Ok(i) => {
                use slint::Model as _;
                window
                    .global::<Preferences>()
                    .get_font_families()
                    .row_data(i)
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            }
        };
        c.update_prefs(|prefs| prefs.font_family = family);
    });
    let weak = Rc::downgrade(controller);
    p.on_font_size_step(move |steps| {
        if let Some(c) = weak.upgrade() {
            c.update_prefs(|prefs| {
                prefs.font_size = if steps == 0 {
                    Prefs::default().font_size
                } else {
                    prefs.font_size.round() + steps as f32
                };
            });
        }
    });
    let weak = Rc::downgrade(controller);
    p.on_cursor_shape_selected(move |index| {
        if let Some(c) = weak.upgrade() {
            c.update_prefs(|prefs| prefs.cursor_shape = CursorShape::from_index(index));
        }
    });
    let weak = Rc::downgrade(controller);
    p.on_cursor_blink_toggled(move |on| {
        if let Some(c) = weak.upgrade() {
            c.update_prefs(|prefs| prefs.cursor_blink = on);
        }
    });
    let weak = Rc::downgrade(controller);
    p.on_scrollback_selected(move |index| {
        let lines = usize::try_from(index).ok().and_then(|i| SCROLLBACK_CHOICES.get(i).copied());
        if let (Some(c), Some(lines)) = (weak.upgrade(), lines) {
            c.update_prefs(|prefs| prefs.scrollback_lines = lines);
        }
    });
    let weak = Rc::downgrade(controller);
    p.on_visual_bell_toggled(move |on| {
        if let Some(c) = weak.upgrade() {
            c.update_prefs(|prefs| prefs.visual_bell = on);
        }
    });
    let weak = Rc::downgrade(controller);
    p.on_bold_is_bright_toggled(move |on| {
        if let Some(c) = weak.upgrade() {
            c.update_prefs(|prefs| prefs.bold_is_bright = on);
        }
    });
}
