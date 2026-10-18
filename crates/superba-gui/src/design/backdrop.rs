//! The window background (user request 24, after Surge): a soft tinted gradient
//! behind the sidebar and the content, in one of several colour themes
//! ([`Background`], Preferences → Appearance), each with a light and a dark
//! variant. Pages sit on it in one rounded content card ([`content_card`], user
//! bug 85); the Overview lays its cards directly on it ([`card_frame`]).
//!
//! The current background lives in the egui context ([`set`] / [`current`]), so
//! widgets read it the way they read the palette ([`Backdrop::of`]).

use egui::{
    Color32, Context, CornerRadius, Frame, Id, Margin, Mesh, Painter, Rect, Shadow, Stroke,
};

pub use superba_core::Background;

use super::tokens::{Palette, chrome, hex, radius};

/// Colours of one background theme in one mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Backdrop {
    /// The theme.
    pub background: Background,
    /// Dark mode.
    pub dark: bool,
    /// Top-left corner of the window gradient.
    pub top: Color32,
    /// Bottom-right corner of the window gradient.
    pub bottom: Color32,
    /// The soft light spot at the top right of the gradient.
    pub glow: Color32,
    /// Selected sidebar row.
    pub selected: Color32,
    /// Hovered sidebar row.
    pub hover: Color32,
    /// Fill of floating cards and of the content panel.
    pub card: Color32,
    /// Hairline around floating cards (visible in dark mode only).
    pub card_border: Color32,
    /// Soft shadow under floating cards and the panel.
    pub shadow: Color32,
    /// Chart series: bars and the first sparkline; the second sparkline.
    pub chart: [Color32; 2],
    /// Swatch shown in Preferences (the theme's mid tone).
    pub swatch: Color32,
}

/// `(top, bottom, glow, chart bars, chart line)` of a light theme.
fn light(bg: Background) -> (u32, u32, u32, u32, u32) {
    match bg {
        Background::Neutral => (0xF2F3F6, 0xE6E8ED, 0xFBFBFD, 0x4AA8E6, 0x6366F1),
        Background::Mint => (0xEDF6F1, 0xDCEEE5, 0xF8FCFA, 0x2FAE84, 0x14958F),
        Background::Sky => (0xEDF3FB, 0xDCE7F5, 0xF8FBFF, 0x3D93E6, 0x5865F2),
        Background::Sand => (0xF7F3EB, 0xECE4D5, 0xFDFBF7, 0xD39B4A, 0xB7794A),
        Background::Lavender => (0xF3F1FA, 0xE5E0F3, 0xFBFAFE, 0x9575E6, 0x6F55D9),
    }
}

/// `(top, bottom, glow, chart bars, chart line)` of a dark theme.
fn dark(bg: Background) -> (u32, u32, u32, u32, u32) {
    match bg {
        Background::Neutral => (0x18191B, 0x111214, 0x1D1E21, 0x52B2EE, 0x8487F7),
        Background::Mint => (0x13201B, 0x0C1411, 0x172620, 0x3CC495, 0x2FB5AE),
        Background::Sky => (0x121C2A, 0x0C121B, 0x17233A, 0x54A6F0, 0x7A84F7),
        Background::Sand => (0x221D14, 0x16130D, 0x29231A, 0xE0AC5E, 0xC98E5C),
        Background::Lavender => (0x1D172A, 0x130F1B, 0x231C33, 0xA58BF0, 0x8C74EB),
    }
}

impl Backdrop {
    /// The colours of `background` in light or dark mode.
    pub fn new(background: Background, dark_mode: bool) -> Backdrop {
        let (top, bottom, glow, bars, line) = if dark_mode {
            dark(background)
        } else {
            light(background)
        };
        let (top, bottom, glow) = (hex(top), hex(bottom), hex(glow));
        let mid = top.lerp_to_gamma(bottom, 0.5);
        if dark_mode {
            Backdrop {
                background,
                dark: dark_mode,
                top,
                bottom,
                glow,
                selected: Color32::from_white_alpha(20),
                hover: Color32::from_white_alpha(10),
                card: super::tokens::DARK.surface,
                card_border: Color32::from_white_alpha(14),
                shadow: Color32::from_black_alpha(70),
                chart: [hex(bars), hex(line)],
                swatch: mid.lerp_to_gamma(hex(bars), 0.35),
            }
        } else {
            Backdrop {
                background,
                dark: dark_mode,
                top,
                bottom,
                glow,
                selected: Color32::from_black_alpha(16),
                hover: Color32::from_black_alpha(9),
                card: super::tokens::LIGHT.surface,
                card_border: Color32::TRANSPARENT,
                shadow: Color32::from_black_alpha(12),
                chart: [hex(bars), hex(line)],
                swatch: mid.lerp_to_gamma(hex(bars), 0.25),
            }
        }
    }

    /// The backdrop of the theme active in `ctx` with the current background.
    pub fn of(ctx: &Context) -> Backdrop {
        Backdrop::new(current(ctx), Palette::of(ctx).dark)
    }

    /// A translucent tint of a chart colour (area under a sparkline).
    pub fn area(color: Color32, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    }
}

fn key() -> Id {
    Id::unique("superba-backdrop-background")
}

