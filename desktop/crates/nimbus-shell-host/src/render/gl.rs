// SPDX-License-Identifier: MIT

//! Slint's FemtoVG renderer drawing with OpenGL ES through EGL.

use super::Renderer;
use crate::state::State;
use anyhow::{Context as _, anyhow, bail};
use glow::HasContext;
use glutin::api::egl::config::Config;
use glutin::api::egl::context::{NotCurrentContext, PossiblyCurrentContext};
use glutin::api::egl::display::Display;
use glutin::api::egl::surface::Surface;
use glutin::config::{Api, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use slint::platform::femtovg_renderer::{FemtoVGRenderer, OpenGLInterface};
use slint::platform::{WindowAdapter, WindowEvent};
use slint::{PhysicalSize, WindowSize};
use std::cell::{Cell, OnceCell};
use std::ffi::{CStr, c_void};
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy, QueueHandle};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// EGL on the compositor's connection, with the configuration every shell surface uses.
pub struct Gl {
    display: Display,
    config: Config,
    /// The context [`Gl::new`] checked, which the first surface takes.
    spare: Cell<Option<PossiblyCurrentContext>>,
    /// EGL uses the connection's `wl_display`, so it stays open as long as EGL does.
    conn: Connection,
}

impl Gl {
    /// Opens EGL on `conn` and checks that it can make an OpenGL ES context current.
    /// With `require_hardware`, a software rasterizer such as llvmpipe is an error,
    /// since Slint's software renderer redraws less.
    pub fn new(conn: &Connection, require_hardware: bool) -> anyhow::Result<Self> {
        let display = NonNull::new(conn.backend().display_ptr().cast::<c_void>())
            .context("the Wayland connection has no display")?;
        let handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display));
        // SAFETY: `_conn` keeps the `wl_display` alive as long as the EGL display.
        let display = unsafe { Display::new(handle) }.context("cannot open EGL")?;
        let template = ConfigTemplateBuilder::new()
            .with_api(Api::GLES2)
            .with_alpha_size(8)
            // FemtoVG fills paths through the stencil buffer.
            .with_stencil_size(8)
            .build();
        // SAFETY: the template has no native window or pixmap that could dangle.
        let config = unsafe { display.find_configs(template) }
            .context("cannot list EGL configurations")?
            .min_by_key(GlConfig::num_samples)
            .context("EGL has no configuration with alpha and stencil channels")?;
        let gl = Self { display, config, spare: Cell::new(None), conn: conn.clone() };

        let context = gl.create_context()?.make_current_surfaceless()?;
        // SAFETY: the context is current, and EGL provides its functions.
        let glow = unsafe {
            glow::Context::from_loader_function_cstr(|name| gl.display.get_proc_address(name))
        };
        // SAFETY: the context is current.
        let renderer = unsafe { glow.get_parameter_string(glow::RENDERER) };
        tracing::info!("OpenGL renderer: {renderer}");
        let software = gl
            .display
            .device()
            .is_ok_and(|device| device.extensions().contains("EGL_MESA_device_software"));
        if require_hardware && (software || is_software_rasterizer(&renderer)) {
            bail!("{renderer} rasterizes in software");
        }
        gl.spare.set(Some(context));
        Ok(gl)
    }

    fn create_context(&self) -> anyhow::Result<NotCurrentContext> {
        let gles2 = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);
        let any = ContextAttributesBuilder::new().build(None);
        // SAFETY: the attributes have no window handle that could dangle.
        unsafe {
            self.display
                .create_context(&self.config, &gles2)
                .or_else(|_| self.display.create_context(&self.config, &any))
        }
        .context("cannot create an OpenGL ES context")
    }
}

fn is_software_rasterizer(renderer: &str) -> bool {
    let renderer = renderer.to_lowercase();
    ["llvmpipe", "softpipe", "software rasterizer", "swiftshader"]
        .iter()
        .any(|name| renderer.contains(name))
}

type Size = (NonZeroU32, NonZeroU32);

/// The EGL context and window surface of one Wayland surface, shared by FemtoVG and [`GlRenderer`].
struct Egl {
    surface: OnceCell<Surface<WindowSurface>>,
    size: Cell<Option<Size>>,
    context: PossiblyCurrentContext,
    handle: RawWindowHandle,
    display: Display,
    config: Config,
    /// See [`Gl::conn`].
    _conn: Connection,
}

impl Egl {
    fn make_current(&self) -> Result<(), Error> {
        if !self.context.is_current() {
            match self.surface.get() {
                Some(surface) => self.context.make_current(surface)?,
                None => self.context.make_current_surfaceless()?,
            }
        }
        Ok(())
    }

    /// Makes the window surface current at `size`; returns whether it had another size.
    fn prepare(&self, size: Size) -> anyhow::Result<bool> {
        let resized = self.size.replace(Some(size)).is_some_and(|old| old != size);
        if let Some(surface) = self.surface.get() {
            self.make_current().map_err(|err| anyhow!(err))?;
            if resized {
                surface.resize(&self.context, size.0, size.1);
            }
            return Ok(resized);
        }
        let attributes =
            SurfaceAttributesBuilder::<WindowSurface>::new().build(self.handle, size.0, size.1);
        // SAFETY: the Wayland surface outlives the renderer; see `GlRenderer::new`.
        let surface = unsafe { self.display.create_window_surface(&self.config, &attributes) }
            .context("cannot create an EGL surface")?;
        self.context.make_current(&surface)?;
        // The Wayland surface's own frame callbacks pace the frames, so swapping buffers never waits.
        if let Err(err) = surface.set_swap_interval(&self.context, SwapInterval::DontWait) {
            tracing::debug!("cannot turn off EGL's frame pacing: {err}");
        }
        let _ = self.surface.set(surface);
        Ok(false)
    }

