// SPDX-License-Identifier: MIT

//! The system's preferred color scheme, queried on a worker thread.
//!
//! The portal query can take over a second, and at startup the portal may need the compositor's own display,
//! so the UI thread must never wait for it.

use std::sync::{Mutex, PoisonError};

/// The preference assumed until a query answers.
const DEFAULT_DARK: bool = true;

struct State {
    /// The last query started.
    started: u64,
    /// The last query finished.
    finished: u64,
    /// The last answer, or `None` before the first.
    dark: Option<bool>,
}

pub struct SchemeQuery {
    ask: fn() -> Option<bool>,
    state: Mutex<State>,
}

pub static SYSTEM: SchemeQuery = SchemeQuery::new(nimbus_theme::system_prefers_dark);

impl SchemeQuery {
    const fn new(ask: fn() -> Option<bool>) -> Self {
        Self { ask, state: Mutex::new(State { started: 0, finished: 0, dark: None }) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a query unless one is running.
    /// Returns the ticket for [`SchemeQuery::answer`], and the last known preference.
    pub fn query(&'static self) -> (u64, bool) {
        let mut state = self.lock();
        let last = state.dark.unwrap_or(DEFAULT_DARK);
        if state.started > state.finished {
            return (state.started, last);
        }
        state.started += 1;
        let ticket = state.started;
        let spawned =
            std::thread::Builder::new().name("nimbus-shell-scheme".into()).spawn(move || {
                let dark = (self.ask)().unwrap_or(DEFAULT_DARK);
                let mut state = self.lock();
                state.finished = state.finished.max(ticket);
                state.dark = Some(dark);
            });
        if let Err(err) = spawned {
            tracing::warn!("Can't query the system color scheme: {err}");
            state.finished = ticket;
        }
        (ticket, last)
    }

    /// Whether the system prefers a dark scheme, once the query for `ticket` finished.
    pub fn answer(&self, ticket: u64) -> Option<bool> {
        let state = self.lock();
        (state.finished >= ticket).then(|| state.dark.unwrap_or(DEFAULT_DARK))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn queries_run_in_the_background_and_are_shared() {
        static SLOW: SchemeQuery = SchemeQuery::new(|| {
            std::thread::sleep(Duration::from_millis(300));
            Some(false)
        });
        let start = Instant::now();
        let (ticket, dark) = SLOW.query();
        assert!(start.elapsed() < Duration::from_millis(200), "querying doesn't wait");
        assert_eq!(dark, DEFAULT_DARK);
        assert_eq!(SLOW.answer(ticket), None);
        assert_eq!(SLOW.query().0, ticket, "a running query is shared");

        let deadline = Instant::now() + Duration::from_secs(5);
        while SLOW.answer(ticket).is_none() {
            assert!(Instant::now() < deadline, "the query never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(SLOW.answer(ticket), Some(false));
        let (next, dark) = SLOW.query();
        assert!(next > ticket);
        assert!(!dark, "the last answer stands in until the next arrives");
    }
}
