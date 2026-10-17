//! Small, resolution-independent shapes for the moving scene.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

use crate::design::tokens::Palette;

pub(super) const CORAL: Color32 = Color32::from_rgb(255, 118, 92);
pub(super) const ROSE: Color32 = Color32::from_rgb(248, 61, 127);
const INK: Color32 = Color32::from_rgb(49, 40, 48);
const PAPER: Color32 = Color32::from_rgb(255, 248, 235);

/// All geometry uses a 720 × 280 scene, scaled once at the boundary.
pub(super) struct Canvas<'a> {
    pub painter: &'a Painter,
    pub origin: Pos2,
    pub scale: f32,
    pub opacity: f32,
}

impl Canvas<'_> {
    pub fn at(&self, x: f32, y: f32) -> Pos2 {
        self.origin + vec2(x, y) * self.scale
    }

    pub fn color(&self, color: Color32) -> Color32 {
        color.gamma_multiply(self.opacity)
    }

    pub fn line(&self, points: &[[f32; 2]], color: Color32, width: f32) {
        self.painter.add(Shape::line(
            points.iter().map(|p| self.at(p[0], p[1])).collect(),
            Stroke::new(width * self.scale, self.color(color)),
        ));
    }

    pub fn polygon(&self, points: &[[f32; 2]], color: Color32) {
        self.painter.add(Shape::convex_polygon(
            points.iter().map(|p| self.at(p[0], p[1])).collect(),
            self.color(color),
            Stroke::NONE,
        ));
    }

    pub fn rect(&self, min: [f32; 2], size: [f32; 2], radius: f32, color: Color32) {
        self.painter.rect_filled(
            Rect::from_min_size(self.at(min[0], min[1]), vec2(size[0], size[1]) * self.scale),
            (radius * self.scale).round() as u8,
            self.color(color),
        );
    }

    pub fn circle(&self, x: f32, y: f32, radius: f32, color: Color32) {
        self.painter
            .circle_filled(self.at(x, y), radius * self.scale, self.color(color));
    }
}

/// The reference mascot: curved segmented abdomen, coral head, eye and feelers.
/// `left` mirrors only the character, never the logos on the boxes.
pub(super) fn shrimp(c: &Canvas<'_>, origin: Pos2, scale: f32, left: bool, gait: f32) {
    let point = |x: f32, y: f32| {
        let x = if left { 70.0 - x } else { x };
        [origin.x + x * scale, origin.y + y * scale]
    };
    let polygon = |points: &[[f32; 2]], color| {
        c.polygon(
            &points.iter().map(|p| point(p[0], p[1])).collect::<Vec<_>>(),
            color,
        );
    };
    // Separate convex segments preserve the dark radial divisions at small sizes.
    for (points, color) in [
        ([[34.0, 17.0], [20.0, 24.0], [33.0, 44.0]], CORAL),
        (
            [[18.0, 25.0], [8.0, 36.0], [32.0, 45.0]],
            Color32::from_rgb(255, 105, 106),
        ),
        (
            [[7.0, 38.0], [5.0, 47.0], [32.0, 47.0]],
            Color32::from_rgb(255, 94, 117),
        ),
        (
            [[5.0, 49.0], [9.0, 61.0], [32.0, 49.0]],
            Color32::from_rgb(252, 79, 124),
        ),
        ([[10.0, 63.0], [22.0, 72.0], [33.0, 50.0]], ROSE),
        ([[24.0, 73.0], [34.0, 77.0], [34.0, 51.0]], ROSE),
        ([[36.0, 77.0], [57.0, 75.0], [36.0, 51.0]], ROSE),
    ] {
        polygon(&points, color);
    }
    polygon(&[[38.0, 49.0], [58.0, 56.0], [58.0, 73.0]], ROSE);
    polygon(
        &[
            [36.0, 16.0],
            [54.0, 12.0],
            [65.0, 16.0],
            [67.0, 23.0],
            [61.0, 37.0],
            [36.0, 46.0],
        ],
        CORAL,
    );
    for points in [
        [[11.0, 5.0 + gait], [59.0, 5.0], [65.0, 10.0]],
        [[25.0, 11.0 + gait * 0.5], [56.0, 11.0], [61.0, 15.0]],
    ] {
        c.line(&points.map(|p| point(p[0], p[1])), CORAL, 1.5 * scale);
    }
    let eye = point(50.0, 26.0);
    c.circle(eye[0], eye[1], 9.2 * scale, INK);
    c.circle(eye[0], eye[1], 7.7 * scale, PAPER);
    c.circle(eye[0], eye[1], 5.8 * scale, INK);
    let glint = point(52.0, 23.5);
    c.circle(glint[0], glint[1], 1.8 * scale, PAPER);
}

