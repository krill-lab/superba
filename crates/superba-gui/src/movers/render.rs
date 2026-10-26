//! Rasterizes the scene's layers once (resvg, at the display's pixel density)
//! into egui textures and paints them, moved by a [`Pose`], with the texts on
//! top (egui text in the app's fonts, translated).
//!
//! Each layer is rendered at `points per scene unit x pixels per point` into a
//! texture covering its own bounding box (static layers aligned to the pixel
//! grid, so they stay sharp); the moving parts are textured quads whose
//! corners go through the layer's transform chain. Textures are rendered again
//! only when that density changes by more than [`RERENDER_RATIO`] (a window
//! resize), never per frame.

use std::sync::{Arc, LazyLock};

use egui::epaint::{Mesh, TextShape, Vertex};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, ColorImage, Context, FontId, Painter, Pos2, Rect, Shape, Stroke, TextureHandle,
    TextureOptions, pos2, vec2,
};
use resvg::tiny_skia::{self, Point, Transform};
use resvg::usvg;

use super::pose::Pose;
use super::svg::{self, Anchor, Layer, Placement, Scene, TextItem};
use crate::design::fonts::{self, Weight};

/// The scene's SVG (the artwork as delivered; see `assets/scenes/`).
pub const SCENE_SVG: &str = include_str!("../../../../assets/scenes/superba-session-movers.svg");

/// Density change that triggers a new rasterization.
pub const RERENDER_RATIO: f32 = 0.08;

/// The analysed scene, shared by every dialog.
pub fn scene() -> Option<&'static Scene> {
    static SCENE: LazyLock<Option<Scene>> = LazyLock::new(|| match svg::analyse(SCENE_SVG) {
        Ok(scene) => Some(scene),
        Err(e) => {
            tracing::warn!(error = %e, "the session migration scene cannot be read");
            None
        }
    });
    SCENE.as_ref()
}

/// Width / height of the scene.
pub fn aspect() -> f32 {
    scene().map_or(1.6, |s| s.size.0 / s.size.1)
}

/// The translation key of a text of the artwork, by its Chinese source text.
/// Texts not listed (the brand name) are drawn as written.
pub fn text_key(source: &str) -> Option<&'static str> {
    // The note's second line ends with a coffee cup emoji the app's fonts lack;
    // it is painted as a little cup instead (see `cup`).
    let source = source.trim_end_matches(CUP).trim_end();
    Some(match source {
        "会话慢慢搬，咖啡慢慢喝。" => "movers.scene.title",
        "一只负责进度，一只负责松弛。" => "movers.scene.subtitle",
        "LOW CONCURRENCY" => "movers.scene.low_concurrency",
        "THREAD 01" => "movers.scene.thread_01",
        "THREAD 02" => "movers.scene.thread_02",
        "搬运中" => "movers.scene.carrying",
        "休息中" => "movers.scene.resting",
        "待搬运" => "movers.scene.to_move",
        "会话归位" => "movers.scene.delivered",
        "会话" => "movers.scene.crate",
        "先喝一口" => "movers.scene.note_1",
        "再说" => "movers.scene.note_2",
        "1 worker · no rush" => "movers.scene.workers",
        "正在慢慢搬会话…" => "movers.scene.status",
        "咖啡线程：持续运行中" => "movers.scene.coffee",
        _ => return None,
    })
}

/// The widest a translated text may be (scene units, before its own
/// transform) where the artwork leaves it little room: the crate label beside
/// its speech-bubble icon, the two lines of the tilted note. Wider
/// translations are drawn smaller.
pub fn text_max_width(source: &str) -> Option<f32> {
    match source.trim_end_matches(CUP).trim_end() {
        "会话" => Some(58.0),
        "先喝一口" | "再说" => Some(84.0),
        _ => None,
    }
}

/// U+2615 HOT BEVERAGE, drawn as a vector cup.
const CUP: char = match char::from_u32(0x2615) {
    Some(c) => c,
    None => ' ',
};

/// A rendered layer: its texture and the box it covers, in layer coordinates.
struct Raster {
    texture: TextureHandle,
    bounds: Rect,
}

/// The textures of the scene at one density.
#[derive(Default)]
pub struct SceneCache {
    density: f32,
    layers: Vec<Option<Raster>>,
    /// Rasterizations so far (tests: textures are not rebuilt per frame).
    pub renders: u32,
}

