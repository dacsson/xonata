//! Trace colors inspired by Konata's dark style; application chrome stays monospace.
use egui::{Color32, Painter, Rect, Stroke, ecolor::rgb_from_hsv};
use serde::{Deserialize, Serialize};

pub(crate) const COLORS: [Color32; 8] = [
    Color32::from_rgb(93, 165, 180),
    Color32::from_rgb(133, 132, 191),
    Color32::from_rgb(184, 142, 107),
    Color32::from_rgb(116, 169, 126),
    Color32::from_rgb(168, 115, 139),
    Color32::from_rgb(94, 135, 172),
    Color32::from_rgb(164, 168, 104),
    Color32::from_rgb(130, 155, 171),
];

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Theme {
    Xonata,
    #[default]
    Classic,
}

impl Theme {
    pub const fn background(self) -> Color32 {
        match self {
            Self::Xonata => Color32::from_rgb(19, 23, 29),
            Self::Classic => Color32::from_rgb(38, 41, 48),
        }
    }

    pub const fn panel(self) -> Color32 {
        match self {
            Self::Xonata => Color32::from_rgb(28, 34, 43),
            Self::Classic => Color32::from_rgb(48, 51, 64),
        }
    }

    pub const fn stage_text(self) -> Color32 {
        match self {
            Self::Xonata => self.background(),
            Self::Classic => Color32::from_rgb(240, 240, 240),
        }
    }

    pub const fn row_background(self, row: u64) -> Option<Color32> {
        match self {
            // Konata dark/style.json: #ffffff08 over #262930, on even rows.
            Self::Classic if row.is_multiple_of(2) => Some(Color32::from_rgb(45, 48, 54)),
            _ => None,
        }
    }

    pub fn stage_color(self, index: u32) -> Color32 {
        match self {
            Self::Xonata => COLORS[index as usize % COLORS.len()],
            Self::Classic => {
                // Konata dark/style.json uses HSL saturation/lightness 40% and hue steps of 55°.
                // Keep the eight-color cycle shared with the compact worker overview palette.
                let level = (index % 8 + 7) % 8;
                let hue = (610 - level * 55) % 360;
                let [r, g, b] = rgb_from_hsv((hue as f32 / 360.0, 4.0 / 7.0, 0.56));
                Color32::from_rgb(
                    (r * 255.0).round() as u8,
                    (g * 255.0).round() as u8,
                    (b * 255.0).round() as u8,
                )
            }
        }
    }
}

pub(crate) fn stage_border(painter: &Painter, slab: Rect, clip: Rect, opacity: f32) {
    let visible = slab.intersect(clip);
    if visible.width() < 4.0 || visible.height() < 4.0 {
        return;
    }
    let stroke = Stroke::new(0.5_f32, Color32::from_gray(240).gamma_multiply(opacity));
    let edge = slab.shrink(stroke.width * 0.5);
    let clipped_edge = edge.intersect(clip);
    let painter = painter.with_clip_rect(clip);
    // Only real stage edges get borders; clipping must not invent an endpoint.
    for y in [edge.top(), edge.bottom()] {
        if clip.y_range().contains(y) {
            painter.hline(clipped_edge.x_range(), y, stroke);
        }
    }
    for x in [edge.left(), edge.right()] {
        if clip.x_range().contains(x) {
            painter.vline(x, clipped_edge.y_range(), stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{CentralPanel, Context, Pos2, Vec2, epaint::Shape};

    fn borders(slab: Rect, opacity: f32) -> Vec<([Pos2; 2], Stroke)> {
        let ctx = Context::default();
        let clip = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::splat(100.0));
        ctx.run(Default::default(), |ctx| {
            CentralPanel::default().show(ctx, |ui| stage_border(ui.painter(), slab, clip, opacity));
        })
        .shapes
        .into_iter()
        .filter_map(|shape| match shape.shape {
            Shape::LineSegment { points, stroke } if stroke.width == 0.5 => Some((points, stroke)),
            _ => None,
        })
        .collect()
    }

    #[test]
    fn borders_should_skip_tiny_slabs_dim_flushed_stages_and_preserve_clipped_endpoints() {
        let slab = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(50.0, 20.0));
        let full = borders(slab, 1.0);
        assert_eq!(full.len(), 4);
        assert!(
            full.iter()
                .all(|(points, _)| points.iter().all(|p| slab.contains(*p)))
        );
        let flushed = borders(slab, 0.22);
        assert!(
            flushed
                .iter()
                .all(|(_, stroke)| stroke.color.a() < full[0].1.color.a() / 3)
        );
        for size in [Vec2::new(3.9, 20.0), Vec2::new(50.0, 3.9)] {
            assert!(borders(Rect::from_min_size(slab.min, size), 1.0).is_empty());
        }
        let clipped = borders(Rect::from_min_max(Pos2::new(-100.0, 20.0), slab.max), 1.0);
        assert_eq!(clipped.len(), 3);
        assert!(
            !clipped
                .iter()
                .any(|(points, _)| points[0].x == 10.0 && points[1].x == 10.0)
        );
        let long = borders(
            Rect::from_min_max(Pos2::new(-1e20, 20.0), Pos2::new(1e20, 40.0)),
            1.0,
        );
        assert_eq!(long.len(), 2);
        assert!(
            long.iter()
                .all(|(points, _)| points.iter().all(|p| (10.0..=110.0).contains(&p.x)))
        );
    }
}
