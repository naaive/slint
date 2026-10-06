// SPDX-License-Identifier: MIT

//! A fake UPower with a display device for one battery, whose state tests change.

use zbus::interface;

use crate::PrivateBus;

const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// UPower's `WarningLevel`, by the values it has on D-Bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningLevel {
    None = 1,
    Low = 3,
    Critical = 4,
    Action = 5,
}

/// The state of the fake battery.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FakeBattery {
    pub percentage: f64,
    pub charging: bool,
    pub warning: WarningLevel,
}

impl FakeBattery {
    /// A battery discharging at `percentage`, with the given warning level.
    pub fn discharging(percentage: f64, warning: WarningLevel) -> Self {
        Self { percentage, charging: false, warning }
    }

    /// A battery charging at `percentage`, which UPower never warns about.
    pub fn charging(percentage: f64) -> Self {
        Self { percentage, charging: true, warning: WarningLevel::None }
    }
}

struct DisplayDevice {
    battery: FakeBattery,
}

#[interface(name = "org.freedesktop.UPower.Device")]
impl DisplayDevice {
    #[zbus(property)]
    fn is_present(&self) -> bool {
        true
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.battery.percentage
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        if self.battery.charging { 1 } else { 2 }
    }

    #[zbus(property)]
    fn time_to_empty(&self) -> i64 {
        if self.battery.charging { 0 } else { 5400 }
    }

    #[zbus(property)]
    fn warning_level(&self) -> u32 {
        self.battery.warning as u32
    }
}

/// The fake UPower on a private bus, which leaves the bus when dropped.
pub struct FakeUPower {
    connection: zbus::Connection,
}

impl FakeUPower {
    /// Serves the display device with `battery` as `org.freedesktop.UPower`.
    pub async fn start(bus: &PrivateBus, battery: FakeBattery) -> Self {
        let connection = bus.connect().await;
        connection.object_server().at(DISPLAY_DEVICE, DisplayDevice { battery }).await.unwrap();
        connection.request_name("org.freedesktop.UPower").await.unwrap();
        Self { connection }
    }

    /// Changes the battery and emits `PropertiesChanged` for every property that changed.
    pub async fn set(&self, battery: FakeBattery) {
        let device = self
            .connection
            .object_server()
            .interface::<_, DisplayDevice>(DISPLAY_DEVICE)
            .await
            .unwrap();
        let emitter = device.signal_emitter();
        let mut device = device.get_mut().await;
        let old = std::mem::replace(&mut device.battery, battery);
        if old.percentage != battery.percentage {
            device.percentage_changed(emitter).await.unwrap();
        }
        if old.charging != battery.charging {
            device.state_changed(emitter).await.unwrap();
            device.time_to_empty_changed(emitter).await.unwrap();
        }
        if old.warning != battery.warning {
            device.warning_level_changed(emitter).await.unwrap();
        }
    }
}
