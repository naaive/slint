// SPDX-License-Identifier: MIT

//! A client of one system daemon on a thread of its own, for apps that need the daemon without the rest of [`crate::Services`].

use std::future::Future;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::oneshot;

use crate::BusAddress;
use crate::bus::{self, BusKind, BusService};

/// How long dropping a [`Worker`] waits for its thread to finish.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Runs a [`BusService`] on the system bus until dropped.
pub(crate) struct Worker<C> {
    commands: UnboundedSender<C>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl<C: std::fmt::Debug + Send + 'static> Worker<C> {
    /// Connects to `system_bus` on a new thread named `name`, and supervises the service that `make` returns.
    /// Without a bus, the service reports itself unavailable and answers every command as unavailable.
    pub(crate) fn spawn<S, F>(name: &str, system_bus: BusAddress, make: F) -> Self
    where
        S: BusService<Command = C>,
        F: FnOnce() -> S + Send + 'static,
    {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let run = async move {
            let mut service = make();
            match bus::connect(&system_bus, BusKind::System).await {
                Some(conn) => {
                    run_until(shutdown_receiver, bus::supervise(service, conn, receiver)).await;
                }
                None => {
                    service.unavailable();
                    run_until(shutdown_receiver, bus::reject_all(service, receiver)).await;
                }
            }
        };
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
            match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => {
                    runtime.block_on(run);
                    runtime.shutdown_timeout(SHUTDOWN_TIMEOUT / 4);
                }
                Err(err) => tracing::error!("Can't start a D-Bus runtime: {err}"),
            }
        });
        let thread =
            spawned.inspect_err(|err| tracing::error!("Can't start the {name} thread: {err}")).ok();
        Self { commands, shutdown: Some(shutdown), thread }
    }

    /// Queues `command`; it never blocks.
    pub(crate) fn send(&self, command: C) {
        if let Err(err) = self.commands.send(command) {
            tracing::debug!("D-Bus client stopped; dropping {:?}", err.0);
        }
    }
}

async fn run_until(shutdown: oneshot::Receiver<()>, work: impl Future<Output = ()>) {
    tokio::select! {
        _ = shutdown => {}
        () = work => {}
    }
}

impl<C> Drop for Worker<C> {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.thread().id() == std::thread::current().id() {
            return;
        }
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!("A D-Bus client didn't stop within {SHUTDOWN_TIMEOUT:?}");
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}
