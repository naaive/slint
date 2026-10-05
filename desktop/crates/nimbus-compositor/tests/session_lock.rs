// SPDX-License-Identifier: MIT

//! ext-session-lock: only the client holding the lock can unlock it.

mod common;

use common::{Compositor, TIMEOUT};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum LockState {
    Waiting,
    Locked,
    Finished,
}

#[derive(Default)]
struct App {
    manager: Option<ExtSessionLockManagerV1>,
    state: Option<LockState>,
}

struct Locker {
    conn: Connection,
    queue: EventQueue<App>,
    app: App,
    lock: Option<ExtSessionLockV1>,
}

impl Locker {
    fn connect(runtime_dir: &Path, display: &str) -> Self {
        let stream = UnixStream::connect(runtime_dir.join(display)).expect("connect");
        let conn = Connection::from_socket(stream).expect("Wayland connection");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App::default();
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(app.manager.is_some(), "no ext_session_lock_manager_v1");
        Self { conn, queue, app, lock: None }
    }

    fn lock(&mut self) -> LockState {
        let qh = self.queue.handle();
        self.app.state = Some(LockState::Waiting);
        self.lock = Some(self.app.manager.as_ref().unwrap().lock(&qh, ()));
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.queue.roundtrip(&mut self.app).expect("roundtrip");
            match self.app.state {
                Some(LockState::Waiting) | None => {}
                Some(state) => return state,
            }
            assert!(Instant::now() < deadline, "the lock was neither confirmed nor refused");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn unlock(&mut self) {
        self.lock.take().unwrap().unlock_and_destroy();
        let _ = self.conn.flush();
        // A client that never held the lock is disconnected for this; ignore the error.
        let _ = self.queue.roundtrip(&mut self.app);
    }
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event
            && interface == "ext_session_lock_manager_v1"
        {
            app.manager = Some(registry.bind(name, 1, qh, ()));
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => app.state = Some(LockState::Locked),
            ext_session_lock_v1::Event::Finished => app.state = Some(LockState::Finished),
            _ => {}
        }
    }
}

delegate_noop!(App: ignore ExtSessionLockManagerV1);

#[test]
fn only_the_lock_holder_unlocks() {
    let compositor = Compositor::start("", &[]);
    let connect = || Locker::connect(&compositor.runtime_dir(), &compositor.display);

    let mut owner = connect();
    assert_eq!(owner.lock(), LockState::Locked);

    // A second lock is refused while the first one's client lives.
    let mut intruder = connect();
    assert_eq!(intruder.lock(), LockState::Finished);
    // Unlocking the refused lock doesn't unlock the session.
    intruder.unlock();
    assert_eq!(connect().lock(), LockState::Finished, "the intruder unlocked the session");

    // The holder can unlock, and then anyone can lock again.
    owner.unlock();
    let mut next = connect();
    assert_eq!(next.lock(), LockState::Locked);
    next.unlock();
}
