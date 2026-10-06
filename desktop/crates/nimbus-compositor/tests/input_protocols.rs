// SPDX-License-Identifier: MIT

//! Pointer and tablet protocols: relative pointer, pointer gestures, pointer constraints, and tablets.

mod common;

use common::{Compositor, TestClient};
use nimbus_ipc::{LayoutMode, Request};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_pointer::WlPointer,
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1;
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1,
    zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1,
    zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1,
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::ZwpRelativePointerV1,
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2, zwp_tablet_seat_v2::ZwpTabletSeatV2,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const LOCKED_APP: &str = "org.nimbus.Locked";
const OTHER_APP: &str = "org.nimbus.Other";

#[derive(Default)]
struct Globals {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    seat: Option<WlSeat>,
    wm_base: Option<XdgWmBase>,
    relative_pointer: Option<ZwpRelativePointerManagerV1>,
    gestures: Option<ZwpPointerGesturesV1>,
    constraints: Option<ZwpPointerConstraintsV1>,
    tablets: Option<ZwpTabletManagerV2>,
    shortcuts_inhibit: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
}

#[derive(Default)]
struct App {
    globals: Globals,
    configured: bool,
    /// Every `locked` (true) and `unlocked` (false) event, in order.
    lock_events: Vec<bool>,
}

/// A client with one toplevel and the pointer protocols.
struct Probe {
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
    pointer: WlPointer,
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
        assert!(g.relative_pointer.is_some(), "missing zwp_relative_pointer_manager_v1");
        assert!(g.gestures.is_some(), "missing zwp_pointer_gestures_v1");
        assert!(g.constraints.is_some(), "missing zwp_pointer_constraints_v1");
        assert!(g.tablets.is_some(), "missing zwp_tablet_manager_v2");
        assert!(g.shortcuts_inhibit.is_some(), "missing zwp_keyboard_shortcuts_inhibit_manager_v1");
        let pointer = g.seat.as_ref().expect("wl_seat").get_pointer(&qh, ());
        let mut probe = Self { conn, queue, qh, app, pointer };
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

    /// Maps a toplevel, which the compositor sizes in the tiling layout.
    fn map_window(&mut self) -> WlSurface {
        let g = &self.app.globals;
        let surface = g.compositor.as_ref().unwrap().create_surface(&self.qh, ());
        let xdg_surface = g.wm_base.as_ref().unwrap().get_xdg_surface(&surface, &self.qh, ());
        let toplevel = xdg_surface.get_toplevel(&self.qh, ());
        toplevel.set_app_id(LOCKED_APP.into());
        surface.commit();
        self.dispatch_until("the first configure", |app| app.configured);
        common::attach_buffer(
            self.app.globals.shm.as_ref().unwrap(),
            &self.qh,
            &surface,
            (400, 300),
        );
        self.roundtrip();
        surface
    }
}

#[test]
fn pointer_and_tablet_objects_are_served() {
    let compositor = common::start("");
    let mut probe = Probe::connect(&compositor);
    let g = &probe.app.globals;
    let seat = g.seat.clone().unwrap();
    g.relative_pointer.as_ref().unwrap().get_relative_pointer(&probe.pointer, &probe.qh, ());
    let gestures = g.gestures.as_ref().unwrap();
    gestures.get_swipe_gesture(&probe.pointer, &probe.qh, ());
    gestures.get_pinch_gesture(&probe.pointer, &probe.qh, ());
    gestures.get_hold_gesture(&probe.pointer, &probe.qh, ());
    g.tablets.as_ref().unwrap().get_tablet_seat(&seat, &probe.qh, ());
    // A protocol error would fail the roundtrip.
    probe.roundtrip();
}

#[test]
fn pointer_locks_follow_pointer_and_keyboard_focus() {
    let compositor = common::start("");
    let mut probe = Probe::connect(&compositor);
    let surface = probe.map_window();
    let mut other = TestClient::connect(&compositor);
    other.create_window(OTHER_APP, "Other");
    compositor.request(Request::SetLayout { layout: LayoutMode::Tiling });
    let state = compositor.wait_state("two windows", |s| s.windows.len() == 2);
    let other_id = state.windows.iter().find(|w| w.app_id == OTHER_APP).unwrap().id;

    let constraints = probe.app.globals.constraints.clone().unwrap();
    constraints.lock_pointer(&surface, &probe.pointer, None, Lifetime::Persistent, &probe.qh, ());
    probe.roundtrip();
    assert!(probe.app.lock_events.is_empty(), "the pointer isn't on the window yet");

    // Find each window's half of the output by clicking near its top left corner,
    // which the probe's buffer covers whatever size the layout asks for.
    let click = |x: f64| {
        compositor.request(Request::Click { output: "HEADLESS-1".into(), x, y: 100.0 });
        compositor.state().windows.into_iter().find(|w| w.focused).map(|w| w.app_id)
    };
    let (locked_x, other_x) =
        if click(100.0).as_deref() == Some(LOCKED_APP) { (100.0, 740.0) } else { (740.0, 100.0) };
    assert_eq!(click(locked_x).as_deref(), Some(LOCKED_APP));
    probe.dispatch_until("the lock", |app| app.lock_events == [true]);

    // Keyboard focus moving away ends the lock while the pointer stays.
    compositor.request(Request::Activate { id: other_id });
    probe.dispatch_until("the unlock", |app| app.lock_events == [true, false]);

    assert_eq!(click(locked_x).as_deref(), Some(LOCKED_APP));
    probe.dispatch_until("the second lock", |app| app.lock_events == [true, false, true]);

    // The pointer leaving ends it too.
    assert_eq!(click(other_x).as_deref(), Some(OTHER_APP));
    probe.dispatch_until("the second unlock", |app| app.lock_events == [true, false, true, false]);
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
            "xdg_wm_base" => g.wm_base = Some(registry.bind(name, version.min(5), qh, ())),
            "zwp_relative_pointer_manager_v1" => {
                g.relative_pointer = Some(registry.bind(name, 1, qh, ()))
            }
            "zwp_pointer_gestures_v1" => {
                g.gestures = Some(registry.bind(name, version.min(3), qh, ()))
            }
            "zwp_pointer_constraints_v1" => g.constraints = Some(registry.bind(name, 1, qh, ())),
            "zwp_tablet_manager_v2" => g.tablets = Some(registry.bind(name, 1, qh, ())),
            "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                g.shortcuts_inhibit = Some(registry.bind(name, 1, qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for App {
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

impl Dispatch<XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            app.configured = true;
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => app.lock_events.push(true),
            zwp_locked_pointer_v1::Event::Unlocked => app.lock_events.push(false),
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
delegate_noop!(App: ignore WlPointer);
delegate_noop!(App: ignore XdgToplevel);
delegate_noop!(App: ignore ZwpRelativePointerManagerV1);
delegate_noop!(App: ignore ZwpRelativePointerV1);
delegate_noop!(App: ignore ZwpPointerGesturesV1);
delegate_noop!(App: ignore ZwpPointerGestureSwipeV1);
delegate_noop!(App: ignore ZwpPointerGesturePinchV1);
delegate_noop!(App: ignore ZwpPointerGestureHoldV1);
delegate_noop!(App: ignore ZwpPointerConstraintsV1);
delegate_noop!(App: ignore ZwpTabletManagerV2);
delegate_noop!(App: ignore ZwpTabletSeatV2);
delegate_noop!(App: ignore ZwpKeyboardShortcutsInhibitManagerV1);
