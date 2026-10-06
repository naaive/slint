// SPDX-License-Identifier: MIT

//! The control socket: newline-delimited JSON requests and responses, plus event streaming.
//!
//! Every connection is a non-blocking calloop source.
//! A client that stops reading is disconnected once its pending output exceeds [`MAX_PENDING_OUTPUT`].
//! Responses go out in request order, even when one is computed off the event loop.

use crate::state::State;
use anyhow::Context;
use nimbus_ipc::{Event, Request, Response};
use smithay::reexports::calloop::channel::{self, Event as ChannelEvent};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// Longest accepted request line.
pub const MAX_LINE: usize = 1 << 20;
/// Output buffered for one client before it counts as unresponsive.
pub const MAX_PENDING_OUTPUT: usize = 8 << 20;

type ConnectionId = u64;

/// A response that a worker thread computes; see [`IpcServer::defer`].
/// Dropping it unanswered sends an error, so later responses on the connection aren't held forever.
pub struct Deferred {
    connection: ConnectionId,
    ticket: u64,
    sender: Option<channel::Sender<(ConnectionId, u64, Response)>>,
}

impl Deferred {
    pub fn reply(mut self, response: Response) {
        self.send(response);
    }

    fn send(&mut self, response: Response) {
        if let Some(sender) = self.sender.take() {
            // The receiver only goes away when the compositor exits.
            let _ = sender.send((self.connection, self.ticket, response));
        }
    }
}

impl Drop for Deferred {
    fn drop(&mut self) {
        self.send(Response::Error { message: "the request was abandoned".into() });
    }
}

enum Reply {
    Ready(Response),
    Pending(u64),
}

struct Connection {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    /// Responses waiting for an earlier, deferred one.
    replies: VecDeque<Reply>,
    subscribed: bool,
    /// The client shut down its write side; the connection closes once every reply is written.
    read_closed: bool,
    dead: bool,
    read_token: Option<RegistrationToken>,
    write_token: Option<RegistrationToken>,
}

impl Connection {
    fn finished(&self) -> bool {
        self.read_closed && self.output.is_empty() && self.replies.is_empty()
    }

    fn listening(&self) -> bool {
        self.subscribed && !self.dead && !self.read_closed
    }
}

/// What a read left of a connection.
enum ReadState {
    Open,
    ReadClosed,
    Dead,
}

pub struct IpcServer {
    path: PathBuf,
    handle: LoopHandle<'static, State>,
    connections: HashMap<ConnectionId, Connection>,
    next_id: ConnectionId,
    next_ticket: u64,
    deferred: channel::Sender<(ConnectionId, u64, Response)>,
}

