// SPDX-License-Identifier: MIT

//! The shortcuts page: listing bindings and the dialog that captures new key chords.

use nimbus_config::{Action, Keybindings};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{Inner, With, index_of};
use crate::shortcuts::{self, ActionKind, Modifiers, Parameter};
use crate::{AppWindow, ShortcutCategory, ShortcutItem, ShortcutsModel, search};

/// The state of the capture dialog.
pub(crate) struct Capture {
    /// The chord of the binding being changed, or `None` when adding one.
    pub editing: Option<String>,
    pub kind: ActionKind,
    pub argument: String,
    pub chord: Option<String>,
}

fn strings(items: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let model = ui.global::<ShortcutsModel>();
    model
        .set_action_kinds(strings(ActionKind::ALL.iter().map(|k| k.label().to_string()).collect()));
    let h = with.clone();
    model.on_filter_edited(move |_| h(&|i| i.sync_shortcuts()));
    let h = with.clone();
    model.on_edit(move |chord| h(&|i| i.start_capture(Some(chord.as_str()))));
    let h = with.clone();
    model.on_add(move || h(&|i| i.start_capture(None)));
    let h = with.clone();
    model.on_remove(move |chord| {
        h(&|i| {
            i.update(|config| {
                config.keybindings.0.remove(chord.as_str());
            });
        });
    });
    let h = with.clone();
    model.on_reset(move || h(&|i| i.update(|config| config.keybindings = Keybindings::default())));
    let h = with.clone();
    model.on_key_captured(move |text, control, alt, shift, meta| {
        h(&|i| i.key_captured(&text, Modifiers { ctrl: control, alt, shift, meta }));
    });
    let h = with.clone();
    model.on_capture_kind_chosen(move |index| {
        h(&|i| {
            if let Some(capture) = i.state.borrow_mut().capture.as_mut() {
                capture.kind =
                    ActionKind::ALL.get(index_of(index)).copied().unwrap_or(ActionKind::Spawn);
                capture.argument.clear();
            }
            i.sync_capture();
        });
    });
    let h = with.clone();
    model.on_capture_argument_edited(move |text| {
        h(&|i| {
            if let Some(capture) = i.state.borrow_mut().capture.as_mut() {
                capture.argument = text.to_string();
            }
            i.sync_capture();
        });
    });
    let h = with.clone();
    model.on_capture_apply(move || h(&|i| i.apply_capture()));
    let h = with.clone();
    model.on_capture_cancel(move || h(&|i| i.close_capture()));
}

impl Inner {
    pub fn sync_shortcuts(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let model = ui.global::<ShortcutsModel>();
        let filter = model.get_filter().to_string();
        let bindings = self.store.borrow().config().keybindings.clone();
        {
            let mut state = self.state.borrow_mut();
            let pushed = Some((filter.clone(), bindings.clone()));
            if state.pushed_shortcuts == pushed {
                return;
            }
            state.pushed_shortcuts = pushed;
        }
        let rows = shortcuts::rows(&bindings);
        model.set_conflict_count(rows.iter().filter(|r| r.conflict).count() as i32);
        let keep =
            search::rank(&filter, rows.iter().map(|r| (r.description.as_str(), r.chord.as_str())));
        let mut keep_sorted = keep;
        keep_sorted.sort_unstable();
        let mut categories: Vec<(&str, Vec<ShortcutItem>)> = Vec::new();
        for row in keep_sorted.into_iter().filter_map(|i| rows.get(i)) {
            let item = ShortcutItem {
                chord: row.chord.as_str().into(),
                keys: strings(row.labels.clone()),
                description: row.description.as_str().into(),
                conflict: row.conflict,
                invalid: row.invalid,
            };
            match categories.iter_mut().find(|(title, _)| *title == row.category) {
                Some((_, items)) => items.push(item),
                None => categories.push((row.category, vec![item])),
            }
        }
        let categories: Vec<ShortcutCategory> = categories
            .into_iter()
            .map(|(title, items)| ShortcutCategory {
                title: title.into(),
                items: ModelRc::new(VecModel::from(items)),
            })
            .collect();
        model.set_categories(ModelRc::new(VecModel::from(categories)));
    }

