//! Line icons painted with the egui painter (no icon font is bundled).
//!
//! Outline style after SF Symbols / Surge (user request 24): geometry on the
//! Lucide icon grid (24 x 24 units), a stroke of 2 grid units (1.5 px at the 18 px
//! sidebar size, never thinner than 1.2 px) with round caps and round joins,
//! scaled to the target rectangle, so icons stay crisp at any size and take the
//! text color.

use std::f32::consts::{PI, TAU};

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, pos2, vec2};

/// Available icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Superba identity.
    Superba,
    /// Tool configuration.
    Tools,
    /// Notifications.
    Bell,
    /// Backups.
    Archive,
    /// Dashboard tiles (overview page).
    Overview,
    /// Globe (lines and sites page).
    Lines,
    /// Monitor (targets page).
    Targets,
    /// Stacked layers (sessions page).
    Sessions,
    /// Pulse line (diagnostics page).
    Diagnostics,
    /// Sliders (settings page).
    Settings,
    /// Check mark.
    Check,
    /// Cross.
    Close,
    /// Chevron pointing right.
    ChevronRight,
    /// Chevron pointing down.
    ChevronDown,
    /// Circular arrow (refresh / probe again).
    Refresh,
    /// Open eye (reveal a key).
    Eye,
    /// Crossed eye (hide a key).
    EyeOff,
    /// Arrow leaving a box (external link).
    External,
    /// Two overlapping squares (copy).
    Copy,
    /// Plus.
    Plus,
    /// Warning triangle.
    Warning,
    /// Info circle.
    Info,
    /// Question mark in a circle.
    Help,
    /// Terminal prompt.
    Terminal,
    /// Server rack.
    Server,
    /// Key.
    Key,
    /// Lightning bolt (hot switch).
    Bolt,
    /// Counter-clockwise arrow (restore).
    Restore,
    /// Pause bars.
    Pause,
    /// Play triangle.
    Play,
    /// Arrow pointing right.
    ArrowRight,
    /// A right arrow over a left arrow (traffic routed through this computer).
    ArrowRightLeft,
    /// Shield (safe mode).
    Shield,
    /// Plug (connection page).
    Plug,
    /// Bar chart (usage page).
    Chart,
    /// Splitting route (router page).
    Route,
    /// Cog (preferences page).
    Gear,
    /// Magnifier (command palette).
    Search,
    /// Up and down arrows (reorder).
    Reorder,
    /// Arrow pointing up.
    ArrowUp,
    /// Arrow pointing down.
    ArrowDown,
    /// A cube (models page).
    Model,
    /// An arrow into a tray (updates page).
    Download,
    /// Three dots in a row (a "more" menu).
    More,
    /// A pulse line in a rounded square (activity).
    Activity,
    /// A speedometer (latency).
    Gauge,
    /// Two speech bubbles (sessions, conversations).
    Chat,
    /// An app window with a title bar (the desktop app).
    AppWindow,
    /// A terminal window with a prompt (Codex processes).
    Process,
    /// Two overlapping colour cards (background theme).
    Swatch,
    /// A coin (cost).
    Coin,
    /// A stethoscope-like probe: a magnifier over a pulse (network diagnosis).
    Probe,
    /// A cloud with an arrow (upstream / provider line).
    Cloud,
}

impl Icon {
    /// Every icon (tests paint each one).
    pub const ALL: [Icon; 53] = [
        Icon::Superba,
        Icon::Tools,
        Icon::Bell,
        Icon::Archive,
        Icon::Overview,
        Icon::Lines,
        Icon::Targets,
        Icon::Sessions,
        Icon::Diagnostics,
        Icon::Settings,
        Icon::Check,
        Icon::Close,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::Refresh,
        Icon::Eye,
        Icon::EyeOff,
        Icon::External,
        Icon::Copy,
        Icon::Plus,
        Icon::Warning,
        Icon::Info,
        Icon::Help,
        Icon::Terminal,
        Icon::Server,
        Icon::Key,
        Icon::Bolt,
        Icon::Restore,
        Icon::Pause,
        Icon::Play,
        Icon::ArrowRight,
        Icon::ArrowRightLeft,
        Icon::Shield,
        Icon::Plug,
        Icon::Chart,
        Icon::Route,
        Icon::Gear,
        Icon::Search,
        Icon::Reorder,
        Icon::ArrowUp,
        Icon::ArrowDown,
        Icon::Model,
        Icon::Download,
        Icon::More,
        Icon::Activity,
        Icon::Gauge,
        Icon::Chat,
        Icon::AppWindow,
        Icon::Process,
        Icon::Swatch,
        Icon::Coin,
        Icon::Probe,
        Icon::Cloud,
    ];
}

