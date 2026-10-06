// SPDX-License-Identifier: MIT

//! Input methods: text-input-v3 for clients, input-method-v2 for the input method, such as fcitx5 or IBus,
//! and virtual-keyboard-v1, through which input methods send the keys they don't take.
//!
//! smithay serves all three; the `Dispatch` implementations here pass requests on to it,
//! except for text and keys meant for the lock screen, and note each text input's content purpose.

use super::{Nimbus, State};
use crate::wm::layout::Rect;
use smithay::desktop::utils::bbox_from_surface_tree;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupManager, WindowSurfaceType, find_popup_root_surface,
    layer_map_for_output,
};
use smithay::input::keyboard::KeyboardHandle;
use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ContentPurpose, ZwpTextInputV3},
};
use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2::{self, ZwpInputMethodV2},
    zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
};
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, delegate_dispatch, delegate_global_dispatch,
};
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size};
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodKeyboardGrab, InputMethodKeyboardUserData,
    InputMethodManagerGlobalData, InputMethodManagerState, InputMethodPopupSurfaceUserData,
    InputMethodSeat, InputMethodUserData, PopupSurface,
};
use smithay::wayland::text_input::{TextInputManagerState, TextInputSeat, TextInputUserData};
use smithay::wayland::virtual_keyboard::{
    VirtualKeyboardManagerGlobalData, VirtualKeyboardManagerState, VirtualKeyboardUserData,
};

impl InputMethodHandler for State {
    fn new_popup(&mut self, mut popup: PopupSurface) {
        // smithay only sends the rectangle when it changes, but the input method needs it for each new parent.
        let cursor = popup.text_input_rectangle();
        popup.set_text_input_rectangle(cursor.loc.x, cursor.loc.y, cursor.size.w, cursor.size.h);
        if let Err(err) = self.nimbus.popups.track_popup(PopupKind::from(popup.clone())) {
            tracing::debug!("input method popup vanished before it was tracked: {err}");
        }
        self.nimbus.place_input_method_popup(&popup);
    }

    fn dismiss_popup(&mut self, popup: PopupSurface) {
        let kind = PopupKind::from(popup);
        if let Ok(root) = find_popup_root_surface(&kind) {
            let _ = PopupManager::dismiss_popup(&root, &kind);
        }
        self.nimbus.queue_redraw_all();
    }

    fn popup_repositioned(&mut self, popup: PopupSurface) {
        self.nimbus.place_input_method_popup(&popup);
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.nimbus.window_geometry(parent)
    }
}

delegate_global_dispatch!(State: [ZwpTextInputManagerV3: ()] => TextInputManagerState);
delegate_dispatch!(State: [ZwpTextInputManagerV3: ()] => TextInputManagerState);
delegate_global_dispatch!(
    State: [ZwpInputMethodManagerV2: InputMethodManagerGlobalData] => InputMethodManagerState
);
delegate_dispatch!(State: [ZwpInputMethodManagerV2: ()] => InputMethodManagerState);
delegate_dispatch!(
    State: [ZwpInputMethodKeyboardGrabV2: InputMethodKeyboardUserData<State>] => InputMethodManagerState
);
delegate_dispatch!(
    State: [ZwpInputPopupSurfaceV2: InputMethodPopupSurfaceUserData] => InputMethodManagerState
);
delegate_global_dispatch!(
    State: [ZwpVirtualKeyboardManagerV1: VirtualKeyboardManagerGlobalData] => VirtualKeyboardManagerState
);
delegate_dispatch!(State: [ZwpVirtualKeyboardManagerV1: ()] => VirtualKeyboardManagerState);

/// A text input's content purpose, which smithay passes to the input method but doesn't expose.
#[derive(Debug, Default)]
pub struct TextInputPurpose {
    enabling: bool,
    pending: Option<ContentPurpose>,
    current: Option<ContentPurpose>,
}

impl TextInputPurpose {
    /// Applies the pending state; enabling resets the purpose to `normal`, as text-input-v3 specifies.
    fn commit(&mut self) {
        if std::mem::take(&mut self.enabling) {
            self.current = None;
        }
        if let Some(purpose) = self.pending.take() {
            self.current = Some(purpose);
        }
    }

    fn is_secret(&self) -> bool {
        matches!(self.current, Some(ContentPurpose::Password | ContentPurpose::Pin))
    }
}