/// Make `background` the current window background.
pub fn set(ctx: &Context, background: Background) {
    if current(ctx) != background {
        ctx.data_mut(|d| d.insert_temp(key(), background));
        ctx.request_repaint();
    }
}

/// The current window background (default until [`set`]).
pub fn current(ctx: &Context) -> Background {
    ctx.data(|d| d.get_temp(key())).unwrap_or_default()
}

/// Paint the window gradient over `rect`: top-left to bottom-right, with a soft
/// light spot at the top right.
pub fn paint(painter: &Painter, rect: Rect, b: &Backdrop) {
    // A 3 x 3 grid of vertices, bilinear between the corners, the top-right
    // vertices lifted toward the glow colour.
    let mut mesh = Mesh::default();
    let xs = [rect.min.x, rect.center().x, rect.max.x];
    let ys = [rect.min.y, rect.center().y, rect.max.y];
    for (j, y) in ys.iter().enumerate() {
        for (i, x) in xs.iter().enumerate() {
            let t = (i + j) as f32 / 4.0;
            let mut c = b.top.lerp_to_gamma(b.bottom, t);
            if j == 0 && i >= 1 {
                c = c.lerp_to_gamma(b.glow, if i == 2 { 0.9 } else { 0.45 });
            } else if j == 1 && i == 2 {
                c = c.lerp_to_gamma(b.glow, 0.35);
            }
            mesh.colored_vertex(egui::pos2(*x, *y), c);
        }
    }
    for j in 0..2u32 {
        for i in 0..2u32 {
            let a = j * 3 + i;
            mesh.add_triangle(a, a + 1, a + 4);
            mesh.add_triangle(a, a + 4, a + 3);
        }
    }
    painter.add(mesh);
}

/// The soft drop shadow of a floating surface.
pub fn shadow(b: &Backdrop) -> Shadow {
    Shadow {
        offset: [0, 2],
        blur: 14,
        spread: 0,
        color: b.shadow,
    }
}

/// Paint the large rounded content panel at `rect` (pages other than the
/// Overview draw inside it).
pub fn panel(painter: &Painter, rect: Rect, b: &Backdrop) {
    let r = CornerRadius::same(radius::PANEL);
    painter.add(shadow(b).as_shape(rect, r));
    painter.rect(
        rect,
        r,
        b.card,
        Stroke::new(1.0, b.card_border),
        egui::StrokeKind::Inside,
    );
}

/// The frame of a card floating on the backdrop: card fill, [`radius::PANEL`],
/// soft shadow, `padding` inside.
pub fn card_frame(b: &Backdrop, padding: i8) -> Frame {
    Frame::new()
        .fill(b.card)
        .stroke(Stroke::new(1.0, b.card_border))
        .corner_radius(radius::PANEL)
        .shadow(shadow(b))
        .inner_margin(Margin::same(padding))
}

/// i18n key of a background's name.
pub fn name_key(background: Background) -> &'static str {
    match background {
        Background::Neutral => "settings.background.neutral",
        Background::Mint => "settings.background.mint",
        Background::Sky => "settings.background.sky",
        Background::Sand => "settings.background.sand",
        Background::Lavender => "settings.background.lavender",
    }
}

/// The content card inside the central area `central` (right of the sidebar):
/// [`chrome::CARD_INSET`] from the window's right and bottom edges, at
/// [`chrome::CARD_TOP`] (below the title band where there is one), flush with
/// the sidebar on the left. Page rectangles ([`super::layout::page_rect`]) lie
/// inside it.
pub fn panel_rect(central: Rect) -> Rect {
    let m = chrome::CARD_INSET;
    Rect::from_min_max(
        egui::pos2(central.min.x, central.min.y + chrome::CARD_TOP),
        egui::pos2(central.max.x - m, central.max.y - m),
    )
}

/// The shadow of the content card: soft, wide and faint, lifting the card off
/// the window background without a visible edge (a pearl hairline outlines it
/// instead, [`content_card`]). None in dark mode, where the card is lighter
/// than the background and its hairline is enough.
pub fn content_shadow(b: &Backdrop) -> Shadow {
    if b.dark {
        Shadow::NONE
    } else {
        Shadow {
            offset: [0, 3],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(18),
        }
    }
}

/// The content card's hairline: a faint pearl-gray line in light mode (the
/// app icon's tile edge, #DFE6F0), a faint light one in dark mode.
pub fn content_border(b: &Backdrop) -> Color32 {
    if b.dark {
        Color32::from_white_alpha(18)
    } else {
        Color32::from_rgba_unmultiplied(0x9A, 0xA8, 0xBC, 46)
    }
}

/// Paint the content card at `rect` (pages other than the Overview draw
/// inside it): card fill, [`radius::CONTENT`], [`content_shadow`],
/// [`content_border`].
pub fn content_card(painter: &Painter, rect: Rect, b: &Backdrop) {
    let r = CornerRadius::same(radius::CONTENT);
    let shadow = content_shadow(b);
    if shadow.color != Color32::TRANSPARENT {
        painter.add(shadow.as_shape(rect, r));
    }
    painter.rect(
        rect,
        r,
        b.card,
        Stroke::new(1.0, content_border(b)),
        egui::StrokeKind::Inside,
    );
}