impl std::fmt::Debug for SceneCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneCache")
            .field("density", &self.density)
            .field("layers", &self.layers.len())
            .field("renders", &self.renders)
            .finish()
    }
}

impl SceneCache {
    /// Rasterize every layer for `density` (pixels per scene unit) unless the
    /// current textures are close enough.
    fn ensure(&mut self, ctx: &Context, scene: &Scene, density: f32) {
        if !self.layers.is_empty()
            && (density / self.density - 1.0).abs() <= RERENDER_RATIO
            && self.density > 0.0
        {
            return;
        }
        self.density = density;
        self.renders += 1;
        let started = std::time::Instant::now();
        self.layers = scene
            .layers
            .iter()
            .enumerate()
            .map(|(i, layer)| rasterize(ctx, layer, density, i))
            .collect();
        tracing::debug!(
            density,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "session migration scene rasterized"
        );
    }

    /// Drop the textures (the dialog closed).
    pub fn clear(&mut self) {
        self.layers.clear();
        self.density = 0.0;
    }
}

fn rasterize(ctx: &Context, layer: &Layer, density: f32, index: usize) -> Option<Raster> {
    let tree = usvg::Tree::from_str(&layer.svg, &usvg::Options::default()).ok()?;
    let b = tree.root().abs_stroke_bounding_box();
    // Pixel-aligned box with room for anti-aliasing.
    let x0 = (b.left() * density).floor() - 2.0;
    let y0 = (b.top() * density).floor() - 2.0;
    let x1 = (b.right() * density).ceil() + 2.0;
    let y1 = (b.bottom() * density).ceil() + 2.0;
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        Transform::from_row(density, 0.0, 0.0, density, -x0, -y0),
        &mut pixmap.as_mut(),
    );
    let image = ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data());
    let texture = ctx.load_texture(
        format!("superba-movers-{index}"),
        image,
        TextureOptions::LINEAR,
    );
    Some(Raster {
        texture,
        bounds: Rect::from_min_max(
            pos2(x0 / density, y0 / density),
            pos2(x1 / density, y1 / density),
        ),
    })
}

/// Where the scene is drawn: screen position of its origin and points per unit.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Screen position of the scene's (0, 0).
    pub origin: Pos2,
    /// Points per scene unit.
    pub scale: f32,
}

impl View {
    /// The largest view of a `w x h` scene inside `rect` (aspect kept, centred,
    /// origin on the pixel grid).
    pub fn fit(rect: Rect, size: (f32, f32), pixels_per_point: f32) -> Self {
        let scale = (rect.width() / size.0).min(rect.height() / size.1).max(0.0);
        let used = vec2(size.0, size.1) * scale;
        let origin = rect.center() - used / 2.0;
        let snap = |v: f32| (v * pixels_per_point).round() / pixels_per_point;
        Self {
            origin: pos2(snap(origin.x), snap(origin.y)),
            scale,
        }
    }

    /// A scene point on screen.
    pub fn to_screen(&self, x: f32, y: f32) -> Pos2 {
        self.origin + vec2(x, y) * self.scale
    }

    /// The scene's rectangle on screen.
    pub fn rect(&self, size: (f32, f32)) -> Rect {
        Rect::from_min_size(self.origin, vec2(size.0, size.1) * self.scale)
    }
}

/// Paint the scene in `pose` through `view`; `translate` turns a catalog key
/// into the display text.
pub fn paint(
    painter: &Painter,
    cache: &mut SceneCache,
    view: View,
    pose: &Pose,
    translate: &dyn Fn(&str) -> String,
) {
    let Some(scene) = scene() else {
        return;
    };
    paint_scene(painter, cache, scene, view, pose, translate);
}

/// Paint an analysed SVG with its own cache, sharing the migration layer renderer.
pub fn paint_scene(
    painter: &Painter,
    cache: &mut SceneCache,
    scene: &Scene,
    view: View,
    pose: &Pose,
    translate: &dyn Fn(&str) -> String,
) {
    let ctx = painter.ctx();
    let density = view.scale * ctx.pixels_per_point();
    if density <= 0.0 {
        return;
    }
    cache.ensure(ctx, scene, density);
    for (layer, raster) in scene.layers.iter().zip(&cache.layers) {
        let Some(raster) = raster else {
            continue;
        };
        let alpha = layer.place.opacity(pose);
        if alpha <= 0.002 {
            continue;
        }
        let m = layer.place.transform(pose);
        clipped_painter(painter, &layer.place, pose, view).add(quad(raster, m, view, alpha));
    }
    for text in &scene.texts {
        paint_text(painter, text, pose, view, translate);
    }
}

