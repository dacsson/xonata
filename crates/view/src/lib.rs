//! Precise viewport transforms and session-only annotations.
#![deny(missing_docs)]
use serde::{Deserialize, Serialize};

/// Default horizontal scale, with room for a short phase name and padding.
pub const DEFAULT_CYCLE_WIDTH: f32 = 72.0;
/// Default instruction height, including space between adjacent slabs.
pub const DEFAULT_ROW_HEIGHT: f32 = 44.0;

/// Navigation state independent of the renderer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Viewport {
    /// Integer cycle origin; converted to pixels only after subtraction.
    pub cycle: u64,
    /// Fractional cycle offset.
    pub cycle_fraction: f64,
    /// First instruction row.
    pub row: f64,
    /// Pixels per cycle.
    pub cycle_width: f32,
    /// Pixels per instruction.
    pub row_height: f32,
}
impl Default for Viewport {
    fn default() -> Self {
        Self {
            cycle: 0,
            cycle_fraction: 0.0,
            row: 0.0,
            cycle_width: DEFAULT_CYCLE_WIDTH,
            row_height: DEFAULT_ROW_HEIGHT,
        }
    }
}
impl Viewport {
    /// Cycle position relative to the left edge.
    pub fn x(&self, cycle: u64) -> f32 {
        let delta = if cycle >= self.cycle {
            (cycle - self.cycle) as f64
        } else {
            -((self.cycle - cycle) as f64)
        };
        ((delta - self.cycle_fraction) * self.cycle_width as f64) as f32
    }
    /// Instruction position relative to the top edge.
    pub fn y(&self, row: u64) -> f32 {
        ((row as f64 - self.row) * self.row_height as f64) as f32
    }
    /// Shared vertical center for an instruction's slabs and disassembly label.
    pub fn row_center(&self, row: u64) -> f32 {
        self.y(row) + self.row_height * 0.5
    }
    /// Converts a pixel offset to the nearest cycle.
    pub fn cycle_at(&self, x: f32) -> u64 {
        let delta = self.cycle_fraction + x as f64 / self.cycle_width as f64;
        if delta >= 0.0 {
            self.cycle.saturating_add(delta as u64)
        } else {
            self.cycle.saturating_sub((-delta).ceil() as u64)
        }
    }
    /// Converts a pixel offset to an instruction row.
    pub fn row_at(&self, y: f32) -> u64 {
        (self.row + y as f64 / self.row_height as f64).max(0.0) as u64
    }
    /// Pans in screen pixels, with nonnegative coordinates.
    pub fn pan(&mut self, x: f32, y: f32) {
        self.shift_cycles(-x as f64 / self.cycle_width as f64);
        self.row = (self.row - y as f64 / self.row_height as f64).max(0.0);
    }
    fn shift_cycles(&mut self, delta: f64) {
        let value = self.cycle_fraction + delta;
        let whole = value.floor();
        if whole >= 0.0 {
            self.cycle = self.cycle.saturating_add(whole as u64);
        } else if (-whole) as u64 > self.cycle {
            self.cycle = 0;
            self.cycle_fraction = 0.0;
            return;
        } else {
            self.cycle -= (-whole) as u64;
        }
        self.cycle_fraction = value - whole;
    }
    /// Zooms around a screen-space anchor without moving its instruction/cycle.
    pub fn zoom(&mut self, factor: f32, x: f32, y: f32) {
        self.set_scale(self.cycle_width * factor, self.row_height * factor, x, y);
    }
    /// Changes each axis independently while preserving the screen anchor.
    pub fn zoom_axes(&mut self, horizontal: f32, vertical: f32, x: f32, y: f32) {
        self.set_scale(
            self.cycle_width * horizontal,
            self.row_height * vertical,
            x,
            y,
        );
    }
    /// Restores the readable default scale while preserving the anchor.
    pub fn reset_zoom(&mut self, x: f32, y: f32) {
        self.set_scale(DEFAULT_CYCLE_WIDTH, DEFAULT_ROW_HEIGHT, x, y);
    }
    fn set_scale(&mut self, width: f32, height: f32, x: f32, y: f32) {
        let old_x = self.cycle_width;
        let old_y = self.row_height;
        self.cycle_width = width.clamp(0.02, 320.0);
        self.row_height = height.clamp(0.5, 160.0);
        self.shift_cycles(x as f64 / old_x as f64 - x as f64 / self.cycle_width as f64);
        self.row =
            (self.row + y as f64 / old_y as f64 - y as f64 / self.row_height as f64).max(0.0);
    }
    /// Row sampling stride bounded by approximately one sample per pixel.
    pub fn stride(&self) -> u64 {
        (1.0 / self.row_height).ceil().max(1.0) as u64
    }
}

