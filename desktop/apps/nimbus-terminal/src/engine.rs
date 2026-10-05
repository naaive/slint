// SPDX-License-Identifier: MIT

//! Terminal sessions: an `alacritty_terminal` grid fed by a shell on a PTY, whose I/O runs on its own thread.

use std::borrow::Cow;
use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event, EventListener, VoidListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Processor;

/// Identifies a session in events from its threads.
pub type SessionId = u64;

/// The size of a terminal grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub columns: usize,
    pub lines: usize,
}

impl GridSize {
    pub const DEFAULT: Self = Self { columns: 80, lines: 24 };

    /// The size passed to the PTY, with cell sizes in physical pixels.
    pub fn window_size(self, cell_width: u32, cell_height: u32) -> WindowSize {
        let clamp = |v: usize| u16::try_from(v).unwrap_or(u16::MAX);
        let clamp32 = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
        WindowSize {
            num_lines: clamp(self.lines),
            num_cols: clamp(self.columns),
            cell_width: clamp32(cell_width),
            cell_height: clamp32(cell_height),
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Feeds `bytes` to `term` as if a program had written them.
pub fn feed<T: EventListener>(term: &mut Term<T>, bytes: &[u8]) {
    let mut processor: Processor = Processor::new();
    processor.advance(term, bytes);
}

/// A terminal with no PTY behind it, for sample content and tests.
pub fn memory_term(columns: usize, lines: usize) -> Term<VoidListener> {
    let size = GridSize { columns: columns.max(2), lines: lines.max(1) };
    Term::new(Config::default(), &size, VoidListener)
}

#[cfg(test)]
pub(crate) fn test_term(columns: usize, lines: usize) -> Term<VoidListener> {
    memory_term(columns, lines)
}

/// What a session's threads report to the UI.
#[derive(Debug)]
pub enum SessionEvent {
    /// An event from the terminal emulation or its I/O loop.
    Term(Event),
    /// The child process started.
    Started(PtyHandle),
    /// The child process couldn't be started.
    Failed(String),
}

/// Receives session events on any thread.
pub type EventSink = Arc<dyn Fn(SessionId, SessionEvent) + Send + Sync>;

/// The terminal's event listener; forwards everything to the sink with the session id.
#[derive(Clone)]
pub struct EventProxy {
    id: SessionId,
    sink: EventSink,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        if !matches!(event, Event::MouseCursorDirty) {
            (self.sink)(self.id, SessionEvent::Term(event));
        }
    }
}

/// The running I/O loop of a session.
pub struct PtyHandle {
    sender: EventLoopSender,
    child_pid: u32,
    /// A duplicate of the PTY's controlling side, for querying its foreground process group.
    master: OwnedFd,
    thread: JoinHandle<()>,
}

impl PtyHandle {
    /// Ends the I/O loop and hangs up the child, for a session that no longer exists.
    pub fn stop(self) {
        stop(self);
    }
}

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHandle").field("child_pid", &self.child_pid).finish_non_exhaustive()
    }
}

/// How to start a session's child process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnOptions {
    /// The program and its arguments; the user's shell when `None`.
    pub command: Option<(String, Vec<String>)>,
    pub working_directory: Option<PathBuf>,
}

enum PtyState {
    /// Starting; input and resizes wait here.
    Pending(Vec<Msg>),
    Running(PtyHandle),
    Stopped,
}

/// One terminal with its child process.
pub struct Session {
    pub id: SessionId,
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    pty: PtyState,
}

/// Environment variables that describe the terminal to its programs.
fn child_environment() -> HashMap<String, String> {
    HashMap::from([
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "nimbus-terminal".into()),
        ("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into()),
    ])
}

