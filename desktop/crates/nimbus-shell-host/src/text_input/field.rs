// SPDX-License-Identifier: MIT

//! A Slint window's focused text field as `zwp_text_input_v3` describes it, and input method changes as Slint events.

use i_slint_core::InternalToken;
use i_slint_core::input::{InternalKeyEvent, KeyEventType};
use i_slint_core::items::{CapitalizationMode, InputType};
use i_slint_core::window::{InputMethodProperties, InputMethodRequest, WindowAdapterInternal};
use slint::platform::WindowAdapter;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};

/// The longest surrounding text `set_surrounding_text` takes, in bytes.
const MAX_SURROUNDING: usize = 4000;

/// The text field that has the focus in a window, as Slint reports it to the window adapter.
#[derive(Clone, Default)]
pub struct TextField(Rc<RefCell<Option<Focused>>>);

struct Focused {
    /// Tells this text field from the ones focused before it.
    id: u64,
    content: Content,
}

impl TextField {
    /// The text field of the window behind `adapter`.
    pub fn of(adapter: &dyn WindowAdapter) -> Option<Self> {
        let internal: &dyn Any = adapter.internal(InternalToken)?;
        internal.downcast_ref::<Self>().cloned()
    }

    /// The focused text field's id and content, while one takes text from input methods.
    pub fn focused(&self) -> Option<(u64, Content)> {
        self.0.borrow().as_ref().map(|focused| (focused.id, focused.content.clone()))
    }
}

impl WindowAdapterInternal for TextField {
    fn input_method_request(&self, request: InputMethodRequest) {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let mut focused = self.0.borrow_mut();
        match request {
            InputMethodRequest::Enable(properties) => {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                *focused = Some(Focused { id, content: Content::new(&properties) });
            }
            // Slint also updates focused read-only fields, which never enabled the input method.
            InputMethodRequest::Update(properties) => {
                if let Some(focused) = focused.as_mut() {
                    focused.content = Content::new(&properties);
                }
            }
            InputMethodRequest::Disable => *focused = None,
            _ => {}
        }
    }
}

/// What a text field tells the compositor.
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    /// The text around the cursor, with the cursor and anchor in bytes; password fields keep it to themselves.
    pub surrounding: Option<(String, i32, i32)>,
    pub hint: ContentHint,
    pub purpose: ContentPurpose,
    /// The text cursor in surface coordinates, as x, y, width, and height.
    pub cursor_rectangle: (i32, i32, i32, i32),
}

impl Content {
    fn new(properties: &InputMethodProperties) -> Self {
        let origin = properties.cursor_rect_origin;
        let size = properties.cursor_rect_size;
        let cursor_rectangle = (
            origin.x.round() as i32,
            origin.y.round() as i32,
            size.width.round() as i32,
            size.height.round() as i32,
        );
        let cursor = properties.cursor_position;
        let anchor = properties.anchor_position.unwrap_or(cursor);
        if properties.input_type == InputType::Password {
            return Self {
                surrounding: None,
                hint: ContentHint::SensitiveData | ContentHint::HiddenText,
                purpose: ContentPurpose::Password,
                cursor_rectangle,
            };
        }
        let purpose = match properties.input_type {
            InputType::Number => ContentPurpose::Digits,
            InputType::Decimal => ContentPurpose::Number,
            _ => ContentPurpose::Normal,
        };
        let hints = &properties.input_method_hints;
        let mut hint = match hints.capitalization {
            CapitalizationMode::Sentences => ContentHint::AutoCapitalization,
            CapitalizationMode::Words => ContentHint::Titlecase,
            CapitalizationMode::Characters => ContentHint::Uppercase,
            _ => ContentHint::None,
        };
        hint.set(ContentHint::Spellcheck, hints.auto_correct);
        hint.set(ContentHint::Completion, hints.auto_complete);
        Self {
            surrounding: Some(surrounding(&properties.text, cursor, anchor)),
            hint,
            purpose,
            cursor_rectangle,
        }
    }
}

/// The part of `text` around the cursor and anchor that fits into `set_surrounding_text`,
/// with the cursor and anchor in it.
fn surrounding(text: &str, cursor: usize, anchor: usize) -> (String, i32, i32) {
    let cursor = cursor.min(text.len());
    // A selection that doesn't fit shrinks to the cursor.
    let anchor = Some(anchor.min(text.len()))
        .filter(|anchor| anchor.abs_diff(cursor) <= MAX_SURROUNDING)
        .unwrap_or(cursor);
    let (low, high) = (cursor.min(anchor), cursor.max(anchor));
    let room = (MAX_SURROUNDING - (high - low)) / 2;
    let mut start = low.saturating_sub(room);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = high.saturating_add(room).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let offset = |position: usize| i32::try_from(position - start).unwrap_or(i32::MAX);
    (text[start..end].to_owned(), offset(cursor), offset(anchor))
}

