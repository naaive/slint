// SPDX-License-Identifier: MIT

//! Locking after `power.lock_after_minutes` of inactivity, through `ext-idle-notify-v1`.

use crate::state::State;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

pub struct Idle {
    notifier: Option<ExtIdleNotifierV1>,
    notification: Option<ExtIdleNotificationV1>,
}

impl Idle {
    pub fn new(notifier: Option<ExtIdleNotifierV1>) -> Self {
        Self { notifier, notification: None }
    }
}

impl State {
    /// Asks for a notification after the configured idle time, replacing the previous one.
    pub fn watch_idle(&mut self) {
        if let Some(notification) = self.idle.notification.take() {
            notification.destroy();
        }
        let minutes = self.settings.current.power.lock_after_minutes;
        let (Some(notifier), Some(seat)) = (&self.idle.notifier, &self.input.seat) else {
            return;
        };
        if minutes == 0 {
            return;
        }
        let timeout = u32::try_from(u64::from(minutes) * 60_000).unwrap_or(u32::MAX);
        self.idle.notification = Some(notifier.get_idle_notification(timeout, seat, &self.qh, ()));
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_idle_notification_v1::Event::Idled = event
            && !state.model.is_locked()
        {
            tracing::info!("locking after inactivity");
            state.lock();
        }
    }
}

wayland_client::delegate_noop!(State: ignore ExtIdleNotifierV1);