/// A helper peeks out from behind the old home's remaining wall.
pub(super) fn helper(c: &Canvas<'_>, peek: f32, carton: Pos2) {
    if peek <= 0.0 {
        return;
    }
    let painter = c.painter.with_clip_rect(
        Rect::from_min_max(c.at(124.0, 145.0), c.at(195.0, 200.0))
            .intersect(c.painter.clip_rect()),
    );
    let doorway = Canvas {
        painter: &painter,
        origin: c.origin,
        scale: c.scale,
        opacity: c.opacity,
    };
    let origin = pos2(74.0 + 33.0 * peek, 166.0);
    shrimp(&doorway, origin, 0.65, false, 0.0);

    let (hx, hy) = (origin.x + 34.0, origin.y + 6.0);
    let yellow = Color32::from_rgb(255, 200, 66);
    doorway.polygon(
        &[
            [hx - 12.0, hy],
            [hx - 11.0, hy - 7.0],
            [hx - 6.0, hy - 12.0],
            [hx + 5.0, hy - 12.0],
            [hx + 10.0, hy - 7.0],
            [hx + 12.0, hy],
        ],
        yellow,
    );
    doorway.rect([hx - 15.0, hy - 1.0], [30.0, 4.0], 1.5, yellow);
    doorway.line(&[[hx, hy - 10.0], [hx, hy - 2.0]], PAPER, 2.0);

    // The forearm follows the back of the carton as it slides outside.
    let hand = pos2(carton.x - 20.0, carton.y);
    let arm_painter = c.painter.with_clip_rect(
        Rect::from_min_max(c.at(124.0, 145.0), c.at(205.0, 237.0))
            .intersect(c.painter.clip_rect()),
    );
    let arm = Canvas {
        painter: &arm_painter,
        origin: c.origin,
        scale: c.scale,
        opacity: c.opacity * peek,
    };
    arm.line(
        &[
            [origin.x + 38.0, origin.y + 28.0],
            [origin.x + 35.0, origin.y + 40.0],
            [hand.x, hand.y],
        ],
        CORAL,
        2.2,
    );
    arm.circle(hand.x, hand.y, 2.3, CORAL);
}

/// A house with its right half dismantled, and an open-front new home.
pub(super) fn houses(c: &Canvas<'_>, pal: &Palette, breeze: f32) {
    let timber = if pal.dark {
        Color32::from_rgb(124, 113, 113)
    } else {
        Color32::from_rgb(166, 149, 145)
    };
    let wall = if pal.dark {
        Color32::from_rgb(52, 48, 54)
    } else {
        Color32::from_rgb(236, 225, 217)
    };
    let room = if pal.dark {
        Color32::from_rgb(49, 45, 49)
    } else {
        Color32::from_rgb(249, 237, 221)
    };
    // The route stays quiet; a few paving stones connect the two thresholds.
    c.line(&[[30.0, 238.0], [695.0, 238.0]], pal.border, 1.0);
    for x in [254.0, 303.0, 352.0, 401.0, 450.0] {
        c.rect([x, 241.0], [27.0, 2.0], 1.0, pal.border);
    }

    c.rect([49.0, 122.0], [75.0, 113.0], 2.0, wall);
    c.polygon(
        &[[46.0, 120.0], [120.0, 62.0], [149.0, 84.0], [112.0, 118.0]],
        wall,
    );
    c.line(&[[39.0, 125.0], [119.0, 61.0], [151.0, 86.0]], timber, 5.0);
    // Exposed rafters stop short of the missing roof and wall.
    c.line(&[[120.0, 65.0], [120.0, 235.0]], timber, 4.0);
    c.line(&[[162.0, 103.0], [162.0, 235.0]], timber, 4.0);
    c.line(&[[122.0, 116.0], [190.0, 116.0]], timber, 3.0);
    c.line(&[[146.0, 91.0], [187.0, 122.0]], timber, 3.0);
    c.line(&[[124.0, 137.0], [158.0, 182.0]], timber, 2.0);
    c.rect([66.0, 142.0], [34.0, 36.0], 2.0, pal.bg);
    c.line(&[[67.0, 143.0], [99.0, 177.0]], timber, 2.0);
    c.line(
        &[
            [100.0, 138.0],
            [109.0, 151.0],
            [103.0, 164.0],
            [111.0, 179.0],
        ],
        timber,
        1.2,
    );
    c.line(&[[57.0, 194.0], [65.0, 204.0], [60.0, 216.0]], timber, 1.2);
    c.polygon(
        &[
            [175.0, 222.0],
            [213.0, 234.0],
            [211.0, 239.0],
            [173.0, 227.0],
        ],
        timber,
    );
    c.rect([35.0, 232.0], [18.0, 6.0], 1.0, wall);
    c.rect([65.0, 233.0], [12.0, 5.0], 1.0, timber);

    // A cutaway with a visible back wall, floor and side jambs: boxes go inside.
    c.rect([505.0, 116.0], [176.0, 121.0], 3.0, room);
    c.polygon(&[[505.0, 117.0], [593.0, 61.0], [681.0, 117.0]], room);
    c.rect([515.0, 126.0], [156.0, 106.0], 2.0, pal.bg_subtle);
    c.polygon(
        &[
            [515.0, 222.0],
            [671.0, 222.0],
            [681.0, 238.0],
            [505.0, 238.0],
        ],
        wall,
    );
    c.line(&[[492.0, 120.0], [593.0, 52.0], [694.0, 120.0]], CORAL, 6.0);
    c.line(&[[505.0, 125.0], [505.0, 236.0]], CORAL, 4.0);
    c.line(&[[681.0, 125.0], [681.0, 236.0]], CORAL, 4.0);
    c.rect([583.0, 83.0], [21.0, 24.0], 10.0, pal.surface);
    c.line(&[[593.5, 86.0], [593.5, 104.0]], CORAL, 1.4);
    c.line(&[[585.0, 95.0], [602.0, 95.0]], CORAL, 1.4);
    c.line(&[[500.0, 239.0], [687.0, 239.0]], CORAL, 3.0);
    // Rotate the stem and leaves together around the root; the pot stays put.
    let (sin, cos) = breeze.sin_cos();
    let leaf = |[x, y]: [f32; 2]| {
        let (dx, dy) = (x - 703.0, y - 224.0);
        [703.0 + dx * cos - dy * sin, 224.0 + dx * sin + dy * cos]
    };
    c.line(&[[703.0, 224.0], leaf([703.0, 203.0])], pal.green, 1.5);
    for points in [
        [[703.0, 215.0], [691.0, 205.0], [700.0, 204.0]],
        [[703.0, 212.0], [713.0, 200.0], [705.0, 201.0]],
    ] {
        c.polygon(&points.map(leaf), pal.green);
    }
    c.rect([696.0, 222.0], [14.0, 16.0], 3.0, timber);
}

