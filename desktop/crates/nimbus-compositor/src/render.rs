// SPDX-License-Identifier: MIT

//! Scene assembly shared by all backends and captures, frame callbacks, presentation feedback, and screenshot files.

use crate::decoration::Look;
use crate::state::Nimbus;
use crate::wm::WindowMode;
use anyhow::{Context, anyhow};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::{
    AsRenderElements, Id, Kind, RenderElementStates, default_primary_scanout_output_compare,
};
use smithay::backend::renderer::{Color32F, ImportAll, ImportMem, Renderer, Texture};
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::{
    OutputPresentationFeedback, send_frames_surface_tree,
    surface_presentation_feedback_flags_from_states, surface_primary_scanout_output,
    update_surface_primary_scanout_output,
};
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::fractional_scale::with_fractional_scale;
use smithay::wayland::shell::wlr_layer::Layer;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CLEAR_COLOR: Color32F = Color32F::new(0.11, 0.12, 0.14, 1.0);
const BLACK: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Memory = MemoryRenderBufferRenderElement<R>,
    Solid = SolidColorRenderElement,
}

/// The id of an output's black backdrop, kept so that damage tracking sees it unchanged from frame to frame.
struct Backdrop(Id);

/// Which optional parts of the scene to draw.
#[derive(Clone, Copy, Debug)]
pub struct SceneOptions {
    pub cursor: bool,
}

/// Builds the elements of one output, front to back, or only black while it's off:
/// cursor, lock screens, overlay layers, top layers (or a fullscreen window above them),
/// windows, bottom and background layers, and the wallpaper.
pub fn output_elements<R>(
    renderer: &mut R,
    nimbus: &Nimbus,
    output: &Output,
    options: SceneOptions,
) -> Vec<OutputRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let Some(output_geo) = nimbus.output_geometry(output) else {
        return Vec::new();
    };
    let output_scale = output.current_scale().fractional_scale();
    let scale = Scale::from(output_scale);
    let name = output.name();
    let mut elements: Vec<OutputRenderElement<R>> = Vec::new();

    let full_output = || {
        output.user_data().insert_if_missing_threadsafe(|| Backdrop(Id::new()));
        let Backdrop(id) = output.user_data().get::<Backdrop>().expect("inserted above");
        SolidColorRenderElement::new(
            id.clone(),
            Rectangle::from_size(output_geo.size.to_physical_precise_round(output_scale)),
            smithay::backend::renderer::utils::CommitCounter::default(),
            BLACK,
            Kind::Unspecified,
        )
    };

    if !nimbus.output_powered(output) {
        elements.push(OutputRenderElement::Solid(full_output()));
        return elements;
    }

    if options.cursor && output_geo.to_f64().contains(nimbus.pointer_location) {
        push_cursor(renderer, nimbus, output_geo, output_scale, &mut elements);
    }

    if nimbus.is_locked() {
        if let Some(lock) = nimbus.lock.client().and_then(|client| client.surface(&name)) {
            elements.extend(render_elements_from_surface_tree(
                renderer,
                lock.wl_surface(),
                Point::<i32, Physical>::from((0, 0)),
                scale,
                1.0,
                Kind::Unspecified,
            ));
        }
        elements.push(OutputRenderElement::Solid(full_output()));
        return elements;
    }

    push_layers(renderer, output, output_scale, &[Layer::Overlay], &mut elements);

    let fullscreen = nimbus.wm.fullscreen_on(&name).map(|w| w.window.clone());

    let window_elements =
        |renderer: &mut R, window: &smithay::desktop::Window| -> Vec<OutputRenderElement<R>> {
            let Some(location) = nimbus.wm.space.element_location(window) else {
                return Vec::new();
            };
            let render_location = (location - window.geometry().loc - output_geo.loc)
                .to_physical_precise_round(output_scale);
            window.render_elements::<OutputRenderElement<R>>(renderer, render_location, scale, 1.0)
        };

    if let Some(fullscreen) = &fullscreen {
        elements.extend(window_elements(renderer, fullscreen));
    }
    push_layers(renderer, output, output_scale, &[Layer::Top], &mut elements);
    for window in nimbus.wm.space.elements().rev() {
        if Some(window) == fullscreen.as_ref() {
            continue;
        }
        let overlaps =
            nimbus.wm.space.element_bbox(window).is_some_and(|bbox| bbox.overlaps(output_geo));
        if overlaps {
            elements.extend(window_elements(renderer, window));
        }
        elements.extend(
            titlebar_element(renderer, nimbus, window, output_geo, output_scale)
                .map(OutputRenderElement::Memory),
        );
    }
    push_layers(renderer, output, output_scale, &[Layer::Bottom, Layer::Background], &mut elements);

    if let Some(wallpaper) =
        nimbus.wallpaper.element(renderer, &name, output_geo.size, output_scale)
    {
        elements.push(OutputRenderElement::Memory(wallpaper));
    }
    elements
}

