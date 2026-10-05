// SPDX-License-Identifier: MIT

//! SVG path commands for the Resources graphs, drawn by a Slint `Path`.
//!
//! The view box is `capacity - 1` wide and [`GRAPH_HEIGHT`] tall; the newest value sits at the right edge.

use std::fmt::Write;

/// Height of the graph view box.
pub const GRAPH_HEIGHT: f32 = 100.0;

fn points(values: &[f32], capacity: usize, max: f32) -> impl Iterator<Item = (f32, f32)> + '_ {
    let capacity = capacity.max(values.len()).max(2);
    let offset = capacity - values.len();
    let max = if max.is_finite() && max > 0.0 { max } else { 1.0 };
    values.iter().enumerate().map(move |(i, v)| {
        let v = if v.is_finite() { v.clamp(0.0, max) } else { 0.0 };
        ((offset + i) as f32, GRAPH_HEIGHT - v / max * GRAPH_HEIGHT)
    })
}

/// The polyline through `values`, scaled so that `max` reaches the top; empty for fewer than two values.
pub fn line(values: &[f32], capacity: usize, max: f32) -> String {
    let mut path = String::new();
    if values.len() < 2 {
        return path;
    }
    for (i, (x, y)) in points(values, capacity, max).enumerate() {
        let command = if i == 0 { 'M' } else { 'L' };
        // Writing to a String can't fail.
        let _ = write!(path, "{command} {x} {y:.2} ");
    }
    path.truncate(path.trim_end().len());
    path
}

/// The area under [`line`], closed along the bottom edge.
pub fn area(values: &[f32], capacity: usize, max: f32) -> String {
    let mut path = line(values, capacity, max);
    if path.is_empty() {
        return path;
    }
    let capacity = capacity.max(values.len()).max(2);
    let first = (capacity - values.len()) as f32;
    let last = (capacity - 1) as f32;
    let _ = write!(path, " L {last} {GRAPH_HEIGHT} L {first} {GRAPH_HEIGHT} Z");
    path
}

/// A round upper bound for a rate graph: 1, 2, or 5 times a power of ten, at least `floor`.
pub fn nice_max(value: f32, floor: f32) -> f32 {
    let value = if value.is_finite() { value.max(floor) } else { floor };
    if value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f32.powf(value.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|candidate| *candidate >= value)
        .unwrap_or(10.0 * magnitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_is_right_aligned() {
        assert_eq!(line(&[0.0, 50.0, 100.0], 5, 100.0), "M 2 100.00 L 3 50.00 L 4 0.00");
        assert_eq!(line(&[1.0], 5, 100.0), "");
    }

    #[test]
    fn values_are_clamped() {
        assert_eq!(line(&[-5.0, 500.0], 2, 100.0), "M 0 100.00 L 1 0.00");
        assert_eq!(line(&[f32::NAN, 0.5], 2, 0.0), "M 0 100.00 L 1 50.00");
    }

    #[test]
    fn area_closes_along_bottom() {
        assert_eq!(area(&[100.0, 100.0], 4, 100.0), "M 2 0.00 L 3 0.00 L 3 100 L 2 100 Z");
        assert_eq!(area(&[], 4, 100.0), "");
    }

    #[test]
    fn nice_maxima() {
        assert_eq!(nice_max(0.0, 1024.0), 2000.0);
        assert_eq!(nice_max(1500.0, 1.0), 2000.0);
        assert_eq!(nice_max(3000.0, 1.0), 5000.0);
        assert_eq!(nice_max(5000.0, 1.0), 5000.0);
        assert_eq!(nice_max(7000.0, 1.0), 10000.0);
        assert_eq!(nice_max(f32::INFINITY, 10.0), 10.0);
    }
}
