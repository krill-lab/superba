//! Layout of the tool detail (ui-spec rules 1–3 and 7): a header with the
//! tool's identity, one state badge, a muted subtitle and secondary actions on
//! the right; sections with a title and an optional caption; and settings rows
//! with the label (and its help) on the left and the controls right-aligned,
//! separated by hairlines.
//!
//! A row's controls may use everything but [`MIN_LABEL`] of the row; the label
//! column gets what the controls leave (measured in the last pass), so a help
//! text wraps beside a narrow control and never runs under a wide one, in
//! either language. Controls that need more than that are stacked under the
//! label. Label block and controls are centered on each other, like
//! [`crate::design::widgets::FormRow`] does for a single line.
use crate::design::tokens::{size, space};
use crate::design::widgets::{Badge, MonogramTile, Status, Text};
use crate::pages::Cx;
use egui::{Align, Id, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

/// Narrowest label column beside the controls.
pub const MIN_LABEL: f32 = 96.0;
/// Side of the tool's monogram in the detail header.
pub const HEADER_TILE: f32 = 40.0;

/// Where a row puts its label block and its controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowLayout {
    /// Width of the label block.
    pub label_w: f32,
    /// Controls are drawn under the label block instead of beside it.
    pub stacked: bool,
}

/// The widest the controls of a row `avail` wide may be beside the label.
pub fn controls_max(avail: f32) -> f32 {
    (avail - MIN_LABEL - space::L).max(0.0)
}

/// Layout of a row `avail` wide whose controls were `controls` wide.
pub fn row_layout(avail: f32, controls: f32) -> RowLayout {
    if controls > controls_max(avail) + 0.5 {
        RowLayout {
            label_w: avail,
            stacked: true,
        }
    } else {
        RowLayout {
            label_w: (avail - controls - space::L).max(MIN_LABEL),
            stacked: false,
        }
    }
}

/// What a row measured in its last pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Measure {
    controls_w: f32,
    controls_h: f32,
    label_h: f32,
}

impl Measure {
    fn differs(&self, other: &Self) -> bool {
        (self.controls_w - other.controls_w).abs() > 0.5
            || (self.controls_h - other.controls_h).abs() > 0.5
            || (self.label_h - other.label_h).abs() > 0.5
    }
}

/// Settings rows of one section: a hairline between two rows, none above the
/// first or below the last.
#[derive(Debug, Default)]
pub struct Rows {
    drawn: usize,
}

impl Rows {
    /// The rows of a new section.
    pub fn new() -> Self {
        Self::default()
    }

