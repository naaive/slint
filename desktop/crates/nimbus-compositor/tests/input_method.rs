// SPDX-License-Identifier: MIT

//! Input methods: a text-input-v3 client, an input-method-v2 input method, and a virtual keyboard.

mod common;

use common::{Compositor, TestClient};
use nimbus_ipc::Request;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ContentHint, ContentPurpose, ZwpTextInputV3},
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::XdgPositioner,
    xdg_surface::{self, XdgSurface},
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

/// Linux input event codes of the A and B keys.
const KEY_A: u32 = 30;
const KEY_B: u32 = 48;

/// Text-input events that take effect with the next `done`.
#[derive(Default)]
struct PendingText {
    preedit: Option<(String, i32, i32)>,
    commit: Option<String>,
}

#[derive(Default)]
struct TypistState {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    seat: Option<WlSeat>,
    layer_shell: Option<ZwlrLayerShellV1>,
    wm_base: Option<XdgWmBase>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    /// The surface the text input entered.
    focus: Option<WlSurface>,
    pending: PendingText,
    preedit: Option<(String, i32, i32)>,
    committed: String,
    /// Keys that reached the client's `wl_keyboard`, as pressed or released.
    keys: Vec<(u32, bool)>,
    keyboard_focus: Option<WlSurface>,
    layer_configure: Option<u32>,
    popup_configured: bool,
    popup_done: bool,
}

/// An xdg popup of a [`Typist`].
struct Popup {
    surface: WlSurface,
    xdg_surface: XdgSurface,
    popup: XdgPopup,
}

/// A text-input client: windows from a [`TestClient`], and a text input and keyboard on the same connection.
struct Typist {
    client: TestClient,
    queue: EventQueue<TypistState>,
    qh: QueueHandle<TypistState>,
    state: TypistState,
    text_input: ZwpTextInputV3,
}

impl Typist {
    fn connect(compositor: &Compositor) -> Self {
        let client = TestClient::connect(compositor);
        let mut queue = client.conn.new_event_queue();
        let qh = queue.handle();
        client.conn.display().get_registry(&qh, ());
        let mut state = TypistState::default();
        queue.roundtrip(&mut state).expect("roundtrip");
        let seat = state.seat.clone().expect("a seat");
        seat.get_keyboard(&qh, ());
        let manager = state.text_input_manager.as_ref().expect("zwp_text_input_manager_v3");
        let text_input = manager.get_text_input(&seat, &qh, ());
        Self { client, queue, qh, state, text_input }
    }

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&TypistState) -> bool) {
        common::wait_for(what, || {
            self.client.roundtrip();
            self.queue.roundtrip(&mut self.state).expect("roundtrip");
            cond(&self.state).then_some(())
        });
    }

    /// Opens a window and waits until the text input enters it.
    fn open_window(&mut self) -> WlSurface {
        let index = self.client.create_window("org.nimbus.Typist", "Typist");
        let surface = self.client.app.windows[index].surface.clone();
        self.dispatch_until("text input focus on the window", |s| {
            s.focus.as_ref() == Some(&surface)
        });
        surface
    }

    /// Enables the text input with "hello" around the cursor at (10, 20) in the focused surface.
    fn enable(&mut self) {
        self.text_input.enable();
        self.text_input.set_surrounding_text("hello".into(), 5, 5);
        self.text_input.set_content_type(ContentHint::None, ContentPurpose::Normal);
        self.text_input.set_cursor_rectangle(10, 20, 2, 16);
        self.text_input.commit();
        self.client.conn.flush().expect("flush");
    }

    fn disable(&mut self) {
        self.text_input.disable();
        self.text_input.commit();
        self.client.conn.flush().expect("flush");
    }

    /// Maps an overlay layer surface that takes the keyboard.
    fn map_exclusive_layer(&mut self) -> (WlSurface, ZwlrLayerSurfaceV1) {
        let surface = self.state.compositor.as_ref().unwrap().create_surface(&self.qh, ());
        let layer_surface = self.state.layer_shell.as_ref().unwrap().get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Overlay,
            "typist".into(),
            &self.qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        layer_surface.set_size(0, 40);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.commit();
        self.dispatch_until("the layer surface configure", |s| s.layer_configure.is_some());
        layer_surface.ack_configure(self.state.layer_configure.unwrap());
        common::attach_buffer(self.state.shm.as_ref().unwrap(), &self.qh, &surface, (1280, 40));
        (surface, layer_surface)
    }

    /// Maps a popup of the window with `parent`, grabbing the keyboard and pointer.
    fn open_popup(&mut self, parent: &XdgSurface) -> Popup {
        let wm_base = self.state.wm_base.clone().expect("xdg_wm_base");
        let surface = self.state.compositor.as_ref().unwrap().create_surface(&self.qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &self.qh, ());
        let positioner = wm_base.create_positioner(&self.qh, ());
        positioner.set_size(100, 50);
        positioner.set_anchor_rect(0, 0, 10, 10);
        let popup = xdg_surface.get_popup(Some(parent), &positioner, &self.qh, ());
        positioner.destroy();
        // smithay doesn't check grab serials; any serial will do while the pointer isn't grabbed.
        popup.grab(self.state.seat.as_ref().unwrap(), 0);
        surface.commit();
        self.dispatch_until("the popup configure", |s| s.popup_configured);
        common::attach_buffer(self.state.shm.as_ref().unwrap(), &self.qh, &surface, (100, 50));
        self.dispatch_until("keyboard focus on the popup", |s| {
            s.keyboard_focus.as_ref() == Some(&surface)
        });
        Popup { surface, xdg_surface, popup }
    }

    fn take_keys(&mut self) -> Vec<(u32, bool)> {
        std::mem::take(&mut self.state.keys)
    }
}

