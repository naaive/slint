// SPDX-License-Identifier: MIT

//! Standard protocols for third-party desktop components: foreign toplevels, idle notification, and layer shell.

mod common;

use common::{Compositor, TestClient};
use nimbus_ipc::Request;
use std::time::Duration;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

/// A foreign toplevel as of its last `done` event.
#[derive(Default)]
struct Toplevel {
    handle: Option<ExtForeignToplevelHandleV1>,
    title: String,
    app_id: String,
    pending_title: Option<String>,
    pending_app_id: Option<String>,
    closed: bool,
}

#[derive(Default)]
struct Globals {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    seat: Option<WlSeat>,
    layer_shell: Option<ZwlrLayerShellV1>,
    toplevel_list: Option<ExtForeignToplevelListV1>,
    idle_notifier: Option<ExtIdleNotifierV1>,
    idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
}

#[derive(Default)]
struct App {
    globals: Globals,
    toplevels: Vec<Toplevel>,
    idle: bool,
    /// The last layer surface configure: serial, width, and height.
    layer_configure: Option<(u32, u32, u32)>,
    keyboard_focus: Option<WlSurface>,
}

/// A client of the protocols above.
struct Probe {
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
}

/// A mapped layer surface.
struct Layer {
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
}

impl Probe {
    fn connect(compositor: &Compositor) -> Self {
        let conn = common::connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App::default();
        queue.roundtrip(&mut app).expect("roundtrip");
        let g = &app.globals;
        assert!(
            g.compositor.is_some()
                && g.shm.is_some()
                && g.seat.is_some()
                && g.layer_shell.is_some()
                && g.toplevel_list.is_some()
                && g.idle_notifier.is_some()
                && g.idle_inhibit.is_some(),
            "missing globals"
        );
        let mut probe = Self { conn, queue, qh, app };
        probe.roundtrip();
        probe
    }

    fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.app).expect("roundtrip");
        self.conn.flush().expect("flush");
    }

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        common::dispatch_until(&self.conn, &mut self.queue, &mut self.app, what, cond);
    }

    /// Maps a layer surface on the top layer, anchored to the top edge, `height` tall.
    fn map_top_bar(&mut self, height: u32, exclusive_zone: i32) -> Layer {
        let surface = self.app.globals.compositor.as_ref().unwrap().create_surface(&self.qh, ());
        let layer_surface = self.app.globals.layer_shell.as_ref().unwrap().get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Top,
            "probe".into(),
            &self.qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        layer_surface.set_size(0, height);
        layer_surface.set_exclusive_zone(exclusive_zone);
        self.app.layer_configure = None;
        surface.commit();
        self.dispatch_until("the layer surface configure", |app| app.layer_configure.is_some());
        let (serial, width, height) = self.app.layer_configure.unwrap();
        layer_surface.ack_configure(serial);
        let size = (i32::try_from(width).unwrap(), i32::try_from(height).unwrap());
        common::attach_buffer(self.app.globals.shm.as_ref().unwrap(), &self.qh, &surface, size);
        self.roundtrip();
        Layer { surface, layer_surface }
    }
}

#[test]
fn foreign_toplevels_follow_windows() {
    let compositor = common::start("");
    let mut probe = Probe::connect(&compositor);
    let mut client = TestClient::connect(&compositor);
    let index = client.create_window("org.nimbus.Listed", "First title");
    probe.dispatch_until("a listed toplevel", |app| {
        app.toplevels
            .iter()
            .any(|t| !t.closed && t.title == "First title" && t.app_id == "org.nimbus.Listed")
    });

    client.app.windows[index].toplevel.set_title("Second title".into());
    client.roundtrip();
    probe.dispatch_until("the new title", |app| {
        app.toplevels.iter().any(|t| !t.closed && t.title == "Second title")
    });

    // Attaching no buffer unmaps the window.
    let surface = client.app.windows[index].surface.clone();
    surface.attach(None, 0, 0);
    surface.commit();
    client.roundtrip();
    probe.dispatch_until("the closed toplevel", |app| app.toplevels.iter().all(|t| t.closed));
    assert_eq!(probe.app.toplevels.len(), 1);
}