impl Session {
    /// Creates the terminal now and starts the child process on another thread.
    ///
    /// The sink receives [`SessionEvent::Started`] or [`SessionEvent::Failed`], to pass to [`Session::started`].
    pub fn spawn(
        id: SessionId,
        options: SpawnOptions,
        size: WindowSize,
        config: Config,
        sink: EventSink,
    ) -> Self {
        let proxy = EventProxy { id, sink: sink.clone() };
        let grid = GridSize {
            columns: usize::from(size.num_cols.max(2)),
            lines: usize::from(size.num_lines.max(1)),
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &grid, proxy.clone())));
        let pty_options = tty::Options {
            shell: options.command.map(|(program, args)| tty::Shell::new(program, args)),
            working_directory: options.working_directory,
            drain_on_exit: true,
            env: child_environment(),
        };
        let thread_term = term.clone();
        let started =
            std::thread::Builder::new().name(format!("pty-start-{id}")).spawn(move || {
                let event = match start_pty(&pty_options, size, id, thread_term, proxy) {
                    Ok(handle) => SessionEvent::Started(handle),
                    Err(error) => SessionEvent::Failed(error.to_string()),
                };
                sink(id, event);
            });
        let pty = match started {
            Ok(_) => PtyState::Pending(Vec::new()),
            Err(error) => {
                tracing::error!("cannot start a thread for the terminal: {error}");
                PtyState::Stopped
            }
        };
        Self { id, term, pty }
    }

    /// A session without a child process, whose content comes from [`feed`]; for samples and tests.
    pub fn detached(id: SessionId, size: GridSize, config: Config, sink: EventSink) -> Self {
        let proxy = EventProxy { id, sink };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, proxy)));
        Self { id, term, pty: PtyState::Stopped }
    }

    /// Takes the running I/O loop from [`SessionEvent::Started`], and sends it the queued input.
    pub fn started(&mut self, handle: PtyHandle) {
        if let PtyState::Pending(queued) = std::mem::replace(&mut self.pty, PtyState::Stopped) {
            for msg in queued {
                let _ = handle.sender.send(msg);
            }
            self.pty = PtyState::Running(handle);
        } else {
            // The session was closed while starting.
            stop(handle);
        }
    }

    /// Marks the child as gone, after it exited or failed to start.
    pub fn stopped(&mut self) {
        if let PtyState::Running(handle) = std::mem::replace(&mut self.pty, PtyState::Stopped) {
            stop(handle);
        }
    }

    pub fn is_running(&self) -> bool {
        !matches!(self.pty, PtyState::Stopped)
    }

    fn send(&mut self, msg: Msg) {
        match &mut self.pty {
            PtyState::Pending(queue) => queue.push(msg),
            PtyState::Running(handle) => {
                if let Err(error) = handle.sender.send(msg) {
                    tracing::debug!("the terminal's I/O loop is gone: {error}");
                }
            }
            PtyState::Stopped => {}
        }
    }

    /// Writes `bytes` to the child's input.
    pub fn write(&mut self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            self.send(Msg::Input(bytes));
        }
    }

    /// Resizes the grid and tells the child about the new size.
    pub fn resize(&mut self, size: WindowSize) {
        let grid = GridSize {
            columns: usize::from(size.num_cols.max(2)),
            lines: usize::from(size.num_lines.max(1)),
        };
        {
            let mut term = self.term.lock();
            if term.columns() == grid.columns && term.screen_lines() == grid.lines {
                return;
            }
            term.resize(grid);
        }
        // Only the latest size matters to a child that hasn't started yet.
        if let PtyState::Pending(queue) = &mut self.pty {
            queue.retain(|msg| !matches!(msg, Msg::Resize(_)));
        }
        self.send(Msg::Resize(size));
    }

    /// The child's process id and a handle to the PTY, for inspecting the processes inside on another thread.
    pub fn process_handle(&self) -> Option<(u32, OwnedFd)> {
        match &self.pty {
            PtyState::Running(handle) => {
                handle.master.as_fd().try_clone_to_owned().ok().map(|fd| (handle.child_pid, fd))
            }
            _ => None,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stopped();
    }
}

/// Asks the I/O loop to finish, which hangs up the child, and reaps the thread in the background.
fn stop(handle: PtyHandle) {
    let _ = handle.sender.send(Msg::Shutdown);
    let reaper = std::thread::Builder::new().name("pty-reaper".into()).spawn(move || {
        let _ = handle.thread.join();
    });
    if let Err(error) = reaper {
        tracing::warn!("cannot reap the terminal's I/O thread: {error}");
    }
}

