// SPDX-License-Identifier: MIT

//! The Displays page: heads from a [`DisplayControl`], the user's edits, applying them,
//! and asking to keep them before they revert.

use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Inner, Message, With, deliver, index_of, post, strings, to_index, with_app};
use crate::dispatch::Dispatch;
use crate::displays::{
    self, ApplyError, DisplayControl, DisplayEvent, DisplayEvents, Head, HeadConfig,
};
use crate::{AppWindow, DisplayItem, DisplaysModel};

/// How long applied settings wait to be kept before they revert.
pub const CONFIRM_SECONDS: i32 = 15;

/// What the page knows about the displays.
#[derive(Default)]
pub(crate) struct Displays {
    control: Option<Rc<dyn DisplayControl>>,
    heads: Vec<Head>,
    /// The configuration the page shows, which differs from `heads` while the user edits it.
    edits: Vec<HeadConfig>,
    /// Why displays can only be listed, when they can't be configured.
    unavailable: Option<String>,
    applying: Option<Applying>,
    confirm: Option<Confirm>,
    loading_outputs: bool,
}

enum Applying {
    /// The user's edits, and the configuration to go back to unless the user keeps them.
    Edits {
        previous: Vec<HeadConfig>,
    },
    Revert,
}

/// Applied settings that revert unless the user keeps them before the countdown ends.
struct Confirm {
    previous: Vec<HeadConfig>,
    seconds_left: i32,
    _timer: slint::Timer,
}

impl Displays {
    fn current(&self) -> Vec<HeadConfig> {
        self.heads.iter().map(Head::config).collect()
    }
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let model = ui.global::<DisplaysModel>();
    let h = with.clone();
    model.on_refresh(move || h(&|i| i.start_displays()));
    let h = with.clone();
    model.on_set_enabled(move |index, enabled| {
        h(&|i| i.edit_display(index, |_, config| config.enabled = enabled));
    });
    let h = with.clone();
    model.on_choose_resolution(move |index, choice| {
        h(&|i| {
            i.edit_display(index, |head, config| {
                let Some(&size) = head.resolutions().get(index_of(choice)) else { return };
                let rates = head.refresh_rates(size);
                let refresh = config.mode.map(|m| m.refresh_mhz);
                config.mode = rates
                    .iter()
                    .find(|m| Some(m.refresh_mhz) == refresh)
                    .or_else(|| rates.iter().find(|m| m.preferred))
                    .or(rates.first())
                    .copied();
            })
        });
    });
    let h = with.clone();
    model.on_choose_refresh(move |index, choice| {
        h(&|i| {
            i.edit_display(index, |head, config| {
                let Some(mode) = config.mode else { return };
                if let Some(&rate) =
                    head.refresh_rates((mode.width, mode.height)).get(index_of(choice))
                {
                    config.mode = Some(rate);
                }
            })
        });
    });
    let h = with.clone();
    model.on_choose_scale(move |index, choice| {
        h(&|i| {
            i.edit_display(index, |_, config| {
                if let Some(&scale) = displays::scale_choices(config.scale).get(index_of(choice)) {
                    config.scale = scale;
                }
            })
        });
    });
    let h = with.clone();
    model.on_choose_rotation(move |index, choice| {
        h(&|i| {
            i.edit_display(index, |_, config| {
                config.transform = config.transform.with_rotation(choice as u32);
            })
        });
    });
    let h = with.clone();
    model.on_moved(move |index, x, y| h(&|i| i.move_display(index, x, y)));
    let h = with.clone();
    model.on_apply(move || h(&|i| i.apply_displays()));
    let h = with.clone();
    model.on_reset(move || h(&|i| i.reset_displays()));
    let h = with.clone();
    model.on_keep(move || h(&|i| i.keep_displays()));
    let h = with.clone();
    model.on_revert(move || h(&|i| i.revert_displays()));
}

impl Inner {
    /// Connects to the compositor's display configuration, or lists the outputs read-only without it.
    pub fn start_displays(&self) {
        let events: DisplayEvents = match self.dispatch {
            Dispatch::Threaded => Box::new(|event| post(Message::Display(event))),
            Dispatch::Inline => Box::new(|event| deliver(Message::Display(event))),
        };
        {
            let mut state = self.state.borrow_mut();
            let displays = &mut state.displays;
            displays.control = None;
            displays.unavailable = None;
            displays.heads.clear();
            displays.edits.clear();
        }
        self.with_ui(|ui| ui.global::<DisplaysModel>().set_state(0));
        // Inline sources report heads right away, so no borrow may be held here.
        let control = self.sources.display_control(events);
        let Some(control) = control else {
            self.load_outputs();
            return;
        };
        let reported = {
            let mut state = self.state.borrow_mut();
            state.displays.control = Some(Rc::from(control));
            !state.displays.heads.is_empty()
        };
        if reported {
            self.show_displays();
        }
    }

