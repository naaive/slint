// SPDX-License-Identifier: MIT

use wayland_client::protocol::{
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
};

use crate::{Compositor, connect, dispatch_until};

/// The text input of the focused client, as of the input method's last `done`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextInputState {
    pub active: bool,
    /// The surrounding text with the cursor and anchor in bytes, unless the client keeps it to itself.
    pub surrounding: Option<(String, u32, u32)>,
    pub content_type: Option<(ContentHint, ContentPurpose)>,
}

#[derive(Default)]
pub struct InputMethodState {
    seat: Option<WlSeat>,
    manager: Option<ZwpInputMethodManagerV2>,
    pending: TextInputState,
    pub current: TextInputState,
    /// The number of `done` events, which `commit` passes back.
    serial: u32,
    /// Another input method took the role first.
    pub unavailable: bool,
}

/// An `input-method-v2` input method, such as fcitx5 would be, on a connection of its own.
pub struct InputMethod {
    conn: Connection,
    queue: EventQueue<InputMethodState>,
    pub state: InputMethodState,
    pub input_method: ZwpInputMethodV2,
}

impl InputMethod {
    /// Connects to `compositor` and takes the input method role on its seat.
    pub fn connect(compositor: &Compositor) -> Self {
        let conn = connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = InputMethodState::default();
        queue.roundtrip(&mut state).expect("roundtrip");
        let seat = state.seat.as_ref().expect("a seat");
        let manager = state.manager.as_ref().expect("zwp_input_method_manager_v2");
        let input_method = manager.get_input_method(seat, &qh, ());
        queue.roundtrip(&mut state).expect("roundtrip");
        assert!(!state.unavailable, "another input method holds the seat");
        Self { conn, queue, state, input_method }
    }

    /// Dispatches events until `cond` holds for the text input, and fails after [`crate::TIMEOUT`].
    pub fn wait(&mut self, what: &str, cond: impl Fn(&TextInputState) -> bool) {
        dispatch_until(&self.conn, &mut self.queue, &mut self.state, what, |s| cond(&s.current));
    }

    /// Shows `text` as the preedit string, with the cursor at its end.
    pub fn preedit(&mut self, text: &str) {
        let end = i32::try_from(text.len()).unwrap();
        self.input_method.set_preedit_string(text.into(), end, end);
        self.commit();
    }

    /// Commits `text` in place of the preedit string.
    pub fn commit_string(&mut self, text: &str) {
        self.input_method.commit_string(text.into());
        self.commit();
    }

    /// Deletes `before` and `after` bytes around the cursor.
    pub fn delete_surrounding(&mut self, before: u32, after: u32) {
        self.input_method.delete_surrounding_text(before, after);
        self.commit();
    }

    fn commit(&mut self) {
        self.input_method.commit(self.state.serial);
        self.conn.flush().expect("flush");
    }
}

impl Dispatch<WlRegistry, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "wl_seat" => state.seat = Some(registry.bind(name, 1, qh, ())),
                "zwp_input_method_manager_v2" => {
                    state.manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for InputMethodState {
    fn event(
        state: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_input_method_v2::Event;
        match event {
            // Activation resets the state the client sent before.
            Event::Activate => {
                state.pending = TextInputState { active: true, ..Default::default() }
            }
            Event::Deactivate => state.pending = TextInputState::default(),
            Event::SurroundingText { text, cursor, anchor } => {
                state.pending.surrounding = Some((text, cursor, anchor));
            }
            Event::ContentType { hint: WEnum::Value(hint), purpose: WEnum::Value(purpose) } => {
                state.pending.content_type = Some((hint, purpose));
            }
            Event::Done => {
                state.serial += 1;
                state.current = state.pending.clone();
            }
            Event::Unavailable => state.unavailable = true,
            _ => {}
        }
    }
}

delegate_noop!(InputMethodState: ignore WlSeat);
delegate_noop!(InputMethodState: ZwpInputMethodManagerV2);