/// The titlebar the compositor draws above `window`, if it has one on the output covering `output_geo`.
fn titlebar_element<R>(
    renderer: &mut R,
    nimbus: &Nimbus,
    window: &smithay::desktop::Window,
    output_geo: Rectangle<i32, Logical>,
    scale: f64,
) -> Option<MemoryRenderBufferRenderElement<R>>
where
    R: Renderer + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let id = nimbus.wm.find_window(window)?;
    let frame = nimbus.wm.frame(id)?;
    let w = nimbus.wm.get(id)?;
    let titlebar = frame.titlebar();
    if !titlebar.overlaps(output_geo) {
        return None;
    }
    let look = Look {
        style: frame.style,
        size: titlebar.size,
        title: w.title.clone(),
        focused: nimbus.wm.focused() == Some(id),
        maximized: w.mode == WindowMode::Maximized,
        hovered: nimbus.decoration_input.hovered_button(id),
        rounded: frame.resizable,
    };
    let location = (titlebar.loc - output_geo.loc).to_f64().to_physical(scale);
    nimbus.decorations.element(renderer, id, look, location, scale)
}

fn push_layers<R>(
    renderer: &mut R,
    output: &Output,
    scale: f64,
    layers: &[Layer],
    elements: &mut Vec<OutputRenderElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let map = layer_map_for_output(output);
    for &layer in layers {
        for surface in map.layers_on(layer).rev() {
            let Some(geo) = map.layer_geometry(surface) else {
                continue;
            };
            elements.extend(surface.render_elements::<OutputRenderElement<R>>(
                renderer,
                geo.loc.to_physical_precise_round(scale),
                Scale::from(scale),
                1.0,
            ));
        }
    }
}

fn push_cursor<R>(
    renderer: &mut R,
    nimbus: &Nimbus,
    output_geo: Rectangle<i32, Logical>,
    scale: f64,
    elements: &mut Vec<OutputRenderElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let pointer = nimbus.pointer_location - output_geo.loc.to_f64();
    if let Some(icon) = &nimbus.dnd_icon {
        elements.extend(render_elements_from_surface_tree(
            renderer,
            icon,
            pointer.to_physical(scale).to_i32_round::<i32>(),
            Scale::from(scale),
            1.0,
            Kind::Unspecified,
        ));
    }
    match &nimbus.cursor_status {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .and_then(|data| data.lock().ok().map(|attrs| attrs.hotspot))
                    .unwrap_or_default()
            });
            let location = (pointer - hotspot.to_f64()).to_physical(scale).to_i32_round::<i32>();
            elements.extend(render_elements_from_surface_tree(
                renderer,
                surface,
                location,
                Scale::from(scale),
                1.0,
                Kind::Cursor,
            ));
        }
        CursorImageStatus::Named(icon) => {
            let frame = nimbus.cursor_theme.frame(*icon, scale, nimbus.start_time.elapsed());
            let hotspot =
                frame.hotspot.to_f64().to_logical(f64::from(frame.scale)).to_physical(scale);
            let location = pointer.to_physical(scale) - hotspot;
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location,
                &frame.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => elements.push(OutputRenderElement::Memory(element)),
                Err(err) => tracing::debug!("cannot upload the cursor: {err:?}"),
            }
        }
    }
}

/// Sends frame callbacks and updates the preferred fractional scale after an output was rendered.
pub fn post_repaint(
    output: &Output,
    states: &RenderElementStates,
    nimbus: &Nimbus,
    time: Duration,
) {
    let throttle = Some(Duration::from_secs(1));
    for window in nimbus.wm.space.elements() {
        window.with_surfaces(|surface, data| {
            let primary = update_surface_primary_scanout_output(
                surface,
                output,
                data,
                states,
                default_primary_scanout_output_compare,
            );
            if let Some(primary) = primary {
                with_fractional_scale(data, |fractional| {
                    fractional.set_preferred_scale(primary.current_scale().fractional_scale());
                });
            }
        });
        if nimbus.wm.space.outputs_for_element(window).contains(output) {
            window.send_frame(output, time, throttle, surface_primary_scanout_output);
        }
    }
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.with_surfaces(|surface, data| {
            let primary = update_surface_primary_scanout_output(
                surface,
                output,
                data,
                states,
                default_primary_scanout_output_compare,
            );
            if let Some(primary) = primary {
                with_fractional_scale(data, |fractional| {
                    fractional.set_preferred_scale(primary.current_scale().fractional_scale());
                });
            }
        });
        layer.send_frame(output, time, throttle, surface_primary_scanout_output);
    }
    drop(map);
    let this_output = |_: &_, _: &_| Some(output.clone());
    if let Some(lock) = nimbus.lock.client().and_then(|c| c.surface(&output.name())) {
        send_frames_surface_tree(
            lock.wl_surface(),
            output,
            time,
            Some(Duration::ZERO),
            this_output,
        );
    }
    if let CursorImageStatus::Surface(surface) = &nimbus.cursor_status {
        send_frames_surface_tree(surface, output, time, Some(Duration::ZERO), this_output);
    }
    if let Some(icon) = &nimbus.dnd_icon {
        send_frames_surface_tree(icon, output, time, Some(Duration::ZERO), this_output);
    }
}

