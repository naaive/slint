// SPDX-License-Identifier: MIT

//! The shell on one output: a surface for each part of its view, sized to what the part shows.
//! The panel, dock, overlay, toasts, and OSD are layer surfaces; a popup is an `xdg_popup` of the part it belongs to.

use crate::platform::Windows;
use crate::state::State;
use crate::surface::{Scaling, SlintSurface};
use nimbus_shell::{Align, Part, PartWindow, Placement, PopupPlacement, Rect, ShellView};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::error::GlobalError;
use smithay_client_toolkit::globals::ProvidesBoundGlobal;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use smithay_client_toolkit::shell::xdg::popup::Popup;
use smithay_client_toolkit::shell::xdg::{XdgPositioner, XdgSurface};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor as PopupAnchor, ConstraintAdjustment, Gravity,
};
use wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase;

/// `xdg_popup.reposition` came with version 3 of `xdg_wm_base`.
const REPOSITION_VERSION: u32 = 3;

/// `xdg_wm_base`, which popups need, without the toplevel windows of smithay-client-toolkit's `XdgShell`.
pub struct WmBase(pub XdgWmBase);

impl ProvidesBoundGlobal<XdgWmBase, 5> for WmBase {
    fn bound_global(&self) -> Result<XdgWmBase, GlobalError> {
        Ok(self.0.clone())
    }
}

impl ProvidesBoundGlobal<XdgWmBase, 6> for WmBase {
    fn bound_global(&self) -> Result<XdgWmBase, GlobalError> {
        Ok(self.0.clone())
    }
}

/// What [`OutputShell::update`] needs from the [`State`].
pub struct Context<'a> {
    pub qh: &'a QueueHandle<State>,
    pub compositor: &'a CompositorState,
    pub layer_shell: &'a LayerShell,
    pub wm_base: Option<&'a WmBase>,
    pub windows: &'a Windows,
    pub scaling: &'a Scaling,
    /// The integer scale of the output, which new surfaces start with.
    pub scale: i32,
    /// The seat and serial of the last press, which a popup grabs input with.
    pub grab: Option<(&'a WlSeat, u32)>,
}

pub struct OutputShell {
    output: WlOutput,
    name: String,
    view: ShellView,
    /// Bottom to top, in the order of [`ShellView::parts`].
    parts: Vec<PartSurface>,
}

