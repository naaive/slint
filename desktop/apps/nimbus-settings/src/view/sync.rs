// SPDX-License-Identifier: MIT

//! Pushing the configuration into the UI, and the edits that need more than one key.

use std::rc::Rc;

use nimbus_config::{Config, PanelPosition};
use nimbus_theme::ThemeSettings;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::{Inner, SystemScheme, index_of, strings};
use crate::settings::{self, ACCENTS, Key, Value};
use crate::{AccentSwatch, InputSource, Nav, OptionGroup, Prefs, SearchHit, clock, search, xkb};

fn color(hex: &str) -> slint::Color {
    let (r, g, b) = nimbus_theme::parse_hex_color(hex).unwrap_or(nimbus_theme::DEFAULT_ACCENT);
    slint::Color::from_rgb_u8(r, g, b)
}

/// Sets a text property unless it already says the same, so typing in a bound field isn't disturbed.
fn set_text_if_changed(current: SharedString, wanted: &str, set: impl FnOnce(SharedString)) {
    if current.as_str() != wanted {
        set(wanted.into());
    }
}

impl Inner {
    pub fn sync_all(&self) {
        self.sync_prefs();
        self.sync_shortcuts();
    }

    /// Pushes every setting except the shortcuts.
    pub fn sync_prefs(&self) {
        let config = self.store.borrow().config().clone();
        let Some(ui) = self.ui.upgrade() else { return };
        let prefs = ui.global::<Prefs>();
        let a = &config.appearance;

        prefs.set_color_scheme(settings::color_scheme_index(a.color_scheme));
        if prefs.get_accents().row_count() == 0 {
            let swatches: Vec<AccentSwatch> = ACCENTS
                .iter()
                .map(|(name, hex)| AccentSwatch {
                    name: (*name).into(),
                    hex: (*hex).into(),
                    color: color(hex),
                })
                .collect();
            prefs.set_accents(ModelRc::new(VecModel::from(swatches)));
        }
        let shown_accent = nimbus_theme::parse_hex_color(&prefs.get_accent());
        if shown_accent != nimbus_theme::parse_hex_color(&a.accent) {
            prefs.set_accent(a.accent.as_str().into());
        }
        prefs.set_accent_is_custom(!ACCENTS.iter().any(|(_, hex)| *hex == a.accent));
        let wallpaper =
            a.wallpaper.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        prefs.set_wallpaper(wallpaper.into());
        prefs.set_font_family(a.font_family.as_str().into());
        prefs.set_font_size(a.font_size);
        prefs.set_scale_index(settings::scale_index(a.scale));
        prefs.set_animations(a.animations);
        prefs.set_corner_radius(a.corner_radius);

        let panel = &config.panel;
        prefs.set_panel_position(i32::from(panel.position == PanelPosition::Bottom));
        prefs.set_panel_height(panel.height as f32);
        prefs.set_show_dock(panel.show_dock);
        prefs.set_dock_autohide(panel.dock_autohide);
        prefs.set_battery_percentage(panel.show_battery_percentage);
        self.sync_clock(&prefs, &config);

        let w = &config.workspaces;
        prefs.set_workspace_count(w.count as i32);
        prefs.set_window_layout(i32::from(w.layout == nimbus_config::DefaultLayout::Tiling));
        prefs.set_gaps(w.gaps as f32);
        prefs.set_focus_follows_mouse(w.focus_follows_mouse);

        self.sync_input(&prefs, &config);

        let p = &config.power;
        let lock = settings::timeout_choices(p.lock_after_minutes);
        prefs.set_lock_index(
            lock.iter().position(|(v, _)| *v == p.lock_after_minutes).unwrap_or(0) as i32,
        );
        {
            let mut state = self.state.borrow_mut();
            let lock_values: Vec<u32> = lock.iter().map(|(v, _)| *v).collect();
            if state.lock_values != lock_values || prefs.get_lock_choices().row_count() == 0 {
                prefs.set_lock_choices(strings(lock.into_iter().map(|(_, label)| label)));
                state.lock_values = lock_values;
            }
        }

        self.request_theme(&config);
    }

    fn sync_clock(&self, prefs: &Prefs<'_>, config: &Config) {
        let format = &config.panel.clock_format;
        let now = self.sources.now();
        if prefs.get_clock_presets().row_count() == 0 {
            let labels = clock::PRESETS
                .iter()
                .map(|(f, label)| clock::preview(f, now).unwrap_or_else(|| (*label).to_string()))
                .chain(["Custom".to_string()]);
            prefs.set_clock_presets(strings(labels));
        }
        let custom = self.state.borrow().custom_clock;
        let index = if custom { clock::PRESETS.len() } else { clock::preset_index(format) };
        prefs.set_clock_preset_index(index as i32);
        let typed = prefs.get_clock_format();
        if !custom || clock::is_valid_format(&typed) {
            set_text_if_changed(typed, format, |t| prefs.set_clock_format(t));
        }
        prefs.set_clock_valid(clock::is_valid_format(&prefs.get_clock_format()));
        prefs.set_clock_preview(clock::preview(format, now).unwrap_or_default().into());
        prefs.set_clock_24_hour(clock::is_24_hour(format));
        self.show_time();
    }