/// Clip in scene coordinates so a moving parcel disappears through the floor;
/// changing the clip never rebuilds its texture.
fn clipped_painter(painter: &Painter, place: &Placement, pose: &Pose, view: View) -> Painter {
    match place.clip_rect(pose) {
        Some(rect) => painter.with_clip_rect(Rect::from_min_max(
            view.to_screen(rect.left(), rect.top()),
            view.to_screen(rect.right(), rect.bottom()),
        )),
        None => painter.clone(),
    }
}

/// The texture of `raster` placed by `m` (layer -> scene) and `view`.
fn quad(raster: &Raster, m: Transform, view: View, alpha: f32) -> Shape {
    let b = raster.bounds;
    let corners = [
        (b.min.x, b.min.y, pos2(0.0, 0.0)),
        (b.max.x, b.min.y, pos2(1.0, 0.0)),
        (b.max.x, b.max.y, pos2(1.0, 1.0)),
        (b.min.x, b.max.y, pos2(0.0, 1.0)),
    ];
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    let color = Color32::from_rgba_premultiplied(a, a, a, a);
    let mut mesh = Mesh::with_texture(raster.texture.id());
    for (x, y, uv) in corners {
        let mut p = Point::from_xy(x, y);
        m.map_point(&mut p);
        mesh.vertices.push(Vertex {
            pos: view.to_screen(p.x, p.y),
            uv,
            color,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    Shape::mesh(mesh)
}

fn paint_text(
    painter: &Painter,
    item: &TextItem,
    pose: &Pose,
    view: View,
    translate: &dyn Fn(&str) -> String,
) {
    let painter = &clipped_painter(painter, &item.place, pose, view);
    let alpha = item.place.opacity(pose);
    if alpha <= 0.01 {
        return;
    }
    let m = item.place.transform(pose);
    let unit = (m.sx * m.sy - m.kx * m.ky).abs().sqrt();
    let angle = m.ky.atan2(m.sx);
    let k = unit * view.scale;
    let size = item.size * k;
    if size < 1.0 {
        return;
    }
    let text = match text_key(&item.text) {
        Some(key) => translate(key),
        None => item.text.clone(),
    };
    let cup = item.text.ends_with(CUP);
    let weight = match item.weight {
        0..=449 => Weight::Regular,
        450..=549 => Weight::Medium,
        _ => Weight::Semibold,
    };
    let family = if item.mono {
        fonts::mono(weight)
    } else {
        fonts::sans(weight)
    };
    let [r, g, b] = item.fill;
    let color = Color32::from_rgb(r, g, b).gamma_multiply(alpha);
    let mut job = LayoutJob::default();
    job.append(
        &text,
        0.0,
        TextFormat {
            font_id: FontId::new(size, family),
            color,
            extra_letter_spacing: item.letter_spacing * k,
            ..TextFormat::default()
        },
    );
    let mut galley = painter.layout_job(job.clone());
    let cup_w = if cup { size * 1.05 } else { 0.0 };
    let gap = if cup && !text.is_empty() {
        size * 0.3
    } else {
        0.0
    };
    let mut size = size;
    if let Some(max) = text_max_width(&item.text).map(|w| w * k) {
        let wide = galley.size().x + gap + cup_w;
        if wide > max {
            let fit = max / wide;
            size *= fit;
            for section in &mut job.sections {
                section.format.font_id.size *= fit;
                section.format.extra_letter_spacing *= fit;
            }
            galley = painter.layout_job(job);
        }
    }
    let (cup_w, gap) = (cup_w * size / (item.size * k), gap * size / (item.size * k));
    let baseline = galley
        .rows
        .first()
        .and_then(|row| {
            row.row
                .glyphs
                .first()
                .map(|g| row.pos.y + g.pos.y)
                .or(Some(row.pos.y + row.row.size.y * 0.8))
        })
        .unwrap_or(size * 0.8);
    let width = galley.size().x + gap + cup_w;
    let dx = match item.anchor {
        Anchor::Start => 0.0,
        Anchor::Middle => width / 2.0,
        Anchor::End => width,
    };
    let mut anchor = Point::from_xy(item.x, item.y);
    m.map_point(&mut anchor);
    let anchor = view.to_screen(anchor.x, anchor.y);
    let rot = egui::emath::Rot2::from_angle(angle);
    let top_left = anchor + rot * vec2(-dx, -baseline);
    let text_w = galley.size().x;
    painter.add(TextShape::new(top_left, Arc::clone(&galley), color).with_angle(angle));
    if cup {
        let center = anchor + rot * vec2(-dx + text_w + gap + cup_w / 2.0, -size * 0.36);
        paint_cup(painter, center, size, rot, color);
    }
}

/// A small coffee cup (the emoji the fonts lack) centred at `center`, about
/// `size` tall, rotated by `rot`.
fn paint_cup(painter: &Painter, center: Pos2, size: f32, rot: egui::emath::Rot2, color: Color32) {
    let at = |x: f32, y: f32| center + rot * (vec2(x, y) * size);
    let stroke = Stroke::new((size * 0.09).max(0.6), color);
    // Body: a cup narrowing to its foot.
    let body = vec![
        at(-0.36, -0.22),
        at(0.26, -0.22),
        at(0.2, 0.26),
        at(-0.3, 0.26),
    ];
    painter.add(Shape::convex_polygon(body, color, Stroke::NONE));
    // Handle.
    let handle: Vec<Pos2> = (0..=8)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i as f32) / 8.0;
            at(0.26 + 0.14 * a.cos(), 0.0 + 0.13 * a.sin())
        })
        .collect();
    painter.add(Shape::line(handle, stroke));
    // Saucer and a wisp of steam.
    painter.add(Shape::line_segment(
        [at(-0.46, 0.36), at(0.4, 0.36)],
        stroke,
    ));
    painter.add(Shape::line(
        vec![at(-0.06, -0.34), at(-0.12, -0.46), at(-0.04, -0.58)],
        stroke,
    ));
}