    /// Whether EGL drew the last frame at `size`.
    fn drew_at(&self, (width, height): Size) -> bool {
        self.surface.get().is_some_and(|surface| {
            surface.width() == Some(width.get()) && surface.height() == Some(height.get())
        })
    }
}

struct SharedEgl(Rc<Egl>);

// SAFETY: `get_proc_address` forwards to EGL.
unsafe impl OpenGLInterface for SharedEgl {
    fn ensure_current(&self) -> Result<(), Error> {
        self.0.make_current()
    }

    fn swap_buffers(&self) -> Result<(), Error> {
        let surface = self.0.surface.get().ok_or("the EGL surface doesn't exist yet")?;
        Ok(surface.swap_buffers(&self.0.context)?)
    }

    fn resize(&self, _width: NonZeroU32, _height: NonZeroU32) -> Result<(), Error> {
        // Slint rounds this size down from logical pixels; `GlRenderer::render` applies the exact one.
        Ok(())
    }

    fn get_proc_address(&self, name: &CStr) -> *const c_void {
        self.0.context.display().get_proc_address(name)
    }
}

/// The window adapter of a Slint window that FemtoVG draws.
struct GlWindow {
    window: slint::Window,
    renderer: FemtoVGRenderer,
    size: Cell<PhysicalSize>,
    needs_redraw: Cell<bool>,
}

impl WindowAdapter for GlWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn set_size(&self, size: WindowSize) {
        let scale_factor = self.window.scale_factor();
        self.size.set(size.to_physical(scale_factor));
        let size = size.to_logical(scale_factor);
        if let Err(err) = self.window.dispatch_event_with_result(WindowEvent::Resized { size }) {
            tracing::warn!("cannot resize a shell window: {err}");
        }
    }

    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
}

/// Renders with FemtoVG into an EGL window surface on the Wayland surface.
///
/// FemtoVG redraws the whole window for every frame.
pub struct GlRenderer {
    window: Rc<GlWindow>,
    egl: Rc<Egl>,
    surface: WlSurface,
}

impl GlRenderer {
    /// Renders onto `surface`, which must outlive the renderer.
    pub fn new(gl: &Gl, surface: &WlSurface) -> anyhow::Result<Self> {
        let pointer = NonNull::new(surface.id().as_ptr().cast::<c_void>())
            .context("the Wayland surface is gone")?;
        let context = match gl.spare.take() {
            Some(context) => context,
            None => gl.create_context()?.make_current_surfaceless()?,
        };
        let egl = Rc::new(Egl {
            surface: OnceCell::new(),
            size: Cell::new(None),
            context,
            handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(pointer)),
            display: gl.display.clone(),
            config: gl.config.clone(),
            _conn: gl.conn.clone(),
        });
        let renderer = FemtoVGRenderer::new(SharedEgl(egl.clone()))?;
        let window = Rc::new_cyclic(|adapter: &Weak<GlWindow>| GlWindow {
            window: slint::Window::new(adapter.clone()),
            renderer,
            size: Cell::default(),
            needs_redraw: Cell::new(true),
        });
        Ok(Self { window, egl, surface: surface.clone() })
    }
}

impl Renderer for GlRenderer {
    fn window_adapter(&self) -> Rc<dyn WindowAdapter> {
        self.window.clone()
    }

    fn render(&mut self, qh: &QueueHandle<State>) -> bool {
        let size = self.window.size.get();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return false;
        };
        if !self.window.needs_redraw.replace(false) {
            return false;
        }
        let resized = match self.egl.prepare((width, height)) {
            Ok(resized) => resized,
            Err(err) => {
                tracing::warn!("cannot draw a shell surface with OpenGL: {err:#}");
                return false;
            }
        };
        // Swapping buffers commits the surface, so the frame callback has to come first.
        self.surface.frame(qh, self.surface.clone());
        if let Err(err) = self.window.renderer.render() {
            tracing::warn!("cannot draw a shell surface with OpenGL: {err}");
        }
        // Mesa's software rasterizer draws the first frame after a resize at the old size.
        if resized && !self.egl.drew_at((width, height)) {
            self.window.needs_redraw.set(true);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_rasterizers_are_recognized() {
        assert!(is_software_rasterizer("llvmpipe (LLVM 19.1.7, 256 bits)"));
        assert!(is_software_rasterizer("Software Rasterizer"));
        assert!(is_software_rasterizer("Google SwiftShader"));
        assert!(!is_software_rasterizer("AMD Radeon Graphics (radeonsi, renoir, LLVM 19.1.7)"));
        assert!(!is_software_rasterizer("Mesa Intel(R) UHD Graphics 620 (KBL GT2)"));
    }
}