    /// Lists the outputs that the control socket reports.
    fn load_outputs(&self) {
        {
            let mut state = self.state.borrow_mut();
            if std::mem::replace(&mut state.displays.loading_outputs, true) {
                return;
            }
        }
        let sources = self.sources.clone();
        self.dispatch.run(
            "nimbus-settings-outputs",
            move || sources.outputs(),
            |o| deliver(Message::Outputs(o)),
        );
    }

    pub(super) fn handle_outputs(&self, result: Result<Vec<nimbus_ipc::OutputInfo>, String>) {
        let mut state = self.state.borrow_mut();
        let displays = &mut state.displays;
        displays.loading_outputs = false;
        if displays.control.is_some() {
            return;
        }
        match result {
            Ok(outputs) => {
                displays.heads = displays::heads_from_outputs(&outputs);
                displays.edits = displays.current();
                drop(state);
                self.show_displays();
            }
            Err(error) => {
                drop(state);
                let mut message = error;
                if let Some(first) = message.get(..1) {
                    message = first.to_uppercase() + &message[1..];
                }
                self.with_ui(|ui| {
                    let model = ui.global::<DisplaysModel>();
                    model.set_message(
                        format!(
                            "{message}. Open Settings from a Nimbus session to see your displays."
                        )
                        .into(),
                    );
                    model.set_state(2);
                });
            }
        }
    }

    pub(super) fn handle_display(&self, event: DisplayEvent) {
        match event {
            DisplayEvent::Heads(heads) => {
                {
                    let mut state = self.state.borrow_mut();
                    let displays = &mut state.displays;
                    let same_heads = displays.heads.len() == heads.len()
                        && displays.heads.iter().zip(&heads).all(|(a, b)| a.name == b.name);
                    let editing = displays.edits != displays.current();
                    displays.heads = heads;
                    if !(same_heads && editing) {
                        displays.edits = displays.current();
                    }
                }
                self.show_displays();
            }
            DisplayEvent::Applied(result) => {
                let applying = {
                    let mut state = self.state.borrow_mut();
                    let displays = &mut state.displays;
                    if result.is_ok() {
                        displays.edits = displays.current();
                    }
                    displays.applying.take()
                };
                match (applying, result) {
                    (Some(Applying::Edits { previous }), Ok(())) => self.ask_to_keep(previous),
                    (_, Err(error)) => self.show_banner(&apply_message(&error), true),
                    _ => {}
                }
                self.show_displays();
            }
            DisplayEvent::Unavailable(reason) => {
                tracing::info!("displays can't be configured: {reason}");
                {
                    let mut state = self.state.borrow_mut();
                    let displays = &mut state.displays;
                    displays.control = None;
                    displays.applying = None;
                    displays.unavailable = Some(reason);
                }
                self.load_outputs();
            }
        }
    }

    /// Changes the edited configuration of head `index`, and moves the others out of its way.
    fn edit_display(&self, index: i32, edit: impl FnOnce(&Head, &mut HeadConfig)) {
        {
            let mut state = self.state.borrow_mut();
            let displays = &mut state.displays;
            let index = index_of(index);
            let (Some(head), Some(config)) = (displays.heads.get(index), displays.edits.get(index))
            else {
                return;
            };
            let before = config.clone();
            let mut after = before.clone();
            edit(head, &mut after);
            if after == before {
                return;
            }
            displays.edits[index] = after;
            displays::make_room(&mut displays.edits, index, &before);
        }
        self.show_displays();
    }

    fn move_display(&self, index: i32, x: f32, y: f32) {
        {
            let mut state = self.state.borrow_mut();
            displays::move_head(&mut state.displays.edits, index_of(index), (x, y));
        }
        self.show_displays();
    }

    fn apply_displays(&self) {
        let (control, edits) = {
            let mut state = self.state.borrow_mut();
            let displays = &mut state.displays;
            let Some(control) = displays.control.clone() else { return };
            if displays.applying.is_some() || displays.edits == displays.current() {
                return;
            }
            displays.applying = Some(Applying::Edits { previous: displays.current() });
            (control, displays.edits.clone())
        };
        self.show_displays();
        control.apply(edits);
    }