    /// A row with a text label, an optional help line under it and the
    /// controls `add` draws right to left (the first control is rightmost).
    /// Controls without a name of their own are named by the label.
    pub fn row(
        &mut self,
        ui: &mut Ui,
        cx: &Cx<'_>,
        label: &str,
        help: Option<&str>,
        add: impl FnOnce(&mut Ui),
    ) {
        let label_id = std::cell::Cell::new(None);
        self.row_with(
            ui,
            label,
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let r = ui.add(
                    egui::Label::new(Text::body(label, cx.pal))
                        .wrap()
                        .selectable(false),
                );
                label_id.set(Some(r.id));
                if let Some(help) = help {
                    ui.add(egui::Label::new(Text::small(help, cx.pal)).wrap());
                }
            },
            |ui| match label_id.get() {
                Some(id) => crate::design::a11y::label_unnamed_by(ui, id, add),
                None => add(ui),
            },
        );
    }

    /// A row whose label block `label` draws top-down in the width it is
    /// given; `salt` keeps the measurements of rows apart.
    pub fn row_with(
        &mut self,
        ui: &mut Ui,
        salt: impl std::hash::Hash + std::fmt::Debug,
        label: impl FnOnce(&mut Ui),
        add: impl FnOnce(&mut Ui),
    ) {
        let pal = crate::design::Palette::of(ui.ctx());
        if self.drawn > 0 {
            hairline(ui, pal.border);
        }
        self.drawn += 1;
        ui.add_space(space::S);
        let id = ui.scope_id().with(("superba-tool-row", salt));
        let last: Option<Measure> = ui.data(|d| d.get_temp(id));
        let prev = last.unwrap_or(Measure {
            controls_w: 0.0,
            controls_h: size::CONTROL_SM,
            label_h: 0.0,
        });
        let top = ui.cursor().min;
        let avail = ui.available_width();
        let right = top.x + avail;
        let layout = row_layout(avail, prev.controls_w);
        let line = prev.controls_h.max(prev.label_h);

        // Label block: centered on the line, or on top when stacked.
        let label_top = if layout.stacked {
            top.y
        } else {
            top.y + (line - prev.label_h).max(0.0) / 2.0
        };
        let mut label_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_size(
                    pos2(top.x, label_top),
                    vec2(layout.label_w, f32::INFINITY),
                ))
                .layout(Layout::top_down(Align::Min)),
        );
        label_ui.set_max_width(layout.label_w);
        label(&mut label_ui);
        let label_rect = label_ui.min_rect();

        // Controls: right-aligned, centered on the line (under the label when stacked).
        let controls_top = if layout.stacked {
            label_rect.max.y + space::XS
        } else {
            top.y + (line - prev.controls_h).max(0.0) / 2.0
        };
        let width = if layout.stacked {
            avail
        } else {
            controls_max(avail)
        };
        let mut controls_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    pos2(right - width, controls_top),
                    pos2(right, controls_top + prev.controls_h),
                ))
                .layout(Layout::right_to_left(Align::Center)),
        );
        add(&mut controls_ui);
        let controls_rect = controls_ui.min_rect();

        let now = Measure {
            controls_w: controls_rect.width(),
            controls_h: controls_rect.height().max(size::CONTROL_SM),
            label_h: label_rect.height(),
        };
        if last.is_none_or(|m| m.differs(&now)) {
            ui.data_mut(|d| d.insert_temp(id, now));
            ui.ctx()
                .request_discard("tool detail: a row's label or controls changed size");
        }
        let bottom = label_rect.max.y.max(controls_rect.max.y);
        ui.advance_cursor_after_rect(Rect::from_min_max(top, pos2(right, bottom)));
        ui.add_space(space::S);
    }
}

/// A full-width hairline.
pub fn hairline(ui: &mut Ui, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(r.x_range(), r.center().y, Stroke::new(1.0, color));
}

/// A section title (SECTION size) with an optional muted caption.
pub fn section(ui: &mut Ui, cx: &Cx<'_>, title: &str, caption: Option<&str>) {
    ui.label(Text::section(title, cx.pal));
    if let Some(caption) = caption {
        ui.add_space(space::XS / 2.0);
        ui.add(egui::Label::new(Text::small(caption, cx.pal)).wrap());
    }
    ui.add_space(space::XS);
}

/// Space between two sections (rows end with [`space::S`] of their own).
pub fn section_gap(ui: &mut Ui) {
    ui.add_space(space::XL - space::S);
}

/// The header of a detail: monogram, name and state badge over a muted
/// subtitle; `actions` (right to left) on the right keep the width they used
/// in the last frame, so the subtitle elides instead of squeezing them.
pub fn header(
    ui: &mut Ui,
    cx: &Cx<'_>,
    monogram: &str,
    name: &str,
    badge: Option<(Status, String)>,
    subtitle: &str,
    actions: impl FnOnce(&mut Ui),
) {
    let id = Id::unique(("superba-tool-header-actions", name));
    let actions_w: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::M;
        ui.add(MonogramTile::new(monogram).size(HEADER_TILE));
        let text_w = (ui.available_width() - actions_w - space::L).max(MIN_LABEL);
        ui.vertical(|ui| {
            ui.set_width(text_w);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.label(Text::title(name, cx.pal));
                if let Some((status, label)) = badge {
                    let badge = Badge::new(label);
                    ui.add(if status == Status::Idle {
                        badge.hollow()
                    } else {
                        badge.status(status)
                    });
                }
            });
            ui.add(egui::Label::new(Text::small(subtitle, cx.pal)).truncate());
        });
        let used = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                actions(ui);
                ui.min_rect().width()
            })
            .inner;
        if (used - actions_w).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(id, used));
            ui.ctx().request_repaint();
        }
    });
}

/// "272K", "1M", "1.5M": a context window in tokens.
pub fn context_label(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = (tokens + 50_000) / 100_000;
        if tenths.is_multiple_of(10) {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else {
        format!("{}K", (tokens + 500) / 1000)
    }
}