    fn sync_input(&self, prefs: &Prefs<'_>, config: &Config) {
        let input = &config.input;
        let rules =
            self.state.borrow().rules.clone().unwrap_or_else(|| Rc::new(xkb::Rules::builtin()));
        let sources = xkb::sources(&input.keyboard_layout, &input.keyboard_variant);
        let shown: Vec<(String, String, bool)> = sources
            .iter()
            .map(|s| {
                (
                    rules.layout_description(&s.layout),
                    rules.variant_description(&s.layout, &s.variant),
                    !rules.variants_of(&s.layout).is_empty(),
                )
            })
            .collect();
        let options: Vec<usize> =
            xkb::OPTION_GROUPS.iter().map(|g| g.current(&input.keyboard_options)).collect();
        let mut state = self.state.borrow_mut();
        if state.pushed_sources != shown || prefs.get_input_sources().row_count() == 0 {
            let items: Vec<InputSource> = shown
                .iter()
                .map(|(title, subtitle, can)| InputSource {
                    title: title.as_str().into(),
                    subtitle: subtitle.as_str().into(),
                    can_choose_variant: *can,
                })
                .collect();
            prefs.set_input_sources(ModelRc::new(VecModel::from(items)));
            state.pushed_sources = shown;
        }
        prefs.set_can_add_source(sources.len() < xkb::MAX_SOURCES);
        if state.pushed_options != options || prefs.get_option_groups().row_count() == 0 {
            let groups: Vec<OptionGroup> = xkb::OPTION_GROUPS
                .iter()
                .zip(&options)
                .map(|(group, current)| OptionGroup {
                    title: group.title.into(),
                    choices: strings(group.choices.iter().map(|(label, _)| (*label).to_string())),
                    current: *current as i32,
                })
                .collect();
            prefs.set_option_groups(ModelRc::new(VecModel::from(groups)));
            state.pushed_options = options;
        }
        drop(state);
        set_text_if_changed(prefs.get_keyboard_options(), &input.keyboard_options, |t| {
            prefs.set_keyboard_options(t)
        });
        prefs.set_repeat_delay(input.repeat_delay_ms as f32);
        prefs.set_repeat_rate(input.repeat_rate as f32);
        prefs.set_natural_scroll(input.natural_scroll);
        prefs.set_tap_to_click(input.tap_to_click);
        prefs.set_pointer_speed(input.pointer_speed as f32);
    }

    pub fn sync_search(&self, query: &str) {
        let hits: Vec<SearchHit> = search::search(query)
            .into_iter()
            .map(|e| SearchHit {
                page: e.page.index() as i32,
                title: e.title.into(),
                page_title: e.page.title().into(),
            })
            .collect();
        self.with_ui(|ui| {
            ui.global::<Nav>().set_search_results(ModelRc::new(VecModel::from(hits)))
        });
    }

    /// Resolves and applies the theme when the appearance changed.
    fn request_theme(&self, config: &Config) {
        let appearance = config.appearance.clone();
        {
            let mut state = self.state.borrow_mut();
            if state.applied_appearance.as_ref() == Some(&appearance) {
                return;
            }
            state.applied_appearance = Some(appearance.clone());
        }
        match (&self.theme_requests, self.system_scheme) {
            (Some(requests), _) => {
                if requests.send(appearance).is_err() {
                    tracing::warn!("the theme worker stopped");
                }
            }
            (None, SystemScheme::Fixed { dark }) => {
                let theme = ThemeSettings::from_config_with_system(&appearance, || dark);
                self.with_ui(|ui| nimbus_theme::apply_theme!(ui, theme, crate::Theme));
            }
            (None, SystemScheme::Portal) => {
                self.dispatch.run(
                    "nimbus-settings-theme",
                    move || ThemeSettings::from_config(&appearance),
                    |theme| super::deliver(super::Message::Theme(theme)),
                );
            }
        }
    }

    pub fn choose_clock_preset(&self, index: i32) {
        let index = index_of(index);
        match clock::PRESETS.get(index) {
            Some((format, _)) => {
                self.state.borrow_mut().custom_clock = false;
                self.edit(Key::ClockFormat.name(), Value::Text((*format).into()));
                self.sync_prefs();
            }
            None => {
                self.state.borrow_mut().custom_clock = true;
                self.sync_prefs();
            }
        }
    }

    pub fn choose_timeout(&self, key: &str, index: i32) {
        let values = match key.parse::<Key>() {
            Ok(Key::LockAfter) => self.state.borrow().lock_values.clone(),
            _ => {
                tracing::error!("'{key}' isn't a timeout setting");
                return;
            }
        };
        if let Some(minutes) = values.get(index_of(index)) {
            self.edit(key, Value::Int(i64::from(*minutes)));
        }
    }

    pub fn choose_option(&self, group: i32, index: i32) {
        let Some(group) = xkb::OPTION_GROUPS.get(index_of(group)) else { return };
        let options =
            group.select(&self.store.borrow().config().input.keyboard_options, index_of(index));
        self.edit(Key::KeyboardOptions.name(), Value::Text(options));
    }

    /// Edits the input sources; `edit` returns whether it changed anything.
    pub fn edit_sources(&self, edit: impl FnOnce(&mut Vec<xkb::Source>) -> bool) {
        let input = self.store.borrow().config().input.clone();
        let mut sources = xkb::sources(&input.keyboard_layout, &input.keyboard_variant);
        if sources.is_empty() {
            sources.push(xkb::Source { layout: "us".into(), variant: String::new() });
        }
        if !edit(&mut sources) {
            return;
        }
        let (layout, variant) = xkb::join(&sources);
        self.update(|config| {
            config.input.keyboard_layout = layout;
            config.input.keyboard_variant = variant;
        });
    }
}
