// SPDX-License-Identifier: MIT

//! The Default Applications page: one choice per kind of application, read and written in the background.

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Inner, Message, With, deliver, index_of, strings, to_index};
use crate::default_apps::Choices;
use crate::{AppWindow, DefaultAppItem, DefaultAppsModel};

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let h = with.clone();
    ui.global::<DefaultAppsModel>().on_chosen(move |category, index| {
        h(&|i| i.choose_default_app(index_of(category), index_of(index)))
    });
}

impl Inner {
    /// Reads the default applications in the background.
    pub(super) fn load_default_apps(&self) {
        let defaults = self.sources.default_apps();
        self.dispatch.run(
            "nimbus-settings-default-apps",
            move || defaults.load(),
            |choices| deliver(Message::DefaultApps(choices)),
        );
    }

    fn choose_default_app(&self, category: usize, index: usize) {
        let chosen = {
            let state = self.state.borrow();
            state
                .default_apps
                .get(category)
                .and_then(|c| Some((c.category, c.apps.get(index)?.clone())))
        };
        let Some((category, app)) = chosen else { return };
        let defaults = self.sources.default_apps();
        self.dispatch.run(
            "nimbus-settings-default-apps",
            move || {
                let result =
                    defaults.set(category, &app.id).map_err(|error| (app.name, error.to_string()));
                (result, defaults.load())
            },
            |(result, choices)| deliver(Message::DefaultAppSet(result, choices)),
        );
    }

    pub(super) fn show_default_apps(&self, choices: Vec<Choices>) {
        let items: Vec<DefaultAppItem> = choices
            .iter()
            .map(|c| {
                let available = !c.apps.is_empty();
                let choices = if available {
                    strings(c.apps.iter().map(|app| app.name.clone()))
                } else {
                    strings(["None installed".to_owned()])
                };
                DefaultAppItem {
                    title: c.category.title().into(),
                    choices,
                    current: if available { to_index(c.current) } else { 0 },
                    available,
                }
            })
            .collect();
        self.state.borrow_mut().default_apps = choices;
        self.with_ui(|ui| {
            let model = ui.global::<DefaultAppsModel>();
            model.set_items(ModelRc::new(VecModel::from(items)));
            model.set_loading(false);
        });
    }

    pub(super) fn default_app_set(
        &self,
        result: Result<(), (String, String)>,
        choices: Vec<Choices>,
    ) {
        if let Err((name, error)) = result {
            self.show_banner(&format!("{name} couldn't be made the default: {error}"), true);
        }
        self.show_default_apps(choices);
    }
}
