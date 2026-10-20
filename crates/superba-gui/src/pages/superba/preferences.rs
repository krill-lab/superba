//! One scrollable settings page; the original section bodies stay in their modules.
use super::{Action, Cx, SuperbaSection, appearance, general, network};
use crate::design::{
    a11y,
    widgets::{Text, focus_ring, show_focus},
};
use egui::{Align, Rect, Sense, Ui, pos2, vec2};
use superba_core::Settings;

pub(super) const SECTIONS: [SuperbaSection; 4] = [
    SuperbaSection::General,
    SuperbaSection::Appearance,
    SuperbaSection::Network,
    SuperbaSection::Notify,
];

#[derive(Debug)]
pub(super) struct State {
    pub active: SuperbaSection,
    pub jump: Option<SuperbaSection>,
    /// The inline settings filter.
    pub filter: String,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active: SuperbaSection::General,
            jump: None,
            filter: String::new(),
        }
    }
}

pub(super) fn toolbar(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    if ui.available_width() < 860.0 {
        search_entry(ui, cx, state);
        jumps(ui, cx, state);
    } else {
        ui.horizontal(|ui| {
            search_entry(ui, cx, state);
            let width = jump_width(ui, cx);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.allocate_ui_with_layout(
                    vec2(width, 32.0),
                    egui::Layout::left_to_right(Align::Center),
                    |ui| jumps(ui, cx, state),
                );
            });
        });
    }
    ui.add_space(4.0);
}

/// The inline settings filter: rows whose name, explanation or key do not
/// contain the typed words are hidden (the command palette is gone, user
/// request 118).
fn search_entry(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let width = 280.0_f32.min(ui.available_width());
    crate::design::widgets::SearchField::new(
        &mut state.filter,
        "superba-settings-filter",
        cx.tr.t("superba.search"),
        cx.tr.t("action.clear_search"),
    )
    .width(width)
    .height(32.0)
    .show(ui);
}

/// Whether `query`'s words all occur in one of `fields` (case-insensitive).
pub fn filter_matches(query: &str, fields: &[&str]) -> bool {
    let hay = fields.join(" ").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|w| hay.contains(w))
}

fn jump_width(ui: &Ui, cx: &Cx<'_>) -> f32 {
    let font = crate::design::fonts::font(13.0, crate::design::fonts::Weight::Regular);
    let width = |text: String| {
        ui.painter()
            .layout_no_wrap(text, font.clone(), cx.pal.text)
            .size()
            .x
    };
    SECTIONS
        .into_iter()
        .map(|s| width(cx.tr.t(s.title_key())) + 24.0)
        .sum::<f32>()
}

fn jumps(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for section in SECTIONS {
            let label = cx.tr.t(section.title_key());
            let selected = state.active == section;
            let galley = ui.painter().layout_no_wrap(
                label.clone(),
                crate::design::fonts::font(13.0, crate::design::fonts::Weight::Regular),
                cx.pal.text,
            );
            let (rect, response) =
                ui.allocate_exact_size(vec2(galley.size().x + 16.0, 30.0), Sense::click());
            if selected || response.hovered() {
                ui.painter().rect_filled(
                    rect,
                    6,
                    if selected {
                        cx.pal.active
                    } else {
                        cx.pal.hover
                    },
                );
            }
            ui.painter().galley(
                pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
                galley,
                cx.pal.text,
            );
            // A tab, so UI Automation exposes SelectionItem.IsSelected for the
            // section in view (a button has no selected state there; alpha.8
            // Windows acceptance, D6).
            a11y::selectable(&response, egui::Role::Tab, &label, selected);
            if show_focus(&response) {
                focus_ring(ui.painter(), rect, 6, cx.pal);
            }
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                state.jump = Some(section);
            }
        }
    });
}

pub(super) fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    settings: &mut Settings,
    notify: &mut crate::pages::notify::State,
    out: &mut Vec<Action>,
) {
    let viewport_top = ui.clip_rect().top();
    let mut active = SuperbaSection::General;
    // A row search takes precedence over a section deep link in the same frame.
    if super::search::pending(ui.ctx()) {
        state.jump = None;
    }
    let jump = state.jump;
    let query = state.filter.trim().to_owned();
    super::search::set_filter(ui.ctx(), &query);
    let mut shown = 0;
    for section in SECTIONS {
        let all = query.is_empty() || filter_matches(&query, &[&cx.tr.t(section.title_key())]);
        if !all
            && !super::search::ROWS.iter().any(|(s, key)| {
                *s == section
                    && filter_matches(&query, &[&cx.tr.t(super::search::label_key(key)), key])
            })
        {
            continue;
        }
        shown += 1;
        super::search::set_section_all(ui.ctx(), all);
        ui.push_id(section, |ui| {
            let top = ui.cursor().min.y;
            if top <= viewport_top + 12.0 {
                active = section;
            }
            ui.label(Text::h2(cx.tr.t(section.title_key()), cx.pal));
            ui.add(egui::Label::new(Text::small(cx.tr.t(section.subtitle_key()), cx.pal)).wrap());
            ui.add_space(12.0);
            if jump == Some(section) {
                // Retain the request through layout passes and any residual
                // wheel animation until this header actually reaches the top.
                if (top - viewport_top).abs() <= 12.0 {
                    state.jump = None;
                } else {
                    ui.ctx().request_repaint();
                }
                let title = Rect::from_min_size(
                    pos2(ui.max_rect().left(), top),
                    vec2(ui.available_width(), 1.0),
                );
                ui.scroll_to_rect_animation(
                    title,
                    Some(Align::Min),
                    if settings.reduce_motion {
                        egui::style::ScrollAnimation::none()
                    } else {
                        egui::style::ScrollAnimation::default()
                    },
                );
            }
            match section {
                SuperbaSection::General => general::show(ui, cx, settings, out),
                SuperbaSection::Appearance => appearance::show(ui, cx, settings, out),
                SuperbaSection::Network => network::show(ui, cx, settings, out),
                SuperbaSection::Notify => {
                    crate::pages::notify::body_ui(ui, cx, settings, notify, out)
                }
                _ => unreachable!(),
            }
            // Even a disabled/empty Notifications section can reach the top.
            // Otherwise the end-of-content clamp leaves Network highlighted.
            if section == SuperbaSection::Notify && query.is_empty() {
                ui.add_space((ui.clip_rect().height() - (ui.cursor().min.y - top)).max(0.0));
            }
            ui.add_space(32.0);
        });
    }
    super::search::set_filter(ui.ctx(), "");
    if shown == 0 {
        ui.add_space(24.0);
        ui.label(Text::muted(
            cx.tr.f("superba.search_empty", &[("query", &query)]),
            cx.pal,
        ));
    }
    if state.active != active {
        state.active = active;
        ui.ctx().request_repaint();
    }
}