/// Map 24-unit icon coordinates into `rect`.
struct Grid {
    rect: Rect,
    scale: f32,
}

impl Grid {
    fn new(rect: Rect) -> Self {
        let side = rect.width().min(rect.height());
        let rect = Rect::from_center_size(rect.center(), vec2(side, side));
        Self {
            rect,
            scale: side / 24.0,
        }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.rect.min.x + x * self.scale,
            self.rect.min.y + y * self.scale,
        )
    }

    fn len(&self, v: f32) -> f32 {
        v * self.scale
    }
}

/// Points of a circular arc (angles in radians, clockwise in screen space).
fn arc(g: &Grid, cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let steps = ((a1 - a0).abs() / (PI / 18.0)).ceil().max(2.0) as usize;
    (0..=steps)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            g.p(cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Stroke width of an icon drawn on `g`: 2 grid units (1.5 px at 18 px), at
/// least 1.2 px.
fn stroke_width(g: &Grid) -> f32 {
    g.len(2.0).max(1.2)
}

/// Round caps and joins: a dot of the stroke's width on every vertex of a short
/// polyline (egui strokes have butt caps and miter joins).
fn round_vertices(painter: &Painter, points: &[Pos2], stroke: Stroke) {
    if points.len() > 8 {
        // Arcs: their joins are flat already; cap only the two ends.
        for p in [points[0], points[points.len() - 1]] {
            painter.circle_filled(p, stroke.width / 2.0, stroke.color);
        }
        return;
    }
    for p in points {
        painter.circle_filled(*p, stroke.width / 2.0, stroke.color);
    }
}

/// Paint `icon` inside `rect` with `color`. The stroke is 1.5 px at the 18 px
/// sidebar size and scales with the icon; caps and joins are round.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let g = Grid::new(rect);
    let width = stroke_width(&g);
    let stroke = Stroke::new(width, color);
    let line = |pts: &[(f32, f32)]| {
        let points: Vec<Pos2> = pts.iter().map(|&(x, y)| g.p(x, y)).collect();
        round_vertices(painter, &points, stroke);
        painter.add(Shape::line(points, stroke));
    };
    let closed = |pts: &[(f32, f32)]| {
        let points: Vec<Pos2> = pts.iter().map(|&(x, y)| g.p(x, y)).collect();
        round_vertices(painter, &points, stroke);
        painter.add(Shape::closed_line(points, stroke));
    };
    let rrect = |x: f32, y: f32, w: f32, h: f32, r: f32| {
        let rect = Rect::from_min_max(g.p(x, y), g.p(x + w, y + h));
        painter.rect_stroke(
            rect,
            g.len(r).round() as u8,
            stroke,
            egui::StrokeKind::Middle,
        );
    };
    let circle = |cx: f32, cy: f32, r: f32| {
        painter.circle_stroke(g.p(cx, cy), g.len(r), stroke);
    };
    let dot = |cx: f32, cy: f32| {
        painter.circle_filled(g.p(cx, cy), width * 0.75, color);
    };
    let path = |pts: Vec<Pos2>| {
        round_vertices(painter, &pts, stroke);
        painter.add(Shape::line(pts, stroke));
    };

    match icon {
        Icon::Superba => brand(painter, rect, color),
        Icon::Tools => {
            line(&[
                (5.0, 20.0),
                (14.0, 11.0),
                (18.0, 11.0),
                (21.0, 8.0),
                (18.0, 8.0),
                (16.0, 6.0),
                (16.0, 3.0),
                (13.0, 6.0),
                (13.0, 10.0),
                (4.0, 19.0),
            ]);
        }
        Icon::Bell => {
            line(&[
                (5.0, 17.0),
                (7.0, 14.0),
                (7.0, 8.0),
                (9.0, 5.0),
                (15.0, 5.0),
                (17.0, 8.0),
                (17.0, 14.0),
                (19.0, 17.0),
                (5.0, 17.0),
            ]);
            line(&[(10.0, 21.0), (14.0, 21.0)]);
        }
        Icon::Archive => {
            rrect(3.0, 3.0, 18.0, 5.0, 1.0);
            rrect(5.0, 8.0, 14.0, 13.0, 1.0);
            line(&[(10.0, 12.0), (14.0, 12.0)]);
        }

        Icon::Overview => {
            rrect(3.5, 3.5, 7.0, 7.0, 2.5);
            rrect(13.5, 3.5, 7.0, 7.0, 2.5);
            rrect(3.5, 13.5, 7.0, 7.0, 2.5);
            rrect(13.5, 13.5, 7.0, 7.0, 2.5);
        }
        Icon::Lines => {
            circle(12.0, 12.0, 10.0);
            line(&[(2.0, 12.0), (22.0, 12.0)]);
            // Meridian: an ellipse of half-width 4 approximated by two arcs.
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = -PI / 2.0 + PI * i as f32 / 24.0;
                    g.p(12.0 + 4.2 * a.cos(), 12.0 + 10.0 * a.sin())
                })
                .collect();
            path(pts);
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = PI / 2.0 + PI * i as f32 / 24.0;
                    g.p(12.0 + 4.2 * a.cos(), 12.0 + 10.0 * a.sin())
                })
                .collect();
            path(pts);
        }
        Icon::Targets => {
            rrect(2.0, 3.0, 20.0, 14.0, 2.0);
            line(&[(8.0, 21.0), (16.0, 21.0)]);
            line(&[(12.0, 17.0), (12.0, 21.0)]);
        }
        Icon::Sessions => {
            closed(&[(12.0, 2.5), (21.5, 7.0), (12.0, 11.5), (2.5, 7.0)]);
            line(&[(2.5, 12.0), (12.0, 16.5), (21.5, 12.0)]);
            line(&[(2.5, 17.0), (12.0, 21.5), (21.5, 17.0)]);
        }
        Icon::Diagnostics => {
            line(&[
                (22.0, 12.0),
                (18.0, 12.0),
                (15.0, 21.0),
                (9.0, 3.0),
                (6.0, 12.0),
                (2.0, 12.0),
            ]);
        }
        Icon::Settings => {
            line(&[(21.0, 4.0), (14.0, 4.0)]);
            line(&[(10.0, 4.0), (3.0, 4.0)]);
            line(&[(21.0, 12.0), (12.0, 12.0)]);
            line(&[(8.0, 12.0), (3.0, 12.0)]);
            line(&[(21.0, 20.0), (16.0, 20.0)]);
            line(&[(12.0, 20.0), (3.0, 20.0)]);
            line(&[(14.0, 2.0), (14.0, 6.0)]);
            line(&[(8.0, 10.0), (8.0, 14.0)]);
            line(&[(16.0, 18.0), (16.0, 22.0)]);
        }
        Icon::Check => line(&[(20.0, 6.0), (9.0, 17.0), (4.0, 12.0)]),
        Icon::Close => {
            line(&[(18.0, 6.0), (6.0, 18.0)]);
            line(&[(6.0, 6.0), (18.0, 18.0)]);
        }
        Icon::ChevronRight => line(&[(9.0, 18.0), (15.0, 12.0), (9.0, 6.0)]),
        Icon::ChevronDown => line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        Icon::Refresh => {
            path(arc(&g, 12.0, 12.0, 8.5, -PI * 0.42, PI * 1.45));
            line(&[(15.5, 3.0), (17.2, 5.3), (14.6, 6.9)]);
        }
        Icon::Restore => {
            path(arc(&g, 12.0, 12.0, 8.5, -PI * 0.58, -PI * 2.45 + TAU * 0.0));
            line(&[(8.5, 3.0), (6.8, 5.3), (9.4, 6.9)]);
            line(&[(12.0, 8.0), (12.0, 12.0), (15.0, 14.0)]);
        }
        Icon::Eye | Icon::EyeOff => {
            let upper: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    g.p(2.0 + 20.0 * t, 12.0 - 7.0 * (PI * t).sin())
                })
                .collect();
            let lower: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    g.p(2.0 + 20.0 * t, 12.0 + 7.0 * (PI * t).sin())
                })
                .collect();
            path(upper);
            path(lower);
            circle(12.0, 12.0, 3.0);
            if icon == Icon::EyeOff {
                line(&[(3.0, 3.0), (21.0, 21.0)]);
            }
        }
        Icon::External => {
            line(&[(15.0, 3.0), (21.0, 3.0), (21.0, 9.0)]);
            line(&[(10.0, 14.0), (21.0, 3.0)]);
            line(&[
                (18.0, 13.0),
                (18.0, 21.0),
                (3.0, 21.0),
                (3.0, 6.0),
                (11.0, 6.0),
            ]);
        }
        Icon::Copy => {
            rrect(8.0, 8.0, 14.0, 14.0, 2.0);
            line(&[
                (4.0, 16.0),
                (2.0, 16.0),
                (2.0, 2.0),
                (16.0, 2.0),
                (16.0, 4.0),
            ]);
        }
        Icon::Plus => {
            line(&[(12.0, 5.0), (12.0, 19.0)]);
            line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::Warning => {
            closed(&[(12.0, 3.0), (22.0, 20.5), (2.0, 20.5)]);
            line(&[(12.0, 9.5), (12.0, 13.5)]);
            dot(12.0, 17.0);
        }
        Icon::Info => {
            circle(12.0, 12.0, 10.0);
            line(&[(12.0, 16.0), (12.0, 11.5)]);
            dot(12.0, 8.0);
        }
        Icon::Help => {
            circle(12.0, 12.0, 10.0);
            line(&[
                (8.5, 8.5),
                (9.5, 6.5),
                (12.0, 6.0),
                (14.5, 7.0),
                (15.0, 9.5),
                (12.0, 12.0),
                (12.0, 14.0),
            ]);
            dot(12.0, 17.5);
        }
        Icon::Terminal => {
            line(&[(4.0, 17.0), (10.0, 11.0), (4.0, 5.0)]);
            line(&[(12.0, 19.0), (20.0, 19.0)]);
        }
        Icon::Server => {
            rrect(2.0, 2.0, 20.0, 8.0, 2.0);
            rrect(2.0, 14.0, 20.0, 8.0, 2.0);
            dot(6.0, 6.0);
            dot(6.0, 18.0);
        }
        Icon::Key => {
            circle(7.5, 15.5, 5.0);
            line(&[(11.0, 12.0), (21.0, 2.0)]);
            line(&[(15.5, 7.5), (18.5, 10.5)]);
            line(&[(18.5, 4.5), (21.0, 7.0)]);
        }
        Icon::Bolt => closed(&[
            (13.0, 2.0),
            (3.0, 14.0),
            (12.0, 14.0),
            (11.0, 22.0),
            (21.0, 10.0),
            (12.0, 10.0),
        ]),
        Icon::Pause => {
            line(&[(9.0, 5.0), (9.0, 19.0)]);
            line(&[(15.0, 5.0), (15.0, 19.0)]);
        }
        Icon::Play => closed(&[(7.0, 4.0), (19.0, 12.0), (7.0, 20.0)]),
        Icon::ArrowRight => {
            line(&[(5.0, 12.0), (19.0, 12.0)]);
            line(&[(13.0, 6.0), (19.0, 12.0), (13.0, 18.0)]);
        }
        Icon::ArrowRightLeft => {
            line(&[(4.0, 7.0), (20.0, 7.0)]);
            line(&[(15.5, 2.5), (20.0, 7.0), (15.5, 11.5)]);
            line(&[(20.0, 17.0), (4.0, 17.0)]);
            line(&[(8.5, 12.5), (4.0, 17.0), (8.5, 21.5)]);
        }
        Icon::Shield => {
            let mut pts: Vec<Pos2> = vec![g.p(12.0, 22.0)];
            pts.extend(
                [
                    (4.0, 18.0),
                    (4.0, 6.0),
                    (12.0, 2.5),
                    (20.0, 6.0),
                    (20.0, 18.0),
                ]
                .iter()
                .map(|&(x, y)| g.p(x, y)),
            );
            painter.add(Shape::closed_line(pts, stroke));
        }
        Icon::Plug => {
            line(&[(9.0, 2.0), (9.0, 7.0)]);
            line(&[(15.0, 2.0), (15.0, 7.0)]);
            closed(&[
                (6.0, 7.0),
                (18.0, 7.0),
                (18.0, 11.0),
                (16.0, 15.0),
                (8.0, 15.0),
                (6.0, 11.0),
            ]);
            line(&[(12.0, 15.0), (12.0, 22.0)]);
        }
        Icon::Chart => {
            line(&[(3.0, 3.0), (3.0, 21.0), (21.0, 21.0)]);
            line(&[(8.0, 17.0), (8.0, 13.0)]);
            line(&[(13.0, 17.0), (13.0, 6.0)]);
            line(&[(18.0, 17.0), (18.0, 10.0)]);
        }
        Icon::Model => {
            closed(&[
                (12.0, 2.5),
                (20.5, 7.0),
                (20.5, 17.0),
                (12.0, 21.5),
                (3.5, 17.0),
                (3.5, 7.0),
            ]);
            line(&[(3.5, 7.0), (12.0, 11.5), (20.5, 7.0)]);
            line(&[(12.0, 11.5), (12.0, 21.5)]);
        }
        Icon::More => {
            circle(5.0, 12.0, 1.2);
            circle(12.0, 12.0, 1.2);
            circle(19.0, 12.0, 1.2);
        }
        Icon::Activity => {
            rrect(2.5, 3.5, 19.0, 17.0, 4.0);
            line(&[
                (6.0, 12.5),
                (8.8, 12.5),
                (10.6, 8.0),
                (13.4, 16.5),
                (15.2, 12.0),
                (18.0, 12.0),
            ]);
        }
        Icon::Gauge => {
            path(arc(&g, 12.0, 14.0, 9.0, PI * 0.82, PI * 2.18));
            line(&[(12.0, 14.0), (16.0, 9.5)]);
            dot(12.0, 14.0);
        }
        Icon::Chat => {
            // Back bubble (top right) and front bubble with a tail.
            line(&[
                (9.0, 6.5),
                (9.0, 5.5),
                (10.5, 3.5),
                (19.5, 3.5),
                (21.0, 5.5),
            ]);
            line(&[(21.0, 5.5), (21.0, 11.0), (19.5, 13.0), (18.0, 13.0)]);
            closed(&[
                (4.5, 7.5),
                (14.5, 7.5),
                (16.0, 9.0),
                (16.0, 15.5),
                (14.5, 17.0),
                (9.0, 17.0),
                (5.5, 20.5),
                (5.5, 17.0),
                (4.5, 17.0),
                (3.0, 15.5),
                (3.0, 9.0),
            ]);
        }
        Icon::AppWindow => {
            rrect(2.5, 4.0, 19.0, 16.0, 3.5);
            line(&[(2.5, 9.0), (21.5, 9.0)]);
            dot(5.5, 6.5);
            dot(8.0, 6.5);
        }
        Icon::Process => {
            rrect(2.5, 4.0, 19.0, 16.0, 3.5);
            line(&[(6.5, 9.0), (9.5, 12.0), (6.5, 15.0)]);
            line(&[(12.0, 15.0), (16.5, 15.0)]);
        }
        Icon::Swatch => {
            rrect(3.0, 8.0, 11.0, 13.0, 2.5);
            line(&[
                (8.0, 8.0),
                (8.0, 5.5),
                (10.0, 3.5),
                (19.0, 3.5),
                (21.0, 5.5),
            ]);
            line(&[(21.0, 5.5), (21.0, 14.5), (19.0, 16.5), (14.0, 16.5)]);
            dot(8.5, 17.0);
        }
        Icon::Coin => {
            circle(12.0, 12.0, 9.5);
            line(&[(12.0, 5.5), (12.0, 18.5)]);
            // An S: over the top to the middle, then round the bottom.
            path(arc(&g, 12.0, 9.6, 2.7, -PI * 0.2, -PI * 1.5));
            path(arc(&g, 12.0, 14.6, 2.7, -PI * 0.5, PI * 0.8));
        }
        Icon::Probe => {
            circle(10.5, 10.5, 7.0);
            line(&[(15.5, 15.5), (21.0, 21.0)]);
            line(&[
                (6.0, 10.5),
                (8.0, 10.5),
                (9.5, 7.8),
                (11.5, 13.2),
                (13.0, 10.5),
                (15.0, 10.5),
            ]);
        }
        Icon::Cloud => {
            let mut pts = arc(&g, 7.5, 14.5, 4.5, PI * 0.5, PI * 1.5);
            pts.extend(arc(&g, 12.0, 9.5, 5.5, PI * 1.05, PI * 1.95));
            pts.extend(arc(&g, 17.0, 14.0, 5.0, -PI * 0.5, PI * 0.5));
            pts.push(g.p(7.5, 19.0));
            path(pts);
        }
        Icon::Download => {
            line(&[(12.0, 3.0), (12.0, 15.0)]);
            line(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]);
            line(&[
                (3.0, 15.0),
                (3.0, 19.0),
                (5.0, 21.0),
                (19.0, 21.0),
                (21.0, 19.0),
                (21.0, 15.0),
            ]);
        }
        Icon::Route => {
            circle(5.0, 12.0, 2.5);
            circle(19.0, 5.0, 2.5);
            circle(19.0, 19.0, 2.5);
            line(&[(7.5, 12.0), (11.0, 12.0)]);
            line(&[(11.0, 12.0), (16.5, 5.6)]);
            line(&[(11.0, 12.0), (16.5, 18.4)]);
        }
        Icon::Gear => {
            circle(12.0, 12.0, 3.0);
            circle(12.0, 12.0, 7.0);
            for i in 0..8 {
                let a = i as f32 * TAU / 8.0;
                line(&[
                    (12.0 + 7.0 * a.cos(), 12.0 + 7.0 * a.sin()),
                    (12.0 + 9.8 * a.cos(), 12.0 + 9.8 * a.sin()),
                ]);
            }
        }
        Icon::Search => {
            circle(10.5, 10.5, 6.5);
            line(&[(15.5, 15.5), (21.0, 21.0)]);
        }
        Icon::Reorder => {
            line(&[(7.0, 9.0), (12.0, 4.0), (17.0, 9.0)]);
            line(&[(7.0, 15.0), (12.0, 20.0), (17.0, 15.0)]);
        }
        Icon::ArrowUp => {
            line(&[(12.0, 19.0), (12.0, 5.0)]);
            line(&[(6.0, 11.0), (12.0, 5.0), (18.0, 11.0)]);
        }
        Icon::ArrowDown => {
            line(&[(12.0, 5.0), (12.0, 19.0)]);
            line(&[(6.0, 13.0), (12.0, 19.0), (18.0, 13.0)]);
        }
    }
}

