// SPDX-License-Identifier: MIT

//! Mapping pointer positions to grid cells, and mouse-driven text selection.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Term, viewport_to_point};

/// The cell grid on screen, in any consistent unit such as physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellGeometry {
    pub cell_width: f32,
    pub cell_height: f32,
    pub columns: usize,
    pub lines: usize,
}

/// A position in the viewport: zero-based column and line, and which half of the cell it's in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportCell {
    pub column: usize,
    pub line: usize,
    pub side: Side,
}

impl ViewportCell {
    /// The grid point of this cell when the view is scrolled back by `display_offset` lines.
    pub fn point(self, display_offset: usize) -> Point {
        viewport_to_point(display_offset, Point::new(self.line, Column(self.column)))
    }
}

/// The cell under `(x, y)`, measured from the grid's top-left corner, clamped to the grid.
pub fn cell_at(x: f32, y: f32, geometry: CellGeometry) -> ViewportCell {
    let clamp = |value: f32, size: f32, count: usize| -> (usize, f32) {
        if count == 0 || size <= 0.0 || !value.is_finite() {
            return (0, 0.0);
        }
        let index = (value / size).floor().clamp(0.0, (count - 1) as f32);
        (index as usize, value - index * size)
    };
    let (column, offset) = clamp(x, geometry.cell_width, geometry.columns);
    let (line, _) = clamp(y, geometry.cell_height, geometry.lines);
    let side =
        if x >= 0.0 && offset >= geometry.cell_width / 2.0 { Side::Right } else { Side::Left };
    ViewportCell { column, line, side }
}

/// The selection kind for a click: characters, words on a double click, lines on a triple click.
/// `block` selects a rectangle, as with Ctrl held.
pub fn selection_type(clicks: u8, block: bool) -> SelectionType {
    match clicks {
        2 => SelectionType::Semantic,
        3 => SelectionType::Lines,
        _ if block => SelectionType::Block,
        _ => SelectionType::Simple,
    }
}

/// Starts a selection at `point`; word and line selections cover their unit right away.
pub fn start<T>(term: &mut Term<T>, ty: SelectionType, point: Point, side: Side) {
    term.selection = Some(Selection::new(ty, point, side));
}

/// Moves the end of the current selection to `point`.
pub fn update<T>(term: &mut Term<T>, point: Point, side: Side) {
    if let Some(selection) = term.selection.as_mut() {
        selection.update(point, side);
    }
}

/// Selects the whole scrollback and screen.
pub fn select_all<T>(term: &mut Term<T>) {
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
    selection.update(end, Side::Right);
    term.selection = Some(selection);
}

/// The selected text without trailing line breaks, so pasting a selected line doesn't run it.
/// Returns `None` when nothing is selected.
pub fn text<T>(term: &Term<T>) -> Option<String> {
    let text = term.selection_to_string()?;
    let text = text.trim_end_matches('\n');
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{feed, test_term};

    const GEOMETRY: CellGeometry =
        CellGeometry { cell_width: 10.0, cell_height: 20.0, columns: 20, lines: 5 };

    #[test]
    fn pixel_to_cell() {
        assert_eq!(
            cell_at(0.0, 0.0, GEOMETRY),
            ViewportCell { column: 0, line: 0, side: Side::Left }
        );
        assert_eq!(
            cell_at(14.0, 25.0, GEOMETRY),
            ViewportCell { column: 1, line: 1, side: Side::Left }
        );
        assert_eq!(cell_at(16.0, 25.0, GEOMETRY).side, Side::Right);
        let far = cell_at(1000.0, 1000.0, GEOMETRY);
        assert_eq!((far.column, far.line), (19, 4));
        let before = cell_at(-5.0, -5.0, GEOMETRY);
        assert_eq!((before.column, before.line, before.side), (0, 0, Side::Left));
        let empty = CellGeometry { columns: 0, ..GEOMETRY };
        assert_eq!(cell_at(f32::NAN, 3.0, empty).column, 0);
    }

    #[test]
    fn viewport_points_follow_scrollback() {
        let cell = ViewportCell { column: 3, line: 2, side: Side::Left };
        assert_eq!(cell.point(0), Point::new(alacritty_terminal::index::Line(2), Column(3)));
        assert_eq!(cell.point(5), Point::new(alacritty_terminal::index::Line(-3), Column(3)));
    }

    #[test]
    fn click_kinds() {
        assert_eq!(selection_type(1, false), SelectionType::Simple);
        assert_eq!(selection_type(1, true), SelectionType::Block);
        assert_eq!(selection_type(2, true), SelectionType::Semantic);
        assert_eq!(selection_type(3, false), SelectionType::Lines);
    }

    #[test]
    fn drag_word_and_line_selection() {
        let mut term = test_term(20, 5);
        feed(&mut term, b"hello brave world\r\nsecond line");
        let at = |column, line| ViewportCell { column, line, side: Side::Left }.point(0);

        start(&mut term, SelectionType::Simple, at(0, 0), Side::Left);
        update(&mut term, at(4, 0), Side::Right);
        assert_eq!(text(&term).as_deref(), Some("hello"));

        start(&mut term, SelectionType::Semantic, at(8, 0), Side::Left);
        assert_eq!(text(&term).as_deref(), Some("brave"));
        update(&mut term, at(13, 0), Side::Left);
        assert_eq!(text(&term).as_deref(), Some("brave world"));

        start(&mut term, SelectionType::Lines, at(2, 1), Side::Left);
        assert_eq!(text(&term).as_deref(), Some("second line"));

        select_all(&mut term);
        assert_eq!(text(&term).as_deref(), Some("hello brave world\nsecond line"));
    }

    #[test]
    fn wide_characters_select_whole() {
        let mut term = test_term(20, 2);
        feed(&mut term, "ab 漢字 cd".as_bytes());
        let at = |column| ViewportCell { column, line: 0, side: Side::Left }.point(0);
        start(&mut term, SelectionType::Semantic, at(4), Side::Left);
        assert_eq!(text(&term).as_deref(), Some("漢字"));
    }

    #[test]
    fn empty_selection_has_no_text() {
        let mut term = test_term(10, 2);
        assert_eq!(text(&term), None);
        update(&mut term, Point::new(alacritty_terminal::index::Line(0), Column(1)), Side::Right);
        assert_eq!(text(&term), None);
    }
}
