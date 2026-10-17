//! Small dashboard charts after Surge (user request 24): a sparkline with a
//! soft area under it, and a bar chart of hourly buckets with dotted grid lines
//! and dots for empty hours. Both take plain values; captions and scale labels
//! are drawn by the caller.

use egui::{Color32, Mesh, Painter, Pos2, Rect, Shape, Stroke, pos2};

use crate::design::backdrop::Backdrop;

/// The largest value of `values` (0 when empty).
pub fn max_of(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, f64::max)
}

/// A "nice" chart maximum at or above `max`: 1, 2 or 5 times a power of ten
/// (0 stays 0).
pub fn nice_max(max: f64) -> f64 {
    if max <= 0.0 || !max.is_finite() {
        return 0.0;
    }
    let pow = 10f64.powf(max.log10().floor());
    for step in [1.0, 2.0, 5.0, 10.0] {
        if step * pow >= max * 0.999_999 {
            return step * pow;
        }
    }
    10.0 * pow
}

/// Paint a sparkline of `values` (oldest first) in `rect`, scaled to `max`
/// (the largest value when `None`): a 1.5 px line and an area that fades to
/// transparent at the bottom. Nothing is drawn for fewer than two values.
pub fn sparkline(painter: &Painter, rect: Rect, values: &[f64], max: Option<f64>, color: Color32) {
    if values.len() < 2 {
        return;
    }
    let max = max.unwrap_or_else(|| max_of(values)).max(f64::EPSILON);
    let step = rect.width() / (values.len() - 1) as f32;
    let points: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let t = (v / max).clamp(0.0, 1.0) as f32;
            pos2(rect.min.x + step * i as f32, rect.max.y - t * rect.height())
        })
        .collect();
    let mut mesh = Mesh::default();
    let top = Backdrop::area(color, 70);
    let bottom = Backdrop::area(color, 0);
    for (i, p) in points.iter().enumerate() {
        // The fade is relative to the card, not the point: high points get the
        // full tint, the bottom edge none.
        let t = ((rect.max.y - p.y) / rect.height().max(1.0)).clamp(0.0, 1.0);
        mesh.colored_vertex(*p, bottom.lerp_to_gamma(top, 0.35 + 0.65 * t));
        mesh.colored_vertex(pos2(p.x, rect.max.y), bottom);
        if i > 0 {
            let a = (i as u32 - 1) * 2;
            mesh.add_triangle(a, a + 2, a + 1);
            mesh.add_triangle(a + 1, a + 2, a + 3);
        }
    }
    painter.add(mesh);
    painter.add(Shape::line(points, Stroke::new(1.6, color)));
}

/// Paint dotted horizontal grid lines at the given fractions of `rect`'s height
/// (0 = bottom, 1 = top).
pub fn grid_lines(painter: &Painter, rect: Rect, fractions: &[f32], color: Color32) {
    for f in fractions {
        let y = rect.max.y - f * rect.height();
        let mut x = rect.min.x;
        while x < rect.max.x {
            painter.circle_filled(pos2(x, y), 0.75, color);
            x += 5.0;
        }
    }
}

/// Paint `values` as rounded vertical bars in equal slots of `rect`, scaled to
/// `max`; an empty slot shows a small dot on the base line (Surge). Returns the
/// x centre of every slot (for axis labels).
pub fn bars(
    painter: &Painter,
    rect: Rect,
    values: &[f64],
    max: f64,
    color: Color32,
    empty: Color32,
) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let slot = rect.width() / values.len() as f32;
    let w = (slot * 0.45).clamp(3.0, 10.0);
    let max = max.max(f64::EPSILON);
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let cx = rect.min.x + slot * (i as f32 + 0.5);
            let h = ((v / max).clamp(0.0, 1.0) as f32) * rect.height();
            if *v <= 0.0 || h < 1.0 {
                painter.circle_filled(pos2(cx, rect.max.y - w / 2.0), w / 2.0, empty);
            } else {
                let h = h.max(w);
                let bar = Rect::from_min_max(
                    pos2(cx - w / 2.0, rect.max.y - h),
                    pos2(cx + w / 2.0, rect.max.y),
                );
                painter.rect_filled(bar, (w / 2.0) as u8, color);
            }
            cx
        })
        .collect()
}
