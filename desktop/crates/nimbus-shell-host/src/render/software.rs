// SPDX-License-Identifier: MIT

//! Slint's software renderer drawing into two alternating `wl_shm` buffers.

use super::Renderer;
use crate::state::State;
use anyhow::Context;
use slint::PhysicalSize;
use slint::platform::WindowAdapter;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use std::rc::Rc;
use wayland_client::QueueHandle;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_surface::WlSurface;

/// A premultiplied `wl_shm` ARGB8888 pixel, which is little-endian and so blue first in memory.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Argb8888 {
    blue: u8,
    green: u8,
    red: u8,
    alpha: u8,
}

impl TargetPixel for Argb8888 {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let mut pixel = PremultipliedRgbaColor {
            red: self.red,
            green: self.green,
            blue: self.blue,
            alpha: self.alpha,
        };
        pixel.blend(color);
        *self = Self { blue: pixel.blue, green: pixel.green, red: pixel.red, alpha: pixel.alpha };
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self { blue, green, red, alpha: u8::MAX }
    }

    fn background() -> Self {
        Self::default()
    }
}

/// Renders with Slint's software renderer into two buffers that take turns on the surface.
///
/// Slint's [`RepaintBufferType::SwappedBuffers`] redraws what changed in the last two frames,
/// so each buffer only needs the regions that changed since it was last shown.
pub struct SoftwareRenderer {
    window: Rc<MinimalSoftwareWindow>,
    surface: WlSurface,
    shm: WlShm,
    pool: Option<SlotPool>,
    buffers: Vec<Buffer>,
    /// The size the buffers were made for.
    size: PhysicalSize,
    next: usize,
    /// The buffers are new, so Slint must draw everything.
    fresh: bool,
}

impl SoftwareRenderer {
    pub fn new(shm: WlShm, surface: WlSurface) -> Self {
        Self {
            window: MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers),
            surface,
            shm,
            pool: None,
            buffers: Vec::new(),
            size: PhysicalSize::default(),
            next: 0,
            fresh: true,
        }
    }

    /// Makes two buffers of the window's size, unless they already exist.
    fn ensure_buffers(&mut self) -> anyhow::Result<()> {
        let size = WindowAdapter::size(&*self.window);
        if size == self.size && !self.buffers.is_empty() {
            return Ok(());
        }
        let width = i32::try_from(size.width).context("the window is too wide")?;
        let height = i32::try_from(size.height).context("the window is too tall")?;
        let stride = width.checked_mul(4).context("the window is too wide")?;
        let len = usize::try_from(stride)?.checked_mul(usize::try_from(height)?).context("size")?;
        let pool = match &mut self.pool {
            Some(pool) => pool,
            None => self.pool.insert(SlotPool::new(2 * len, &Shm::from(self.shm.clone()))?),
        };
        // Buffers the compositor still holds are destroyed once it releases them.
        self.buffers.clear();
        for _ in 0..2 {
            let (buffer, _) =
                pool.create_buffer(width, height, stride, wl_shm::Format::Argb8888)?;
            self.buffers.push(buffer);
        }
        self.size = size;
        self.next = 0;
        self.fresh = true;
        self.window.request_redraw();
        Ok(())
    }
}

impl Renderer for SoftwareRenderer {
    fn window_adapter(&self) -> Rc<dyn WindowAdapter> {
        self.window.clone()
    }

    fn render(&mut self, qh: &QueueHandle<State>) -> bool {
        let size = WindowAdapter::size(&*self.window);
        if size.width == 0 || size.height == 0 {
            return false;
        }
        if let Err(err) = self.ensure_buffers() {
            tracing::warn!("cannot allocate shell buffers: {err:#}");
            return false;
        }
        let Self { window, surface, pool: Some(pool), buffers, next, fresh, .. } = self else {
            return false;
        };
        let buffer = &buffers[*next];
        // The compositor still reads this buffer; its release wakes the event loop for another try.
        let Some(canvas) = buffer.canvas(pool) else {
            return false;
        };
        let full = *fresh;
        let mut damage = Vec::new();
        let drawn =
            window.draw_if_needed(|renderer| {
                if full {
                    // Changing the buffer type drops what the renderer remembers of earlier frames.
                    renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
                    renderer.set_repaint_buffer_type(RepaintBufferType::SwappedBuffers);
                }
                let pixels: &mut [Argb8888] = bytemuck::cast_slice_mut(canvas);
                let region = renderer.render(pixels, size.width as usize);
                damage.extend(region.iter().map(|(origin, size)| {
                    (origin.x, origin.y, size.width as i32, size.height as i32)
                }));
            });
        if !drawn {
            return false;
        }
        *fresh = false;
        // Slint counts every render as a frame shown in the other buffer, so every render is committed.
        if let Err(err) = buffer.attach_to(surface) {
            tracing::warn!("cannot attach a shell buffer: {err}");
            return false;
        }
        for (x, y, width, height) in damage {
            surface.damage_buffer(x, y, width, height);
        }
        surface.frame(qh, surface.clone());
        *next = (*next + 1) % buffers.len();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_blend_premultiplied_in_argb_order() {
        let mut pixel = Argb8888::background();
        assert_eq!(bytemuck::bytes_of(&pixel), [0, 0, 0, 0]);
        pixel.blend(PremultipliedRgbaColor { red: 128, green: 0, blue: 0, alpha: 128 });
        assert_eq!(bytemuck::bytes_of(&pixel), [0, 0, 128, 128]);
        assert_eq!(u32::from_le_bytes(bytemuck::cast(Argb8888::from_rgb(1, 2, 3))), 0xff01_0203);
    }
}
