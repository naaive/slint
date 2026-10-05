// SPDX-License-Identifier: MIT

//! Notification history and the toasts currently on screen.

use std::time::Duration;

use nimbus_services::{DEFAULT_NOTIFICATION_TIMEOUT, Notification, Urgency};

/// Toasts shown at once; older ones stay in the history only.
pub const MAX_TOASTS: usize = 3;
/// Notifications kept in the notification center.
pub const MAX_HISTORY: usize = 100;
/// The shortest time a toast stays, so that a tiny timeout can still be read.
const MIN_TOAST_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Toast {
    pub id: u32,
    /// Fading out; removed once the fade ends.
    pub closing: bool,
}

#[derive(Debug, Default)]
pub struct Notifications {
    /// Newest first.
    history: Vec<Notification>,
    /// Newest first.
    toasts: Vec<Toast>,
}

impl Notifications {
    pub fn history(&self) -> &[Notification] {
        &self.history
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.history.iter().find(|n| n.id == id)
    }

    /// Stores `notification`, replacing one with the same id, and shows it as a toast when `toast` is set.
    /// Returns the ids of toasts pushed off the stack.
    pub fn add(&mut self, notification: Notification, toast: bool) -> Vec<u32> {
        let id = notification.id;
        self.history.retain(|n| n.id != id);
        self.history.insert(0, notification);
        self.history.truncate(MAX_HISTORY);
        let mut evicted = Vec::new();
        if toast {
            self.toasts.retain(|t| t.id != id);
            self.toasts.insert(0, Toast { id, closing: false });
            while self.toasts.iter().filter(|t| !t.closing).count() > MAX_TOASTS {
                if let Some(oldest) = self.toasts.iter().rposition(|t| !t.closing) {
                    evicted.push(self.toasts.remove(oldest).id);
                }
            }
        } else if let Some(existing) = self.toasts.iter_mut().find(|t| t.id == id) {
            existing.closing = false;
        }
        evicted
    }

    /// Starts fading the toast out; returns whether it was showing.
    pub fn close_toast(&mut self, id: u32) -> bool {
        match self.toasts.iter_mut().find(|t| t.id == id && !t.closing) {
            Some(toast) => {
                toast.closing = true;
                true
            }
            None => false,
        }
    }

    /// Removes a toast that finished fading out.
    pub fn drop_toast(&mut self, id: u32) {
        self.toasts.retain(|t| t.id != id || !t.closing);
    }

    /// Removes the notification from the history and the toasts.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.history.len();
        self.history.retain(|n| n.id != id);
        self.toasts.retain(|t| t.id != id);
        self.history.len() != before
    }

    /// Removes everything and returns the ids that were stored.
    pub fn clear(&mut self) -> Vec<u32> {
        self.toasts.clear();
        self.history.drain(..).map(|n| n.id).collect()
    }
}

/// How long a toast stays on screen, or `None` if it stays until dismissed.
pub fn toast_timeout(notification: &Notification) -> Option<Duration> {
    match notification.expire_timeout {
        Some(Duration::ZERO) => None,
        Some(timeout) => Some(timeout.max(MIN_TOAST_TIMEOUT)),
        None if notification.urgency == Urgency::Critical => None,
        None => Some(DEFAULT_NOTIFICATION_TIMEOUT),
    }
}

/// The action key that activating the notification itself invokes.
pub const DEFAULT_ACTION: &str = "default";

/// Actions shown as buttons: all but the default action.
pub fn button_actions(notification: &Notification) -> impl Iterator<Item = &(String, String)> {
    notification
        .actions
        .iter()
        .filter(|(key, label)| key != DEFAULT_ACTION && !label.trim().is_empty())
}

