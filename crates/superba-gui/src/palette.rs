//! The Cmd/Ctrl+K command palette: a floating search box over the window that
//! lists actions (switch line or profile, restore, open a Codex settings key,
//! toggle the router, go to a page) gathered from the shell and the page modules.
//!
//! Matching is a case-insensitive AND of the query's words over the title, detail
//! and keywords; titles that start with the query rank first. Arrow keys move the
//! selection, Enter runs it, Escape or a click on the backdrop closes the palette.

use egui::{
    Align, Align2, Frame, Id, Key, KeyboardShortcut, Layout, Margin, Modal, Modifiers, Rect,
    RichText, ScrollArea, Sense, Shadow, Stroke, TextEdit, Ui, pos2, vec2,
};

use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};
use crate::design::widgets::{Text, kbd};
use crate::i18n::Tr;
use crate::pages::{Action, PaletteGroup, PaletteItem};

/// The shortcut that opens and closes the palette (Cmd+K on macOS, Ctrl+K elsewhere).
pub const SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::K);

/// Most rows shown at once.
pub const MAX_RESULTS: usize = 60;

/// Palette state.
#[derive(Debug, Default)]
pub struct CommandPalette {
    open: bool,
    query: String,
    selected: usize,
    focus: bool,
    scroll_to_selected: bool,
    settings_only: bool,
}

/// Rank of `item` for `query` (`None` = no match; lower is better).
pub fn score(query: &str, item: &PaletteItem) -> Option<u32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(1000);
    }
    let title = item.title.to_lowercase();
    let mut haystack = title.clone();
    if let Some(d) = &item.detail {
        haystack.push(' ');
        haystack.push_str(&d.to_lowercase());
    }
    for k in &item.keywords {
        haystack.push(' ');
        haystack.push_str(&k.to_lowercase());
    }
    if !query.split_whitespace().all(|w| haystack.contains(w)) {
        return None;
    }
    Some(if title.starts_with(&query) {
        0
    } else if title.split_whitespace().any(|w| w.starts_with(&query)) {
        1
    } else if let Some(pos) = title.find(&query) {
        2 + pos.min(50) as u32
    } else if item.keywords.iter().any(|k| k.to_lowercase() == query) {
        60
    } else {
        100
    })
}

/// Filter and order `items` for `query`: grouped by [`PaletteGroup`], best match
/// first inside a group when there is a query, otherwise in the given order.
pub fn filter(query: &str, items: Vec<PaletteItem>) -> Vec<PaletteItem> {
    let mut scored: Vec<(PaletteGroup, u32, usize, PaletteItem)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, item)| score(query, &item).map(|s| (item.group, s, i, item)))
        .collect();
    if query.trim().is_empty() {
        scored.sort_by_key(|(g, _, i, _)| (*g, *i));
    } else {
        // Groups keep their order; the best score of a group decides nothing more.
        scored.sort_by_key(|(g, s, i, _)| (*g, *s, *i));
    }
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, _, _, item)| item)
        .collect()
}

impl CommandPalette {
    /// Whether the palette is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open with an empty query.
    pub fn open(&mut self) {
        self.open = true;
        self.settings_only = false;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
    }

    /// Open a search restricted to settings, even with an empty query.
    pub fn open_settings(&mut self) {
        self.open();
        self.settings_only = true;
    }

    /// Open with `query` typed in (tests, screenshot tooling).
    pub fn open_with(&mut self, query: &str) {
        self.open();
        self.query = query.to_owned();
    }

