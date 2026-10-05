// SPDX-License-Identifier: MIT

//! Running blocking work away from the UI thread and delivering results back to it.

use std::sync::Arc;

/// How background work reaches the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// Work runs on its own thread; results arrive through `slint::invoke_from_event_loop`.
    Threaded,
    /// Work runs immediately on the calling thread, for headless rendering and tests without an event loop.
    Inline,
}

impl Dispatch {
    /// Runs `job` and passes its result to `deliver` on the UI thread.
    pub fn run<T: Send + 'static>(
        self,
        name: &str,
        job: impl FnOnce() -> T + Send + 'static,
        deliver: impl FnOnce(T) + Send + 'static,
    ) {
        let deliver = std::sync::Mutex::new(Some(deliver));
        self.stream(
            name,
            move |emit| emit(job()),
            move |value| {
                if let Some(deliver) = deliver.lock().ok().and_then(|mut d| d.take()) {
                    deliver(value);
                }
            },
        );
    }

    /// Runs `producer`, which may emit any number of values, each passed to `deliver` on the UI thread in order.
    pub fn stream<T: Send + 'static>(
        self,
        name: &str,
        producer: impl FnOnce(&dyn Fn(T)) + Send + 'static,
        deliver: impl Fn(T) + Send + Sync + 'static,
    ) {
        match self {
            Dispatch::Inline => producer(&deliver),
            Dispatch::Threaded => {
                let deliver = Arc::new(deliver);
                let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
                    producer(&|value| {
                        let deliver = deliver.clone();
                        if let Err(error) = slint::invoke_from_event_loop(move || deliver(value)) {
                            tracing::debug!("dropping a background result: {error}");
                        }
                    });
                });
                if let Err(error) = spawned {
                    tracing::error!("cannot start the {name} thread: {error}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn inline_delivers_in_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        Dispatch::Inline.stream(
            "test",
            |emit| (1..=3).for_each(emit),
            move |v| sink.lock().unwrap().push(v),
        );
        let sink = seen.clone();
        Dispatch::Inline.run("test", || 10, move |v| sink.lock().unwrap().push(v));
        assert_eq!(*seen.lock().unwrap(), [1, 2, 3, 10]);
    }
}
