// SPDX-License-Identifier: MIT

//! How a Slint window becomes the contents of a Wayland surface.

mod gl;
mod software;
mod window;

use crate::state::State;
use slint::platform::WindowAdapter;
use std::rc::Rc;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};

pub use gl::{Gl, GlRenderer};
pub use software::SoftwareRenderer;

/// Draws one Slint window onto the Wayland surface it was created for.
///
/// The platform creates a renderer for every Slint window; see [`crate::platform::Windows`].
pub trait Renderer {
    /// The window adapter Slint draws through.
    fn window_adapter(&self) -> Rc<dyn WindowAdapter>;

    /// Draws what changed since the last frame onto the surface, and requests a frame callback for it.
    /// Returns whether there is a frame to commit; without changes, or while no buffer is free, there's none.
    fn render(&mut self, qh: &QueueHandle<State>) -> bool;
}

/// The renderer `NIMBUS_SHELL_RENDERER` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preference {
    /// OpenGL on a GPU, otherwise software.
    Auto,
    Software,
    /// OpenGL, even through a software rasterizer, otherwise software.
    Gl,
}

impl Preference {
    const VARIABLE: &str = "NIMBUS_SHELL_RENDERER";

    pub fn from_env() -> Self {
        Self::parse(std::env::var(Self::VARIABLE).ok().as_deref())
    }

    fn parse(value: Option<&str>) -> Self {
        match value {
            None | Some("") => Self::Auto,
            Some("software") => Self::Software,
            Some("gl") => Self::Gl,
            Some(other) => {
                tracing::warn!("ignoring {}={other}; use 'software' or 'gl'", Self::VARIABLE);
                Self::Auto
            }
        }
    }

    /// Opens OpenGL with `open` unless software is preferred, and returns it if that works.
    /// `open` takes whether a software rasterizer counts as a failure.
    fn choose<T>(self, open: impl FnOnce(bool) -> anyhow::Result<T>) -> Option<T> {
        let opened = match self {
            Self::Software => return None,
            Self::Auto => open(true),
            Self::Gl => open(false),
        };
        match opened {
            Ok(gl) => Some(gl),
            Err(err) if self == Self::Auto => {
                tracing::info!("not using OpenGL: {err:#}");
                None
            }
            Err(err) => {
                tracing::warn!("cannot use OpenGL, falling back to software: {err:#}");
                None
            }
        }
    }
}

/// Makes the renderer of each Slint window: a [`GlRenderer`] if OpenGL works, otherwise a [`SoftwareRenderer`].
pub struct Renderers {
    shm: WlShm,
    gl: Option<Gl>,
}

impl Renderers {
    pub fn new(conn: &Connection, shm: WlShm, preference: Preference) -> Self {
        let gl = preference.choose(|require_hardware| Gl::new(conn, require_hardware));
        tracing::info!(
            "rendering the shell {}",
            if gl.is_some() { "with OpenGL" } else { "in software" }
        );
        Self { shm, gl }
    }

    /// Makes the renderer for a window on `surface`, which must outlive it.
    pub fn create(&self, surface: &WlSurface) -> Box<dyn Renderer> {
        if let Some(gl) = &self.gl {
            match GlRenderer::new(gl, surface) {
                Ok(renderer) => return Box::new(renderer),
                Err(err) => {
                    tracing::warn!("cannot draw a surface with OpenGL, using software: {err:#}")
                }
            }
        }
        Box::new(SoftwareRenderer::new(self.shm.clone(), surface.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn the_variable_selects_the_preference() {
        assert_eq!(Preference::parse(None), Preference::Auto);
        assert_eq!(Preference::parse(Some("")), Preference::Auto);
        assert_eq!(Preference::parse(Some("software")), Preference::Software);
        assert_eq!(Preference::parse(Some("gl")), Preference::Gl);
        assert_eq!(Preference::parse(Some("vulkan")), Preference::Auto);
    }

    #[test]
    fn software_never_opens_opengl() {
        let chosen = Preference::Software
            .choose(|_| -> anyhow::Result<()> { panic!("software must not open OpenGL") });
        assert_eq!(chosen, None);
    }

    #[test]
    fn auto_requires_hardware_and_gl_does_not() {
        assert_eq!(Preference::Auto.choose(Ok), Some(true));
        assert_eq!(Preference::Gl.choose(Ok), Some(false));
    }

    #[test]
    fn failing_opengl_falls_back_to_software() {
        for preference in [Preference::Auto, Preference::Gl] {
            assert_eq!(
                preference.choose(|_| -> anyhow::Result<()> { Err(anyhow!("no EGL")) }),
                None
            );
        }
    }
}
