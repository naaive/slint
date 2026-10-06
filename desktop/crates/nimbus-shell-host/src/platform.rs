// SPDX-License-Identifier: MIT

//! The Slint platform: each Slint window gets a [`Renderer`] that draws it onto a Wayland surface.

use crate::render::Renderer;
use anyhow::anyhow;
use slint::PlatformError;
use slint::platform::{Platform, WindowAdapter};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

type NewRenderer = Box<dyn Fn() -> Box<dyn Renderer>>;

struct ShellPlatform {
    new_renderer: NewRenderer,
    created: Rc<RefCell<Vec<Box<dyn Renderer>>>>,
    start: Instant,
}

impl Platform for ShellPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let renderer = (self.new_renderer)();
        let adapter = renderer.window_adapter();
        self.created.borrow_mut().push(renderer);
        Ok(adapter)
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Creates Slint components together with the renderers of their windows.
pub struct Windows {
    created: Rc<RefCell<Vec<Box<dyn Renderer>>>>,
}

impl Windows {
    /// Installs the Slint platform for this thread; `new_renderer` makes the renderer of each new window.
    pub fn install(new_renderer: impl Fn() -> Box<dyn Renderer> + 'static) -> anyhow::Result<Self> {
        let created = Rc::new(RefCell::new(Vec::new()));
        slint::platform::set_platform(Box::new(ShellPlatform {
            new_renderer: Box::new(new_renderer),
            created: created.clone(),
            start: Instant::now(),
        }))
        .map_err(|e| anyhow!("cannot install the Slint platform: {e}"))?;
        Ok(Self { created })
    }

    /// Creates a component with `create` and returns it with the renderer of its window.
    pub fn create<T>(
        &self,
        create: impl FnOnce() -> Result<T, PlatformError>,
    ) -> Result<(T, Box<dyn Renderer>), PlatformError> {
        let component = create();
        let renderer = self.created.borrow_mut().pop();
        self.created.borrow_mut().clear();
        Ok((component?, renderer.ok_or_else(|| PlatformError::from("no window was created"))?))
    }
}
