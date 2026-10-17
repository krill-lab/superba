//! Design tokens: the Geist-derived color palettes (dark first, light), spacing,
//! radii, sizes and motion.
//!
//! Color rules (plan 11.2):
//! - neutral grays carry almost everything (backgrounds, 1 px borders, text);
//! - the accent blue is used only for keyboard focus rings and links;
//! - green / amber / red appear only on status dots, the countdown bar state and
//!   destructive buttons.
//!
//! Text colors are chosen so that body text reaches at least 4.5:1 and secondary
//! text at least 3:1 against every surface they are drawn on; the tests assert this
//! with [`contrast_ratio`].

use egui::Color32;

/// Convert a `0xRRGGBB` literal to an opaque color.
pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// All colors of one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// True for the dark palette.
    pub dark: bool,
    /// Content background (Geist background-100).
    pub bg: Color32,
    /// Sunken / secondary background: sidebar, card footers (background-200).
    pub bg_subtle: Color32,
    /// Card and dialog surface.
    pub surface: Color32,
    /// Hovered row / ghost button (gray-100).
    pub hover: Color32,
    /// Pressed row, segmented-control track (gray-200).
    pub active: Color32,
    /// 1 px borders and dividers (gray-400).
    pub border: Color32,
    /// Stronger border for hovered inputs (gray-500).
    pub border_strong: Color32,
    /// Primary text (gray-1000).
    pub text: Color32,
    /// Secondary text (gray-900).
    pub text_muted: Color32,
    /// Tertiary text: placeholders, captions (gray-700).
    pub text_subtle: Color32,
    /// Disabled text.
    pub text_disabled: Color32,
    /// Keyboard focus ring.
    pub focus: Color32,
    /// Links.
    pub link: Color32,
    /// Status: success / available.
    pub green: Color32,
    /// Status: warning / waiting / paused.
    pub amber: Color32,
    /// Status: error / destructive.
    pub red: Color32,
    /// Neutral status dot (idle / unknown).
    pub gray_dot: Color32,
    /// Primary button fill (white in dark mode, near-black in light mode).
    pub primary: Color32,
    /// Primary button fill while hovered.
    pub primary_hover: Color32,
    /// Text on the primary button.
    pub on_primary: Color32,
    /// Text on red (danger) buttons.
    pub on_danger: Color32,
    /// Backdrop behind modal dialogs.
    pub backdrop: Color32,
    /// Drop shadow color for floating surfaces.
    pub shadow: Color32,
}

/// Dark palette (default). Content panels and cards are an elevated graphite on a
/// darker tinted window background (`design::backdrop`), like macOS dark mode.
pub const DARK: Palette = Palette {
    dark: true,
    bg: hex(0x202020),
    bg_subtle: hex(0x191919),
    surface: hex(0x202020),
    hover: hex(0x2B2B2B),
    active: hex(0x313131),
    border: hex(0x363636),
    border_strong: hex(0x4B4B4B),
    text: hex(0xEDEDED),
    text_muted: hex(0xA6A6A6),
    text_subtle: hex(0x8F8F8F),
    text_disabled: hex(0x5C5C5C),
    focus: hex(0x0A72EF),
    link: hex(0x52A8FF),
    green: hex(0x45A557),
    amber: hex(0xF5A623),
    red: hex(0xE5484D),
    gray_dot: hex(0x5E5E5E),
    primary: hex(0xEDEDED),
    primary_hover: hex(0xFFFFFF),
    on_primary: hex(0x0A0A0A),
    on_danger: hex(0xFFFFFF),
    backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 160),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 140),
};

/// Light palette.
pub const LIGHT: Palette = Palette {
    dark: false,
    bg: hex(0xFFFFFF),
    bg_subtle: hex(0xFAFAFA),
    surface: hex(0xFFFFFF),
    hover: hex(0xF2F2F2),
    active: hex(0xEBEBEB),
    border: hex(0xE6E6E6),
    border_strong: hex(0xC9C9C9),
    text: hex(0x171717),
    text_muted: hex(0x5E5E5E),
    text_subtle: hex(0x7D7D7D),
    text_disabled: hex(0xB0B0B0),
    focus: hex(0x0070F3),
    link: hex(0x0062D1),
    green: hex(0x2E9D48),
    amber: hex(0xE59700),
    red: hex(0xDA2F35),
    gray_dot: hex(0xB4B4B4),
    primary: hex(0x171717),
    primary_hover: hex(0x383838),
    on_primary: hex(0xFFFFFF),
    on_danger: hex(0xFFFFFF),
    backdrop: Color32::from_rgba_premultiplied(0, 0, 0, 90),
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 34),
};

impl Palette {
    /// The palette for an egui theme.
    pub fn for_theme(theme: egui::Theme) -> &'static Palette {
        match theme {
            egui::Theme::Dark => &DARK,
            egui::Theme::Light => &LIGHT,
        }
    }

    /// The palette of the theme currently active in `ctx`.
    pub fn of(ctx: &egui::Context) -> &'static Palette {
        Self::for_theme(ctx.theme())
    }

    /// Color of a status.
    pub fn status(&self, status: crate::design::widgets::Status) -> Color32 {
        use crate::design::widgets::Status;
        match status {
            Status::Ok => self.green,
            Status::Warn => self.amber,
            Status::Error => self.red,
            Status::Idle => self.gray_dot,
            Status::Busy => self.text_muted,
        }
    }

    /// A translucent tint of `color` for subtle badge backgrounds.
    pub fn tint(&self, color: Color32) -> Color32 {
        let alpha = if self.dark { 38 } else { 26 };
        Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
    }
}