/// The changes an input method sends before each `done`.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// The new preedit string, with the cursor's start and end in bytes, or -1 to hide it.
    pub preedit: Option<(String, i32, i32)>,
    pub commit: Option<String>,
    /// How many bytes to delete before and after the cursor.
    pub delete: Option<(u32, u32)>,
}

impl Changes {
    /// The Slint event that applies the changes to the focused text field, in `done`'s order.
    pub fn into_event(self) -> InternalKeyEvent {
        let mut event =
            InternalKeyEvent { event_type: KeyEventType::UpdateComposition, ..Default::default() };
        // Without a new preedit string, the old one goes.
        if let Some((text, begin, end)) = self.preedit {
            event.preedit_text = text.into();
            event.preedit_selection = (begin >= 0 && end >= 0).then_some(begin..end);
        }
        if self.commit.is_some() || self.delete.is_some() {
            event.event_type = KeyEventType::CommitComposition;
            event.key_event.text = self.commit.unwrap_or_default().into();
            event.replacement_range = self.delete.map(|(before, after)| {
                let length = |bytes: u32| i32::try_from(bytes).unwrap_or(i32::MAX);
                -length(before)..length(after)
            });
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_core::items::InputMethodHints;

    fn properties(text: &str, cursor: usize, input_type: InputType) -> InputMethodProperties {
        let mut properties = InputMethodProperties::default();
        properties.text = text.into();
        properties.cursor_position = cursor;
        properties.input_type = input_type;
        properties.cursor_rect_origin = slint::LogicalPosition::new(10.4, 20.6);
        properties.cursor_rect_size = slint::LogicalSize::new(1.0, 16.0);
        properties
    }

    #[test]
    fn text_fields_describe_their_text_and_cursor() {
        let content = Content::new(&properties("你好 world", 6, InputType::Text));
        assert_eq!(content.surrounding, Some(("你好 world".into(), 6, 6)));
        assert_eq!(content.purpose, ContentPurpose::Normal);
        assert_eq!(
            content.hint,
            ContentHint::AutoCapitalization | ContentHint::Spellcheck | ContentHint::Completion
        );
        assert_eq!(content.cursor_rectangle, (10, 21, 1, 16));
    }

    #[test]
    fn password_fields_hide_their_text() {
        let content = Content::new(&properties("secret", 6, InputType::Password));
        assert_eq!(content.surrounding, None);
        assert_eq!(content.purpose, ContentPurpose::Password);
        assert_eq!(content.hint, ContentHint::SensitiveData | ContentHint::HiddenText);
    }

    #[test]
    fn hints_follow_the_text_field() {
        let mut properties = properties("", 0, InputType::Number);
        let mut hints = InputMethodHints::default();
        hints.capitalization = CapitalizationMode::None;
        hints.auto_correct = false;
        properties.input_method_hints = hints;
        let content = Content::new(&properties);
        assert_eq!(content.purpose, ContentPurpose::Digits);
        assert_eq!(content.hint, ContentHint::Completion);
    }

    #[test]
    fn long_text_is_cut_around_the_cursor_on_character_boundaries() {
        let text = "日".repeat(3000);
        let (part, cursor, anchor) = surrounding(&text, 4500, 4503);
        assert!(part.len() <= MAX_SURROUNDING);
        assert!(part.chars().all(|c| c == '日'));
        let start = 4500 - usize::try_from(cursor).unwrap();
        assert_eq!(&text[start..start + part.len()], part);
        assert_eq!(anchor - cursor, 3);

        // A selection longer than the limit leaves only the cursor.
        let (part, cursor, anchor) = surrounding(&text, 0, 9000);
        assert_eq!((cursor, anchor), (0, 0));
        assert!(part.len() <= MAX_SURROUNDING / 2);
    }

    #[test]
    fn preedit_strings_update_the_composition() {
        let event =
            Changes { preedit: Some(("ni".into(), 2, 2)), ..Default::default() }.into_event();
        assert_eq!(event.event_type, KeyEventType::UpdateComposition);
        assert_eq!(event.preedit_text, "ni");
        assert_eq!(event.preedit_selection, Some(2..2));

        let hidden = Changes { preedit: Some(("ni".into(), -1, -1)), ..Default::default() };
        assert_eq!(hidden.into_event().preedit_selection, None);
    }

    #[test]
    fn commits_replace_the_preedit_and_delete_around_the_cursor() {
        let event = Changes {
            preedit: Some(("hao".into(), 3, 3)),
            commit: Some("你".into()),
            delete: Some((3, 1)),
        }
        .into_event();
        assert_eq!(event.event_type, KeyEventType::CommitComposition);
        assert_eq!(event.key_event.text, "你");
        assert_eq!(event.replacement_range, Some(-3..1));
        assert_eq!(event.preedit_text, "hao");

        let event = Changes { commit: Some("好".into()), ..Default::default() }.into_event();
        assert_eq!(event.preedit_text, "");
        assert_eq!(event.replacement_range, None);
    }
}
