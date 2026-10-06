// SPDX-License-Identifier: MIT

//! Input methods such as fcitx5 and IBus type into the shell's text fields through `zwp_text_input_v3`.
//!
//! The compositor moves the text input along with the keyboard focus.
//! After each dispatch, [`State::sync_text_input`] tells it about the text field focused on that surface.

mod field;

pub use field::TextField;

use crate::state::State;
use field::{Changes, Content};
use i_slint_core::input::{InternalKeyEvent, KeyEventType};
use slint::platform::WindowEvent;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ChangeCause, ZwpTextInputV3,
};

#[derive(Default)]
pub struct TextInput {
    manager: Option<ZwpTextInputManagerV3>,
    text_input: Option<ZwpTextInputV3>,
    /// The surface the text input entered.
    focus: Option<WlSurface>,
    /// The text field the compositor knows about, by id, while the text input is enabled.
    enabled: Option<(u64, Content)>,
    /// The changes the next `done` applies.
    pending: Changes,
    /// The focused surface shows a preedit string.
    preedit: bool,
    /// The input method changed the text since the last commit.
    changed_by_input_method: bool,
}

impl TextInput {
    pub fn new(manager: Option<ZwpTextInputManagerV3>) -> Self {
        if manager.is_none() {
            tracing::info!("no input methods without zwp_text_input_manager_v3");
        }
        Self { manager, ..Default::default() }
    }

    /// Follows the text input focus on `seat`.
    pub fn set_seat(&mut self, seat: Option<&WlSeat>, qh: &QueueHandle<State>) {
        if let Some(text_input) = self.text_input.take() {
            text_input.destroy();
        }
        *self = Self { manager: self.manager.take(), ..Default::default() };
        self.text_input = self
            .manager
            .as_ref()
            .zip(seat)
            .map(|(manager, seat)| manager.get_text_input(seat, qh, ()));
    }

    /// Tells the compositor about `field`, the text field focused on the text input's surface, if it changed.
    fn sync(&mut self, field: Option<(u64, Content)>) {
        let Some(text_input) = &self.text_input else {
            return;
        };
        match (&field, &self.enabled) {
            (None, None) => return,
            (None, Some(_)) => text_input.disable(),
            (Some(field), Some(enabled)) if field == enabled => return,
            (Some((id, content)), Some((enabled_id, enabled))) if id == enabled_id => {
                if content.surrounding != enabled.surrounding {
                    if let Some((text, cursor, anchor)) = &content.surrounding {
                        text_input.set_surrounding_text(text.clone(), *cursor, *anchor);
                    }
                    let cause = if std::mem::take(&mut self.changed_by_input_method) {
                        ChangeCause::InputMethod
                    } else {
                        ChangeCause::Other
                    };
                    text_input.set_text_change_cause(cause);
                }
                if (content.hint, content.purpose) != (enabled.hint, enabled.purpose) {
                    text_input.set_content_type(content.hint, content.purpose);
                }
                if content.cursor_rectangle != enabled.cursor_rectangle {
                    let (x, y, width, height) = content.cursor_rectangle;
                    text_input.set_cursor_rectangle(x, y, width, height);
                }
            }
            (Some((_, content)), _) => {
                text_input.enable();
                if let Some((text, cursor, anchor)) = &content.surrounding {
                    text_input.set_surrounding_text(text.clone(), *cursor, *anchor);
                }
                text_input.set_content_type(content.hint, content.purpose);
                let (x, y, width, height) = content.cursor_rectangle;
                text_input.set_cursor_rectangle(x, y, width, height);
            }
        }
        text_input.commit();
        self.enabled = field;
        self.changed_by_input_method = false;
    }
}

impl State {
    /// Tells the compositor about the text field focused on the text input's surface; see [`TextInput`].
    pub fn sync_text_input(&mut self) {
        let field = self
            .text_input
            .focus
            .as_ref()
            .and_then(|focus| self.surface(focus)?.text_field()?.focused());
        self.text_input.sync(field);
    }

    fn compose(&self, surface: &WlSurface, event: InternalKeyEvent) {
        if let Some(surface) = self.surface(surface) {
            surface.dispatch(WindowEvent::internal(event));
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_text_input_v3::Event;
        let text_input = &mut state.text_input;
        match event {
            Event::Enter { surface } => text_input.focus = Some(surface),
            Event::Leave { surface } => {
                // The compositor forgets the text input's state, and the surface drops its preedit string.
                text_input.focus = None;
                text_input.enabled = None;
                text_input.pending = Changes::default();
                if std::mem::take(&mut text_input.preedit) {
                    let clear = InternalKeyEvent {
                        event_type: KeyEventType::UpdateComposition,
                        ..Default::default()
                    };
                    state.compose(&surface, clear);
                }
            }
            Event::PreeditString { text, cursor_begin, cursor_end } => {
                text_input.pending.preedit = text.map(|text| (text, cursor_begin, cursor_end));
            }
            Event::CommitString { text } => text_input.pending.commit = text,
            Event::DeleteSurroundingText { before_length, after_length } => {
                text_input.pending.delete = Some((before_length, after_length));
            }
            Event::Done { .. } => {
                let changes = std::mem::take(&mut text_input.pending);
                let edits = changes.commit.is_some() || changes.delete.is_some();
                let preedit = changes.preedit.is_some();
                if !edits && !preedit && !text_input.preedit {
                    return;
                }
                text_input.changed_by_input_method |= edits;
                text_input.preedit = preedit;
                if let Some(focus) = text_input.focus.clone() {
                    state.compose(&focus, changes.into_event());
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ZwpTextInputManagerV3);