/// Collects presentation feedback of the surfaces shown on `output`.
pub fn take_presentation_feedback(
    output: &Output,
    nimbus: &Nimbus,
    states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    for window in nimbus.wm.space.elements() {
        if nimbus.wm.space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut feedback,
                surface_primary_scanout_output,
                |surface, _| surface_presentation_feedback_flags_from_states(surface, states),
            );
        }
    }
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.take_presentation_feedback(
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| surface_presentation_feedback_flags_from_states(surface, states),
        );
    }
    feedback
}

/// An RGBA image of an output, as drawn.
pub struct Capture {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA rows, top to bottom.
    pub pixels: Vec<u8>,
}

impl Capture {
    pub fn save_png(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let image = image::RgbaImage::from_raw(self.width, self.height, self.pixels.clone())
            .ok_or_else(|| {
                anyhow!("the captured buffer is smaller than {}x{}", self.width, self.height)
            })?;
        // Without O_NONBLOCK, opening a FIFO blocks until a reader shows up.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if !file.metadata()?.file_type().is_file() {
            anyhow::bail!("{} isn't a regular file", path.display());
        }
        let mut writer = std::io::BufWriter::new(file);
        image
            .write_to(&mut writer, image::ImageFormat::Png)
            .and_then(|()| writer.flush().map_err(image::ImageError::IoError))
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

/// The configured wallpaper, scaled to fill each output and cached per output size.
pub struct Wallpaper {
    path: Option<PathBuf>,
    source: RefCell<Option<Option<image::RgbaImage>>>,
    scaled: RefCell<HashMap<String, (Size<i32, Physical>, MemoryRenderBuffer)>>,
}

impl Wallpaper {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path, source: RefCell::new(None), scaled: RefCell::default() }
    }

    fn element<R>(
        &self,
        renderer: &mut R,
        output: &str,
        logical: Size<i32, Logical>,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let physical = logical.to_physical_precise_round(scale);
        if physical.w <= 0 || physical.h <= 0 {
            return None;
        }
        let mut scaled = self.scaled.borrow_mut();
        let cached =
            scaled.get(output).filter(|(size, _)| *size == physical).map(|(_, b)| b.clone());
        let buffer = match cached {
            Some(buffer) => buffer,
            None => {
                let mut source = self.source.borrow_mut();
                let image = source.get_or_insert_with(|| {
                    let path = self.path.as_ref()?;
                    image::open(path)
                        .map_err(|err| {
                            tracing::warn!("cannot load wallpaper {}: {err}", path.display());
                        })
                        .ok()
                        .map(|image| image.into_rgba8())
                });
                let (width, height) = (physical.w.unsigned_abs(), physical.h.unsigned_abs());
                let filled = match image.as_ref() {
                    Some(image) => fill(image, width, height),
                    None => default_backdrop(width, height),
                };
                let buffer = MemoryRenderBuffer::from_slice(
                    filled.as_raw(),
                    Fourcc::Abgr8888,
                    (physical.w, physical.h),
                    1,
                    Transform::Normal,
                    Some(vec![Rectangle::from_size((physical.w, physical.h).into())]),
                );
                scaled.insert(output.to_owned(), (physical, buffer.clone()));
                buffer
            }
        };
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((0.0, 0.0)),
            &buffer,
            None,
            None,
            Some(logical),
            Kind::Unspecified,
        )
        .map_err(|err| tracing::debug!("cannot upload the wallpaper: {err:?}"))
        .ok()
    }
}