    fn reset_displays(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.displays.edits = state.displays.current();
        }
        self.show_displays();
    }

    fn ask_to_keep(&self, previous: Vec<HeadConfig>) {
        let weak = self.ui.clone();
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
            if weak.upgrade().is_some() {
                with_app(|inner| inner.tick_countdown());
            }
        });
        self.state.borrow_mut().displays.confirm =
            Some(Confirm { previous, seconds_left: CONFIRM_SECONDS, _timer: timer });
    }

    fn tick_countdown(&self) {
        let left = {
            let mut state = self.state.borrow_mut();
            let Some(confirm) = &mut state.displays.confirm else { return };
            confirm.seconds_left -= 1;
            confirm.seconds_left
        };
        if left <= 0 {
            self.revert_displays();
        } else {
            self.show_displays();
        }
    }

    fn keep_displays(&self) {
        {
            self.state.borrow_mut().displays.confirm = None;
        }
        self.show_displays();
    }

    fn revert_displays(&self) {
        let revert = {
            let mut state = self.state.borrow_mut();
            let displays = &mut state.displays;
            let previous = displays.confirm.take().map(|confirm| confirm.previous);
            match (displays.control.clone(), previous) {
                (Some(control), Some(previous)) => {
                    displays.applying = Some(Applying::Revert);
                    displays.edits = previous.clone();
                    Some((control, previous))
                }
                _ => None,
            }
        };
        self.show_displays();
        if let Some((control, previous)) = revert {
            control.apply(previous);
        }
    }

    /// Pushes the heads and edits into the page.
    fn show_displays(&self) {
        let state = self.state.borrow();
        let displays = &state.displays;
        let (rects, aspect) = displays::arrangement(&displays.edits);
        let items: Vec<DisplayItem> = displays
            .heads
            .iter()
            .zip(&displays.edits)
            .zip(rects)
            .map(|((head, config), rect)| item(head, config, rect.unwrap_or_default()))
            .collect();
        let editable = displays.control.is_some();
        let changed = displays.edits != displays.current();
        let applying = displays.applying.is_some();
        let seconds_left = displays.confirm.as_ref().map_or(0, |confirm| confirm.seconds_left);
        let message = match &displays.unavailable {
            Some(reason) => format!("{reason}, so displays are shown without changing them."),
            None => String::new(),
        };
        self.with_ui(|ui| {
            let model = ui.global::<DisplaysModel>();
            if items.is_empty() {
                model.set_message("The compositor reports no connected displays.".into());
                model.set_state(2);
                return;
            }
            model.set_items(ModelRc::new(VecModel::from(items)));
            model.set_arrangement_aspect(aspect);
            model.set_editable(editable);
            model.set_changed(changed);
            model.set_applying(applying);
            model.set_confirm_seconds(seconds_left);
            model.set_message(message.into());
            model.set_state(1);
        });
    }
}

fn item(head: &Head, config: &HeadConfig, [x, y, width, height]: [f32; 4]) -> DisplayItem {
    let resolutions = head.resolutions();
    let size = config.mode.map(|m| (m.width, m.height));
    let rates = size.map(|size| head.refresh_rates(size)).unwrap_or_default();
    let scales = displays::scale_choices(config.scale);
    DisplayItem {
        title: head.title().into(),
        connector: head.name.as_str().into(),
        enabled: config.enabled,
        resolutions: strings(resolutions.iter().map(|(w, h)| format!("{w} × {h}"))),
        resolution_index: to_index(resolutions.iter().position(|&r| Some(r) == size)),
        refresh_rates: strings(rates.iter().map(|m| {
            let text = displays::format_refresh(m.refresh_mhz);
            if text.is_empty() { "Unknown".into() } else { text }
        })),
        refresh_index: to_index(
            rates.iter().position(|m| Some(m.refresh_mhz) == config.mode.map(|c| c.refresh_mhz)),
        ),
        scales: strings(scales.iter().map(|&s| displays::format_scale(s))),
        scale_index: to_index(scales.iter().position(|&s| (s - config.scale).abs() < 1e-3)),
        rotation_index: config.transform.rotation() as i32,
        frac_x: x,
        frac_y: y,
        frac_width: width,
        frac_height: height,
    }
}

fn apply_message(error: &ApplyError) -> String {
    match error {
        ApplyError::Failed => format!("{error}. The previous settings are still in use."),
        ApplyError::Outdated => error.to_string(),
    }
}