impl OutputShell {
    pub fn new(output: &WlOutput, name: String, view: ShellView) -> Self {
        Self { output: output.clone(), name, view, parts: Vec::new() }
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

    pub fn surfaces(&self) -> impl Iterator<Item = &SlintSurface> {
        self.parts.iter().map(|p| &p.surface)
    }

    pub fn surfaces_mut(&mut self) -> impl Iterator<Item = &mut SlintSurface> {
        self.parts.iter_mut().map(|p| &mut p.surface)
    }

    /// Whether `layer` is the layer surface of one of the parts.
    pub fn has_layer(&self, layer: &LayerSurface) -> bool {
        self.parts.iter().any(|p| p.layer() == Some(layer))
    }

    /// Applies a configure of a part's layer surface, in logical pixels.
    pub fn configure_layer(&mut self, layer: &LayerSurface, (width, height): (u32, u32)) {
        if let Some(part) = self.parts.iter_mut().find(|p| p.layer() == Some(layer)) {
            part.surface.configure(width, height);
        }
    }

    /// Applies a configure of the popup's surface; returns whether `popup` was this output's.
    pub fn configure_popup(&mut self, popup: &Popup) -> bool {
        let Some(part) = self.parts.iter_mut().find(|p| p.popup() == Some(popup)) else {
            return false;
        };
        if let Role::Popup { size, .. } = part.role {
            part.surface.configure(size.0, size.1);
        }
        true
    }

    /// Closes the popup once the compositor dismissed it; returns whether `popup` was this output's.
    pub fn popup_done(&mut self, popup: &Popup) -> bool {
        let Some(index) = self.parts.iter().position(|p| p.popup() == Some(popup)) else {
            return false;
        };
        let part = self.parts[index].window.part();
        tracing::debug!(output = %self.name, part = ?part, "part closed");
        self.parts.remove(index);
        self.view.close_popup();
        true
    }

    /// Brings the surfaces in line with the view's parts, then draws what changed.
    pub fn update(&mut self, context: &Context) {
        let wanted = self.view.parts();
        let placement = self.view.popup_placement();
        // Popups go before the parts below them, so a popup's parent never goes first.
        for index in (0..self.parts.len()).rev() {
            let part = &self.parts[index];
            let stale = match &part.role {
                Role::Popup { placement: shown, .. } => Some(*shown) != placement,
                Role::Layer { .. } => false,
            };
            if stale || !wanted.contains(&part.window.part()) {
                tracing::debug!(output = %self.name, part = ?part.window.part(), "part closed");
                self.parts.remove(index);
            }
        }
        for (order, part) in wanted.iter().enumerate() {
            if self.parts.iter().any(|p| p.window.part() == *part) {
                continue;
            }
            match self.create(context, *part, placement) {
                Ok(created) => {
                    tracing::debug!(output = %self.name, part = ?part, "part opened");
                    let at = self
                        .parts
                        .iter()
                        .filter(|p| wanted[..order].contains(&p.window.part()))
                        .count();
                    self.parts.insert(at, created);
                }
                Err(err) => {
                    tracing::warn!(output = %self.name, part = ?part, "cannot show a part: {err:#}");
                    if matches!(part, Part::Popup(_)) {
                        self.view.close_popup();
                    }
                }
            }
        }
        let mut reopen = false;
        for part in &mut self.parts {
            reopen |= !part.update(context);
        }
        if reopen {
            // The popup changed size without `xdg_popup.reposition`; a new one takes its place.
            self.parts.retain(|p| !p.reopen);
        }
    }

    fn create(
        &self,
        context: &Context,
        part: Part,
        placement: Option<PopupPlacement>,
    ) -> anyhow::Result<PartSurface> {
        let wl_surface = context.compositor.create_surface(context.qh);
        let created = context.windows.create(&wl_surface, || self.view.create(part)).and_then(
            |(window, renderer)| {
                window.show()?;
                Ok((window, renderer))
            },
        );
        let (window, renderer) = created.inspect_err(|_| wl_surface.destroy())?;
        let role = match window.placement() {
            Some(layer) => Role::layer(context, &self.output, &wl_surface, part, &layer),
            None => {
                let popup = || {
                    let placement = placement.ok_or_else(|| anyhow::anyhow!("no popup is open"))?;
                    let parent = self
                        .parts
                        .iter()
                        .find(|p| p.window.part() == placement.parent)
                        .and_then(PartSurface::layer)
                        .ok_or_else(|| anyhow::anyhow!("the popup's part isn't shown"))?;
                    Role::popup(context, &wl_surface, parent, &window, placement)
                };
                popup().inspect_err(|_| wl_surface.destroy())?
            }
        };
        let surface =
            SlintSurface::new(wl_surface, renderer, context.scaling, context.scale, context.qh);
        Ok(PartSurface { surface, window, role, reopen: false })
    }
}

impl Drop for OutputShell {
    fn drop(&mut self) {
        // Popups go before the parts below them.
        while self.parts.pop().is_some() {}
    }
}

/// The window of one part on its surface.
struct PartSurface {
    surface: SlintSurface,
    window: PartWindow,
    role: Role,
    /// The popup changed size, which only a new popup can show.
    reopen: bool,
}

enum Role {
    Layer {
        layer: LayerSurface,
        /// What the layer surface was last set up with.
        applied: Placement,
    },
    Popup {
        popup: Popup,
        placement: PopupPlacement,
        /// The size of its surface, with the room for its shadow, in logical pixels.
        size: (u32, u32),
        /// The popup within its surface.
        geometry: Rect,
    },
}

impl Role {
    fn layer(
        context: &Context,
        output: &WlOutput,
        wl_surface: &WlSurface,
        part: Part,
        placement: &Placement,
    ) -> Self {
        let namespace = match part {
            Part::Panel => "nimbus-panel",
            Part::Dock => "nimbus-dock",
            Part::Overlay => "nimbus-overlay",
            Part::Toasts => "nimbus-toasts",
            Part::Osd => "nimbus-osd",
            Part::Popup(_) => "nimbus-popup",
        };
        let layer = context.layer_shell.create_layer_surface(
            context.qh,
            wl_surface.clone(),
            layer_of(placement),
            Some(namespace),
            Some(output),
        );
        apply(&layer, placement);
        layer.commit();
        Self::Layer { layer, applied: *placement }
    }

    fn popup(
        context: &Context,
        wl_surface: &WlSurface,
        parent: &LayerSurface,
        window: &PartWindow,
        placement: PopupPlacement,
    ) -> anyhow::Result<Self> {
        let wm_base =
            context.wm_base.ok_or_else(|| anyhow::anyhow!("the compositor lacks xdg-shell"))?;
        let geometry = window.geometry();
        let positioner = positioner(wm_base, &placement, geometry)?;
        let popup =
            Popup::from_surface(None, &positioner, context.qh, wl_surface.clone(), wm_base)?;
        parent.get_popup(popup.xdg_popup());
        set_geometry(&popup, geometry);
        if let Some((seat, serial)) = context.grab {
            popup.xdg_popup().grab(seat, serial);
        }
        popup.wl_surface().commit();
        Ok(Self::Popup { popup, placement, size: surface_size(window), geometry })
    }
}

impl PartSurface {
    fn layer(&self) -> Option<&LayerSurface> {
        match &self.role {
            Role::Layer { layer, .. } => Some(layer),
            Role::Popup { .. } => None,
        }
    }

    fn popup(&self) -> Option<&Popup> {
        match &self.role {
            Role::Popup { popup, .. } => Some(popup),
            Role::Layer { .. } => None,
        }
    }