#[derive(Default)]
struct ImeState {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    seat: Option<WlSeat>,
    manager: Option<ZwpInputMethodManagerV2>,
    virtual_keyboards: Option<ZwpVirtualKeyboardManagerV1>,
    lock_manager: Option<ExtSessionLockManagerV1>,
    pending_active: bool,
    pending_surrounding: Option<String>,
    /// Whether the input method is active, as of the last `done`.
    active: bool,
    surrounding: Option<String>,
    /// The number of `done` events, which `commit` passes back.
    serial: u32,
    keymap: Option<(OwnedFd, u32)>,
    /// Keys that reached the keyboard grab, as pressed or released.
    keys: Vec<(u32, bool)>,
    popup_rectangle: Option<(i32, i32, i32, i32)>,
    locked: bool,
}

/// An input method, such as fcitx5 would be.
struct Ime {
    conn: Connection,
    queue: EventQueue<ImeState>,
    qh: QueueHandle<ImeState>,
    state: ImeState,
    input_method: ZwpInputMethodV2,
}

impl Ime {
    fn connect(compositor: &Compositor) -> Self {
        let conn = common::connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = ImeState::default();
        queue.roundtrip(&mut state).expect("roundtrip");
        let seat = state.seat.as_ref().expect("a seat");
        let manager = state.manager.as_ref().expect("zwp_input_method_manager_v2");
        let input_method = manager.get_input_method(seat, &qh, ());
        queue.roundtrip(&mut state).expect("roundtrip");
        Self { conn, queue, qh, state, input_method }
    }

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&ImeState) -> bool) {
        common::dispatch_until(&self.conn, &mut self.queue, &mut self.state, what, cond);
    }

    fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.state).expect("roundtrip");
    }

    fn commit(&mut self) {
        self.input_method.commit(self.state.serial);
        self.conn.flush().expect("flush");
    }
}

#[test]
fn preedit_and_commit_strings_reach_the_focused_text_input() {
    let compositor = common::start("");
    let mut ime = Ime::connect(&compositor);
    let mut typist = Typist::connect(&compositor);
    typist.open_window();
    typist.enable();
    ime.dispatch_until("activation with the surrounding text", |s| {
        s.active && s.surrounding.as_deref() == Some("hello")
    });

    ime.input_method.set_preedit_string("ni".into(), 0, 2);
    ime.commit();
    typist.dispatch_until("the preedit string", |s| {
        s.preedit == Some(("ni".into(), 0, 2)) && s.committed.is_empty()
    });

    ime.input_method.commit_string("你".into());
    ime.commit();
    typist.dispatch_until("the committed string", |s| s.committed == "你" && s.preedit.is_none());
}

