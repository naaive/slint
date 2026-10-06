// SPDX-License-Identifier: MIT

use super::{Job, Rendered, Resolved, Subject, Target};
use crate::render::{self, CLEAR_COLOR, Capture, OutputRenderElement, SceneOptions};
use crate::state::Nimbus;
use anyhow::{Context, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{AsRenderElements, Kind, RenderElement};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{
    Bind, Color32F, ExportMem, ImportAll, ImportMem, Offscreen, Renderer, Texture,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Size, Transform};
use smithay::wayland::shm::with_buffer_contents_mut;

type CaptureElement<R> = RelocateRenderElement<OutputRenderElement<R>>;

/// Renders `job` with `renderer`, or returns `None` when it waits for damage and nothing changed.
pub fn render<R, T>(
    renderer: &mut R,
    nimbus: &Nimbus,
    job: Job<'_>,
) -> anyhow::Result<Option<Rendered>>
where
    R: Renderer + ImportAll + ImportMem + Offscreen<T> + ExportMem + Bind<Dmabuf>,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let resolved = job.resolved;
    let elements = scene(renderer, nimbus, &job);
    let full = vec![Rectangle::from_size(resolved.buffer_size())];
    let damage = match job.damage {
        None => full,
        Some(damage) => {
            let (tracker, fresh) = damage.tracker(resolved);
            let (rects, _) = tracker.damage_output(1, &elements).map_err(|e| anyhow!("{e:?}"))?;
            match rects {
                _ if fresh => full,
                None => return Ok(None),
                Some(rects) => buffer_damage(rects, resolved),
            }
        }
    };
    let buffer_size = resolved.transform.transform_size(resolved.area.size);
    let image = match job.target {
        Target::Memory => Some(render_to_memory(renderer, buffer_size, resolved, &elements)?),
        Target::Shm(buffer) => {
            let image = render_to_memory(renderer, buffer_size, resolved, &elements)?;
            write_shm(buffer, &image)?;
            None
        }
        Target::Dmabuf(mut dmabuf) => {
            let mut framebuffer = renderer.bind(&mut dmabuf).map_err(|e| anyhow!("{e}"))?;
            let mut tracker =
                OutputDamageTracker::new(buffer_size, resolved.scale, resolved.transform);
            let result = tracker
                .render_output(renderer, &mut framebuffer, 0, &elements, CLEAR_COLOR)
                .map_err(|e| anyhow!("{e:?}"))?;
            result.sync.wait().map_err(|_| anyhow!("waiting for the GPU was interrupted"))?;
            None
        }
    };
    Ok(Some(Rendered { damage, image }))
}

/// The elements of `job`, moved so its area starts at the origin.
fn scene<R>(renderer: &mut R, nimbus: &Nimbus, job: &Job<'_>) -> Vec<CaptureElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let resolved = job.resolved;
    let elements = if nimbus.is_locked() && !job.reveal_lock {
        vec![OutputRenderElement::Solid(SolidColorRenderElement::new(
            nimbus.capture.locked.clone(),
            resolved.area,
            CommitCounter::default(),
            Color32F::BLACK,
            Kind::Unspecified,
        ))]
    } else {
        match &resolved.subject {
            Subject::Output(output) => render::output_elements(
                renderer,
                nimbus,
                output,
                SceneOptions { cursor: job.cursor },
            ),
            Subject::Window(window) => window.render_elements(
                renderer,
                window.geometry().loc.upscale(-1).to_physical_precise_round(resolved.scale),
                Scale::from(resolved.scale),
                1.0,
            ),
        }
    };
    elements
        .into_iter()
        .map(|e| {
            RelocateRenderElement::from_element(
                e,
                resolved.area.loc.upscale(-1),
                Relocate::Relative,
            )
        })
        .collect()
}

/// Turns damage of the upright area into buffer coordinates.
fn buffer_damage(
    rects: &[Rectangle<i32, Physical>],
    resolved: &Resolved,
) -> Vec<Rectangle<i32, Buffer>> {
    let area = resolved.area.size.to_logical(1);
    rects.iter().map(|r| r.to_logical(1).to_buffer(1, resolved.transform, &area)).collect()
}

/// Renders `elements` into an offscreen buffer of `size` and reads it back.
fn render_to_memory<R, T, E>(
    renderer: &mut R,
    size: Size<i32, Physical>,
    resolved: &Resolved,
    elements: &[E],
) -> anyhow::Result<Capture>
where
    R: Renderer + Offscreen<T> + ExportMem,
    R::TextureId: Texture,
    E: RenderElement<R>,
{
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    let mut target =
        renderer.create_buffer(Fourcc::Abgr8888, buffer_size).map_err(|e| anyhow!("{e}"))?;
    let mut framebuffer = renderer.bind(&mut target).map_err(|e| anyhow!("{e}"))?;
    let mut tracker = OutputDamageTracker::new(size, resolved.scale, resolved.transform);
    tracker
        .render_output(renderer, &mut framebuffer, 0, elements, CLEAR_COLOR)
        .map_err(|e| anyhow!("{e:?}"))?;
    let mapping = renderer
        .copy_framebuffer(&framebuffer, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
        .map_err(|e| anyhow!("{e}"))?;
    let mut pixels = renderer.map_texture(&mapping).map_err(|e| anyhow!("{e}"))?.to_vec();
    // The desktop is opaque; dropping alpha keeps viewers from showing a checkerboard through premultiplied edges.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let width = u32::try_from(size.w).context("negative width")?;
    let height = u32::try_from(size.h).context("negative height")?;
    Ok(Capture { width, height, pixels })
}

/// Copies `image` into an shm buffer of its size in one of [`super::SHM_FORMATS`].
fn write_shm(buffer: &WlBuffer, image: &Capture) -> anyhow::Result<()> {
    let row = image.width as usize * 4;
    with_buffer_contents_mut(buffer, |ptr, len, data| {
        let stride = usize::try_from(data.stride).ok()?;
        let offset = usize::try_from(data.offset).ok()?;
        let height = usize::try_from(data.height).ok()?;
        let swap = match data.format {
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => true,
            wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => false,
            _ => return None,
        };
        if stride < row || height != image.height as usize || offset + stride * height > len {
            return None;
        }
        let mut line = vec![0u8; row];
        for (y, source) in image.pixels.chunks_exact(row).enumerate() {
            line.copy_from_slice(source);
            if swap {
                for pixel in line.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }
            // SAFETY: the row lies inside the pool, as checked above, and no reference into it is made.
            unsafe {
                std::ptr::copy_nonoverlapping(line.as_ptr(), ptr.add(offset + y * stride), row)
            };
        }
        Some(())
    })
    .map_err(|e| anyhow!("cannot access the buffer: {e}"))?
    .context("the buffer doesn't match the capture")
}