    pub fn start_capture(&self, editing: Option<&str>) {
        let action = editing
            .and_then(|chord| self.store.borrow().config().keybindings.0.get(chord).cloned());
        if editing.is_some() && action.is_none() {
            return;
        }
        let (kind, argument) = match &action {
            Some(Action::Spawn(command)) => (ActionKind::Spawn, command.clone()),
            Some(workspace @ (Action::Workspace(n) | Action::MoveToWorkspace(n))) => {
                (ActionKind::of(workspace), (u64::from(*n) + 1).to_string())
            }
            Some(other) => (ActionKind::of(other), String::new()),
            None => (ActionKind::Spawn, String::new()),
        };
        self.state.borrow_mut().capture =
            Some(Capture { editing: editing.map(str::to_string), kind, argument, chord: None });
        self.with_ui(|ui| {
            let model = ui.global::<ShortcutsModel>();
            model.set_capture_adding(editing.is_none());
            model.set_capture_action(
                action.as_ref().map(shortcuts::describe).unwrap_or_default().into(),
            );
            model.set_capture_kind(kind.index() as i32);
        });
        self.sync_capture();
        self.with_ui(|ui| ui.global::<ShortcutsModel>().set_capture_open(true));
    }

    /// The action the dialog would bind, when its inputs are complete.
    fn capture_action(&self, capture: &Capture) -> Option<Action> {
        match &capture.editing {
            Some(chord) => self.store.borrow().config().keybindings.0.get(chord).cloned(),
            None => capture.kind.build(&capture.argument),
        }
    }

    fn sync_capture(&self) {
        let state = self.state.borrow();
        let Some(capture) = &state.capture else { return };
        let config = self.store.borrow();
        let bindings = &config.config().keybindings;
        let conflict = capture
            .chord
            .as_deref()
            .and_then(|chord| shortcuts::binding_for(bindings, chord, capture.editing.as_deref()))
            .map(|(_, action)| {
                format!(
                    "These keys are used by “{}”. Replacing removes that shortcut.",
                    shortcuts::describe(action)
                )
            })
            .unwrap_or_default();
        let keys = capture.chord.as_deref().map(shortcuts::chord_labels).unwrap_or_default();
        let can_apply = capture.chord.is_some() && self.capture_action(capture).is_some();
        let parameter = match capture.kind.parameter() {
            Parameter::None => 0,
            Parameter::Command => 1,
            Parameter::Workspace => 2,
        };
        let argument = capture.argument.clone();
        drop(config);
        drop(state);
        self.with_ui(|ui| {
            let model = ui.global::<ShortcutsModel>();
            model.set_captured_keys(strings(keys));
            model.set_capture_conflict(conflict.into());
            model.set_capture_can_apply(can_apply);
            model.set_capture_parameter(parameter);
            if model.get_capture_argument().as_str() != argument {
                model.set_capture_argument(argument.into());
            }
        });
    }

    pub fn key_captured(&self, text: &str, modifiers: Modifiers) {
        let Some(chord) = shortcuts::chord_from_key_event(text, modifiers) else { return };
        if let Some(capture) = self.state.borrow_mut().capture.as_mut() {
            capture.chord = Some(chord);
        }
        self.sync_capture();
    }

    pub fn apply_capture(&self) {
        let capture = self.state.borrow_mut().capture.take();
        let Some(capture) = capture else { return };
        let (Some(chord), Some(action)) = (capture.chord.clone(), self.capture_action(&capture))
        else {
            self.state.borrow_mut().capture = Some(capture);
            return;
        };
        let mut bindings = self.store.borrow().config().keybindings.clone();
        match shortcuts::bind(&mut bindings, &chord, action, capture.editing.as_deref(), true) {
            Ok(()) => {
                self.close_capture();
                self.update(|config| config.keybindings = bindings);
            }
            Err(error) => {
                tracing::warn!("{error}");
                self.state.borrow_mut().capture = Some(capture);
                self.with_ui(|ui| {
                    ui.global::<ShortcutsModel>().set_capture_conflict(error.to_string().into())
                });
            }
        }
    }

    pub fn close_capture(&self) {
        self.state.borrow_mut().capture = None;
        self.with_ui(|ui| ui.global::<ShortcutsModel>().set_capture_open(false));
    }
}
