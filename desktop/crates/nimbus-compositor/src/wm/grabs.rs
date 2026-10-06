// SPDX-License-Identifier: MIT

//! Interactive move and resize pointer grabs.

use super::layout::Rect;
use super::{ResizeState, floating};
use crate::state::State;
use nimbus_ipc::WindowId;
use smithay::input::SeatHandler;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::{self, ResizeEdge};
use smithay::utils::{Logical, Point, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;

/// Pixels of a dragged window that must stay on screen.
const REACHABLE_MARGIN: i32 = 48;
/// Smallest size an interactive resize produces when the client sets no minimum.
const MIN_SIZE: i32 = 64;

type Focus = Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>;

pub struct MoveGrab {
    pub start_data: GrabStartData<State>,
    pub window: WindowId,
    pub initial_location: Point<i32, Logical>,
}

impl PointerGrab<State> for MoveGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Focus,
        event: &MotionEvent,
    ) {
        // While moving, no client receives pointer events.
        handle.motion(data, None, event);
        let delta = event.location - self.start_data.location;
        let mut location = self.initial_location + delta.to_i32_round();
        let size =
            data.nimbus.wm.get(self.window).map(|w| w.window.geometry().size).unwrap_or_default();
        if let Some(area) = data.nimbus.output_area_at(event.location) {
            let titlebar = Point::from((0, data.nimbus.wm.titlebar_height(self.window)));
            let outer = Rect::new(location - titlebar, size + Size::from((0, titlebar.y)));
            location = floating::keep_reachable(outer, area.usable, REACHABLE_MARGIN) + titlebar;
        }
        data.nimbus.wm.move_floating(self.window, location);
        data.nimbus.queue_redraw_all();
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        focus: Focus,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut State) {
        let areas = data.nimbus.output_areas();
        data.nimbus.wm.update_output(self.window, &areas);
        data.nimbus.arrange();
    }
}

pub struct ResizeGrab {
    pub start_data: GrabStartData<State>,
    pub window: WindowId,
    pub edges: ResizeEdge,
    pub initial: Rect,
}

fn has_edge(edges: ResizeEdge, edge: ResizeEdge) -> bool {
    u32::from(edges) & u32::from(edge) != 0
}

/// The size a resize from `initial` by `delta` produces, within the client's limits.
pub fn resized(
    edges: ResizeEdge,
    initial: Size<i32, Logical>,
    delta: Point<i32, Logical>,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
) -> Size<i32, Logical> {
    let mut w = initial.w;
    let mut h = initial.h;
    if has_edge(edges, ResizeEdge::Left) {
        w -= delta.x;
    } else if has_edge(edges, ResizeEdge::Right) {
        w += delta.x;
    }
    if has_edge(edges, ResizeEdge::Top) {
        h -= delta.y;
    } else if has_edge(edges, ResizeEdge::Bottom) {
        h += delta.y;
    }
    let limit = |value: i32, min: i32, max: i32| {
        let min = min.max(MIN_SIZE);
        let max = if max > 0 { max.max(min) } else { i32::MAX };
        value.clamp(min, max)
    };
    Size::from((limit(w, min.w, max.w), limit(h, min.h, max.h)))
}

/// The location that keeps the edges opposite to the dragged ones in place.
pub fn resized_location(
    edges: ResizeEdge,
    initial: Rect,
    size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let mut location = initial.loc;
    if has_edge(edges, ResizeEdge::Left) {
        location.x = initial.loc.x + initial.size.w - size.w;
    }
    if has_edge(edges, ResizeEdge::Top) {
        location.y = initial.loc.y + initial.size.h - size.h;
    }
    location
}

/// The edges nearest to `point` inside `rect`, for Super+right-drag resizing.
pub fn edges_for_point(rect: Rect, point: Point<f64, Logical>) -> ResizeEdge {
    let left = point.x < f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0;
    let top = point.y < f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0;
    match (left, top) {
        (true, true) => ResizeEdge::TopLeft,
        (false, true) => ResizeEdge::TopRight,
        (true, false) => ResizeEdge::BottomLeft,
        (false, false) => ResizeEdge::BottomRight,
    }
}

impl PointerGrab<State> for ResizeGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Focus,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        let Some(w) = data.nimbus.wm.get_mut(self.window) else {
            handle.unset_grab(self, data, event.serial, event.time, true);
            return;
        };
        let Some(toplevel) = w.toplevel().cloned() else {
            return;
        };
        let (min, max) = with_states(toplevel.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            (current.min_size, current.max_size)
        });
        let delta = (event.location - self.start_data.location).to_i32_round();
        let size = resized(self.edges, self.initial.size, delta, min, max);
        w.resize = Some(ResizeState { edges: self.edges, initial: self.initial, finishing: false });
        toplevel.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(size);
        });
        toplevel.send_pending_configure();
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        focus: Focus,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut State) {
        if let Some(w) = data.nimbus.wm.get_mut(self.window) {
            if let Some(resize) = w.resize.as_mut() {
                resize.finishing = true;
            }
            if let Some(toplevel) = w.toplevel() {
                toplevel
                    .with_pending_state(|state| state.states.unset(xdg_toplevel::State::Resizing));
                toplevel.send_pending_configure();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn bottom_right_resize_grows_with_pointer() {
        let size = resized(
            ResizeEdge::BottomRight,
            (400, 300).into(),
            (50, -20).into(),
            (0, 0).into(),
            (0, 0).into(),
        );
        assert_eq!(size, Size::from((450, 280)));
        assert_eq!(
            resized_location(ResizeEdge::BottomRight, rect(10, 10, 400, 300), size),
            Point::from((10, 10))
        );
    }

    #[test]
    fn top_left_resize_keeps_the_opposite_corner() {
        let initial = rect(100, 100, 400, 300);
        let size = resized(
            ResizeEdge::TopLeft,
            initial.size,
            (-30, 40).into(),
            (0, 0).into(),
            (0, 0).into(),
        );
        assert_eq!(size, Size::from((430, 260)));
        let loc = resized_location(ResizeEdge::TopLeft, initial, size);
        assert_eq!((loc.x + size.w, loc.y + size.h), (500, 400));
    }

    #[test]
    fn resize_respects_client_limits() {
        let size = resized(
            ResizeEdge::Right,
            (400, 300).into(),
            (-1000, 0).into(),
            (200, 100).into(),
            (0, 0).into(),
        );
        assert_eq!(size, Size::from((200, 300)));
        let size = resized(
            ResizeEdge::Bottom,
            (400, 300).into(),
            (0, 1000).into(),
            (0, 0).into(),
            (0, 500).into(),
        );
        assert_eq!(size, Size::from((400, 500)));
        let size = resized(
            ResizeEdge::Left,
            (100, 100).into(),
            (1000, 0).into(),
            (0, 0).into(),
            (0, 0).into(),
        );
        assert_eq!(size.w, MIN_SIZE);
    }

    #[test]
    fn quadrant_selects_edges() {
        let r = rect(0, 0, 100, 100);
        assert_eq!(edges_for_point(r, (10.0, 10.0).into()), ResizeEdge::TopLeft);
        assert_eq!(edges_for_point(r, (90.0, 90.0).into()), ResizeEdge::BottomRight);
        assert_eq!(edges_for_point(r, (90.0, 10.0).into()), ResizeEdge::TopRight);
        assert_eq!(edges_for_point(r, (10.0, 90.0).into()), ResizeEdge::BottomLeft);
    }
}
