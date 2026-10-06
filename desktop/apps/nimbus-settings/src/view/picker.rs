// SPDX-License-Identifier: MIT

//! The searchable picker dialog for fonts, keyboard layouts, layout variants, and time zones.

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Inner, With, to_index};
use crate::settings::{Key, Value};
use crate::{AppWindow, Picker, PickerItem, search, xkb};

/// The most entries shown at once; a search narrows longer lists.
const MAX_SHOWN: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    Font,
    Layout,
    /// The variant of the input source at this index.
    Variant(usize),
    Timezone,
}

/// An entry: the id passed back when chosen, a title, and a subtitle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub subtitle: String,
}

pub(crate) struct Open {
    pub kind: PickerKind,
    pub entries: Vec<Entry>,
    pub current: String,
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let picker = ui.global::<Picker>();
    let h = with.clone();
    picker.on_query_edited(move |query| h(&|i| i.filter_picker(&query)));
    let h = with.clone();
    picker.on_chosen(move |id| h(&|i| i.picker_chosen(&id)));
    let h = with.clone();
    picker.on_close(move || h(&|i| i.close_picker()));
}

impl Inner {
    fn picker_entries(&self, kind: PickerKind) -> Option<(String, Vec<Entry>, String)> {
        let config = self.store.borrow().config().clone();
        let state = self.state.borrow();
        let rules = state.rules.clone().unwrap_or_else(|| std::rc::Rc::new(xkb::Rules::builtin()));
        let sources = xkb::sources(&config.input.keyboard_layout, &config.input.keyboard_variant);
        Some(match kind {
            PickerKind::Font => {
                let mut fonts = state.fonts.clone();
                let current = config.appearance.font_family.clone();
                if !fonts.contains(&current) {
                    fonts.insert(0, current.clone());
                }
                let entries = fonts
                    .into_iter()
                    .map(|f| Entry { id: f.clone(), title: f, subtitle: String::new() })
                    .collect();
                ("Font".into(), entries, current)
            }
            PickerKind::Layout => {
                let entries = rules
                    .layouts
                    .iter()
                    .filter(|l| !sources.iter().any(|s| s.layout == l.name && s.variant.is_empty()))
                    .map(|l| Entry {
                        id: l.name.clone(),
                        title: l.description.clone(),
                        subtitle: l.name.clone(),
                    })
                    .collect();
                ("Add Input Source".into(), entries, String::new())
            }
            PickerKind::Variant(index) => {
                let source = sources.get(index)?;
                let default = Entry {
                    id: String::new(),
                    title: "Default".into(),
                    subtitle: source.layout.clone(),
                };
                let entries = std::iter::once(default)
                    .chain(rules.variants_of(&source.layout).iter().map(|v| Entry {
                        id: v.name.clone(),
                        title: v.description.clone(),
                        subtitle: format!("{}({})", source.layout, v.name),
                    }))
                    .collect();
                (rules.layout_description(&source.layout), entries, source.variant.clone())
            }
            PickerKind::Timezone => {
                let entries = state
                    .time
                    .zones
                    .iter()
                    .map(|zone| Entry {
                        id: zone.clone(),
                        title: crate::date_time::city(zone),
                        subtitle: zone.clone(),
                    })
                    .collect();
                ("Time Zone".into(), entries, state.time.state.timezone.clone())
            }
        })
    }

    pub fn open_picker(&self, kind: PickerKind) {
        let Some((title, entries, current)) = self.picker_entries(kind) else { return };
        self.state.borrow_mut().picker = Some(Open { kind, entries, current: current.clone() });
        self.with_ui(|ui| {
            let picker = ui.global::<Picker>();
            picker.set_title(title.into());
            picker.set_placeholder(
                match kind {
                    PickerKind::Font => "Search fonts",
                    PickerKind::Layout => "Search languages and layouts",
                    PickerKind::Variant(_) => "Search variants",
                    PickerKind::Timezone => "Search cities and time zones",
                }
                .into(),
            );
            picker.set_current(current.into());
            picker.set_query("".into());
        });
        self.filter_picker("");
        self.with_ui(|ui| ui.global::<Picker>().set_open(true));
    }

    /// Refreshes an open picker after its data arrived.
    pub fn refresh_picker(&self, kind: PickerKind) {
        let open_kind = self.state.borrow().picker.as_ref().map(|p| p.kind);
        if open_kind != Some(kind) {
            return;
        }
        if let Some((_, entries, _)) = self.picker_entries(kind)
            && let Some(open) = self.state.borrow_mut().picker.as_mut()
        {
            open.entries = entries;
        }
        let query = self
            .ui
            .upgrade()
            .map(|ui| ui.global::<Picker>().get_query().to_string())
            .unwrap_or_default();
        self.filter_picker(&query);
    }

    pub fn filter_picker(&self, query: &str) {
        let state = self.state.borrow();
        let Some(open) = &state.picker else { return };
        let shown: Vec<&Entry> = search::rank(
            query,
            open.entries.iter().map(|e| (e.title.as_str(), e.subtitle.as_str())),
        )
        .into_iter()
        .take(MAX_SHOWN)
        .filter_map(|i| open.entries.get(i))
        .collect();
        let current_index = to_index(shown.iter().position(|e| e.id == open.current));
        let items: Vec<PickerItem> = shown
            .iter()
            .map(|e| PickerItem {
                id: e.id.as_str().into(),
                title: e.title.as_str().into(),
                subtitle: e.subtitle.as_str().into(),
            })
            .collect();
        drop(state);
        self.with_ui(|ui| {
            let picker = ui.global::<Picker>();
            picker.set_items(ModelRc::new(VecModel::from(items)));
            picker.set_current_index(current_index);
        });
    }

    pub fn close_picker(&self) {
        self.state.borrow_mut().picker = None;
        self.with_ui(|ui| ui.global::<Picker>().set_open(false));
    }

    pub fn picker_chosen(&self, id: &str) {
        let Some(kind) = self.state.borrow().picker.as_ref().map(|p| p.kind) else { return };
        self.close_picker();
        match kind {
            PickerKind::Font => self.edit(Key::FontFamily.name(), Value::Text(id.into())),
            PickerKind::Layout => self.edit_sources(|sources| xkb::add_source(sources, id, "")),
            PickerKind::Timezone => {
                self.send_time(nimbus_services::timedate::Command::SetTimezone(id.into()))
            }
            PickerKind::Variant(index) => {
                self.edit_sources(|sources| match sources.get_mut(index) {
                    Some(source) if source.variant != id => {
                        source.variant = id.into();
                        true
                    }
                    _ => false,
                })
            }
        }
    }
}