#[test]
fn the_input_method_follows_keyboard_focus() {
    let compositor = common::start("");
    let mut ime = Ime::connect(&compositor);
    let mut typist = Typist::connect(&compositor);
    let window = typist.open_window();
    typist.enable();
    ime.dispatch_until("activation", |s| s.active);

    // A new window of another client takes the focus.
    let mut other = TestClient::connect(&compositor);
    other.create_window("org.nimbus.Other", "Other");
    typist.dispatch_until("text input focus leaving", |s| s.focus.is_none());
    ime.dispatch_until("deactivation", |s| !s.active);

    // The text input enters the window again with the focus, and enables itself anew.
    let id = compositor
        .state()
        .windows
        .iter()
        .find(|w| w.app_id == "org.nimbus.Typist")
        .expect("the typist's window")
        .id;
    compositor.request(Request::Activate { id });
    typist.dispatch_until("text input focus on the window", |s| s.focus.as_ref() == Some(&window));
    typist.enable();
    ime.dispatch_until("activation", |s| s.active);

    // A layer surface that takes the keyboard takes the text input too.
    let (layer, layer_surface) = typist.map_exclusive_layer();
    typist.dispatch_until("text input focus on the layer surface", |s| {
        s.focus.as_ref() == Some(&layer)
    });
    ime.dispatch_until("deactivation", |s| !s.active);
    typist.enable();
    ime.dispatch_until("activation on the layer surface", |s| s.active);
    layer_surface.destroy();
    layer.destroy();
    typist.dispatch_until("text input focus on the window", |s| s.focus.as_ref() == Some(&window));
    ime.dispatch_until("deactivation", |s| !s.active);

    typist.enable();
    ime.dispatch_until("activation", |s| s.active);
    typist.disable();
    ime.dispatch_until("deactivation", |s| !s.active);
}

#[test]
fn the_input_method_grabs_the_keyboard_and_sends_keys_back() {
    let compositor = common::start("");
    let mut ime = Ime::connect(&compositor);
    let mut typist = Typist::connect(&compositor);
    typist.open_window();
    typist.enable();
    ime.dispatch_until("activation", |s| s.active);
    let grab = ime.input_method.grab_keyboard(&ime.qh, ());
    ime.dispatch_until("the grab's keymap", |s| s.keymap.is_some());

    compositor.request(Request::PressKey { code: KEY_A });
    ime.dispatch_until("the grabbed key", |s| s.keys == [(KEY_A, true), (KEY_A, false)]);

    // The input method passes the key on through a virtual keyboard.
    let seat = ime.state.seat.clone().unwrap();
    let keyboard =
        ime.state.virtual_keyboards.as_ref().unwrap().create_virtual_keyboard(&seat, &ime.qh, ());
    let (fd, size) = ime.state.keymap.as_ref().unwrap();
    keyboard.keymap(wl_keyboard::KeymapFormat::XkbV1.into(), fd.as_fd(), *size);
    keyboard.key(0, KEY_A, wl_keyboard::KeyState::Pressed.into());
    keyboard.key(0, KEY_A, wl_keyboard::KeyState::Released.into());
    ime.conn.flush().expect("flush");
    typist.dispatch_until("the forwarded key", |s| s.keys == [(KEY_A, true), (KEY_A, false)]);

    // The lock screen's keys never reach the input method, which gets the keyboard back after unlocking.
    let lock = ime.state.lock_manager.as_ref().unwrap().lock(&ime.qh, ());
    ime.dispatch_until("the session lock", |s| s.locked);
    compositor.request(Request::PressKey { code: KEY_B });
    ime.roundtrip();
    assert_eq!(ime.state.keys.len(), 2, "a locked session's key reached the input method");
    lock.unlock_and_destroy();
    ime.conn.flush().expect("flush");
    common::wait_for("the unlocked session", || (!compositor.locked()).then_some(()));
    compositor.request(Request::PressKey { code: KEY_B });
    ime.dispatch_until("the grabbed key after unlocking", |s| {
        s.keys.ends_with(&[(KEY_B, true), (KEY_B, false)])
    });

    // Released, the keyboard goes to the window again.
    grab.release();
    ime.conn.flush().expect("flush");
    ime.roundtrip();
    compositor.request(Request::PressKey { code: KEY_B });
    typist.dispatch_until("the key after the release", |s| {
        s.keys.ends_with(&[(KEY_B, true), (KEY_B, false)])
    });
}

