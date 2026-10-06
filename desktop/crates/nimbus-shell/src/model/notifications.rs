// SPDX-License-Identifier: MIT

//! Notifications in the shared model: the history, the toasts every view shows, and their timeouts.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use nimbus_services::{CloseReason, Notification, ServiceCommand};
use slint::{ModelRc, Timer, TimerMode, VecModel};

use super::{Model, sync_rows};
use crate::client_images::ClientImages;
use crate::icons::IconCache;
use crate::notifications::{self, DEFAULT_ACTION};
use crate::{NotificationAction, NotificationItem, Popup, ShellAction, clock};

/// How long a closing toast fades before it's removed; a little longer than `Theme.duration-normal`.
const TOAST_FADE: Duration = Duration::from_millis(250);

impl Model {
    pub(super) fn add_notification(&self, notification: &Notification) {
        let id = notification.id;
        let reading = self.views().iter().any(|view| view.ui().get_popup() == Popup::Calendar);
        let evicted = {
            let mut state = self.state.borrow_mut();
            let toast = !state.system.do_not_disturb && !state.desktop.locked;
            state.desktop.has_unread |= !reading;
            state.action_models.remove(&id);
            state.notifications.add(notification.clone(), toast)
        };
        let mut timers = self.toast_timers.borrow_mut();
        for evicted in evicted {
            timers.remove(&evicted);
        }
        timers.remove(&id);
        let showing = self.state.borrow().notifications.toasts().iter().any(|t| t.id == id);
        if let Some(timeout) = notifications::toast_timeout(notification).filter(|_| showing) {
            let timer = Timer::default();
            let weak = self.this.clone();
            timer.start(TimerMode::SingleShot, timeout, move || {
                if let Some(this) = weak.upgrade() {
                    this.close_toast(id);
                }
            });
            timers.insert(id, timer);
        }
        drop(timers);
        self.refresh_notifications();
        self.publish();
    }

    /// Fades the toast out, keeping the notification in the history.
    pub(super) fn close_toast(&self, id: u32) {
        if !self.state.borrow_mut().notifications.close_toast(id) {
            return;
        }
        let timer = Timer::default();
        let weak = self.this.clone();
        timer.start(TimerMode::SingleShot, TOAST_FADE, move || {
            if let Some(this) = weak.upgrade() {
                this.state.borrow_mut().notifications.drop_toast(id);
                this.toast_timers.borrow_mut().remove(&id);
                this.refresh_notifications();
            }
        });
        self.toast_timers.borrow_mut().insert(id, timer);
        self.refresh_notifications();
    }

    pub(super) fn remove_notification(&self, id: u32) -> bool {
        let removed = {
            let mut state = self.state.borrow_mut();
            state.action_models.remove(&id);
            state.notifications.remove(id)
        };
        self.toast_timers.borrow_mut().remove(&id);
        self.refresh_notifications();
        removed
    }

    /// Runs the notification's default action, or without one, hides its toast.
    /// Returns whether it ran an action.
    pub fn activate_notification(&self, id: u32) -> bool {
        let has_default = self
            .state
            .borrow()
            .notifications
            .get(id)
            .is_some_and(|n| n.actions.iter().any(|(key, _)| key == DEFAULT_ACTION));
        if !has_default {
            self.close_toast(id);
            return false;
        }
        self.remove_notification(id);
        self.emit(ShellAction::Service(ServiceCommand::InvokeNotificationAction {
            id,
            action: DEFAULT_ACTION.into(),
        }));
        true
    }

    pub fn invoke_notification_action(&self, id: u32, key: String) {
        if self.remove_notification(id) {
            self.emit(ShellAction::Service(ServiceCommand::InvokeNotificationAction {
                id,
                action: key,
            }));
        }
    }

    pub fn dismiss_notification(&self, id: u32) {
        if self.remove_notification(id) {
            self.emit(ShellAction::Service(ServiceCommand::CloseNotification {
                id,
                reason: CloseReason::Dismissed,
            }));
        }
    }

    pub fn clear_notifications(&self) {
        let ids = {
            let mut state = self.state.borrow_mut();
            state.action_models.clear();
            state.notifications.clear()
        };
        self.toast_timers.borrow_mut().clear();
        self.refresh_notifications();
        for id in ids {
            self.emit(ShellAction::Service(ServiceCommand::CloseNotification {
                id,
                reason: CloseReason::Dismissed,
            }));
        }
    }

    pub(super) fn refresh_notifications(&self) {
        let now = SystemTime::now();
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let item = |n: &Notification,
                    closing: bool,
                    icons: &mut IconCache,
                    client_images: &mut ClientImages,
                    actions: &mut HashMap<u32, ModelRc<NotificationAction>>| {
            let actions = actions
                .entry(n.id)
                .or_insert_with(|| {
                    let buttons: Vec<_> = notifications::button_actions(n)
                        .map(|(key, label)| NotificationAction {
                            key: key.as_str().into(),
                            label: label.as_str().into(),
                        })
                        .collect();
                    ModelRc::new(VecModel::from(buttons))
                })
                .clone();
            let icon = n.app_icon.trim();
            let icon = icon.strip_prefix("file://").unwrap_or(icon);
            let name =
                if n.app_name.trim().is_empty() { "Notification" } else { n.app_name.as_str() };
            let visual = if icon.starts_with('/') {
                crate::icons::visual(name, client_images.image(Path::new(icon)))
            } else {
                icons.visual(name, Some(icon))
            };
            NotificationItem {
                id: n.id as i32,
                app_name: name.into(),
                summary: notifications::plain_text(&n.summary).into(),
                body: notifications::plain_text(&n.body).into(),
                visual,
                time: clock::relative_time(n.received, now).into(),
                critical: n.urgency == nimbus_services::Urgency::Critical,
                actions,
                closing,
            }
        };
        let history: Vec<_> = state
            .notifications
            .history()
            .iter()
            .map(|n| {
                item(n, false, &mut state.icons, &mut state.client_images, &mut state.action_models)
            })
            .collect();
        let toasts: Vec<_> = state
            .notifications
            .toasts()
            .iter()
            .filter_map(|t| {
                let n = state.notifications.get(t.id)?;
                Some(item(
                    n,
                    t.closing,
                    &mut state.icons,
                    &mut state.client_images,
                    &mut state.action_models,
                ))
            })
            .collect();
        let pending = state.client_images.is_pending();
        sync_rows(&self.models.notifications, history);
        sync_rows(&self.models.toasts, toasts);
        if pending {
            self.schedule_poll();
        }
    }
}
