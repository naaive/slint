// SPDX-License-Identifier: MIT

//! The geometry of server-side decorations: titlebar height, button places, and hit-testing.

use crate::wm::layout::Rect;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
use smithay::utils::{Logical, Point, Size};

/// How much of a titlebar a window gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    /// The title and the minimize, maximize, and close buttons, for floating and maximized windows.
    Full,
    /// A thinner bar with the title and the close button, for tiled windows.
    Slim,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

/// The part of a frame under a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Titlebar,
    Button(Button),
    /// A resize border, outside the titlebar and the content.
    Edge(ResizeEdge),
}

/// Sizes of the decoration parts, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub full_height: i32,
    pub slim_height: i32,
    /// The width of the resize borders around floating windows.
    pub border: i32,
}

impl Metrics {
    /// Metrics that fit text of `font_size` logical pixels.
    pub fn for_font_size(font_size: f32) -> Self {
        let font_size = font_size.clamp(6.0, 64.0);
        Self {
            full_height: (font_size * 1.5).round() as i32 + 12,
            slim_height: font_size.round() as i32 + 8,
            border: 8,
        }
    }

    pub fn height(&self, style: Style) -> i32 {
        match style {
            Style::Full => self.full_height,
            Style::Slim => self.slim_height,
        }
    }
}

/// The buttons of a titlebar of `size`, right to left, as rectangles relative to the titlebar's origin.
///
/// Buttons are squares inset from the bar's edges; the close button's center is half the bar's height from the right edge.
/// Buttons that don't fit are left out.
pub fn buttons(style: Style, size: Size<i32, Logical>) -> Vec<(Button, Rect)> {
    let all: &[Button] = match style {
        Style::Full => &[Button::Close, Button::Maximize, Button::Minimize],
        Style::Slim => &[Button::Close],
    };
    let inset = button_inset(size.h);
    let side = size.h - 2 * inset;
    if side <= 0 {
        return Vec::new();
    }
    let mut x = size.w - inset - side;
    let mut placed = Vec::new();
    for &button in all {
        if x < inset {
            break;
        }
        placed.push((button, Rect::new((x, inset).into(), (side, side).into())));
        x -= side + inset;
    }
    placed
}

/// The space between a button and the titlebar's edges.
pub fn button_inset(height: i32) -> i32 {
    (height as f32 * 0.18).round() as i32
}

/// The part of a titlebar of `size` that holds the title, relative to its origin.
pub fn title_area(style: Style, size: Size<i32, Logical>) -> Rect {
    let inset = button_inset(size.h);
    let left = 2 * inset;
    let right = buttons(style, size).last().map_or(size.w - 2 * inset, |(_, r)| r.loc.x - inset);
    Rect::new((left, 0).into(), ((right - left).max(0), size.h).into())
}

/// A decorated window's frame: the titlebar on top of `content`, and the resize borders around both when `resizable`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub content: Rect,
    pub style: Style,
    pub resizable: bool,
    pub metrics: Metrics,
}

impl Frame {
    pub fn titlebar(&self) -> Rect {
        let height = self.metrics.height(self.style);
        Rect::new(
            (self.content.loc.x, self.content.loc.y - height).into(),
            (self.content.size.w, height).into(),
        )
    }

    /// The titlebar and the content.
    pub fn outer(&self) -> Rect {
        self.titlebar().merge(self.content)
    }

