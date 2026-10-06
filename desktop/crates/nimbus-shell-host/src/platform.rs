// SPDX-License-Identifier: MIT

//! The Slint platform: each Slint window gets a [`Renderer`] that draws it onto a Wayland surface.

use crate::render::Renderer;
use anyhow::anyhow;
use slint::PlatformError;
use slint::platform::{Platform, WindowAdapter};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_surface::WlSurface;

type NewRenderer = Box<dyn Fn(&WlSurface) -> Box<dyn Renderer>>;

/// The window [`Windows::create`] is creating.
#[derive(Default)]
struct Creation {
    surface: Option<WlSurface>,
    renderer: Option<Box<dyn Renderer>>,
}

struct ShellPlatform {
    new_renderer: NewRenderer,
    creation: Rc<RefCell<Creation>>,
    start: Instant,
}

impl Platform for ShellPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let mut creation = self.creation.borrow_mut();
        let surface = creation
            .surface
            .take()
            .ok_or_else(|| PlatformError::from("a shell window needs a surface"))?;
        let renderer = (self.new_renderer)(&surface);
        let adapter = renderer.window_adapter();
        creation.renderer = Some(renderer);
        Ok(adapter)
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Creates Slint components together with the renderers of their windows.
pub struct Windows {
    creation: Rc<RefCell<Creation>>,
}

impl Windows {
    /// Installs the Slint platform for this thread; `new_renderer` makes the renderer of each new window,
    /// for the surface it shows on.
    pub fn install(
        new_renderer: impl Fn(&WlSurface) -> Box<dyn Renderer> + 'static,
    ) -> anyhow::Result<Self> {
        let creation = Rc::new(RefCell::new(Creation::default()));
        slint::platform::set_platform(Box::new(ShellPlatform {
            new_renderer: Box::new(new_renderer),
            creation: creation.clone(),
            start: Instant::now(),
        }))
        .map_err(|e| anyhow!("cannot install the Slint platform: {e}"))?;
        Ok(Self { creation })
    }

    /// Creates a component with `create`, with its window on `surface`, and returns it with the window's renderer.
    pub fn create<T>(
        &self,
        surface: &WlSurface,
        create: impl FnOnce() -> Result<T, PlatformError>,
    ) -> Result<(T, Box<dyn Renderer>), PlatformError> {
        self.creation.borrow_mut().surface = Some(surface.clone());
        let component = create();
        let Creation { renderer, .. } = std::mem::take(&mut *self.creation.borrow_mut());
        Ok((component?, renderer.ok_or_else(|| PlatformError::from("no window was created"))?))
    }
}