/// The scene's layers in `pose`, composited on the CPU at `density` pixels
/// per unit, without the texts (which egui draws): the same layers, transform
/// chains and opacities [`paint`] uses, for comparing the port with the SVG
/// played by a browser (`examples/movers_frames.rs`) and for tests.
pub fn composite(pose: &Pose, density: f32) -> Option<tiny_skia::Pixmap> {
    let scene = scene()?;
    let w = (scene.size.0 * density).round() as u32;
    let h = (scene.size.1 * density).round() as u32;
    let mut out = tiny_skia::Pixmap::new(w, h)?;
    for layer in &scene.layers {
        let alpha = layer.place.opacity(pose);
        if alpha <= 0.002 {
            continue;
        }
        let tree = usvg::Tree::from_str(&layer.svg, &usvg::Options::default()).ok()?;
        let m = Transform::from_scale(density, density).pre_concat(layer.place.transform(pose));
        let clip = layer.place.clip_rect(pose);
        if alpha >= 0.999 && clip.is_none() {
            resvg::render(&tree, m, &mut out.as_mut());
        } else {
            let mut tmp = tiny_skia::Pixmap::new(w, h)?;
            resvg::render(&tree, m, &mut tmp.as_mut());
            let paint = tiny_skia::PixmapPaint {
                opacity: alpha,
                ..tiny_skia::PixmapPaint::default()
            };
            let mut mask = None;
            if let Some(clip) = clip {
                let mut ground = tiny_skia::Mask::new(w, h)?;
                ground.fill_path(
                    &tiny_skia::PathBuilder::from_rect(clip),
                    tiny_skia::FillRule::Winding,
                    true,
                    Transform::from_scale(density, density),
                );
                mask = Some(ground);
            }
            out.draw_pixmap(
                0,
                0,
                tmp.as_ref(),
                &paint,
                Transform::identity(),
                mask.as_ref(),
            );
        }
    }
    Some(out)
}

/// Size of the scene (viewBox) in units.
pub fn size() -> (f32, f32) {
    scene().map_or((1440.0, 900.0), |s| s.size)
}

/// Placeholder shown when the scene cannot be drawn: the paper colour.
pub fn paint_placeholder(painter: &Painter, rect: Rect) {
    painter.rect_filled(rect, 16, Color32::from_rgb(0xF7, 0xF4, 0xEF));
}
