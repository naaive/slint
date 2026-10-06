// SPDX-License-Identifier: MIT

//! ext-session-lock: the compositor owns the lock state, and only the client holding the lock can unlock it.

mod common;

use common::{Compositor, TIMEOUT};
use nimbus_ipc::{Event, Request};
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
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
    compositor: Option<WlCompositor>,
    outputs: Vec<WlOutput>,
    state: Option<LockState>,
    /// The size of the last configure of a lock surface.
    configured: Option<(u32, u32)>,
    /// Whether a lock surface was told which output it's on.
    entered: bool,
}

struct Locker {
    conn: Connection,
    queue: EventQueue<App>,
    app: App,
    lock: Option<ExtSessionLockV1>,
}

impl Locker {
    fn connect(compositor: &Compositor) -> Self {
        let conn = common::connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App::default();
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(app.manager.is_some(), "no ext_session_lock_manager_v1");
        Self { conn, queue, app, lock: None }
    }

    fn lock(&mut self) -> LockState {
        self.lock_with_surfaces(false)
    }

    /// Locks, creating lock surfaces for every output right away when `surfaces` is set,
    /// before the compositor confirms or refuses the lock.
    fn lock_with_surfaces(&mut self, surfaces: bool) -> LockState {
        let qh = self.queue.handle();
        self.app.state = Some(LockState::Waiting);
        let lock = self.app.manager.as_ref().unwrap().lock(&qh, ());
        if surfaces {
            let compositor = self.app.compositor.as_ref().expect("no wl_compositor");
            for output in &self.app.outputs {
                lock.get_lock_surface(&compositor.create_surface(&qh, ()), output, &qh, ());
            }
        }
        self.lock = Some(lock);
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

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        common::dispatch_until(&self.conn, &mut self.queue, &mut self.app, what, cond);
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
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "ext_session_lock_manager_v1" => app.manager = Some(registry.bind(name, 1, qh, ())),
                "wl_compositor" => app.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_output" => app.outputs.push(registry.bind(name, 1, qh, ())),
                _ => {}
            }
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

impl Dispatch<ExtSessionLockSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            surface.ack_configure(serial);
            app.configured = Some((width, height));
        }
    }
}

impl Dispatch<WlSurface, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_surface::Event::Enter { .. } = event {
            app.entered = true;
        }
    }
}

delegate_noop!(App: ignore ExtSessionLockManagerV1);
delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlOutput);

#[test]
fn only_the_lock_holder_unlocks() {
    let compositor = common::start("");
    let connect = || Locker::connect(&compositor);

    let mut owner = connect();
    assert_eq!(owner.lock(), LockState::Locked);

    // A second lock is refused while the first one's client lives.
    let mut intruder = connect();
    assert_eq!(intruder.lock(), LockState::Finished);
    // Unlocking the refused lock doesn't unlock the session.
    intruder.unlock();
    assert_eq!(connect().lock(), LockState::Finished, "the intruder unlocked the session");
    // A refused lock's surfaces are ignored instead of disconnecting the client.
    let mut eager = connect();
    assert_eq!(eager.lock_with_surfaces(true), LockState::Finished);
    eager.queue.roundtrip(&mut eager.app).expect("the refused locker was disconnected");

    // The holder can unlock, and then anyone can lock again.
    owner.unlock();
    let mut next = connect();
    assert_eq!(next.lock(), LockState::Locked);
    next.unlock();
}

#[test]
fn a_new_client_locks_after_the_holder_dies() {
    let compositor = common::start("");
    let connect = || Locker::connect(&compositor);

    let events = compositor.subscribe();

    let mut owner = connect();
    assert_eq!(owner.lock_with_surfaces(true), LockState::Locked);
    events.wait("the holder", |event| {
        (event == Event::LockState { locked: true, held: true }).then_some(())
    });
    drop(owner);
    events.wait("the shell to be asked to take over", |event| {
        (event == Event::LockState { locked: true, held: false }).then_some(())
    });
    assert!(compositor.locked(), "the dead holder unlocked the session");

    let mut next = connect();
    assert_eq!(next.lock(), LockState::Locked);
    assert_eq!(connect().lock(), LockState::Finished, "the new holder is live");
    next.unlock();
    assert!(!compositor.locked());
}

#[test]
fn the_lock_request_locks_and_asks_for_a_lock_screen() {
    let compositor = common::start("");
    let events = compositor.subscribe();
    compositor.request(Request::Lock);
    let lock_state = |event: Event| match event {
        Event::LockState { locked, held } => Some((locked, held)),
        _ => None,
    };
    assert_eq!(events.wait("the lock state", lock_state), (true, false));
    assert!(compositor.locked());
    assert!(nimbus_ipc::lock_marker_path(&compositor.runtime_dir()).exists());

    // The lock screen client takes over the lock and ends it.
    let mut locker = Locker::connect(&compositor);
    assert_eq!(locker.lock(), LockState::Locked);
    assert_eq!(events.wait("the lock state", lock_state), (true, true));
    locker.unlock();
    assert_eq!(events.wait("the lock state", lock_state), (false, false));
    assert!(!compositor.locked());
    assert!(!nimbus_ipc::lock_marker_path(&compositor.runtime_dir()).exists());
}

#[test]
fn lock_surfaces_follow_their_output() {
    let compositor = common::start("[appearance]\nscale = 1.0\n");
    let mut locker = Locker::connect(&compositor);
    assert_eq!(locker.lock_with_surfaces(true), LockState::Locked);
    locker.dispatch_until("configure with the output's size", |app| {
        app.configured == Some((1280, 720)) && app.entered
    });

    let config = compositor.dir.path().join("config.toml");
    std::fs::write(config, "[appearance]\nscale = 2.0\n").unwrap();
    locker
        .dispatch_until("configure with the scaled size", |app| app.configured == Some((640, 360)));
    locker.unlock();
    assert!(!compositor.locked());
}

#[test]
fn a_compositor_started_locked_waits_for_a_lock_client() {
    let compositor = common::compositor("").arg("--locked").start();
    assert!(compositor.locked(), "{}", compositor.log());
    assert!(nimbus_ipc::lock_marker_path(&compositor.runtime_dir()).exists());
    let mut locker = Locker::connect(&compositor);
    assert_eq!(locker.lock(), LockState::Locked);
    locker.unlock();
    assert!(!compositor.locked());
}