    /// What's under the global point `p`; `None` on the content and outside the frame.
    pub fn hit(&self, p: Point<f64, Logical>) -> Option<Hit> {
        let titlebar = self.titlebar();
        if titlebar.to_f64().contains(p) {
            let local = p - titlebar.loc.to_f64();
            let button = buttons(self.style, titlebar.size)
                .into_iter()
                .find(|(_, rect)| rect.to_f64().contains(local))
                .map(|(button, _)| button);
            return Some(button.map_or(Hit::Titlebar, Hit::Button));
        }
        if !self.resizable {
            return None;
        }
        let outer = self.outer().to_f64();
        let border = f64::from(self.metrics.border);
        let (left, top) = (outer.loc.x, outer.loc.y);
        let (right, bottom) = (left + outer.size.w, top + outer.size.h);
        if p.x < left - border
            || p.x >= right + border
            || p.y < top - border
            || p.y >= bottom + border
        {
            return None;
        }
        // Corners reach a border's width along the edges, so they're easy to grab.
        let corner = 2.0 * border;
        let near_left = p.x < left + corner - border;
        let near_right = p.x >= right - corner + border;
        let near_top = p.y < top + corner - border;
        let near_bottom = p.y >= bottom - corner + border;
        let on_left = p.x < left;
        let on_right = p.x >= right;
        let on_top = p.y < top;
        let on_bottom = p.y >= bottom;
        let mut edges = 0;
        if on_left || ((on_top || on_bottom) && near_left) {
            edges |= u32::from(ResizeEdge::Left);
        }
        if on_right || ((on_top || on_bottom) && near_right) {
            edges |= u32::from(ResizeEdge::Right);
        }
        if on_top || ((on_left || on_right) && near_top) {
            edges |= u32::from(ResizeEdge::Top);
        }
        if on_bottom || ((on_left || on_right) && near_bottom) {
            edges |= u32::from(ResizeEdge::Bottom);
        }
        ResizeEdge::try_from(edges).ok().filter(|_| edges != 0).map(Hit::Edge)
    }
}

