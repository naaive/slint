// SPDX-License-Identifier: MIT

//! Rubber-band selection: which cells of a regular grid a dragged rectangle touches.

/// The layout of a view's cells in content coordinates; a list is a grid with one column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridGeometry {
    pub columns: usize,
    pub count: usize,
    pub cell_width: f32,
    pub cell_height: f32,
    /// The top-left corner of the first cell.
    pub origin_x: f32,
    pub origin_y: f32,
}

/// The indices of cells intersecting the rectangle spanned by two corners, in order.
pub fn cells_in_rect(geometry: GridGeometry, a: (f32, f32), b: (f32, f32)) -> Vec<usize> {
    let GridGeometry { columns, count, cell_width, cell_height, origin_x, origin_y } = geometry;
    if columns == 0 || count == 0 || cell_width <= 0.0 || cell_height <= 0.0 {
        return Vec::new();
    }
    let values = [a.0, a.1, b.0, b.1];
    if values.iter().any(|v| !v.is_finite()) {
        return Vec::new();
    }
    let (left, right) = (a.0.min(b.0) - origin_x, a.0.max(b.0) - origin_x);
    let (top, bottom) = (a.1.min(b.1) - origin_y, a.1.max(b.1) - origin_y);
    let rows = count.div_ceil(columns);
    let grid_width = columns as f32 * cell_width;
    let grid_height = rows as f32 * cell_height;
    if right < 0.0 || bottom < 0.0 || left >= grid_width || top >= grid_height {
        return Vec::new();
    }
    let first_col = (left.max(0.0) / cell_width) as usize;
    let last_col = ((right.min(grid_width - 0.001)) / cell_width) as usize;
    let first_row = (top.max(0.0) / cell_height) as usize;
    let last_row = ((bottom.min(grid_height - 0.001)) / cell_height) as usize;
    let mut cells = Vec::new();
    for row in first_row..=last_row.min(rows - 1) {
        for col in first_col..=last_col.min(columns - 1) {
            let index = row * columns + col;
            if index < count {
                cells.push(index);
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: GridGeometry = GridGeometry {
        columns: 4,
        count: 10,
        cell_width: 100.0,
        cell_height: 120.0,
        origin_x: 10.0,
        origin_y: 0.0,
    };

    #[test]
    fn grid_hits() {
        assert_eq!(cells_in_rect(GRID, (15.0, 5.0), (20.0, 10.0)), [0]);
        assert_eq!(cells_in_rect(GRID, (250.0, 200.0), (50.0, 10.0)), [0, 1, 2, 4, 5, 6]);
        assert_eq!(cells_in_rect(GRID, (110.0, 250.0), (1000.0, 1000.0)), [9]);
        assert_eq!(cells_in_rect(GRID, (0.0, 0.0), (5.0, 500.0)), Vec::<usize>::new());
        assert_eq!(cells_in_rect(GRID, (-50.0, -50.0), (2000.0, 2000.0)).len(), 10);
        assert_eq!(cells_in_rect(GRID, (f32::NAN, 0.0), (1.0, 1.0)), Vec::<usize>::new());
    }

    #[test]
    fn list_hits() {
        let list = GridGeometry {
            columns: 1,
            count: 5,
            cell_width: 600.0,
            cell_height: 36.0,
            origin_x: 0.0,
            origin_y: 0.0,
        };
        assert_eq!(cells_in_rect(list, (10.0, 40.0), (20.0, 100.0)), [1, 2]);
        assert_eq!(cells_in_rect(list, (10.0, 500.0), (20.0, 600.0)), Vec::<usize>::new());
        let empty = GridGeometry { count: 0, ..list };
        assert!(cells_in_rect(empty, (0.0, 0.0), (10.0, 10.0)).is_empty());
    }
}