impl IpcServer {
    /// Binds the socket, replacing a stale one left by a crashed compositor, and starts accepting.
    pub fn bind(path: PathBuf, handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                anyhow::bail!("another compositor is serving {}", path.display());
            }
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove stale socket {}", path.display()))?;
        }
        let listener =
            UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
        listener.set_nonblocking(true)?;
        handle
            .insert_source(
                Generic::new(listener, Interest::READ, Mode::Level),
                |_, listener, state| {
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => state.nimbus.ipc.add_connection(stream),
                            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                            Err(err) => {
                                tracing::warn!("accepting a control connection failed: {err}");
                                break;
                            }
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("cannot watch the control socket: {e}"))?;
        let (deferred, replies) = channel::channel();
        handle
            .insert_source(replies, |event, _, state| {
                if let ChannelEvent::Msg((id, ticket, response)) = event {
                    state.nimbus.ipc.complete(id, ticket, response);
                }
            })
            .map_err(|e| anyhow::anyhow!("cannot receive deferred control responses: {e}"))?;
        Ok(Self {
            path,
            handle: handle.clone(),
            connections: HashMap::new(),
            next_id: 1,
            next_ticket: 1,
            deferred,
        })
    }

    fn add_connection(&mut self, stream: UnixStream) {
        let reader = match stream.set_nonblocking(true).and_then(|()| stream.try_clone()) {
            Ok(reader) => reader,
            Err(err) => {
                tracing::warn!("cannot set up a control connection: {err}");
                return;
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let token = self.handle.insert_source(
            Generic::new(reader, Interest::READ, Mode::Level),
            move |_, reader, state| {
                let read = state.ipc_readable(id, reader);
                let Some(conn) = state.nimbus.ipc.connections.get_mut(&id) else {
                    return Ok(PostAction::Remove);
                };
                match read {
                    ReadState::Open => return Ok(PostAction::Continue),
                    ReadState::ReadClosed => conn.read_closed = true,
                    ReadState::Dead => conn.dead = true,
                }
                if conn.finished() {
                    conn.dead = true;
                }
                conn.read_token = None;
                Ok(PostAction::Remove)
            },
        );
        match token {
            Ok(token) => {
                self.connections.insert(
                    id,
                    Connection {
                        stream,
                        input: Vec::new(),
                        output: Vec::new(),
                        replies: VecDeque::new(),
                        subscribed: false,
                        read_closed: false,
                        dead: false,
                        read_token: Some(token),
                        write_token: None,
                    },
                );
            }
            Err(err) => tracing::warn!("cannot watch a control connection: {err}"),
        }
    }

    /// Queues one message line for a connection and writes what the socket accepts.
    fn send<T: serde::Serialize>(&mut self, id: ConnectionId, message: &T) {
        let Some(conn) = self.connections.get_mut(&id) else {
            return;
        };
        if conn.dead {
            return;
        }
        if let Err(err) = serde_json::to_writer(&mut conn.output, message) {
            tracing::error!("cannot serialize a control message: {err}");
            return;
        }
        conn.output.push(b'\n');
        self.flush(id);
    }

    /// Sends a response once every earlier response on the connection went out.
    fn respond(&mut self, id: ConnectionId, response: Response) {
        if let Some(conn) = self.connections.get_mut(&id) {
            conn.replies.push_back(Reply::Ready(response));
        }
        self.send_ready(id);
    }

    /// Reserves the place of a response that a worker thread sends later through the returned handle.
    fn defer(&mut self, id: ConnectionId) -> Option<Deferred> {
        let conn = self.connections.get_mut(&id)?;
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        conn.replies.push_back(Reply::Pending(ticket));
        Some(Deferred { connection: id, ticket, sender: Some(self.deferred.clone()) })
    }

    fn complete(&mut self, id: ConnectionId, ticket: u64, response: Response) {
        let Some(conn) = self.connections.get_mut(&id) else {
            return;
        };
        if let Some(reply) =
            conn.replies.iter_mut().find(|r| matches!(r, Reply::Pending(t) if *t == ticket))
        {
            *reply = Reply::Ready(response);
        }
        self.send_ready(id);
    }

    fn send_ready(&mut self, id: ConnectionId) {
        loop {
            let response = match self.connections.get_mut(&id) {
                Some(conn) if matches!(conn.replies.front(), Some(Reply::Ready(_))) => {
                    match conn.replies.pop_front() {
                        Some(Reply::Ready(response)) => response,
                        _ => return,
                    }
                }
                _ => return,
            };
            self.send(id, &response);
        }
    }

    /// Writes pending output; returns whether some remains.
    fn flush(&mut self, id: ConnectionId) -> bool {
        let Some(conn) = self.connections.get_mut(&id) else {
            return false;
        };
        while !conn.output.is_empty() && !conn.dead {
            match (&conn.stream).write(&conn.output) {
                Ok(0) => conn.dead = true,
                Ok(n) => {
                    conn.output.drain(..n);
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => {
                    tracing::debug!("control client went away: {err}");
                    conn.dead = true;
                }
            }
        }
        if conn.output.len() > MAX_PENDING_OUTPUT {
            tracing::warn!("disconnecting a control client that stopped reading");
            conn.dead = true;
        }
        if conn.finished() {
            conn.dead = true;
        }
        let pending = !conn.output.is_empty() && !conn.dead;
        if pending && conn.write_token.is_none() {
            let writer = match conn.stream.try_clone() {
                Ok(writer) => writer,
                Err(err) => {
                    tracing::debug!("cannot watch a control client for writing: {err}");
                    conn.dead = true;
                    return false;
                }
            };
            match self.handle.insert_source(
                Generic::new(writer, Interest::WRITE, Mode::Level),
                move |_, _, state| {
                    let ipc = &mut state.nimbus.ipc;
                    if ipc.flush(id) {
                        Ok(PostAction::Continue)
                    } else {
                        if let Some(conn) = ipc.connections.get_mut(&id) {
                            conn.write_token = None;
                        }
                        Ok(PostAction::Remove)
                    }
                },
            ) {
                Ok(token) => conn.write_token = Some(token),
                Err(err) => {
                    tracing::debug!("cannot watch a control client for writing: {err}");
                    conn.dead = true;
                }
            }
        }
        pending
    }

    /// Sends `events` to every subscribed connection.
    pub fn broadcast(&mut self, events: &[Event]) {
        let subscribed: Vec<ConnectionId> =
            self.connections.iter().filter(|(_, c)| c.listening()).map(|(&id, _)| id).collect();
        for id in subscribed {
            for event in events {
                self.send(id, event);
            }
        }
    }

    /// Whether any connection receives events, as the shell's does.
    pub fn has_subscribers(&self) -> bool {
        self.connections.values().any(Connection::listening)
    }

    /// Flushes every connection and drops dead ones; runs outside of connection callbacks.
    pub fn flush_all(&mut self) {
        let ids: Vec<ConnectionId> = self.connections.keys().copied().collect();
        for id in ids {
            self.flush(id);
        }
        let dead: Vec<ConnectionId> =
            self.connections.iter().filter(|(_, c)| c.dead).map(|(&id, _)| id).collect();
        for id in dead {
            if let Some(conn) = self.connections.remove(&id) {
                for token in [conn.read_token, conn.write_token].into_iter().flatten() {
                    self.handle.remove(token);
                }
            }
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != ErrorKind::NotFound
        {
            tracing::debug!("cannot remove {}: {err}", self.path.display());
        }
    }
}

/// Splits complete lines off `buffer`, skipping blank ones; fails once the unterminated rest exceeds [`MAX_LINE`].
pub fn take_lines(buffer: &mut Vec<u8>) -> Result<Vec<Vec<u8>>, ()> {
    let mut lines = Vec::new();
    while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buffer.drain(..=pos).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if !line.iter().all(u8::is_ascii_whitespace) {
            lines.push(line);
        }
    }
    if buffer.len() > MAX_LINE { Err(()) } else { Ok(lines) }
}

impl State {
    /// Reads and answers requests.
    fn ipc_readable(&mut self, id: ConnectionId, reader: &UnixStream) -> ReadState {
        let mut chunk = [0u8; 8192];
        let mut closed = false;
        let lines = {
            let Some(conn) = self.nimbus.ipc.connections.get_mut(&id) else {
                return ReadState::Dead;
            };
            loop {
                match (&*reader).read(&mut chunk) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => {
                        conn.input.extend_from_slice(&chunk[..n]);
                        if conn.input.len() > MAX_LINE {
                            break;
                        }
                    }
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    Err(err) => {
                        tracing::debug!("control client read failed: {err}");
                        return ReadState::Dead;
                    }
                }
            }
            take_lines(&mut conn.input)
        };
        let lines = match lines {
            Ok(lines) => lines,
            Err(()) => {
                self.nimbus.ipc.send(
                    id,
                    &Response::Error { message: format!("request longer than {MAX_LINE} bytes") },
                );
                return ReadState::Dead;
            }
        };
        for line in lines {
            let response = match serde_json::from_slice::<Request>(&line) {
                Ok(Request::Subscribe) => {
                    if let Some(conn) = self.nimbus.ipc.connections.get_mut(&id) {
                        conn.subscribed = true;
                    }
                    Response::Ok
                }
                Ok(Request::Screenshot { output, path: Some(path) }) => {
                    // Writing the file can block, for example on a FIFO, so a worker thread does it.
                    if let Some(reply) = self.nimbus.ipc.defer(id) {
                        self.screenshot_to(output, path, reply);
                    }
                    continue;
                }
                Ok(request) => {
                    tracing::debug!(?request, "control request");
                    self.handle_request(request)
                }
                Err(err) => Response::Error { message: format!("malformed request: {err}") },
            };
            self.nimbus.ipc.respond(id, response);
        }
        match self.nimbus.ipc.connections.get(&id) {
            Some(conn) if !conn.dead && !closed => ReadState::Open,
            // Answers to the final requests may still be pending; the write side stays usable for them.
            Some(conn) if !conn.dead => ReadState::ReadClosed,
            _ => ReadState::Dead,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_and_keeps_partial_input() {
        let mut buffer = b"{\"a\":1}\n\n{\"b\":2}\r\n{\"c\"".to_vec();
        let lines = take_lines(&mut buffer).unwrap();
        assert_eq!(lines, vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]);
        assert_eq!(buffer, b"{\"c\"".to_vec());
    }

    #[test]
    fn rejects_overlong_lines() {
        let mut buffer = vec![b'x'; MAX_LINE + 1];
        assert!(take_lines(&mut buffer).is_err());
    }
}
