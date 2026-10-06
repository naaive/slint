// SPDX-License-Identifier: MIT

//! Seat capabilities that follow the connected devices: touch and tablets.

use crate::state::State;
use smithay::backend::input::{Device, DeviceCapability};

impl State {
    pub(super) fn on_device_added<D: Device>(&mut self, device: &D) {
        if device.has_capability(DeviceCapability::Touch) {
            self.nimbus.touch_devices.insert(device.id());
            if self.nimbus.seat.get_touch().is_none() {
                self.nimbus.seat.add_touch();
            }
        }
        if device.has_capability(DeviceCapability::TabletTool) {
            self.add_tablet(device);
        }
    }

    pub(super) fn on_device_removed<D: Device>(&mut self, device: &D) {
        if self.nimbus.touch_devices.remove(&device.id()) && self.nimbus.touch_devices.is_empty() {
            self.nimbus.seat.remove_touch();
        }
        if device.has_capability(DeviceCapability::TabletTool) {
            self.remove_tablet(device);
        }
    }
}
