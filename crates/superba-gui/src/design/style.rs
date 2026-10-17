//! Maps the design tokens onto egui's global [`egui::Style`] (dark and light), so
//! the few stock widgets still in use (text edits, scroll bars, combo boxes,
//! tooltips, popups) match the custom ones.

use egui::{
    Color32, CornerRadius, FontId, Margin, Shadow, Stroke, Style, TextStyle, Theme, Visuals,
    style::{ScrollStyle, Selection, TextCursorStyle, WidgetVisuals, Widgets},
    vec2,
};

use super::fonts::{Weight, font, mono_font};
use super::tokens::{ANIM_SECS, DARK, LIGHT, Palette, radius, text};

fn widget(fill: Color32, stroke: Color32, fg: Color32) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::new(1.0, stroke),
        corner_radius: CornerRadius::same(radius::CONTROL),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    }
}

/// Visuals for one palette.
pub fn visuals(pal: &Palette) -> Visuals {
    let mut v = if pal.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.dark_mode = pal.dark;
    v.override_text_color = None;
    v.widgets = Widgets {
        noninteractive: widget(pal.surface, pal.border, pal.text),
        inactive: widget(pal.surface, pal.border, pal.text),
        hovered: widget(pal.hover, pal.border_strong, pal.text),
        active: widget(pal.active, pal.border_strong, pal.text),
        open: widget(pal.hover, pal.border_strong, pal.text),
    };
    v.selection = Selection {
        bg_fill: Color32::from_rgba_unmultiplied(
            pal.focus.r(),
            pal.focus.g(),
            pal.focus.b(),
            if pal.dark { 90 } else { 60 },
        ),
        stroke: Stroke::new(1.0, pal.text),
    };
    v.hyperlink_color = pal.link;
    v.faint_bg_color = pal.bg_subtle;
    v.extreme_bg_color = pal.bg;
    v.text_edit_bg_color = Some(pal.bg);
    v.code_bg_color = pal.hover;
    v.warn_fg_color = pal.amber;
    v.error_fg_color = pal.red;
    v.window_corner_radius = CornerRadius::same(radius::CARD);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 32,
        spread: 0,
        color: pal.shadow,
    };
    v.window_fill = pal.surface;
    v.window_stroke = Stroke::new(1.0, pal.border);
    v.menu_corner_radius = CornerRadius::same(radius::CONTROL + 2);
    v.panel_fill = pal.bg;
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 20,
        spread: 0,
        color: pal.shadow,
    };
    v.text_cursor = TextCursorStyle {
        stroke: Stroke::new(1.5, pal.text),
        ..TextCursorStyle::default()
    };
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.weak_text_color = Some(pal.text_muted);
    v
}

/// Full style for one palette.
pub fn style(pal: &Palette) -> Style {
    let mut s = Style {
        visuals: visuals(pal),
        ..Style::default()
    };
    s.text_styles = [
        (TextStyle::Small, font(text::CAPTION, Weight::Regular)),
        (TextStyle::Body, font(text::BODY, Weight::Regular)),
        (TextStyle::Button, font(text::BODY, Weight::Medium)),
        (TextStyle::Heading, font(text::H1, Weight::Semibold)),
        (
            TextStyle::Monospace,
            mono_font(text::SMALL, Weight::Regular),
        ),
    ]
    .into_iter()
    .collect::<std::collections::BTreeMap<TextStyle, FontId>>();
    s.spacing.item_spacing = vec2(8.0, 8.0);
    s.spacing.button_padding = vec2(12.0, 6.0);
    // Rows (`ui.horizontal`) use interact_size.y as their minimum height.
    s.spacing.interact_size = vec2(32.0, 22.0);
    s.spacing.window_margin = Margin::same(16);
    s.spacing.menu_margin = Margin::same(6);
    s.spacing.combo_height = 280.0;
    s.spacing.tooltip_width = 320.0;
    s.spacing.scroll = ScrollStyle {
        bar_width: 6.0,
        floating_width: 4.0,
        ..ScrollStyle::floating()
    };
    s.animation_time = ANIM_SECS;
    s.interaction.tooltip_delay = 0.35;
    s.interaction.selectable_labels = false;
    s.url_in_tooltip = false;
    s
}

/// Install the dark and light styles and select `theme` (`None` follows the OS),
/// and the focus-ring plugin (rings only for keyboard focus, never clipped; see
/// [`super::widgets::focus`]).
pub fn install(ctx: &egui::Context, theme: Option<Theme>) {
    super::widgets::focus::install(ctx);
    ctx.set_style_of(Theme::Dark, style(&DARK));
    ctx.set_style_of(Theme::Light, style(&LIGHT));
    ctx.set_theme(match theme {
        Some(Theme::Dark) => egui::ThemePreference::Dark,
        Some(Theme::Light) => egui::ThemePreference::Light,
        None => egui::ThemePreference::System,
    });
}