#[test]
fn popup_grabs_and_the_input_method_take_turns_with_the_keyboard() {
    let compositor = common::start("");
    let mut ime = Ime::connect(&compositor);
    let mut typist = Typist::connect(&compositor);
    let window = typist.open_window();
    typist.enable();
    ime.dispatch_until("activation", |s| s.active);
    let grab = ime.input_method.grab_keyboard(&ime.qh, ());
    ime.dispatch_until("the grab's keymap", |s| s.keymap.is_some());
    let press = |code| compositor.request(Request::PressKey { code });
    let pressed = |code| vec![(code, true), (code, false)];

    // A popup grabs the keyboard even though the input method holds it.
    let parent = typist.client.app.windows[0].xdg_surface.clone();
    let popup = typist.open_popup(&parent);
    press(KEY_A);
    typist.dispatch_until("the key in the popup", |s| s.keys == pressed(KEY_A));
    assert!(!typist.state.popup_done, "the popup's grab was refused");

    // The popup's grab outlasts the input method's release.
    grab.release();
    ime.conn.flush().expect("flush");
    ime.roundtrip();
    typist.take_keys();
    press(KEY_B);
    typist.dispatch_until("the key in the popup", |s| s.keys == pressed(KEY_B));

    // A new input method grab takes the keys, and the keyboard focus stays on the popup.
    let grab = ime.input_method.grab_keyboard(&ime.qh, ());
    ime.state.keys.clear();
    ime.dispatch_until("the grab's keymap", |s| s.keymap.is_some());
    press(KEY_A);
    ime.dispatch_until("the grabbed key", |s| s.keys == pressed(KEY_A));
    assert_eq!(typist.state.keyboard_focus.as_ref(), Some(&popup.surface));

    // Once the popup goes, the input method keeps its grab.
    popup.popup.destroy();
    popup.xdg_surface.destroy();
    popup.surface.destroy();
    typist.dispatch_until("keyboard focus on the window", |s| {
        s.keyboard_focus.as_ref() == Some(&window)
    });
    ime.state.keys.clear();
    press(KEY_B);
    ime.dispatch_until("the grabbed key", |s| s.keys == pressed(KEY_B));

    grab.release();
    ime.conn.flush().expect("flush");
    ime.roundtrip();
    typist.take_keys();
    press(KEY_A);
    typist.dispatch_until("the key in the window", |s| s.keys == pressed(KEY_A));
}

#[test]
fn input_method_popups_sit_below_the_text_cursor() {
    let compositor = common::start("");
    let mut ime = Ime::connect(&compositor);
    let mut typist = Typist::connect(&compositor);
    typist.open_window();
    typist.enable();
    ime.dispatch_until("activation", |s| s.active);

    let surface = ime.state.compositor.as_ref().unwrap().create_surface(&ime.qh, ());
    ime.input_method.get_input_popup_surface(&surface, &ime.qh, ());
    attach_red_buffer(ime.state.shm.as_ref().unwrap(), &ime.qh, &surface, (100, 30));
    ime.dispatch_until("the text cursor rectangle", |s| s.popup_rectangle == Some((10, 20, 2, 16)));

    let (window, popup) = common::wait_for("the popup on screen", || {
        let shot = compositor.screenshot("HEADLESS-1");
        let window = bounds(&shot, |p| p == [255, 255, 255, 255])?;
        let popup = bounds(&shot, |p| p == [255, 0, 0, 255])?;
        Some((window, popup))
    });
    assert_eq!(popup, (window.0 + 10, window.1 + 36, 100, 30), "window at {window:?}");
}

/// The top left corner and size of the pixels that `matches` picks out.
fn bounds(
    image: &image::RgbaImage,
    matches: impl Fn([u8; 4]) -> bool,
) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in image.enumerate_pixels() {
        if matches(pixel.0) {
            let (left, top, right, bottom) = found.unwrap_or((x, y, x, y));
            found = Some((left.min(x), top.min(y), right.max(x), bottom.max(y)));
        }
    }
    found.map(|(left, top, right, bottom)| (left, top, right - left + 1, bottom - top + 1))
}

/// Attaches and commits an opaque red buffer of `width` x `height` pixels.
fn attach_red_buffer(
    shm: &WlShm,
    qh: &QueueHandle<ImeState>,
    surface: &WlSurface,
    (width, height): (i32, i32),
) {
    let stride = width * 4;
    let pixels = usize::try_from(width * height).unwrap();
    let mut file = tempfile::tempfile().expect("shm file");
    // Little-endian ARGB: blue, green, red, alpha.
    file.write_all(&[0, 0, 0xff, 0xff].repeat(pixels)).unwrap();
    let pool = shm.create_pool(file.as_fd(), stride * height, qh, ());
    let buffer = pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, width, height);
    surface.commit();
}

