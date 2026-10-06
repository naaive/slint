// SPDX-License-Identifier: MIT

//! The shell on one output: its view on a full-output layer surface,
//! and thin layer surfaces along the edges that reserve the panel's and dock's space.

use crate::state::State;
use crate::surface::SlintSurface;
use nimbus_shell::{Exclusive, ShellView};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use wayland_client::QueueHandle;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

pub struct OutputShell {
    output: WlOutput,
    name: String,
    view: ShellView,
    pub surface: SlintSurface,
    layer: LayerSurface,
    reservations: Vec<Reservation>,
    exclusive: Option<Exclusive>,
    wants_keyboard: bool,
}

impl OutputShell {
    /// Creates the view for the output named `name`, on a transparent layer surface over all of it.
    pub fn new(state: &State, output: &WlOutput, name: String) -> anyhow::Result<Self> {
        let wl_surface = state.compositor.create_surface(&state.qh);
        let created = state
            .windows
            .create(&wl_surface, || ShellView::new(&state.model, &name))
            .and_then(|(view, renderer)| {
                view.show()?;
                Ok((view, renderer))
            });
        let (view, renderer) = created.inspect_err(|_| wl_surface.destroy())?;
        let layer = state.layer_shell.create_layer_surface(
            &state.qh,
            wl_surface.clone(),
            Layer::Top,
            Some("nimbus-shell"),
            Some(output),
        );
        layer.set_anchor(Anchor::all());
        // Covers the whole output, including the space other surfaces reserve.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let scale = state.output_scale(output);
        let mut surface = SlintSurface::new(wl_surface, renderer, &state.scaling, scale, &state.qh);
        surface.set_input_region(&state.compositor, Vec::new());
        layer.commit();
        Ok(Self {
            output: output.clone(),
            name,
            view,
            surface,
            layer,
            reservations: Vec::new(),
            exclusive: None,
            wants_keyboard: false,
        })
    }

    pub fn output(&self) -> &WlOutput {
        &self.output
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn view(&self) -> &ShellView {
        &self.view
    }

    pub fn is(&self, layer: &LayerSurface) -> bool {
        &self.layer == layer
    }

    /// Applies a configure of one of the reservations; returns whether `layer` was one.
    pub fn configure_reservation(
        &mut self,
        layer: &LayerSurface,
        (width, height): (u32, u32),
        buffer: Option<&WlBuffer>,
    ) -> bool {
        let Some(reservation) = self.reservations.iter().find(|r| &r.layer == layer) else {
            return false;
        };
        let size = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        reservation.viewport.set_destination(size(width), size(height));
        reservation.layer.wl_surface().attach(buffer, 0, 0);
        reservation.layer.commit();
        true
    }

    /// Forgets a surface the compositor closed; returns whether it was the view's own.
    pub fn closed(&mut self, layer: &LayerSurface) -> bool {
        self.reservations.retain(|r| &r.layer != layer);
        self.is(layer)
    }

    /// Brings the surfaces in line with the view, then draws what changed.
    pub fn update(&mut self, globals: &Globals) {
        // Without keyboard interactivity otherwise, clicks on the panel never take the keyboard from windows.
        let wants_keyboard = self.view.wants_keyboard();
        let keyboard_changed = wants_keyboard != self.wants_keyboard;
        if keyboard_changed {
            self.wants_keyboard = wants_keyboard;
            let (interactivity, layer) = if wants_keyboard {
                // The overlay layer is above fullscreen windows.
                (KeyboardInteractivity::Exclusive, Layer::Overlay)
            } else {
                (KeyboardInteractivity::None, Layer::Top)
            };
            self.layer.set_keyboard_interactivity(interactivity);
            self.layer.set_layer(layer);
            self.surface.commit_later();
        }
        let rendered = self.surface.render(globals.qh);
        if rendered || keyboard_changed || self.exclusive.is_none() {
            let exclusive = self.view.exclusive_zone();
            if self.exclusive != Some(exclusive) {
                self.reserve(globals, exclusive);
            }
            // See `ShellView::input_region` for why the region follows rendering.
            self.surface.set_input_region(globals.compositor, self.view.input_region());
        }
        self.surface.commit();
    }

    /// Replaces the reservations with one per edge the view occupies.
    fn reserve(&mut self, globals: &Globals, exclusive: Exclusive) {
        self.exclusive = Some(exclusive);
        self.reservations.clear();
        let Exclusive { top, bottom } = exclusive;
        for (edge, zone) in [(Anchor::TOP, top), (Anchor::BOTTOM, bottom)] {
            let zone = zone.ceil() as i32;
            if zone > 0 {
                match Reservation::new(globals, &self.output, edge, zone) {
                    Some(reservation) => self.reservations.push(reservation),
                    None => tracing::warn!(
                        "the compositor lacks viewporter or single-pixel-buffer; windows may cover the panel"
                    ),
                }
            }
        }
    }
}

/// What [`OutputShell::update`] needs from the [`State`].
pub struct Globals<'a> {
    pub qh: &'a QueueHandle<State>,
    pub compositor: &'a CompositorState,
    pub layer_shell: &'a LayerShell,
    pub viewporter: Option<&'a WpViewporter>,
    /// A transparent single-pixel buffer, which the reservations show.
    pub transparent: Option<&'a WlBuffer>,
}

/// A transparent strip along the top or bottom edge that reserves an exclusive zone, without taking input.
struct Reservation {
    viewport: WpViewport,
    layer: LayerSurface,
}

impl Reservation {
    fn new(globals: &Globals, output: &WlOutput, edge: Anchor, zone: i32) -> Option<Self> {
        let (Some(viewporter), Some(_)) = (globals.viewporter, globals.transparent) else {
            return None;
        };
        let wl_surface = globals.compositor.create_surface(globals.qh);
        let viewport = viewporter.get_viewport(&wl_surface, globals.qh, ());
        let layer = globals.layer_shell.create_layer_surface(
            globals.qh,
            wl_surface.clone(),
            Layer::Top,
            Some("nimbus-shell-reservation"),
            Some(output),
        );
        layer.set_anchor(edge | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, zone.unsigned_abs());
        layer.set_exclusive_zone(zone);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        if let Ok(region) = Region::new(globals.compositor) {
            wl_surface.set_input_region(Some(region.wl_region()));
        }
        layer.commit();
        Some(Self { viewport, layer })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.viewport.destroy();
    }
}
