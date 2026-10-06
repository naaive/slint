// SPDX-License-Identifier: MIT

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_registry::{self, WlRegistry},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use crate::{Compositor, TIMEOUT};

/// Connects to the Wayland socket at `socket`.
pub fn connect(socket: &Path) -> Connection {
    let stream = UnixStream::connect(socket).expect("connect to the Wayland socket");
    Connection::from_socket(stream).expect("Wayland connection")
}

/// Dispatches `queue` until `cond` holds for `state`, and fails after [`TIMEOUT`].
///
/// Each round flushes the requests that event handlers queued, which a roundtrip doesn't send.
pub fn dispatch_until<D: 'static>(
    conn: &Connection,
    queue: &mut EventQueue<D>,
    state: &mut D,
    what: &str,
    cond: impl Fn(&D) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        queue.roundtrip(state).expect("roundtrip");
        conn.flush().expect("flush");
        if cond(state) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Attaches and commits an opaque white buffer of `width` x `height` pixels.
pub fn attach_buffer<D>(
    shm: &WlShm,
    qh: &QueueHandle<D>,
    surface: &WlSurface,
    (width, height): (i32, i32),
) where
    D: Dispatch<WlShmPool, ()> + Dispatch<WlBuffer, ()> + 'static,
{
    let stride = width * 4;
    let len = usize::try_from(stride * height).unwrap();
    let mut file = tempfile::tempfile().expect("shm file");
    file.write_all(&vec![0xffu8; len]).unwrap();
    let pool = shm.create_pool(file.as_fd(), stride * height, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
}

/// An xdg toplevel of a [`TestClient`], showing a white buffer of the size the compositor asks for.
pub struct TestWindow {
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    pub configured: bool,
    /// The last size the compositor asked for; (0, 0) lets the client choose.
    pub requested: (i32, i32),
    /// The size of the attached buffer.
    pub size: (i32, i32),
    pub states: Vec<xdg_toplevel::State>,
    pub close_requested: bool,
    pub destroyed: bool,
    pending: (i32, i32),
    pending_states: Vec<xdg_toplevel::State>,
}

#[derive(Default)]
pub struct TestClientState {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    pub windows: Vec<TestWindow>,
    /// The buffer size of windows the compositor lets choose their size.
    pub default_size: (i32, i32),
}

/// A Wayland client with xdg toplevels.
pub struct TestClient {
    pub conn: Connection,
    queue: EventQueue<TestClientState>,
    qh: QueueHandle<TestClientState>,
    pub app: TestClientState,
}

impl TestClient {
    pub fn connect(compositor: &Compositor) -> Self {
        let conn = connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = TestClientState { default_size: (400, 300), ..TestClientState::default() };
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(
            app.compositor.is_some() && app.shm.is_some() && app.wm_base.is_some(),
            "missing globals"
        );
        Self { conn, queue, qh, app }
    }

    /// Creates a toplevel and returns its index once it's mapped with a buffer.
    pub fn create_window(&mut self, app_id: &str, title: &str) -> usize {
        let index = self.app.windows.len();
        let compositor = self.app.compositor.as_ref().unwrap();
        let surface = compositor.create_surface(&self.qh, ());
        let xdg_surface =
            self.app.wm_base.as_ref().unwrap().get_xdg_surface(&surface, &self.qh, index);
        let toplevel = xdg_surface.get_toplevel(&self.qh, index);
        toplevel.set_app_id(app_id.into());
        toplevel.set_title(title.into());
        surface.commit();
        self.app.windows.push(TestWindow {
            surface,
            xdg_surface,
            toplevel,
            configured: false,
            requested: (0, 0),
            size: (0, 0),
            states: Vec::new(),
            close_requested: false,
            destroyed: false,
            pending: (0, 0),
            pending_states: Vec::new(),
        });
        self.dispatch_until("the first configure", |app| app.windows[index].configured);
        index
    }

    pub fn destroy_window(&mut self, index: usize) {
        let window = &mut self.app.windows[index];
        window.toplevel.destroy();
        window.xdg_surface.destroy();
        window.surface.destroy();
        window.destroyed = true;
        self.conn.flush().expect("flush");
    }

    pub fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.app).expect("roundtrip");
        self.conn.flush().expect("flush");
    }

    pub fn dispatch_until(&mut self, what: &str, cond: impl Fn(&TestClientState) -> bool) {
        dispatch_until(&self.conn, &mut self.queue, &mut self.app, what, cond);
    }
}

impl Dispatch<WlRegistry, ()> for TestClientState {
    fn event(
        app: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    app.compositor = Some(registry.bind(name, version.min(5), qh, ()))
                }
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, version.min(5), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgWmBase, ()> for TestClientState {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, usize> for TestClientState {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        &index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            let window = &mut app.windows[index];
            if window.destroyed {
                return;
            }
            window.requested = window.pending;
            window.states = window.pending_states.clone();
            let size = if window.pending.0 > 0 && window.pending.1 > 0 {
                window.pending
            } else if window.size.0 > 0 {
                window.size
            } else {
                app.default_size
            };
            window.configured = true;
            let changed = size != window.size;
            window.size = size;
            if changed {
                attach_buffer(app.shm.as_ref().unwrap(), qh, &window.surface, size);
            } else {
                window.surface.commit();
            }
        }
    }
}

impl Dispatch<XdgToplevel, usize> for TestClientState {
    fn event(
        app: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        &index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let window = &mut app.windows[index];
        match event {
            xdg_toplevel::Event::Configure { width, height, states } => {
                window.pending = (width, height);
                window.pending_states = states
                    .chunks_exact(4)
                    .filter_map(|c| {
                        xdg_toplevel::State::try_from(u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                            .ok()
                    })
                    .collect();
            }
            xdg_toplevel::Event::Close => window.close_requested = true,
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, ()> for TestClientState {
    fn event(
        _: &mut Self,
        buffer: &WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

delegate_noop!(TestClientState: ignore WlCompositor);
delegate_noop!(TestClientState: ignore WlSurface);
delegate_noop!(TestClientState: ignore WlShm);
delegate_noop!(TestClientState: ignore WlShmPool);