impl Dispatch<WlRegistry, ()> for TypistState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(5), qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
            "zwlr_layer_shell_v1" => {
                state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
            }
            "xdg_wm_base" => state.wm_base = Some(registry.bind(name, version.min(5), qh, ())),
            "zwp_text_input_manager_v3" => {
                state.text_input_manager = Some(registry.bind(name, 1, qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for TypistState {
    fn event(
        state: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { surface } => state.focus = Some(surface),
            zwp_text_input_v3::Event::Leave { .. } => state.focus = None,
            zwp_text_input_v3::Event::PreeditString { text, cursor_begin, cursor_end } => {
                state.pending.preedit = text.map(|text| (text, cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::CommitString { text } => state.pending.commit = text,
            zwp_text_input_v3::Event::Done { .. } => {
                let pending = std::mem::take(&mut state.pending);
                state.preedit = pending.preedit;
                state.committed.extend(pending.commit);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for TypistState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { surface, .. } => state.keyboard_focus = Some(surface),
            wl_keyboard::Event::Leave { .. } => state.keyboard_focus = None,
            wl_keyboard::Event::Key { key, state: key_state, .. } => {
                state.keys.push((key, key_state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for TypistState {
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

impl Dispatch<XdgSurface, ()> for TypistState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.popup_configured = true;
        }
    }
}

impl Dispatch<XdgPopup, ()> for TypistState {
    fn event(
        state: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            state.popup_done = true;
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for TypistState {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            state.layer_configure = Some(serial);
        }
    }
}

delegate_noop!(TypistState: ignore WlCompositor);
delegate_noop!(TypistState: ignore WlSurface);
delegate_noop!(TypistState: ignore WlShm);
delegate_noop!(TypistState: ignore WlShmPool);
delegate_noop!(TypistState: ignore WlBuffer);
delegate_noop!(TypistState: ignore WlSeat);
delegate_noop!(TypistState: ignore ZwlrLayerShellV1);
delegate_noop!(TypistState: ignore ZwpTextInputManagerV3);
delegate_noop!(TypistState: ignore XdgPositioner);

impl Dispatch<WlRegistry, ()> for ImeState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(5), qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
            "zwp_input_method_manager_v2" => state.manager = Some(registry.bind(name, 1, qh, ())),
            "zwp_virtual_keyboard_manager_v1" => {
                state.virtual_keyboards = Some(registry.bind(name, 1, qh, ()))
            }
            "ext_session_lock_manager_v1" => {
                state.lock_manager = Some(registry.bind(name, 1, qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for ImeState {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                state.pending_active = true;
                state.pending_surrounding = None;
            }
            zwp_input_method_v2::Event::Deactivate => state.pending_active = false,
            zwp_input_method_v2::Event::SurroundingText { text, .. } => {
                state.pending_surrounding = Some(text);
            }
            zwp_input_method_v2::Event::Done => {
                state.active = state.pending_active;
                state.surrounding = state.pending_surrounding.clone();
                state.serial += 1;
            }
            zwp_input_method_v2::Event::Unavailable => panic!("the input method is unavailable"),
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for ImeState {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { fd, size, .. } => {
                state.keymap = Some((fd, size));
            }
            zwp_input_method_keyboard_grab_v2::Event::Key { key, state: key_state, .. } => {
                state.keys.push((key, key_state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for ImeState {
    fn event(
        state: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle { x, y, width, height } = event
        {
            state.popup_rectangle = Some((x, y, width, height));
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for ImeState {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.locked = true,
            ext_session_lock_v1::Event::Finished => panic!("the session lock was refused"),
            _ => {}
        }
    }
}

delegate_noop!(ImeState: ignore WlCompositor);
delegate_noop!(ImeState: ignore WlSurface);
delegate_noop!(ImeState: ignore WlShm);
delegate_noop!(ImeState: ignore WlShmPool);
delegate_noop!(ImeState: ignore WlBuffer);
delegate_noop!(ImeState: ignore WlSeat);
delegate_noop!(ImeState: ignore ZwpInputMethodManagerV2);
delegate_noop!(ImeState: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(ImeState: ignore ZwpVirtualKeyboardV1);
delegate_noop!(ImeState: ignore ExtSessionLockManagerV1);
