// SPDX-License-Identifier: MIT

//! The Nimbus shell: panel, dock, launcher, notifications, and lock screen, as a Wayland client of the compositor.
//!
//! It exits with status 0 on SIGTERM or SIGINT, and with a failure when it loses the compositor.

mod actions;
mod auth;
mod config;
mod idle;
mod input;
mod ipc;
mod lock;
mod media;
mod output;
mod platform;
mod render;
mod services;
mod state;
mod surface;
mod wayland;

use anyhow::{Context, anyhow};
use clap::Parser;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{
    EventLoop, Interest, LoopHandle, Mode, PostAction,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use state::State;
use std::io::{IsTerminal, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use wayland_client::Connection;
use wayland_client::globals::registry_queue_init;

/// The Nimbus desktop shell.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Configuration file; defaults to $XDG_CONFIG_HOME/nimbus/config.toml.
    #[arg(long)]
    config: Option<PathBuf>,
}

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(filter)
        .init();

    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    let mut event_loop: EventLoop<'static, State> =
        EventLoop::try_new().context("cannot create the event loop")?;
    watch_signals(&event_loop.handle())?;

    let conn = Connection::connect_to_env().context("cannot connect to the Wayland compositor")?;
    let (globals, queue) =
        registry_queue_init::<State>(&conn).context("cannot list the compositor's globals")?;
    let mut state = State::new(
        &conn,
        &globals,
        queue.handle(),
        event_loop.handle(),
        event_loop.get_signal(),
        args.config,
    )?;
    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow!("cannot watch the Wayland connection: {e}"))?;

    while state.running() {
        let timeout = state.next_timeout();
        if let Err(err) = event_loop.dispatch(timeout, &mut state) {
            state.stop(Err(anyhow!(err).context("lost the compositor")));
            break;
        }
        state.update();
    }
    state.into_exit()
}

/// Ends the event loop successfully on SIGTERM and SIGINT.
fn watch_signals(handle: &LoopHandle<'static, State>) -> anyhow::Result<()> {
    let (reader, writer) = UnixStream::pair().context("cannot create the signal pipe")?;
    writer.set_nonblocking(true)?;
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::low_level::pipe::register(signal, writer.try_clone()?)
            .context("cannot handle signals")?;
    }
    handle
        .insert_source(Generic::new(reader, Interest::READ, Mode::Level), |_, reader, state| {
            let _ = (&**reader).read(&mut [0; 16]);
            tracing::info!("terminating");
            state.stop(Ok(()));
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow!("cannot watch signals: {e}"))?;
    Ok(())
}