impl Dispatch<ZwpTextInputV3, TextInputUserData> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        data: &TextInputUserData,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let purpose = state.nimbus.text_input_purposes.entry(resource.clone()).or_default();
        match &request {
            zwp_text_input_v3::Request::Enable => purpose.enabling = true,
            zwp_text_input_v3::Request::SetContentType { purpose: new, .. } => {
                purpose.pending = new.into_result().ok();
            }
            zwp_text_input_v3::Request::Commit => purpose.commit(),
            _ => {}
        }
        <TextInputManagerState as Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, dh, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &ZwpTextInputV3,
        data: &TextInputUserData,
    ) {
        state.nimbus.text_input_purposes.remove(resource);
        <TextInputManagerState as Dispatch<_, _, Self>>::destroyed(state, client, resource, data);
    }
}

impl Dispatch<ZwpInputMethodV2, InputMethodUserData<State>> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpInputMethodV2,
        request: zwp_input_method_v2::Request,
        data: &InputMethodUserData<State>,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let edits_text = matches!(
            request,
            zwp_input_method_v2::Request::CommitString { .. }
                | zwp_input_method_v2::Request::SetPreeditString { .. }
                | zwp_input_method_v2::Request::DeleteSurroundingText { .. }
        );
        if edits_text && state.nimbus.is_locked() {
            return;
        }
        <InputMethodManagerState as Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, dh, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &ZwpInputMethodV2,
        data: &InputMethodUserData<State>,
    ) {
        <InputMethodManagerState as Dispatch<_, _, Self>>::destroyed(state, client, resource, data);
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData<State>> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        data: &VirtualKeyboardUserData<State>,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, zwp_virtual_keyboard_v1::Request::Key { .. })
            && state.nimbus.is_locked()
        {
            return;
        }
        <VirtualKeyboardManagerState as Dispatch<_, _, Self>>::request(
            state, client, resource, request, data, dh, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &ZwpVirtualKeyboardV1,
        data: &VirtualKeyboardUserData<State>,
    ) {
        <VirtualKeyboardManagerState as Dispatch<_, _, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

/// Whether the input method's grab is the keyboard's current grab.
pub fn is_input_method_grab(keyboard: &KeyboardHandle<State>) -> bool {
    keyboard.with_grab(|_, grab| grab.is::<InputMethodKeyboardGrab>()).unwrap_or(false)
}

impl State {
    /// Puts the keyboard grabs in order; see "Input Methods" in `docs/architecture.md`.
    ///
    /// smithay keeps one keyboard grab, and a new one replaces the last,
    /// so this brings back the popup's or the input method's grab when the other ends.
    pub fn refresh_keyboard_grab(&mut self, keyboard: &KeyboardHandle<State>) {
        self.nimbus.remember_input_method_grab(keyboard);
        if self.nimbus.popup_grab.as_ref().is_some_and(|grab| grab.has_ended()) {
            self.nimbus.popup_grab = None;
        }
        if !self.nimbus.seat.input_method().keyboard_grabbed() {
            self.nimbus.input_method_grab = None;
        }
        if self.nimbus.is_locked() {
            if keyboard.is_grabbed() {
                keyboard.unset_grab(self);
            }
            return;
        }
        let typing_secret = self.nimbus.typing_secret();
        if typing_secret && is_input_method_grab(keyboard) {
            keyboard.unset_grab(self);
        }
        if keyboard.is_grabbed() {
            return;
        }
        if let Some(grab) = self.nimbus.popup_grab.clone() {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), grab.serial());
        } else if let Some(grab) = self.nimbus.input_method_grab.clone().filter(|_| !typing_secret)
        {
            keyboard.set_grab(self, grab, SERIAL_COUNTER.next_serial());
        }
    }
}

impl Nimbus {
    /// Whether the active text input asks for a password or PIN, whose keys the input method mustn't see.
    fn typing_secret(&self) -> bool {
        let mut secret = false;
        self.seat.text_input().with_active_text_input(|text_input, _| {
            secret =
                self.text_input_purposes.get(text_input).is_some_and(TextInputPurpose::is_secret);
        });
        secret
    }

