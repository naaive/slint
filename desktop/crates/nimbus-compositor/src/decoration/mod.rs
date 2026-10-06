// SPDX-License-Identifier: MIT

//! Server-side window decorations: titlebars the compositor draws for windows that don't draw their own.

mod draw;
mod font;
pub mod frame;
mod theme;

pub use draw::Look;
pub use frame::{Button, Frame, Hit, Metrics, Style};

use ab_glyph::FontVec;
use nimbus_config::Appearance;
use nimbus_ipc::WindowId;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::{ImportMem, Renderer, Texture};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::utils::{Physical, Point, Rectangle, Transform};
use std::cell::RefCell;
use std::collections::HashMap;
use theme::Theme;

/// Whether the compositor decorates a window.
///
/// `negotiated` is the xdg-decoration mode the compositor sent, `None` without a decoration object.
/// Without one, a client that sets its window geometry is taken to draw its own decorations,
/// as GTK does without supporting xdg-decoration.
pub fn server_side(negotiated: Option<Mode>, sets_window_geometry: bool) -> bool {
    match negotiated {
        Some(mode) => mode == Mode::ServerSide,
        None => !sets_window_geometry,
    }
}

/// The decoration theme and the titlebars drawn with it.
pub struct Decorations {
    theme: Theme,
    font: Option<FontVec>,
    /// One titlebar per window and output scale, redrawn only when its [`Look`] changes.
    cache: RefCell<HashMap<(WindowId, u64), (Look, MemoryRenderBuffer)>>,
}

impl Decorations {
    pub fn new(appearance: &Appearance) -> Self {
        let theme = Theme::from_config(appearance);
        let font = font::load(&theme.font_family);
        Self { theme, font, cache: RefCell::default() }
    }

    pub fn metrics(&self) -> Metrics {
        self.theme.metrics()
    }

    /// Follows a new `[appearance]`, and redraws every titlebar if the decorations' part of it changed.
    pub fn set_appearance(&mut self, appearance: &Appearance) {
        let theme = Theme::from_config(appearance);
        if theme == self.theme {
            return;
        }
        if theme.font_family != self.theme.font_family {
            self.font = font::load(&theme.font_family);
        }
        self.theme = theme;
        self.cache.borrow_mut().clear();
    }

    /// Drops the titlebars of a window that's gone.
    pub fn forget(&self, id: WindowId) {
        self.cache.borrow_mut().retain(|(window, _), _| *window != id);
    }

    /// The titlebar of window `id` looking like `look`, drawn at `location` on an output of `scale`.
    pub fn element<R>(
        &self,
        renderer: &mut R,
        id: WindowId,
        look: Look,
        location: Point<f64, Physical>,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let size = look.size;
        let key = (id, scale.to_bits());
        let mut cache = self.cache.borrow_mut();
        let buffer = match cache.get(&key).filter(|(cached, _)| *cached == look) {
            Some((_, buffer)) => buffer.clone(),
            None => {
                let pixmap = draw::titlebar(&look, scale, &self.theme, self.font.as_ref())?;
                let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
                let buffer = MemoryRenderBuffer::from_slice(
                    pixmap.data(),
                    Fourcc::Abgr8888,
                    (width, height),
                    1,
                    Transform::Normal,
                    Some(vec![Rectangle::from_size((width, height).into())]),
                );
                cache.insert(key, (look, buffer.clone()));
                buffer
            }
        };
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location,
            &buffer,
            None,
            None,
            Some(size),
            Kind::Unspecified,
        )
        .map_err(|err| tracing::debug!("cannot upload a titlebar: {err:?}"))
        .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoration_modes_follow_the_negotiation() {
        assert!(server_side(Some(Mode::ServerSide), true));
        assert!(!server_side(Some(Mode::ClientSide), false));
        assert!(server_side(None, false), "clients that don't negotiate get a titlebar");
        assert!(!server_side(None, true), "unless they draw their own");
    }

    #[test]
    fn titlebars_draw_title_buttons_and_focus() {
        let decorations = Decorations::new(&Appearance::default());
        let look = Look {
            style: Style::Full,
            size: (300, 34).into(),
            title: "Hello".into(),
            focused: true,
            maximized: false,
            hovered: None,
            rounded: false,
        };
        let pixmap = draw::titlebar(&look, 2.0, &decorations.theme, decorations.font.as_ref())
            .expect("a titlebar");
        assert_eq!((pixmap.width(), pixmap.height()), (600, 68));
        let pixel = |x: u32, y: u32| pixmap.pixel(x, y).unwrap();
        let background = pixel(4, 4);
        assert_eq!(background.alpha(), 255, "focused titlebars are opaque");
        // The close button's symbol crosses at its center, 17 logical pixels from the right.
        assert_ne!(pixel(600 - 34, 34), background);

        let unfocused = Look { focused: false, ..look.clone() };
        let other = draw::titlebar(&unfocused, 2.0, &decorations.theme, None).unwrap();
        assert_ne!(other.pixel(4, 4).unwrap(), background);

        if decorations.font.is_some() {
            let blank = Look { title: String::new(), ..look.clone() };
            let blank =
                draw::titlebar(&blank, 2.0, &decorations.theme, decorations.font.as_ref()).unwrap();
            assert_ne!(blank.data(), pixmap.data(), "the title shows");
        } else {
            eprintln!("skipped the title check: no font is installed");
        }

        let rounded = Look { rounded: true, ..look };
        let rounded = draw::titlebar(&rounded, 1.0, &decorations.theme, None).unwrap();
        assert_eq!(rounded.pixel(0, 0).unwrap().alpha(), 0, "rounded corners are transparent");
    }

    #[test]
    fn empty_titlebars_draw_nothing() {
        let decorations = Decorations::new(&Appearance::default());
        let look = Look {
            style: Style::Slim,
            size: (0, 23).into(),
            title: "Nothing".into(),
            focused: false,
            maximized: false,
            hovered: None,
            rounded: false,
        };
        assert!(draw::titlebar(&look, 1.0, &decorations.theme, None).is_none());
    }
}