/// The backdrop without a configured wallpaper: a dark blue-violet gradient with two soft glows.
fn default_backdrop(width: u32, height: u32) -> image::RgbaImage {
    const TOP_LEFT: [f32; 3] = [16.0, 22.0, 44.0];
    const BOTTOM_RIGHT: [f32; 3] = [38.0, 26.0, 62.0];
    // Glows as (center x, center y, radius as a fraction of the diagonal, color, strength).
    const GLOWS: [(f32, f32, f32, [f32; 3], f32); 2] = [
        (0.22, 0.18, 0.55, [53.0, 132.0, 228.0], 0.35),
        (0.82, 0.88, 0.5, [155.0, 107.0, 242.0], 0.28),
    ];
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let diagonal = (w * w + h * h).sqrt();
    image::RgbaImage::from_fn(width, height, |x, y| {
        let (fx, fy) = (x as f32 / w, y as f32 / h);
        let t = ((fx + fy) / 2.0).clamp(0.0, 1.0);
        let mut color = [0.0f32; 3];
        for (c, channel) in color.iter_mut().enumerate() {
            *channel = TOP_LEFT[c] + (BOTTOM_RIGHT[c] - TOP_LEFT[c]) * t;
        }
        for (gx, gy, radius, glow, strength) in GLOWS {
            let (dx, dy) = ((fx - gx) * w, (fy - gy) * h);
            let d = (dx * dx + dy * dy).sqrt() / (radius * diagonal);
            let weight = strength * (1.0 - d).clamp(0.0, 1.0).powi(2);
            for (c, channel) in color.iter_mut().enumerate() {
                *channel += (glow[c] - *channel) * weight;
            }
        }
        // Ordered dithering hides banding in the smooth gradient.
        let dither = (((x * 7 + y * 13) % 4) as f32 - 1.5) * 0.5;
        image::Rgba([
            (color[0] + dither).round().clamp(0.0, 255.0) as u8,
            (color[1] + dither).round().clamp(0.0, 255.0) as u8,
            (color[2] + dither).round().clamp(0.0, 255.0) as u8,
            255,
        ])
    })
}

/// Scales `image` to cover `width`×`height`, cropping the overflow evenly, and makes it opaque.
fn fill(image: &image::RgbaImage, width: u32, height: u32) -> image::RgbaImage {
    let (iw, ih) = image.dimensions();
    if iw == 0 || ih == 0 {
        return image::RgbaImage::from_pixel(width, height, image::Rgba([28, 30, 36, 255]));
    }
    let scale = (f64::from(width) / f64::from(iw)).max(f64::from(height) / f64::from(ih));
    let crop_w = ((f64::from(width) / scale).round() as u32).clamp(1, iw);
    let crop_h = ((f64::from(height) / scale).round() as u32).clamp(1, ih);
    let x = (iw - crop_w) / 2;
    let y = (ih - crop_h) / 2;
    let cropped = image::imageops::crop_imm(image, x, y, crop_w, crop_h).to_image();
    let mut filled =
        image::imageops::resize(&cropped, width, height, image::imageops::FilterType::Triangle);
    for pixel in filled.pixels_mut() {
        pixel.0[3] = 255;
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_covers_and_crops_centered() {
        let mut image = image::RgbaImage::from_pixel(400, 100, image::Rgba([255, 0, 0, 255]));
        for y in 0..100 {
            for x in 150..250 {
                image.put_pixel(x, y, image::Rgba([0, 0, 255, 128]));
            }
        }
        let filled = fill(&image, 100, 100);
        assert_eq!(filled.dimensions(), (100, 100));
        // The 100x100 crop is the blue center band.
        assert_eq!(filled.get_pixel(50, 50).0, [0, 0, 255, 255]);
    }

    #[test]
    fn default_backdrop_is_opaque_and_varied() {
        let backdrop = default_backdrop(64, 36);
        assert_eq!(backdrop.dimensions(), (64, 36));
        assert!(backdrop.pixels().all(|p| p.0[3] == 255));
        assert_ne!(backdrop.get_pixel(0, 0), backdrop.get_pixel(63, 35));
        assert_eq!(default_backdrop(0, 0).dimensions(), (0, 0));
    }

    #[test]
    fn fill_handles_empty_images() {
        let filled = fill(&image::RgbaImage::new(0, 0), 4, 3);
        assert_eq!(filled.dimensions(), (4, 3));
    }

    #[test]
    fn capture_writes_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shots/out.png");
        let capture = Capture { width: 2, height: 1, pixels: vec![255, 0, 0, 255, 0, 255, 0, 255] };
        capture.save_png(&path).unwrap();
        let image = image::open(&path).unwrap().into_rgba8();
        assert_eq!(image.get_pixel(1, 0).0, [0, 255, 0, 255]);
        let short = Capture { width: 4, height: 4, pixels: vec![0; 4] };
        assert!(short.save_png(&path).is_err());
    }
}
