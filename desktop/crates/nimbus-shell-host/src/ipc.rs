// SPDX-License-Identifier: MIT

//! The compositor's control socket: requests out, and responses and events in, on one connection.

use crate::state::State;
use anyhow::{Context, anyhow};
use nimbus_ipc::{Event, Request, Response};
use serde::Deserialize;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
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
    stream: UnixStream,
    /// Request lines the socket hasn't accepted yet.
    output: Vec<u8>,
    /// Whether the event loop watches the socket for room to write `output`.
    writing: bool,
    /// What to do with each response, in request order.
    replies: VecDeque<Reply>,
    /// Model events read since the last other message, which [`State::apply_events`] applies together.
    events: Vec<Event>,
}

impl Ipc {
    /// Connects to `$NIMBUS_SOCKET` and feeds what arrives to [`State::ipc_message`].
    pub fn connect(handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        let path = nimbus_ipc::socket_path()
            .context("neither NIMBUS_SOCKET nor XDG_RUNTIME_DIR is set")?;
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("cannot connect to {}", path.display()))?;
        stream.set_nonblocking(true)?;
        let reader = stream.try_clone()?;
        let mut input = Vec::new();
        handle
            .insert_source(
                Generic::new(reader, Interest::READ, Mode::Level),
                move |_, reader, state| {
                    if let Err(err) = read_available(reader, &mut input) {
                        state.stop(Err(err));
                        return Ok(PostAction::Remove);
                    }
                    let mut start = 0;
                    while let Some(len) = input[start..].iter().position(|&b| b == b'\n') {
                        let line = &input[start..start + len];
                        start += len + 1;
                        match serde_json::from_slice(line) {
                            Ok(message) => state.ipc_message(message),
                            Err(err) => {
                                tracing::warn!("malformed message on the control socket: {err}")
                            }
                        }
                    }
                    input.drain(..start);
                    state.apply_events();
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow!("cannot watch the control socket: {e}"))?;
        Ok(Self {
            stream,
            output: Vec::new(),
            writing: false,
            replies: VecDeque::new(),
            events: Vec::new(),
        })
    }

    /// Writes pending output; returns whether some remains.
    fn flush(&mut self) -> io::Result<bool> {
        while !self.output.is_empty() {
            match (&self.stream).write(&self.output) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.output.drain(..written);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(!self.output.is_empty())
    }
}

/// Appends everything `reader` has to `input`.
fn read_available(mut reader: &UnixStream, input: &mut Vec<u8>) -> anyhow::Result<()> {
    let mut chunk = [0; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Err(anyhow!("the compositor closed the control socket")),
            Ok(read) => input.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(()),
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(anyhow!(err).context("cannot read the control socket")),
        }
    }
}

impl State {
    /// Sends `request` to the compositor; `reply` gets the response.
    pub fn request(
        &mut self,
        request: &Request,
        reply: impl FnOnce(&mut State, Response) + 'static,
    ) {
        if let Err(err) = serde_json::to_writer(&mut self.ipc.output, request) {
            tracing::error!("cannot serialize a control request: {err}");
            return;
        }
        self.ipc.output.push(b'\n');
        self.ipc.replies.push_back(Box::new(reply));
        self.flush_ipc();
    }

    /// Writes pending requests, and watches the socket until it takes the rest.
    fn flush_ipc(&mut self) {
        let pending = match self.ipc.flush() {
            Ok(pending) => pending,
            Err(err) => return self.stop(Err(anyhow!(err).context("lost the control socket"))),
        };
        if !pending || self.ipc.writing {
            return;
        }
        let watched = self.ipc.stream.try_clone().map_err(|e| anyhow!(e)).and_then(|writer| {
            self.loop_handle
                .insert_source(Generic::new(writer, Interest::WRITE, Mode::Level), |_, _, state| {
                    state.flush_ipc();
                    if state.ipc.output.is_empty() || !state.running() {
                        state.ipc.writing = false;
                        Ok(PostAction::Remove)
                    } else {
                        Ok(PostAction::Continue)
                    }
                })
                .map_err(|e| anyhow!("{e}"))
        });
        match watched {
            Ok(_) => self.ipc.writing = true,
            Err(err) => self.stop(Err(err.context("cannot watch the control socket for writing"))),
        }
    }

    fn ipc_message(&mut self, message: Message) {
        match message {
            Message::Event(Event::ShellCommand { command, output }) => {
                self.apply_events();
                self.shell_command(command, output.as_deref());
            }
            Message::Event(Event::LockState { locked, held }) => {
                self.apply_events();
                self.lock_state(locked, held);
            }
            Message::Event(event) => self.ipc.events.push(event),
            Message::Response(response) => {
                self.apply_events();
                match self.ipc.replies.pop_front() {
                    Some(reply) => reply(self, response),
                    None => {
                        tracing::warn!("unexpected response on the control socket: {response:?}")
                    }
                }
            }
        }
    }

    /// Applies the model events read so far, refreshing the views once for all of them.
    fn apply_events(&mut self) {
        let events = std::mem::take(&mut self.ipc.events);
        if !events.is_empty() {
            self.model.handle_compositor_events(&events);
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