#[test]
fn idle_notifications_fire_unless_inhibited() {
    let compositor = common::start("");
    let mut probe = Probe::connect(&compositor);
    let bar = probe.map_top_bar(20, 0);
    let inhibitor = probe.app.globals.idle_inhibit.as_ref().unwrap().create_inhibitor(
        &bar.surface,
        &probe.qh,
        (),
    );
    probe.roundtrip();
    let seat = probe.app.globals.seat.clone().unwrap();
    probe.app.globals.idle_notifier.as_ref().unwrap().get_idle_notification(
        100,
        &seat,
        &probe.qh,
        (),
    );
    probe.roundtrip();
    std::thread::sleep(Duration::from_millis(500));
    probe.roundtrip();
    assert!(!probe.app.idle, "a visible surface inhibits idling");

    inhibitor.destroy();
    probe.dispatch_until("the idle notification", |app| app.idle);
}

#[test]
fn layer_surfaces_reserve_space_and_take_the_keyboard() {
    let compositor = common::start("");
    let mut probe = Probe::connect(&compositor);
    let seat = probe.app.globals.seat.clone().unwrap();
    seat.get_keyboard(&probe.qh, ());
    let bar = probe.map_top_bar(40, 40);

    let mut client = TestClient::connect(&compositor);
    let index = client.create_window("org.nimbus.Big", "Big");
    let id = compositor.wait_state("the window", |s| s.windows.len() == 1).windows[0].id;
    compositor.request(Request::SetMaximized { id, maximized: true });
    client.dispatch_until("the maximized size below the bar", |app| {
        app.windows[index].requested == (1280, 680)
    });

    bar.layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    bar.surface.commit();
    probe.dispatch_until("keyboard focus on the bar", |app| {
        app.keyboard_focus.as_ref() == Some(&bar.surface)
    });
    bar.layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    bar.surface.commit();
    probe.dispatch_until("keyboard focus back on the window", |app| app.keyboard_focus.is_none());

    // Without the bar, maximized windows cover the whole output.
    bar.layer_surface.destroy();
    bar.surface.destroy();
    probe.roundtrip();
    client.dispatch_until("the full maximized size", |app| {
        app.windows[index].requested == (1280, 720)
    });
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
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        let g = &mut app.globals;
        match interface.as_str() {
            "wl_compositor" => g.compositor = Some(registry.bind(name, version.min(5), qh, ())),
            "wl_shm" => g.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_seat" => g.seat = Some(registry.bind(name, version.min(7), qh, ())),
            "zwlr_layer_shell_v1" => {
                g.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
            }
            "ext_foreign_toplevel_list_v1" => {
                g.toplevel_list = Some(registry.bind(name, 1, qh, ()))
            }
            "ext_idle_notifier_v1" => g.idle_notifier = Some(registry.bind(name, 1, qh, ())),
            "zwp_idle_inhibit_manager_v1" => g.idle_inhibit = Some(registry.bind(name, 1, qh, ())),
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            app.toplevels.push(Toplevel { handle: Some(toplevel), ..Toplevel::default() });
        }
    }

    event_created_child!(App, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for App {
    fn event(
        app: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(toplevel) = app.toplevels.iter_mut().find(|t| t.handle.as_ref() == Some(handle))
        else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                toplevel.pending_title = Some(title)
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                toplevel.pending_app_id = Some(app_id)
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(title) = toplevel.pending_title.take() {
                    toplevel.title = title;
                }
                if let Some(app_id) = toplevel.pending_app_id.take() {
                    toplevel.app_id = app_id;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                toplevel.closed = true;
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => app.idle = true,
            ext_idle_notification_v1::Event::Resumed => app.idle = false,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = event {
            app.layer_configure = Some((serial, width, height));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => app.keyboard_focus = Some(surface),
            wl_keyboard::Event::Leave { .. } => app.keyboard_focus = None,
            _ => {}
        }
    }
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: ignore WlSeat);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore ExtIdleNotifierV1);
delegate_noop!(App: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(App: ignore ZwpIdleInhibitorV1);