    /// Close.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Open or close.
    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// Current query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Draw the palette over everything; returns the chosen action.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        tr: &Tr,
        items: Vec<PaletteItem>,
    ) -> Option<Action> {
        if !self.open {
            return None;
        }
        let pal = Palette::of(ctx);
        let items = if self.settings_only {
            items
                .into_iter()
                .filter(|item| item.group == PaletteGroup::Settings)
                .collect()
        } else {
            items
        };
        let mut results = filter(&self.query, items);
        if self.selected >= results.len() {
            self.selected = results.len().saturating_sub(1);
        }
        // Keys are read before the text edit so arrows never move its cursor.
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if esc {
            self.close();
            return None;
        }
        if up && self.selected > 0 {
            self.selected -= 1;
            self.scroll_to_selected = true;
        }
        if down && self.selected + 1 < results.len() {
            self.selected += 1;
            self.scroll_to_selected = true;
        }
        let mut chosen: Option<usize> = enter.then_some(self.selected);

        let screen = ctx.content_rect();
        let width = (screen.width() - 48.0).clamp(320.0, 620.0);
        let top = (screen.height() * 0.14).clamp(48.0, 120.0);
        let frame = Frame::new()
            .fill(pal.surface)
            .stroke(Stroke::new(1.0, pal.border_strong))
            .corner_radius(radius::CARD)
            .inner_margin(Margin::ZERO)
            .shadow(Shadow {
                offset: [0, 16],
                blur: 48,
                spread: 0,
                color: pal.shadow,
            });
        let area = Modal::default_area(Id::unique("command-palette"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, top));
        let query_before = self.query.clone();
        let resp = Modal::new(Id::unique("command-palette"))
            .area(area)
            .backdrop_color(pal.backdrop.gamma_multiply(0.6))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                self.search_row(ui, tr, pal);
                divider(ui, pal);
                let max_h = (screen.height() - top - 140.0).clamp(160.0, 380.0);
                if results.is_empty() {
                    ui.add_space(28.0);
                    ui.vertical_centered(|ui| {
                        ui.label(Text::small(tr.t("palette.empty"), pal));
                    });
                    ui.add_space(28.0);
                } else {
                    ScrollArea::vertical()
                        .id_salt("palette-results")
                        .max_height(max_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add_space(6.0);
                            let mut last: Option<PaletteGroup> = None;
                            for (i, item) in results.iter().enumerate() {
                                if last != Some(item.group) {
                                    if last.is_some() {
                                        ui.add_space(4.0);
                                    }
                                    ui.horizontal(|ui| {
                                        ui.set_min_height(24.0);
                                        ui.add_space(18.0);
                                        ui.label(Text::caption(
                                            tr.t(if self.settings_only {
                                                "superba.tab.settings"
                                            } else {
                                                item.group.caption_key()
                                            }),
                                            pal,
                                        ));
                                    });
                                    last = Some(item.group);
                                }
                                let r = row(ui, pal, item, i == self.selected);
                                if r.hovered()
                                    && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
                                {
                                    self.selected = i;
                                }
                                if i == self.selected && self.scroll_to_selected {
                                    r.scroll_to_me(None);
                                    self.scroll_to_selected = false;
                                }
                                if r.clicked() {
                                    chosen = Some(i);
                                }
                            }
                            ui.add_space(6.0);
                        });
                }
                divider(ui, pal);
                footer(ui, tr, pal);
            });
        if self.query != query_before {
            self.selected = 0;
        }
        if resp.backdrop_response.clicked() {
            self.close();
            return None;
        }
        let action = chosen
            .filter(|i| *i < results.len())
            .map(|i| results.swap_remove(i).action);
        if action.is_some() {
            self.close();
        }
        action
    }

    fn search_row(&mut self, ui: &mut Ui, tr: &Tr, pal: &Palette) {
        let h = 48.0;
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(rect.min.x + 26.0, rect.center().y), vec2(16.0, 16.0)),
            Icon::Search,
            pal.text_subtle,
        );
        let esc_w = 34.0;
        let edit_rect = Rect::from_min_max(
            pos2(rect.min.x + 44.0, rect.min.y),
            pos2(rect.max.x - esc_w - 20.0, rect.max.y),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(edit_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let edit = TextEdit::singleline(&mut self.query)
            .id(Id::unique("command-palette-query"))
            .frame(Frame::NONE)
            .font(font(text::BODY + 1.0, Weight::Regular))
            .text_color(pal.text)
            .hint_text(
                RichText::new(tr.t("palette.placeholder"))
                    .font(font(text::BODY + 1.0, Weight::Regular))
                    .color(pal.text_subtle),
            )
            .desired_width(edit_rect.width())
            .align(Align2::LEFT_CENTER)
            .min_size(edit_rect.size());
        let r = child.add(edit).accessible_name(tr.t("palette.placeholder"));
        if self.focus {
            r.request_focus();
            self.focus = false;
        }
        let mut esc = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(rect.max.x - esc_w - 16.0, rect.min.y),
                    pos2(rect.max.x - 12.0, rect.max.y),
                ))
                .layout(Layout::left_to_right(Align::Center)),
        );
        kbd(&mut esc, "Esc");
    }
}

fn divider(ui: &mut Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, pal.border));
}

fn row(ui: &mut Ui, pal: &Palette, item: &PaletteItem, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let inner = rect.shrink2(vec2(8.0, 1.0));
    if selected {
        ui.painter()
            .rect_filled(inner, radius::CONTROL + 2, pal.hover);
    }
    let left = inner.min.x + 10.0;
    let detail_w = item.detail.as_ref().map_or(0.0, |d| {
        ui.painter()
            .layout_no_wrap(
                d.clone(),
                font(text::SMALL, Weight::Regular),
                pal.text_subtle,
            )
            .size()
            .x
            .min(inner.width() * 0.45)
    });
    let title_w =
        (inner.width() - 20.0 - detail_w - if detail_w > 0.0 { 16.0 } else { 0.0 }).max(40.0);
    paint_line(
        ui,
        pos2(left, inner.center().y),
        Align2::LEFT_CENTER,
        &item.title,
        title_w,
        font(text::BODY, Weight::Regular),
        if selected {
            pal.text
        } else {
            pal.text_muted.lerp_to_gamma(pal.text, 0.6)
        },
    );
    if let Some(d) = &item.detail {
        paint_line(
            ui,
            pos2(inner.max.x - 10.0, inner.center().y),
            Align2::RIGHT_CENTER,
            d,
            detail_w,
            font(text::SMALL, Weight::Regular),
            pal.text_subtle,
        );
    }
    let title = item.title.clone();
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::ListItem, true, selected, &title));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn paint_line(
    ui: &Ui,
    at: egui::Pos2,
    align: Align2,
    s: &str,
    max_w: f32,
    font_id: egui::FontId,
    color: egui::Color32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(s.to_owned(), font_id, color);
    job.wrap.max_width = max_w.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    let rect = align.anchor_size(at, galley.size());
    ui.painter().galley(rect.min, galley, color);
}

fn footer(ui: &mut Ui, tr: &Tr, pal: &Palette) {
    ui.horizontal(|ui| {
        ui.set_min_height(36.0);
        ui.add_space(16.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        kbd(ui, "↑↓");
        ui.label(Text::caption(tr.t("palette.hint.move"), pal));
        ui.add_space(8.0);
        kbd(ui, "↵");
        ui.label(Text::caption(tr.t("palette.hint.run"), pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(16.0);
            ui.label(Text::caption(tr.t("palette.hint.toggle"), pal));
            kbd(ui, crate::shell::palette_shortcut_label());
        });
    });
}