/// Spacing scale (8 px grid with a 4 px half step).
pub mod space {
    /// 4 px.
    pub const XS: f32 = 4.0;
    /// 8 px.
    pub const S: f32 = 8.0;
    /// 12 px.
    pub const M: f32 = 12.0;
    /// 16 px.
    pub const L: f32 = 16.0;
    /// 24 px.
    pub const XL: f32 = 24.0;
    /// 32 px.
    pub const XXL: f32 = 32.0;
    /// 48 px.
    pub const XXXL: f32 = 48.0;
}

/// Corner radii (soft, macOS-like: user request 24 after Surge).
pub mod radius {
    /// Controls: inputs, segmented controls, badges.
    pub const CONTROL: u8 = 8;
    /// Inner radius of a segment inside a segmented control.
    pub const INNER: u8 = 6;
    /// Sidebar rows and list rows with a selection fill.
    pub const ROW: u8 = 10;
    /// Cards, dialogs, toasts, banners.
    pub const CARD: u8 = 16;
    /// Cards floating on the window background (Overview) and the large
    /// content panel of the other pages.
    pub const PANEL: u8 = 20;
    /// Fully rounded (pill buttons, dots).
    pub const PILL: u8 = 255;
}

/// Geometry of the keyboard focus ring (Geist: 2 px ring, 2 px gap).
pub mod focus {
    /// Space between a widget's edge and its ring.
    pub const GAP: f32 = 2.0;
    /// Ring stroke width.
    pub const WIDTH: f32 = 2.0;
    /// How far an outside ring reaches beyond its widget. The layout helpers let
    /// their panes paint this far past their rectangles (into the gaps and page
    /// padding), so a widget flush with a pane edge keeps its whole ring.
    pub const BLEED: f32 = GAP + WIDTH;
}

/// Fixed sizes from plan 11.1.
pub mod size {
    /// Left navigation width.
    pub const SIDEBAR: f32 = 220.0;
    /// Maximum content width of a Scroll page.
    pub const CONTENT_MAX: f32 = 960.0;
    /// Maximum content width of a Fit page (996 px at 1280, 1156 px at 1440).
    pub const CONTENT_WIDE: f32 = 1200.0;
    /// Maximum width of the floating switch overlay.
    pub const OVERLAY_MAX: f32 = 680.0;
    /// Minimum window size.
    pub const MIN_WINDOW: [f32; 2] = [960.0, 640.0];
    /// Default window size.
    pub const DEFAULT_WINDOW: [f32; 2] = [1280.0, 800.0];
    /// Space above the page header.
    pub const PAGE_TOP: f32 = 28.0;
    /// Space below a Fit page.
    pub const PAGE_BOTTOM: f32 = 16.0;
    /// Height of the page header with its divider and spacing (`pages::page_header`).
    pub const PAGE_HEADER: f32 = 78.0;
    /// Below this body height (header excluded) a Fit page scrolls as a whole.
    pub const FIT_MIN_BODY: f32 = 560.0;
    /// Two-line list rows (Settings list).
    pub const ROW_LIST2: f32 = 52.0;
    /// Two-line table rows (Models).
    pub const ROW_TABLE2: f32 = 44.0;
    /// One-line table rows.
    pub const ROW: f32 = 32.0;
    /// Table header row.
    pub const TABLE_HEADER: f32 = 28.0;
    /// Footer of a Fit page (pager and pending bar).
    pub const FOOTER: f32 = 40.0;
    /// Small control height.
    pub const CONTROL_SM: f32 = 28.0;
    /// Default control height.
    pub const CONTROL: f32 = 32.0;
    /// Large control height.
    pub const CONTROL_LG: f32 = 40.0;
    /// Icon size inside controls and the navigation.
    pub const ICON: f32 = 16.0;
    /// Sidebar row height (Surge-like, comfortable).
    pub const NAV_ROW: f32 = 40.0;
    /// Icon size in sidebar rows.
    pub const NAV_ICON: f32 = 18.0;
    /// Space between the window edge and the content panel (top, right, bottom).
    pub const PANEL_MARGIN: f32 = 8.0;
}

/// Type scale (points).
pub mod text {
    /// Captions, badges, table headers.
    pub const CAPTION: f32 = 12.0;
    /// Secondary body text.
    pub const SMALL: f32 = 13.0;
    /// Body text and controls.
    pub const BODY: f32 = 14.0;
    /// Flat section headings.
    pub const SECTION: f32 = 15.0;
    /// Card titles.
    pub const TITLE: f32 = 16.0;
    /// Section headings.
    pub const H2: f32 = 20.0;
    /// Page titles.
    pub const H1: f32 = 24.0;
    /// Large numbers (latency, counts).
    pub const DISPLAY: f32 = 28.0;
    /// Large page titles (Overview, after Surge's "活动").
    pub const TITLE_XL: f32 = 30.0;
    /// Values of the header facts row (provider, line, auth, speed).
    pub const FACT: f32 = 17.0;
    /// KPI numbers on dashboard cards ("97" of "97 ms").
    pub const KPI: f32 = 36.0;
    /// Smaller KPI numbers (secondary figures inside a card).
    pub const KPI_SMALL: f32 = 22.0;
    /// The unit next to a KPI number ("ms" of "97 ms").
    pub const KPI_UNIT: f32 = 15.0;
}

/// Motion: every transition takes 150 ms.
pub const ANIM_SECS: f32 = 0.15;

/// Toast lifetime in seconds (plan 7.3: 4 s).
pub const TOAST_SECS: f64 = 4.0;

/// WCAG 2.x contrast ratio between two opaque colors (1.0 ..= 21.0).
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    fn channel(c: u8) -> f32 {
        let c = f32::from(c) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    fn luminance(c: Color32) -> f32 {
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