/// A session marker snapped to the cycle and pipeline-row grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marker {
    /// Cycle grid coordinate.
    pub cycle: u64,
    /// Pipeline-row grid coordinate.
    pub row: u64,
}
impl Marker {
    /// Absolute cycle and pipeline-row distances to another marker.
    pub fn distance(self, other: Self) -> (u64, u64) {
        (
            self.cycle.abs_diff(other.cycle),
            self.row.abs_diff(other.row),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_zoom_should_preserve_the_other_axis_and_anchor() {
        let mut view = Viewport {
            cycle: 100,
            row: 30.0,
            ..Default::default()
        };
        let anchor = (view.cycle_at(220.0), view.row_at(200.0));
        view.zoom_axes(2.0, 1.0, 220.0, 200.0);
        assert_eq!(
            (view.row_height, view.row, view.cycle_at(220.0)),
            (DEFAULT_ROW_HEIGHT, 30.0, anchor.0)
        );
        let cycle_origin = (view.cycle, view.cycle_fraction);
        view.zoom_axes(1.0, 2.0, 220.0, 200.0);
        assert_eq!(
            ((view.cycle, view.cycle_fraction), view.row_at(200.0)),
            (cycle_origin, anchor.1)
        );
    }
    #[test]
    fn marker_distances_should_be_symmetric_and_handle_large_coordinates() {
        let a = Marker {
            cycle: u64::MAX - 2,
            row: 4,
        };
        let b = Marker {
            cycle: u64::MAX,
            row: 10,
        };
        assert_eq!((a.distance(b), b.distance(a)), ((2, 6), (2, 6)));
    }
    #[test]
    fn large_cycles_should_preserve_pixel_precision() {
        let view = Viewport {
            cycle: u64::MAX - 100,
            ..Default::default()
        };
        assert_eq!(view.x(u64::MAX - 99), DEFAULT_CYCLE_WIDTH);
    }
    #[test]
    fn zoom_should_preserve_cursor_anchor() {
        let mut view = Viewport {
            cycle: 100,
            row: 30.0,
            ..Default::default()
        };
        let cycle = view.cycle_at(240.0);
        let row = view.row_at(300.0);
        view.zoom(2.0, 240.0, 300.0);
        assert_eq!((view.cycle_at(240.0), view.row_at(300.0)), (cycle, row));
    }
    #[test]
    fn disassembly_centers_should_hit_their_rows_after_fractional_pan_and_zoom() {
        let mut view = Viewport {
            row: 12.35,
            ..Default::default()
        };
        view.zoom(1.7, 280.0, 180.0);
        assert!((16..24).all(|row| view.row_at(view.row_center(row)) == row));
    }
    #[test]
    fn reset_zoom_should_restore_readable_scale_without_moving_the_anchor() {
        let mut view = Viewport {
            cycle: 1000,
            row: 300.0,
            ..Default::default()
        };
        view.zoom(20.0, 200.0, 180.0);
        let anchor = (view.cycle_at(200.0), view.row_at(180.0));
        view.reset_zoom(200.0, 180.0);
        assert_eq!(
            (
                view.cycle_at(200.0),
                view.row_at(180.0),
                view.cycle_width,
                view.row_height
            ),
            (anchor.0, anchor.1, DEFAULT_CYCLE_WIDTH, DEFAULT_ROW_HEIGHT)
        );
    }
}
