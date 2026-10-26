//! Animated gradient and horizontal stardust, clipped to the slider capsule.
use egui::{Color32, Mesh, Painter, Rect, Shape, Stroke, pos2, vec2};
use superba_core::SpeedTier;

fn inset(rect: Rect, x: f32) -> f32 {
    let radius = rect.height() / 2.0;
    let edge = (x - rect.min.x).min(rect.max.x - x).max(0.0);
    if edge >= radius {
        0.0
    } else {
        radius - (radius * radius - (radius - edge).powi(2)).max(0.0).sqrt()
    }
}

fn hash(n: usize) -> f32 {
    ((n as f32 * 127.1 + 311.7).sin() * 43758.547).fract().abs()
}

pub(super) fn paint(p: &Painter, rect: Rect, accent: Color32, tier: SpeedTier, time: f64) {
    let ultra = tier == SpeedTier::Ultrafast;
    let speed = if ultra { 125.0 } else { 42.0 };
    let phase = (time % 4096.0) as f32;
    let radius = rect.height() / 2.0;
    for i in (1..=4).rev() {
        p.rect_filled(
            rect.expand(i as f32 * 1.5),
            radius + i as f32 * 1.5,
            accent.gamma_multiply(0.025),
        );
    }
    let highlight = if ultra {
        Color32::from_rgb(230, 162, 255)
    } else {
        Color32::from_rgb(136, 248, 220)
    };
    let mut mesh = Mesh::default();
    for i in 0..=72 {
        let x = rect.min.x + rect.width() * i as f32 / 72.0;
        let edge = inset(rect, x);
        let pulse = ((i as f32 * 0.13 - phase * if ultra { 2.4 } else { 0.8 }).sin() + 1.0) / 2.0;
        let color = accent.lerp_to_gamma(highlight, 0.12 + pulse * 0.32);
        mesh.colored_vertex(pos2(x, rect.min.y + edge), color);
        mesh.colored_vertex(pos2(x, rect.max.y - edge), accent.lerp_to_gamma(color, 0.5));
        if i > 0 {
            let b = (i * 2) as u32;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b, b - 1, b + 1);
        }
    }
    p.add(Shape::mesh(mesh));
    particles(p, rect, phase, speed, ultra);
}

fn particles(p: &Painter, rect: Rect, phase: f32, speed: f32, ultra: bool) {
    let count = if ultra { 36 } else { 24 };
    for i in 0..count {
        let x = rect.min.x
            + (hash(i * 3) * rect.width() + phase * speed * (0.6 + hash(i + 91)))
                .rem_euclid(rect.width().max(1.0));
        let y = rect.min.y + rect.height() * (0.12 + hash(i * 3 + 1) * 0.76);
        let pad = inset(rect, x) + 1.5;
        if y < rect.min.y + pad || y > rect.max.y - pad {
            continue;
        }
        let at = pos2(x, y);
        let length =
            if ultra { 7.0 } else { 3.0 } + hash(i * 3 + 2) * if ultra { 12.0 } else { 5.0 };
        let start = (x - length).max(rect.min.x + 2.0);
        let fade = ((x - rect.min.x) / 12.0)
            .min((rect.max.x - x) / 12.0)
            .clamp(0.0, 1.0);
        let light = Color32::WHITE.gamma_multiply(fade * (0.4 + hash(i + 121) * 0.5));
        for glow in (1..=3).rev() {
            p.circle_filled(at, glow as f32 * 1.3, light.gamma_multiply(0.035));
        }
        if start + 1.0 < x
            && y > rect.min.y + inset(rect, start) + 1.0
            && y < rect.max.y - inset(rect, start) - 1.0
        {
            p.line_segment(
                [pos2(start, y), at - vec2(1.0, 0.0)],
                Stroke::new(1.0, light.gamma_multiply(0.35)),
            );
        }
        p.circle_filled(at, 0.65 + hash(i + 57) * 0.45, light);
    }
}