/// Paint a spinner (a 270° arc rotating once per second) inside `rect`.
pub fn spinner(painter: &Painter, rect: Rect, color: Color32, time: f64) {
    let g = Grid::new(rect);
    let start = (time.fract() as f32) * TAU;
    let width = (g.len(2.0) * 0.85).max(1.2);
    let track = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 50);
    painter.circle_stroke(g.p(12.0, 12.0), g.len(9.0), Stroke::new(width, track));
    painter.add(Shape::line(
        arc(&g, 12.0, 12.0, 9.0, start, start + PI * 1.2),
        Stroke::new(width, color),
    ));
}

/// Paint the existing Superba silhouette, tinted by the surrounding UI.
fn brand(painter: &Painter, rect: Rect, color: Color32) {
    const PNG: &[u8] = include_bytes!("../../../../assets/icon/tray/superba-tray-template-36.png");
    let id = egui::Id::unique("superba-monochrome-mark");
    let cached = painter
        .ctx()
        .data(|d| d.get_temp::<egui::TextureHandle>(id));
    let texture = cached.unwrap_or_else(|| {
        let mut icon = eframe::icon_data::from_png_bytes(PNG).expect("bundled brand image");
        for pixel in icon.rgba.as_chunks_mut::<4>().0 {
            pixel[..3].fill(255);
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        );
        let texture = painter.ctx().load_texture(
            "superba-monochrome-mark",
            image,
            egui::TextureOptions::LINEAR,
        );
        painter
            .ctx()
            .data_mut(|d| d.insert_temp(id, texture.clone()));
        texture
    });
    painter.image(
        texture.id(),
        Grid::new(rect).rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        color,
    );
}