/// The part of `area` below a titlebar of `height`, where a decorated window's content goes.
pub fn below_titlebar(area: Rect, height: i32) -> Rect {
    let height = height.clamp(0, area.size.h);
    Rect::new((area.loc.x, area.loc.y + height).into(), (area.size.w, area.size.h - height).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const METRICS: Metrics = Metrics { full_height: 34, slim_height: 23, border: 8 };

    fn frame(style: Style, resizable: bool) -> Frame {
        Frame {
            content: Rect::new((100, 134).into(), (400, 300).into()),
            style,
            resizable,
            metrics: METRICS,
        }
    }

    fn hit(frame: &Frame, x: f64, y: f64) -> Option<Hit> {
        frame.hit(Point::from((x, y)))
    }

    #[test]
    fn metrics_follow_the_font_size() {
        let metrics = Metrics::for_font_size(11.0 * 96.0 / 72.0);
        assert_eq!(metrics.full_height, 34);
        assert_eq!(metrics.slim_height, 23);
        assert!(Metrics::for_font_size(24.0).full_height > metrics.full_height);
    }

    #[test]
    fn the_titlebar_sits_on_top_of_the_content() {
        let frame = frame(Style::Full, true);
        assert_eq!(frame.titlebar(), Rect::new((100, 100).into(), (400, 34).into()));
        assert_eq!(frame.outer(), Rect::new((100, 100).into(), (400, 334).into()));
        let slim = super::Frame { style: Style::Slim, ..frame };
        assert_eq!(slim.titlebar().loc.y, 134 - 23);
    }

    #[test]
    fn buttons_line_up_from_the_right() {
        let buttons = buttons(Style::Full, (400, 34).into());
        let order: Vec<Button> = buttons.iter().map(|(b, _)| *b).collect();
        assert_eq!(order, [Button::Close, Button::Maximize, Button::Minimize]);
        // A 6 pixel inset leaves 22 pixel buttons, with the close button centered 17 pixels from the right.
        assert_eq!(buttons[0].1, Rect::new((372, 6).into(), (22, 22).into()));
        assert_eq!(buttons[1].1, Rect::new((344, 6).into(), (22, 22).into()));
        assert_eq!(buttons[2].1, Rect::new((316, 6).into(), (22, 22).into()));
        let slim = super::buttons(Style::Slim, (400, 23).into());
        assert_eq!(slim.len(), 1);
        assert_eq!(slim[0].0, Button::Close);
    }

    #[test]
    fn narrow_titlebars_drop_buttons() {
        assert_eq!(buttons(Style::Full, (70, 34).into()).len(), 2);
        assert_eq!(buttons(Style::Full, (60, 34).into()).len(), 1);
        assert!(buttons(Style::Full, (20, 34).into()).is_empty());
        assert!(buttons(Style::Full, (400, 0).into()).is_empty());
    }

    #[test]
    fn the_title_stays_clear_of_the_buttons() {
        let area = title_area(Style::Full, (400, 34).into());
        assert_eq!(area, Rect::new((12, 0).into(), (298, 34).into()));
        let slim = title_area(Style::Slim, (400, 23).into());
        assert!(slim.size.w > area.size.w);
        assert_eq!(title_area(Style::Full, (10, 34).into()).size.w, 0);
    }

    #[test]
    fn hits_find_buttons_titlebar_and_content() {
        let frame = frame(Style::Full, true);
        assert_eq!(hit(&frame, 100.0 + 383.0, 117.0), Some(Hit::Button(Button::Close)));
        assert_eq!(hit(&frame, 100.0 + 355.0, 117.0), Some(Hit::Button(Button::Maximize)));
        assert_eq!(hit(&frame, 100.0 + 327.0, 117.0), Some(Hit::Button(Button::Minimize)));
        // Between buttons and on the title.
        assert_eq!(hit(&frame, 100.0 + 369.0, 117.0), Some(Hit::Titlebar));
        assert_eq!(hit(&frame, 200.0, 100.0), Some(Hit::Titlebar));
        assert_eq!(hit(&frame, 200.0, 133.9), Some(Hit::Titlebar));
        assert_eq!(hit(&frame, 200.0, 134.0), None, "the content belongs to the client");
        assert_eq!(hit(&frame, 50.0, 50.0), None);
    }

    #[test]
    fn borders_resize_floating_windows() {
        let frame = frame(Style::Full, true);
        assert_eq!(hit(&frame, 96.0, 300.0), Some(Hit::Edge(ResizeEdge::Left)));
        assert_eq!(hit(&frame, 503.0, 300.0), Some(Hit::Edge(ResizeEdge::Right)));
        assert_eq!(hit(&frame, 300.0, 95.0), Some(Hit::Edge(ResizeEdge::Top)));
        assert_eq!(hit(&frame, 300.0, 440.0), Some(Hit::Edge(ResizeEdge::Bottom)));
        assert_eq!(hit(&frame, 95.0, 95.0), Some(Hit::Edge(ResizeEdge::TopLeft)));
        assert_eq!(hit(&frame, 505.0, 440.0), Some(Hit::Edge(ResizeEdge::BottomRight)));
        // A border's width along an edge from the corner still grabs the corner.
        assert_eq!(hit(&frame, 105.0, 437.0), Some(Hit::Edge(ResizeEdge::BottomLeft)));
        assert_eq!(hit(&frame, 497.0, 97.0), Some(Hit::Edge(ResizeEdge::TopRight)));
        assert_eq!(hit(&frame, 503.0, 105.0), Some(Hit::Edge(ResizeEdge::TopRight)));
        assert_eq!(hit(&frame, 91.9, 300.0), None, "beyond the border");
        assert_eq!(hit(&frame, 300.0, 442.0), None);
    }

    #[test]
    fn maximized_and_tiled_frames_have_no_borders() {
        let frame = frame(Style::Slim, false);
        assert_eq!(hit(&frame, 96.0, 300.0), None);
        assert_eq!(hit(&frame, 200.0, 120.0), Some(Hit::Titlebar));
    }

    #[test]
    fn content_goes_below_the_titlebar() {
        let area = Rect::new((0, 32).into(), (1280, 688).into());
        assert_eq!(below_titlebar(area, 34), Rect::new((0, 66).into(), (1280, 654).into()));
        assert_eq!(below_titlebar(area, 0), area);
        assert_eq!(below_titlebar(area, 1000).size.h, 0);
    }
}