/// Converts a notification body to plain text.
///
/// The Desktop Notifications Specification allows `<b>`, `<i>`, `<u>`, `<a>`, and `<img>` markup with XML entities;
/// the shell drops the tags and decodes the entities.
pub fn plain_text(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                text.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    text.push_str(rest);
    let decoded = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn notification(id: u32) -> Notification {
        Notification {
            id,
            app_name: "Mail".into(),
            app_icon: String::new(),
            summary: format!("Message {id}"),
            body: String::new(),
            actions: Vec::new(),
            urgency: Urgency::Normal,
            expire_timeout: None,
            received: SystemTime::UNIX_EPOCH,
            transient: false,
            resident: false,
        }
    }

    #[test]
    fn toasts_are_capped_and_history_keeps_everything() {
        let mut store = Notifications::default();
        for id in 1..=3 {
            assert!(store.add(notification(id), true).is_empty());
        }
        assert_eq!(store.add(notification(4), true), vec![1]);
        assert_eq!(store.toasts().iter().map(|t| t.id).collect::<Vec<_>>(), [4, 3, 2]);
        assert_eq!(store.history().iter().map(|n| n.id).collect::<Vec<_>>(), [4, 3, 2, 1]);
    }

    #[test]
    fn replacing_moves_to_the_top() {
        let mut store = Notifications::default();
        store.add(notification(1), true);
        store.add(notification(2), true);
        let mut updated = notification(1);
        updated.summary = "Updated".into();
        store.add(updated, true);
        assert_eq!(store.history().len(), 2);
        assert_eq!(store.history()[0].summary, "Updated");
        assert_eq!(store.toasts()[0].id, 1);
        assert_eq!(store.toasts().len(), 2);
    }

    #[test]
    fn silent_notifications_only_reach_the_history() {
        let mut store = Notifications::default();
        store.add(notification(1), false);
        assert!(store.toasts().is_empty());
        assert_eq!(store.history().len(), 1);
    }

    #[test]
    fn closing_and_removal() {
        let mut store = Notifications::default();
        store.add(notification(1), true);
        store.add(notification(2), true);
        assert!(store.close_toast(1));
        assert!(!store.close_toast(1));
        assert!(store.toasts().iter().any(|t| t.id == 1 && t.closing));
        // A closing toast doesn't count toward the limit.
        store.add(notification(3), true);
        store.add(notification(4), true);
        assert_eq!(store.toasts().len(), 4);
        store.drop_toast(1);
        assert_eq!(store.toasts().iter().map(|t| t.id).collect::<Vec<_>>(), [4, 3, 2]);
        assert_eq!(store.history().len(), 4);
        assert!(store.remove(3));
        assert!(!store.remove(3));
        assert_eq!(store.clear(), vec![4, 2, 1]);
        assert!(store.toasts().is_empty());
    }

    #[test]
    fn history_is_bounded() {
        let mut store = Notifications::default();
        for id in 0..(MAX_HISTORY as u32 + 10) {
            store.add(notification(id), false);
        }
        assert_eq!(store.history().len(), MAX_HISTORY);
        assert_eq!(store.history()[0].id, MAX_HISTORY as u32 + 9);
    }

    #[test]
    fn timeouts() {
        let mut n = notification(1);
        assert_eq!(toast_timeout(&n), Some(DEFAULT_NOTIFICATION_TIMEOUT));
        n.expire_timeout = Some(Duration::ZERO);
        assert_eq!(toast_timeout(&n), None);
        n.expire_timeout = Some(Duration::from_millis(10));
        assert_eq!(toast_timeout(&n), Some(MIN_TOAST_TIMEOUT));
        n.expire_timeout = Some(Duration::from_secs(8));
        assert_eq!(toast_timeout(&n), Some(Duration::from_secs(8)));
        n.expire_timeout = None;
        n.urgency = Urgency::Critical;
        assert_eq!(toast_timeout(&n), None);
    }

    #[test]
    fn markup_becomes_plain_text() {
        assert_eq!(plain_text("<b>Bold</b> and <i>italic</i>"), "Bold and italic");
        assert_eq!(plain_text("a &lt;tag&gt; &amp;amp; &quot;q&quot;"), "a <tag> &amp; \"q\"");
        assert_eq!(plain_text("line\n  two"), "line two");
        assert_eq!(plain_text("1 < 2"), "1 < 2");
        assert_eq!(plain_text("<img src=\"x\"/>"), "");
    }

    #[test]
    fn default_action_is_not_a_button() {
        let mut n = notification(1);
        n.actions = vec![
            ("default".into(), "Open".into()),
            ("reply".into(), "Reply".into()),
            ("blank".into(), " ".into()),
        ];
        assert_eq!(button_actions(&n).map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["reply"]);
    }
}
