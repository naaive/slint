// SPDX-License-Identifier: MIT

//! The Sound page: output and input devices from the sound server, with their volume and mute state.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use nimbus_services::sound::{Command, Device, Direction, Event, SoundState};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{Inner, Message, With, index_of, strings, to_index};
use crate::sound::{direction, level, percent};
use crate::sources::Control;
use crate::{AppWindow, SoundDeviceItem, SoundModel};

/// How long a device's volume follows the slider instead of the sound server, which lags behind a drag.
const DRAG_GRACE: Duration = Duration::from_secs(1);

/// What the page knows about sound.
pub(crate) struct Sound {
    control: Option<Rc<dyn Control<Command>>>,
    state: SoundState,
    outputs: Rc<VecModel<SoundDeviceItem>>,
    inputs: Rc<VecModel<SoundDeviceItem>>,
    /// When the user last moved each device's slider.
    dragged: HashMap<(Direction, String), Instant>,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            control: None,
            state: SoundState::default(),
            outputs: Rc::new(VecModel::default()),
            inputs: Rc::new(VecModel::default()),
            dragged: HashMap::new(),
        }
    }
}

impl Sound {
    fn model(&self, direction: Direction) -> Rc<VecModel<SoundDeviceItem>> {
        match direction {
            Direction::Output => self.outputs.clone(),
            Direction::Input => self.inputs.clone(),
        }
    }

    fn device(&self, direction: Direction, index: usize) -> Option<&Device> {
        self.state.devices(direction).get(index)
    }
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let model = ui.global::<SoundModel>();
    let h = with.clone();
    model.on_choose_default(move |input, index| {
        h(&|i| {
            let direction = direction(input);
            let name =
                i.state.borrow().sound.device(direction, index_of(index)).map(|d| d.name.clone());
            if let Some(name) = name {
                i.send_sound(Command::SetDefault { direction, name });
            }
        })
    });
    let h = with.clone();
    model.on_set_volume(move |input, index, volume| {
        h(&|i| i.set_volume(direction(input), index_of(index), volume))
    });
    let h = with.clone();
    model.on_set_muted(move |input, index, muted| {
        h(&|i| {
            let direction = direction(input);
            let name =
                i.state.borrow().sound.device(direction, index_of(index)).map(|d| d.name.clone());
            if let Some(name) = name {
                i.send_sound(Command::SetMute { direction, name, muted });
            }
        })
    });
}

impl Inner {
    /// Starts the sound client.
    pub fn start_sound(&self) {
        {
            let state = self.state.borrow();
            let (outputs, inputs) = (state.sound.outputs.clone(), state.sound.inputs.clone());
            self.with_ui(|ui| {
                let model = ui.global::<SoundModel>();
                model.set_outputs(outputs.into());
                model.set_inputs(inputs.into());
            });
        }
        // Inline sources report right away, so no borrow may be held here.
        let control = self.sources.sound(self.events(Message::Sound));
        self.state.borrow_mut().sound.control = Some(Rc::from(control));
    }

    pub(super) fn send_sound(&self, command: Command) {
        // Inline sources report right away, so no borrow may be held while sending.
        let control = self.state.borrow().sound.control.clone();
        if let Some(control) = control {
            control.send(command);
        }
    }

    fn set_volume(&self, direction: Direction, index: usize, percent: f32) {
        let name = {
            let mut state = self.state.borrow_mut();
            let Some(name) = state.sound.device(direction, index).map(|d| d.name.clone()) else {
                return;
            };
            state.sound.dragged.insert((direction, name.clone()), Instant::now());
            name
        };
        self.send_sound(Command::SetVolume { direction, name, volume: percent / 100.0 });
    }

    pub(super) fn handle_sound(&self, event: Event) {
        match event {
            Event::State(state) => {
                self.state.borrow_mut().sound.state = state;
                self.show_sound();
            }
            Event::Failed(reason) => {
                self.show_banner(
                    &format!("The sound settings couldn't be changed: {reason}"),
                    true,
                );
            }
        }
    }

    fn show_sound(&self) {
        let sound = &mut self.state.borrow_mut().sound;
        sound.dragged.retain(|_, at| at.elapsed() < DRAG_GRACE);
        let mut defaults = [(Direction::Output, -1), (Direction::Input, -1)];
        for (direction, default) in &mut defaults {
            let devices = sound.state.devices(*direction);
            *default = to_index(devices.iter().position(|d| d.default));
            let model = sound.model(*direction);
            let items: Vec<SoundDeviceItem> = devices
                .iter()
                .enumerate()
                .map(|(row, device)| {
                    let dragging = sound.dragged.contains_key(&(*direction, device.name.clone()));
                    let volume = match model.row_data(row) {
                        Some(shown) if dragging => shown.volume,
                        _ => percent(device.volume),
                    };
                    SoundDeviceItem {
                        name: device.description.as_str().into(),
                        volume,
                        muted: device.muted,
                        level: level(device),
                    }
                })
                .collect();
            update_rows(&model, items);
        }
        let names = |direction| -> ModelRc<slint::SharedString> {
            strings(sound.state.devices(direction).iter().map(|d| d.description.clone()))
        };
        let (output_names, input_names) = (names(Direction::Output), names(Direction::Input));
        let available = sound.state.available;
        self.with_ui(|ui| {
            let model = ui.global::<SoundModel>();
            model.set_state(if available { 1 } else { 2 });
            model.set_output_names(output_names);
            model.set_output_index(defaults[0].1);
            model.set_input_names(input_names);
            model.set_input_index(defaults[1].1);
        });
    }
}

/// Replaces the rows of `model` with `items`, changing only the rows that differ,
/// so a slider that's being dragged keeps its row.
fn update_rows(model: &VecModel<SoundDeviceItem>, items: Vec<SoundDeviceItem>) {
    if model.row_count() != items.len() {
        model.set_vec(items);
        return;
    }
    for (row, item) in items.into_iter().enumerate() {
        if model.row_data(row).as_ref() != Some(&item) {
            model.set_row_data(row, item);
        }
    }
}
