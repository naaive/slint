// SPDX-License-Identifier: MIT

//! A Slint window on a Wayland surface: its size, scale, input region, and frames.

use crate::render::Renderer;
use nimbus_shell::Rect;
use slint::platform::{WindowAdapter, WindowEvent};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use std::rc::Rc;
use wayland_client::QueueHandle;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::state::State;

/// The globals that let surfaces render at fractional scales.
#[derive(Default)]
pub struct Scaling {
    pub viewporter: Option<WpViewporter>,
    pub fractional: Option<WpFractionalScaleManagerV1>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    /// From `wp_fractional_scale_v1`, drawn through a viewport.
    Fractional(f64),
    /// From the outputs the surface is on, drawn with `wl_surface.set_buffer_scale`.
    Integer(i32),
}

impl Scale {
    pub fn factor(self) -> f64 {
        match self {
            Self::Fractional(scale) => scale,
            Self::Integer(scale) => f64::from(scale),
        }
    }
}

/// A Slint window shown on a Wayland surface, which some role such as a layer surface owns.
///
/// It renders at the surface's buffer scale, only when Slint has changes and the last frame was shown.
pub struct SlintSurface {
    surface: WlSurface,
    renderer: Box<dyn Renderer>,
    adapter: Rc<dyn WindowAdapter>,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    /// The size the compositor configured, in logical pixels.
    size: Option<(u32, u32)>,
    scale: Scale,
    input_region: Option<Vec<Rect>>,
    /// The frame callback of the last frame hasn't arrived yet.
    frame_pending: bool,
    /// State changed that only applies with the next commit.
    needs_commit: bool,
}

impl SlintSurface {
    /// Wraps `surface`; `scale` is the integer scale of its output until the compositor says otherwise.
    pub fn new(
        surface: WlSurface,
        renderer: Box<dyn Renderer>,
        scaling: &Scaling,
        scale: i32,
        qh: &QueueHandle<State>,
    ) -> Self {
        let viewport = scaling.viewporter.as_ref().map(|v| v.get_viewport(&surface, qh, ()));
        let fractional = scaling
            .fractional
            .as_ref()
            .filter(|_| viewport.is_some())
            .map(|f| f.get_fractional_scale(&surface, qh, surface.clone()));
        let adapter = renderer.window_adapter();
        Self {
            surface,
            renderer,
            adapter,
            viewport,
            fractional,
            size: None,
            scale: Scale::Integer(scale.max(1)),
            input_region: None,
            frame_pending: false,
            needs_commit: false,
        }
    }

    pub fn wl_surface(&self) -> &WlSurface {
        &self.surface
    }

    pub fn window(&self) -> &slint::Window {
        self.adapter.window()
    }

    pub fn dispatch(&self, event: WindowEvent) {
        if let Err(err) = self.window().dispatch_event_with_result(event) {
            tracing::warn!("the shell rejected an event: {err}");
        }
    }

    /// Applies the size the compositor configured, in logical pixels.
    pub fn configure(&mut self, width: u32, height: u32) {
        if self.size != Some((width, height)) {
            self.size = Some((width, height));
            self.apply_size();
        }
    }

    pub fn set_scale(&mut self, scale: Scale) {
        // Once the compositor reports fractional scales, they take precedence over output scales.
        if matches!(scale, Scale::Integer(_)) && self.fractional.is_some() {
            return;
        }
        if scale != self.scale {
            self.scale = scale;
            self.apply_size();
        }
    }

    /// Sizes the window to the surface's size in buffer pixels, so it renders exactly as many pixels as the buffer holds.
    fn apply_size(&mut self) {
        let Some((width, height)) = self.size else {
            return;
        };
        let factor = self.scale.factor();
        let physical = |logical: u32| (f64::from(logical) * factor).round() as u32;
        self.dispatch(WindowEvent::ScaleFactorChanged { scale_factor: factor as f32 });
        self.window().set_size(slint::PhysicalSize::new(physical(width), physical(height)));
        match self.scale {
            Scale::Fractional(_) => {
                self.surface.set_buffer_scale(1);
                if let Some(viewport) = &self.viewport {
                    let size = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
                    viewport.set_destination(size(width), size(height));
                }
            }
            Scale::Integer(scale) => self.surface.set_buffer_scale(scale),
        }
        self.window().request_redraw();
    }

    /// Limits pointer input to `rects`, in logical pixels; elsewhere it reaches the surfaces below.
    pub fn set_input_region(&mut self, compositor: &CompositorState, rects: Vec<Rect>) {
        if self.input_region.as_ref() == Some(&rects) {
            return;
        }
        match Region::new(compositor) {
            Ok(region) => {
                for rect in &rects {
                    let x = rect.x.floor();
                    let y = rect.y.floor();
                    let width = (rect.x + rect.width).ceil() - x;
                    let height = (rect.y + rect.height).ceil() - y;
                    region.add(x as i32, y as i32, width as i32, height as i32);
                }
                self.surface.set_input_region(Some(region.wl_region()));
                self.input_region = Some(rects);
                self.needs_commit = true;
            }
            Err(err) => tracing::warn!("cannot create an input region: {err}"),
        }
    }

    /// Marks role state, such as a layer surface's keyboard interactivity, for the next commit.
    pub fn commit_later(&mut self) {
        self.needs_commit = true;
    }

    pub fn frame_done(&mut self) {
        self.frame_pending = false;
    }

    /// Whether Slint animates and no frame callback will wake the event loop for it.
    pub fn animates_unpaced(&self) -> bool {
        !self.frame_pending && self.window().has_active_animations()
    }

    /// Renders what changed, once the surface is configured and the last frame was shown.
    /// Returns whether it drew a frame.
    pub fn render(&mut self, qh: &QueueHandle<State>) -> bool {
        if self.size.is_none() || self.frame_pending {
            return false;
        }
        let rendered = self.renderer.render(qh);
        if rendered {
            self.frame_pending = true;
            self.needs_commit = true;
        }
        rendered
    }

    /// Commits a rendered frame and other pending state.
    pub fn commit(&mut self) {
        if std::mem::take(&mut self.needs_commit) {
            self.surface.commit();
        }
    }
}

impl Drop for SlintSurface {
    fn drop(&mut self) {
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        if let Some(fractional) = self.fractional.take() {
            fractional.destroy();
        }
        // A shown Slint window keeps its component, timers, and models alive until hidden.
        if let Err(err) = self.window().hide() {
            tracing::warn!("cannot hide a shell window: {err}");
        }
    }
}