    /// Keeps the input method's keyboard grab while it's the current one, so it can come back after another grab.
    pub fn remember_input_method_grab(&mut self, keyboard: &KeyboardHandle<State>) {
        let grab = keyboard
            .with_grab(|_, grab| grab.downcast_ref::<InputMethodKeyboardGrab>().cloned())
            .flatten();
        if grab.is_some() {
            self.input_method_grab = grab;
        }
    }

    /// The window geometry of `surface`, in its own coordinates:
    /// the one of its xdg toplevel or popup, or the whole surface for other roles.
    fn window_geometry(&self, surface: &WlSurface) -> Rect {
        if let Some(window) = self.wm.find_surface(surface).and_then(|id| self.wm.get(id)) {
            return window.window.geometry();
        }
        match self.popups.find_popup(surface) {
            Some(popup @ PopupKind::Xdg(_)) => popup.geometry(),
            _ => bbox_from_surface_tree(surface, (0, 0)),
        }
    }

    /// Where the window geometry of a window or layer surface is, in global coordinates.
    fn geometry_origin(&self, surface: &WlSurface) -> Option<Point<i32, Logical>> {
        if let Some(window) = self.wm.find_surface(surface).and_then(|id| self.wm.get(id)) {
            return self.wm.space.element_location(&window.window);
        }
        self.outputs().find_map(|output| {
            let map = layer_map_for_output(output);
            let layer = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)?;
            Some(self.output_geometry(output)?.loc + map.layer_geometry(layer)?.loc)
        })
    }

    /// Places an input method popup below the text cursor of its parent, or above it when there's no room below,
    /// inside the output that shows the cursor.
    pub fn place_input_method_popup(&mut self, popup: &PopupSurface) {
        self.queue_redraw_all();
        let kind = PopupKind::from(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let Some(origin) = self.geometry_origin(&root) else {
            return;
        };
        let Some(offset) =
            PopupManager::popups_for_surface(&root).find(|(p, _)| *p == kind).map(|(_, o)| o)
        else {
            return;
        };
        // smithay draws the popup at `origin + offset - geometry.loc`, and `offset` includes its location.
        let parent = origin + offset - popup.location() - kind.geometry().loc;
        let cursor = popup.text_input_rectangle();
        let Some(area) = self.output_area_at((parent + cursor.loc).to_f64()).map(|a| a.geometry)
        else {
            return;
        };
        let size = bbox_from_surface_tree(popup.wl_surface(), (0, 0)).size;
        popup.set_location(popup_location(cursor, size, parent, area));
    }
}

/// The location, relative to its parent surface at `parent`, of an input method popup of `size`:
/// below the text cursor rectangle `cursor`, or above it when only that fits, and inside `area`.
fn popup_location(
    cursor: Rect,
    size: Size<i32, Logical>,
    parent: Point<i32, Logical>,
    area: Rect,
) -> Point<i32, Logical> {
    let cursor = Rect::new(parent + cursor.loc, cursor.size);
    let right = area.loc.x + area.size.w - size.w;
    let x = cursor.loc.x.min(right).max(area.loc.x);
    let bottom = area.loc.y + area.size.h - size.h;
    let below = cursor.loc.y + cursor.size.h;
    let above = cursor.loc.y - size.h;
    let y = if below > bottom && above >= area.loc.y { above } else { below.min(bottom) };
    Point::from((x, y.max(area.loc.y))) - parent
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Places a popup of `size` for a parent surface at (100, 50) on a 1280x720 output.
    fn place(cursor: (i32, i32, i32, i32), size: (i32, i32)) -> (i32, i32) {
        let cursor = Rect::new((cursor.0, cursor.1).into(), (cursor.2, cursor.3).into());
        let area = Rect::from_size((1280, 720).into());
        let location = popup_location(cursor, size.into(), (100, 50).into(), area);
        (location.x, location.y)
    }

    #[test]
    fn popups_go_below_the_cursor() {
        assert_eq!(place((10, 20, 2, 16), (200, 40)), (10, 36));
    }

    #[test]
    fn popups_go_above_the_cursor_without_room_below() {
        assert_eq!(place((10, 640, 2, 16), (200, 40)), (10, 600));
    }

    #[test]
    fn popups_stay_inside_the_output() {
        // Slides left from the right edge.
        assert_eq!(place((1150, 20, 2, 16), (200, 40)), (980, 36));
        // Without room above or below, covers the cursor rather than leaving the output.
        assert_eq!(place((10, 300, 2, 16), (200, 700)), (10, -30));
    }
}