    /// Brings the role in line with the window, then draws what changed.
    /// Returns false when the part needs a new surface.
    fn update(&mut self, context: &Context) -> bool {
        match &mut self.role {
            Role::Layer { layer, applied } => {
                if let Some(placement) = self.window.placement()
                    && placement != *applied
                {
                    if layer_of(&placement) != layer_of(applied) {
                        layer.set_layer(layer_of(&placement));
                    }
                    apply(layer, &placement);
                    *applied = placement;
                    self.surface.commit_later();
                }
            }
            Role::Popup { popup, placement, size, geometry } => {
                let new_size = surface_size(&self.window);
                let new_geometry = self.window.geometry();
                if (new_size, new_geometry) != (*size, *geometry) {
                    let reposition = context
                        .wm_base
                        .filter(|_| popup.xdg_popup().version() >= REPOSITION_VERSION);
                    let Some(positioner) = reposition
                        .and_then(|wm_base| positioner(wm_base, placement, new_geometry).ok())
                    else {
                        self.reopen = true;
                        return false;
                    };
                    popup.reposition(&positioner, 0);
                    set_geometry(popup, new_geometry);
                    (*size, *geometry) = (new_size, new_geometry);
                    self.surface.commit_later();
                }
            }
        }
        let rendered = self.surface.render(context.qh);
        if rendered {
            // Parts lay out what they show while rendering, such as new toasts.
            self.surface.set_input_region(context.compositor, self.window.input_region());
        }
        self.surface.commit();
        true
    }
}

fn layer_of(placement: &Placement) -> Layer {
    // The overlay layer is above fullscreen windows.
    if placement.above_fullscreen { Layer::Overlay } else { Layer::Top }
}

/// A logical length as whole pixels for the protocol, rounded up.
fn pixels(length: f32) -> u32 {
    length.max(0.0).ceil() as u32
}

/// Sets up a layer surface for `placement`; applies with the next commit.
fn apply(layer: &LayerSurface, placement: &Placement) {
    let edges = placement.edges;
    let anchor = [
        (edges.top, Anchor::TOP),
        (edges.bottom, Anchor::BOTTOM),
        (edges.left, Anchor::LEFT),
        (edges.right, Anchor::RIGHT),
    ]
    .into_iter()
    .filter(|(attached, _)| *attached)
    .fold(Anchor::empty(), |anchor, (_, edge)| anchor | edge);
    layer.set_anchor(anchor);
    // A size of 0 spans the output between two attached edges, and is an error otherwise.
    let size = |length: f32, spans: bool| if spans { 0 } else { pixels(length).max(1) };
    layer.set_size(
        size(placement.width, edges.left && edges.right),
        size(placement.height, edges.top && edges.bottom),
    );
    let margin = placement.margin.round() as i32;
    layer.set_margin(margin, margin, margin, margin);
    layer.set_exclusive_zone(placement.exclusive_zone.map_or(-1, |zone| pixels(zone) as i32));
    layer.set_keyboard_interactivity(if placement.keyboard {
        KeyboardInteractivity::Exclusive
    } else {
        KeyboardInteractivity::None
    });
}

fn surface_size(window: &PartWindow) -> (u32, u32) {
    let (width, height) = window.size();
    (pixels(width).max(1), pixels(height).max(1))
}

/// Tells the compositor which part of the popup's surface is the popup, without the room for its shadow.
fn set_geometry(popup: &Popup, geometry: Rect) {
    popup.xdg_shell_surface().set_window_geometry(
        geometry.x.round() as u32,
        geometry.y.round() as u32,
        pixels(geometry.width).max(1),
        pixels(geometry.height).max(1),
    );
}

/// Places a popup of `geometry`'s size next to its anchor, as `placement` says, and on the output.
fn positioner(
    wm_base: &WmBase,
    placement: &PopupPlacement,
    geometry: Rect,
) -> anyhow::Result<XdgPositioner> {
    let positioner = XdgPositioner::new(wm_base)?;
    positioner
        .set_size(pixels(geometry.width).max(1) as i32, pixels(geometry.height).max(1) as i32);
    let anchor = placement.anchor;
    positioner.set_anchor_rect(
        anchor.x.floor() as i32,
        anchor.y.floor() as i32,
        pixels(anchor.width).max(1) as i32,
        pixels(anchor.height).max(1) as i32,
    );
    let (edge, gravity) = match (placement.below, placement.align) {
        (true, Align::Center) => (PopupAnchor::Bottom, Gravity::Bottom),
        (true, Align::End) => (PopupAnchor::BottomRight, Gravity::BottomLeft),
        (false, Align::Center) => (PopupAnchor::Top, Gravity::Top),
        (false, Align::End) => (PopupAnchor::TopRight, Gravity::TopLeft),
    };
    positioner.set_anchor(edge);
    positioner.set_gravity(gravity);
    let gap = placement.gap.round() as i32;
    positioner.set_offset(0, if placement.below { gap } else { -gap });
    positioner
        .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY);
    Ok(positioner)
}
