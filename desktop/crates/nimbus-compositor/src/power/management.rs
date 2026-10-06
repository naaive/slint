// SPDX-License-Identifier: MIT

//! The server side of wlr-output-power-management-unstable-v1, which idle daemons such as `swayidle`
//! and tools such as `wlopm` use to turn outputs off and on.
//!
//! As in wlroots, one power object at a time controls an output; another one for it fails.

use crate::state::State;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::{
    zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
    zwlr_output_power_v1::{self, Mode, ZwlrOutputPowerV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

const VERSION: u32 = 1;

/// The live power objects and the outputs they control.
pub struct OutputPowerState {
    objects: Vec<(ZwlrOutputPowerV1, String)>,
}

impl OutputPowerState {
    pub fn new(display: &DisplayHandle) -> Self {
        display.create_global::<State, ZwlrOutputPowerManagerV1, ()>(VERSION, ());
        Self { objects: Vec::new() }
    }

    /// Fails the objects of outputs that aren't `enabled` any more.
    pub fn retain_outputs(&mut self, enabled: &[String]) {
        self.objects.retain(|(object, output)| {
            let present = enabled.contains(output);
            if !present {
                object.failed();
            }
            present
        });
    }

    /// Sends the mode of each output that turned on or off.
    pub fn send_modes(&self, was_off: &[String], off: &[String]) {
        for (object, output) in &self.objects {
            let on = !off.contains(output);
            if on == was_off.contains(output) {
                object.mode(mode(on));
            }
        }
    }

    fn controls(&self, output: &str) -> bool {
        self.objects.iter().any(|(_, name)| name == output)
    }

    fn output_of(&self, object: &ZwlrOutputPowerV1) -> Option<&str> {
        self.objects.iter().find(|(o, _)| o == object).map(|(_, name)| name.as_str())
    }

    fn remove(&mut self, object: &ZwlrOutputPowerV1) {
        self.objects.retain(|(o, _)| o != object);
    }
}

fn mode(on: bool) -> Mode {
    if on { Mode::On } else { Mode::Off }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } = request else {
            return;
        };
        let object = data_init.init(id, ());
        let nimbus = &state.nimbus;
        let output = Output::from_resource(&output)
            .map(|o| o.name())
            .filter(|name| nimbus.output_by_name(name).is_some());
        match output {
            Some(name) if !nimbus.output_power.controls(&name) => {
                object.mode(mode(!nimbus.power.is_off(&name)));
                state.nimbus.output_power.objects.push((object, name));
            }
            _ => object.failed(),
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let zwlr_output_power_v1::Request::SetMode { mode } = request else {
            return;
        };
        let Some(output) = state.nimbus.output_power.output_of(resource).map(str::to_owned) else {
            return;
        };
        match mode {
            WEnum::Value(mode) => state.nimbus.set_output_power(&output, mode == Mode::On),
            WEnum::Unknown(_) => resource.post_error(
                zwlr_output_power_v1::Error::InvalidMode,
                "unknown power management mode",
            ),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwlrOutputPowerV1, _data: &()) {
        state.nimbus.output_power.remove(resource);
    }
}