fn start_pty(
    options: &tty::Options,
    size: WindowSize,
    id: SessionId,
    term: Arc<FairMutex<Term<EventProxy>>>,
    proxy: EventProxy,
) -> std::io::Result<PtyHandle> {
    let pty = tty::new(options, size, id)?;
    let child_pid = pty.child().id();
    let master = pty.file().as_fd().try_clone_to_owned()?;
    let event_loop = EventLoop::new(term, proxy, pty, options.drain_on_exit, false)?;
    let sender = event_loop.channel();
    let io = event_loop.spawn();
    let thread = std::thread::Builder::new().name(format!("pty-wait-{id}")).spawn(move || {
        let _ = io.join();
    })?;
    Ok(PtyHandle { sender, child_pid, master, thread })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn grid_size_converts_and_clamps() {
        let size = GridSize { columns: 100, lines: 70_000 }.window_size(9, 100_000);
        assert_eq!((size.num_cols, size.num_lines), (100, u16::MAX));
        assert_eq!((size.cell_width, size.cell_height), (9, u16::MAX));
    }

    #[test]
    fn memory_terms_render_fed_text() {
        let mut term = memory_term(10, 3);
        feed(&mut term, b"hi\r\n\x1b[31mred");
        let line: String = (0..3)
            .map(|c| {
                term.grid()[alacritty_terminal::index::Line(1)]
                    [alacritty_terminal::index::Column(c)]
                .c
            })
            .collect();
        assert_eq!(line, "red");
        assert_eq!(term.columns(), 10);
        let tiny = memory_term(0, 0);
        assert_eq!((tiny.columns(), tiny.screen_lines()), (2, 1));
    }

    /// Runs `/bin/sh -c` in a real PTY and collects the events until the child exits.
    #[test]
    fn spawns_a_command_and_reports_its_exit() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |id, event| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((id, event));
            }
        });
        let options = SpawnOptions {
            command: Some((
                "/bin/sh".into(),
                vec!["-c".into(), "printf 'hello %s' \"$TERM\"; sleep 0.3; exit 3".into()],
            )),
            working_directory: Some(std::env::temp_dir()),
        };
        let mut session = Session::spawn(
            7,
            options,
            GridSize::DEFAULT.window_size(8, 16),
            Config::default(),
            sink,
        );
        assert!(session.is_running());
        session.resize(GridSize { columns: 90, lines: 30 }.window_size(8, 16));

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exit_code = None;
        while Instant::now() < deadline && exit_code.is_none() {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok((7, SessionEvent::Started(handle))) => session.started(handle),
                Ok((7, SessionEvent::Failed(error))) => panic!("spawn failed: {error}"),
                Ok((7, SessionEvent::Term(Event::ChildExit(status)))) => exit_code = status.code(),
                Ok(_) | Err(_) => {}
            }
        }
        assert_eq!(exit_code, Some(3));
        // The output arrives before the exit, with the drain on exit.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text: String = {
                let term = session.term.lock();
                (0..term.screen_lines() as i32)
                    .flat_map(|l| (0..term.columns()).map(move |c| (l, c)))
                    .map(|(l, c)| {
                        term.grid()[alacritty_terminal::index::Line(l)]
                            [alacritty_terminal::index::Column(c)]
                        .c
                    })
                    .collect()
            };
            if text.contains("hello xterm-256color") || Instant::now() > deadline {
                assert!(text.contains("hello xterm-256color"), "{:?}", text.trim());
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(session.term.lock().columns(), 90);
        session.stopped();
        assert!(!session.is_running());
        session.write(&b"ignored"[..]);
    }

    #[test]
    fn missing_programs_fail_to_start() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EventSink = Arc::new(move |_, event| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(event);
            }
        });
        let options = SpawnOptions {
            command: Some(("/nonexistent/nimbus-program".into(), Vec::new())),
            working_directory: None,
        };
        let _session = Session::spawn(
            1,
            options,
            GridSize::DEFAULT.window_size(8, 16),
            Config::default(),
            sink,
        );
        let failed = (0..100).find_map(|_| match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(SessionEvent::Failed(error)) => Some(error),
            _ => None,
        });
        assert!(failed.is_some_and(|e| e.contains("nimbus-program")));
    }
}