/// A sealed carton. The cream sticker covers exactly the old mark's footprint.
pub(super) fn parcel(c: &Canvas<'_>, center: Pos2, sticker: f32) {
    let (x, y) = (center.x, center.y);
    let kraft = Color32::from_rgb(208, 165, 118);
    c.rect([x - 20.0, y - 16.0], [40.0, 32.0], 1.5, kraft);
    c.polygon(
        &[
            [x - 20.0, y - 16.0],
            [x - 14.0, y - 23.0],
            [x + 26.0, y - 23.0],
            [x + 20.0, y - 16.0],
        ],
        Color32::from_rgb(231, 191, 142),
    );
    c.polygon(
        &[
            [x + 20.0, y - 16.0],
            [x + 26.0, y - 23.0],
            [x + 26.0, y + 9.0],
            [x + 20.0, y + 16.0],
        ],
        Color32::from_rgb(174, 129, 90),
    );
    c.line(&[[x, y - 16.0], [x + 6.0, y - 23.0]], PAPER, 4.0);
    c.rect([x - 2.0, y - 16.0], [4.0, 6.0], 0.0, PAPER);
    // Codex's terminal mark, drawn as geometry so even tiny boxes stay sharp.
    c.rect([x - 12.0, y - 10.0], [24.0, 21.0], 4.0, INK);
    c.line(
        &[[x - 7.0, y - 5.0], [x - 2.0, y], [x - 7.0, y + 5.0]],
        PAPER,
        1.8,
    );
    c.line(&[[x + 1.0, y + 5.0], [x + 7.0, y + 5.0]], PAPER, 1.8);
    if sticker > 0.0 {
        let s = sticker.clamp(0.0, 1.0);
        let top = y - 10.5 - (1.0 - s) * 7.0;
        let width = 25.0 * s;
        let rect = Rect::from_min_size(c.at(x - width / 2.0, top), vec2(width, 22.0) * c.scale);
        c.painter.rect(
            rect,
            (2.0 * c.scale).round() as u8,
            c.color(PAPER),
            Stroke::new(0.6 * c.scale, c.color(kraft)),
            StrokeKind::Inside,
        );
        if s > 0.1 {
            shrimp(c, pos2(x - 8.0 * s, top + 1.0), 0.25 * s, false, 0.0);
        }
    }
}
