// SPDX-License-Identifier: MIT

//! Pointer locks and confinement from `pointer-constraints`; see `docs/architecture.md`.

use crate::state::State;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::RegionAttributes;
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};

/// The active constraint on the surface under the pointer.
pub enum Constraint {
    Locked,
    Confined { surface: WlSurface, origin: Point<f64, Logical>, region: Option<RegionAttributes> },
}

impl State {
    /// The pointer focus and the global position of its origin, while it's the surface under the pointer.
    fn constrainable_focus(&self) -> Option<(WlSurface, Point<f64, Logical>)> {
        let focus = self.nimbus.pointer.current_focus()?;
        let (surface, origin) = self.nimbus.pointer_target(self.nimbus.pointer_location)?;
        (surface == focus).then_some((surface, origin))
    }

    pub(super) fn active_constraint(&self) -> Option<Constraint> {
        let (surface, origin) = self.constrainable_focus()?;
        let local = (self.nimbus.pointer_location - origin).to_i32_round();
        with_pointer_constraint(&surface, &self.nimbus.pointer, |constraint| {
            let constraint = constraint.filter(|c| c.is_active())?;
            if !constraint.region().is_none_or(|r| r.contains(local)) {
                return None;
            }
            Some(match &*constraint {
                PointerConstraint::Locked(_) => Constraint::Locked,
                PointerConstraint::Confined(confined) => Constraint::Confined {
                    surface: surface.clone(),
                    origin,
                    region: confined.region().cloned(),
                },
            })
        })
    }

    /// Activates or deactivates the constraint on the pointer focus, as the pointer or a new constraint requires.
    pub fn refresh_pointer_constraint(&mut self) {
        match self.nimbus.keyboard.as_ref().map(|k| k.current_focus()) {
            Some(focus) => self.update_pointer_constraint(focus.as_ref()),
            None => self.update_pointer_constraint_with(true),
        }
    }

    /// Activates or deactivates the constraint on the pointer focus for the new `keyboard_focus`.
    fn update_pointer_constraint(&mut self, keyboard_focus: Option<&WlSurface>) {
        let Some((surface, _)) = self.constrainable_focus() else {
            return;
        };
        let root = super::root_surface(&surface);
        self.update_pointer_constraint_with(
            keyboard_focus.is_some_and(|focus| super::root_surface(focus) == root),
        );
    }

    fn update_pointer_constraint_with(&mut self, has_keyboard: bool) {
        let Some((surface, origin)) = self.constrainable_focus() else {
            return;
        };
        let local = (self.nimbus.pointer_location - origin).to_i32_round();
        with_pointer_constraint(&surface, &self.nimbus.pointer, |constraint| {
            let Some(constraint) = constraint else {
                return;
            };
            let inside = constraint.region().is_none_or(|r| r.contains(local));
            if constraint.is_active() && !has_keyboard {
                constraint.deactivate();
            } else if !constraint.is_active() && has_keyboard && inside {
                constraint.activate();
            }
        });
    }

    /// Where a relative motion from `from` to `to` ends inside a confinement, sliding along its edges.
    pub(super) fn confine(
        &self,
        from: Point<f64, Logical>,
        to: Point<f64, Logical>,
        surface: &WlSurface,
        origin: Point<f64, Logical>,
        region: Option<&RegionAttributes>,
    ) -> Point<f64, Logical> {
        let allowed = |p: Point<f64, Logical>| {
            let on_surface =
                self.nimbus.pointer_target(p).is_some_and(|(target, _)| &target == surface);
            on_surface && region.is_none_or(|r| r.contains((p - origin).to_i32_round()))
        };
        [to, Point::from((to.x, from.y)), Point::from((from.x, to.y))]
            .into_iter()
            .find(|&p| allowed(p))
            .unwrap_or(from)
    }

    /// Warps the pointer to a locked pointer's cursor hint, so the pointer reappears where the client drew it.
    pub fn apply_cursor_hint(&mut self, surface: &WlSurface, hint: Point<f64, Logical>) {
        let Some((focus, origin)) = self.constrainable_focus() else {
            return;
        };
        let locked = with_pointer_constraint(surface, &self.nimbus.pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active() && matches!(&*c, PointerConstraint::Locked(_)))
        });
        if &focus == surface && locked {
            let location = origin + hint;
            self.nimbus.pointer_location = location;
            self.nimbus.pointer.set_location(location);
            self.nimbus.queue_redraw_all();
        }
    }
}
