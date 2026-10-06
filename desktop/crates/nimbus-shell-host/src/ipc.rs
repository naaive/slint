// SPDX-License-Identifier: MIT

//! The compositor's control socket: requests out, and responses and events in, on one connection.

use crate::state::State;
use anyhow::{Context, anyhow};
use nimbus_ipc::{Event, Request, Response};
use serde::Deserialize;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;

type Reply = Box<dyn FnOnce(&mut State, Response)>;

/// A line from the compositor: subscribed connections get events between responses.
#[derive(Deserialize)]
#[serde(untagged)]
enum Message {
    Response(Response),
    Event(Event),
}

pub struct Ipc {
    writer: UnixStream,
    /// What to do with each response, in request order.
    replies: VecDeque<Reply>,
}

impl Ipc {
    /// Connects to `$NIMBUS_SOCKET` and feeds what arrives to [`State::ipc_message`].
    pub fn connect(handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        let path = nimbus_ipc::socket_path()
            .context("neither NIMBUS_SOCKET nor XDG_RUNTIME_DIR is set")?;
        let writer = UnixStream::connect(&path)
            .with_context(|| format!("cannot connect to {}", path.display()))?;
        let reader = writer.try_clone()?;
        let mut input = Vec::new();
        handle
            .insert_source(
                Generic::new(reader, Interest::READ, Mode::Level),
                move |_, reader, state| {
                    let mut chunk = [0; 8192];
                    // The socket is readable, so one read doesn't block.
                    let read = match (&**reader).read(&mut chunk) {
                        Ok(0) => Err(anyhow!("the compositor closed the control socket")),
                        Ok(read) => Ok(read),
                        Err(err) if err.kind() == ErrorKind::Interrupted => Ok(0),
                        Err(err) => Err(anyhow!(err).context("cannot read the control socket")),
                    };
                    match read {
                        Ok(read) => input.extend_from_slice(&chunk[..read]),
                        Err(err) => {
                            state.stop(Err(err));
                            return Ok(PostAction::Remove);
                        }
                    }
                    while let Some(end) = input.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = input.drain(..=end).collect();
                        match serde_json::from_slice(&line) {
                            Ok(message) => state.ipc_message(message),
                            Err(err) => {
                                tracing::warn!("malformed message on the control socket: {err}")
                            }
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow!("cannot watch the control socket: {e}"))?;
        Ok(Self { writer, replies: VecDeque::new() })
    }
}

impl State {
    /// Sends `request` to the compositor; `reply` gets the response.
    pub fn request(
        &mut self,
        request: &Request,
        reply: impl FnOnce(&mut State, Response) + 'static,
    ) {
        match nimbus_ipc::write_message(&mut self.ipc.writer, request) {
            Ok(()) => self.ipc.replies.push_back(Box::new(reply)),
            Err(err) => self.stop(Err(anyhow!(err).context("lost the control socket"))),
        }
    }

    fn ipc_message(&mut self, message: Message) {
        match message {
            Message::Response(response) => match self.ipc.replies.pop_front() {
                Some(reply) => reply(self, response),
                None => tracing::warn!("unexpected response on the control socket: {response:?}"),
            },
            Message::Event(Event::ShellCommand { command, output }) => {
                self.shell_command(command, output.as_deref());
            }
            Message::Event(Event::LockState { locked, held }) => self.lock_state(locked, held),
            Message::Event(event) => self.model.handle_compositor_event(&event),
        }
    }
}

/// A reply that logs failures of a request made on the user's behalf.
pub fn log_failure(what: &'static str) -> impl FnOnce(&mut State, Response) {
    move |_, response| {
        if let Response::Error { message } = response {
            tracing::warn!("{what} failed: {message}");
        }
    }
}
