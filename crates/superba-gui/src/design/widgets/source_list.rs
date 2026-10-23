//! Surge-like source list (UX §3.5): the left column of the request log. A
//! grouping control at the top (按会话 / 按供应商), an "all" entry with the
//! total, then one row per group with its count, newest activity first, and a
//! footnote at the bottom. Sub-entries (a provider's lines) sit indented under
//! their entry, with their own counts and a guide line. The rows scroll inside
//! the list (log-style); the column collapses to a narrow strip with a chevron.
//!
//! Screen readers and UI Automation (user bug 56) see the grouping as tabs, the
//! chevron as a button named by its tooltip, and every entry as a selectable,
//! invokable list item named "label, count".

use egui::accesskit::Role;
use egui::{CursorIcon, Id, Rect, ScrollArea, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use super::log_table::elided;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};

/// Width of a collapsed list.
pub const COLLAPSED_WIDTH: f32 = 28.0;
/// Height of one entry.
pub const ENTRY_HEIGHT: f32 = 26.0;
/// Indent of a sub-entry's label.
pub const CHILD_INDENT: f32 = 14.0;
/// Width of the collapse chevron next to the grouping tabs.
const HEAD_CHEVRON: f32 = 20.0;

/// One entry of the list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceEntry {
    /// Key passed back on selection.
    pub key: String,
    /// Display name.
    pub label: String,
    /// Count shown on the right.
    pub count: u64,
    /// Hover text (the full key, last activity).
    pub tooltip: Option<String>,
    /// A sub-entry of the entry before it (a provider's line): indented.
    pub child: bool,
    /// The tool's icon tile before the label (tool, tool × model, session).
    pub tool: Option<&'static str>,
}

/// What happened in a [`SourceList`] this frame.
#[derive(Debug, Default)]
pub struct SourceListResponse {
    /// The selected entry changed (`None` = all).
    pub selection_changed: bool,
    /// The grouping changed.
    pub grouping_changed: bool,
    /// The list was collapsed or expanded.
    pub collapsed_changed: bool,
}

/// The list. `G` is the grouping value (`RequestGrouping`).
#[must_use = "call .show(ui)"]
pub struct SourceList<'a, G: PartialEq + Clone> {
    id: Id,
    selected: &'a mut Option<String>,
    collapsed: &'a mut bool,
    grouping: Option<(&'a mut G, Vec<(G, String)>)>,
    grouping_label: Option<String>,
    all: (String, u64),
    entries: &'a [SourceEntry],
    footnote: Option<String>,
    collapse_tip: (String, String),
}

impl<'a, G: PartialEq + Clone> SourceList<'a, G> {
    /// A list over `entries`; `selected` holds the selected key (`None` = all).
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        selected: &'a mut Option<String>,
        collapsed: &'a mut bool,
        entries: &'a [SourceEntry],
    ) -> Self {
        Self {
            id: Id::unique(("superba-source-list", id_salt)),
            selected,
            collapsed,
            grouping: None,
            grouping_label: None,
            all: (String::new(), 0),
            entries,
            footnote: None,
            collapse_tip: (String::new(), String::new()),
        }
    }

    /// The grouping control and its options (value, label).
    pub fn grouping(mut self, value: &'a mut G, options: Vec<(G, String)>) -> Self {
        self.grouping = Some((value, options));
        self
    }

    /// Accessible name of the grouping selector.
    pub fn grouping_label(mut self, label: impl Into<String>) -> Self {
        self.grouping_label = Some(label.into());
        self
    }

    /// The "all" entry.
    pub fn all(mut self, label: impl Into<String>, count: u64) -> Self {
        self.all = (label.into(), count);
        self
    }

    /// A note under the entries.
    pub fn footnote(mut self, text: impl Into<String>) -> Self {
        self.footnote = Some(text.into());
        self
    }

    /// Tooltips of the chevron (collapse, expand).
    pub fn collapse_tips(mut self, collapse: impl Into<String>, expand: impl Into<String>) -> Self {
        self.collapse_tip = (collapse.into(), expand.into());
        self
    }

    /// Draw in the available rectangle. Everything is placed in fixed
    /// rectangles of the column, so long labels never widen it.
    pub fn show(self, ui: &mut Ui) -> SourceListResponse {
        let pal = Palette::of(ui.ctx());
        let rect = ui.available_rect_before_wrap();
        let mut out = SourceListResponse::default();
        ui.painter().vline(
            rect.max.x - 0.5,
            rect.y_range(),
            Stroke::new(1.0, pal.border),
        );
        if *self.collapsed {
            let b = Rect::from_min_size(rect.min, vec2(COLLAPSED_WIDTH, COLLAPSED_WIDTH));
            if chevron(
                ui,
                self.id.with("expand"),
                b,
                Icon::ChevronRight,
                &self.collapse_tip.1,
                pal,
            )
            .on_hover_text(&self.collapse_tip.1)
            .clicked()
            {
                *self.collapsed = false;
                out.collapsed_changed = true;
            }
            ui.allocate_rect(rect, Sense::hover());
            return out;
        }
        let inner_w = rect.width() - 8.0;
        let mut y = rect.min.y;
        // Grouping tabs (text with an underline, like the detail pane) and the
        // collapse chevron.
        let head = Rect::from_min_size(pos2(rect.min.x, y), vec2(inner_w, 28.0));
        let chevron_r = Rect::from_min_size(
            pos2(head.max.x - HEAD_CHEVRON, head.min.y),
            vec2(HEAD_CHEVRON, 28.0),
        );
        if let Some((value, options)) = self.grouping {
            if options.len() > 2 {
                let rect = Rect::from_min_max(head.min, pos2(chevron_r.min.x - 4.0, head.max.y));
                let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
                let label = self.grouping_label.unwrap_or_else(|| {
                    options
                        .iter()
                        .find(|(v, _)| *v == *value)
                        .map(|(_, l)| l.clone())
                        .unwrap_or_default()
                });
                if super::Dropdown::new(self.id.with("grouping"))
                    .label(label)
                    .fixed_width(rect.width())
                    .show_options(&mut child, value, &options)
                    .changed()
                {
                    out.grouping_changed = true;
                }
            } else {
                let mut x = head.min.x;
                for (i, (v, label)) in options.into_iter().enumerate() {
                    let selected = *value == v;
                    let weight = if selected {
                        Weight::Medium
                    } else {
                        Weight::Regular
                    };
                    let room = chevron_r.min.x - x - 2.0;
                    if room < 24.0 {
                        break;
                    }
                    // Caption-size labels with tight padding: both tabs and the
                    // chevron fit the 150 px column in English ("Sessions",
                    // "Providers") and Chinese.
                    let g = elided(
                        ui,
                        &label,
                        font(text::CAPTION, weight),
                        pal.text,
                        room - 6.0,
                    );
                    let r = Rect::from_min_size(pos2(x, head.min.y), vec2(g.size().x + 6.0, 28.0));
                    let resp = ui
                        .interact(r, self.id.with(("group", i)), Sense::click())
                        .on_hover_cursor(CursorIcon::PointingHand);
                    crate::design::a11y::selectable(&resp, Role::Tab, &label, selected);
                    let color = if selected || resp.hovered() {
                        pal.text
                    } else {
                        pal.text_muted
                    };
                    ui.painter().galley(
                        pos2(r.min.x + 3.0, r.center().y - g.size().y / 2.0),
                        g,
                        color,
                    );
                    if selected {
                        ui.painter().hline(
                            (r.min.x + 2.0)..=(r.max.x - 2.0),
                            r.max.y - 2.0,
                            Stroke::new(2.0, pal.text),
                        );
                    }
                    if resp.clicked() && !selected {
                        *value = v;
                        out.grouping_changed = true;
                    }
                    x = r.max.x + 2.0;
                }
            }
        }
        if chevron(
            ui,
            self.id.with("collapse"),
            chevron_r,
            Icon::ChevronDown,
            &self.collapse_tip.0,
            pal,
        )
        .on_hover_text(&self.collapse_tip.0)
        .clicked()
        {
            *self.collapsed = true;
            out.collapsed_changed = true;
        }
        y = head.max.y + 6.0;
        // "All".
        let all_sel = self.selected.is_none();
        let all_r = Rect::from_min_size(pos2(rect.min.x, y), vec2(inner_w, ENTRY_HEIGHT));
        if entry(
            ui,
            self.id.with("all"),
            all_r,
            &self.all.0,
            self.all.1,
            all_sel,
            Kind::All,
            pal,
            false,
        )
        .clicked()
            && !all_sel
        {
            *self.selected = None;
            out.selection_changed = true;
        }
        y = all_r.max.y + 4.0;
        ui.painter().hline(
            rect.min.x..=(rect.min.x + inner_w),
            y,
            Stroke::new(1.0, pal.border),
        );
        y += 5.0;
        // Footnote at the bottom.
        let foot = self.footnote.as_ref().map(|f| {
            ui.painter().layout(
                f.clone(),
                font(text::CAPTION, Weight::Regular),
                pal.text_subtle,
                inner_w,
            )
        });
        let foot_h = foot.as_ref().map_or(0.0, |g| g.size().y + 10.0);
        let list = Rect::from_min_max(
            pos2(rect.min.x, y),
            pos2(rect.min.x + inner_w + 4.0, (rect.max.y - foot_h).max(y)),
        );
        let entries = self.entries;
        let selected = &mut *self.selected;
        let id = self.id;
        let mut list_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(list)
                .accessibility_role(Role::List)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        list_ui.set_clip_rect(list.intersect(ui.clip_rect()));
        ScrollArea::vertical()
            .id_salt(id.with("entries"))
            .auto_shrink([false, false])
            .show_rows(&mut list_ui, ENTRY_HEIGHT, entries.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    let e = &entries[i];
                    let (r, _) =
                        ui.allocate_exact_size(vec2(inner_w - 4.0, ENTRY_HEIGHT), Sense::hover());
                    let sel = selected.as_deref() == Some(e.key.as_str());
                    let kind = if e.child { Kind::Child } else { Kind::Entry };
                    let mut resp = entry(
                        ui,
                        id.with(("e", i)),
                        r,
                        &e.label,
                        e.count,
                        sel,
                        kind,
                        pal,
                        e.tool.is_some(),
                    );
                    if let Some(m) = e.tool {
                        crate::pages::shared::tool_label::paint(
                            ui,
                            m,
                            pos2(
                                r.min.x + 6.0 + crate::pages::shared::tool_label::TILE / 2.0,
                                r.center().y,
                            ),
                        );
                    }
                    if let Some(t) = &e.tooltip {
                        resp = resp.on_hover_text(t);
                    }
                    if resp.clicked() && !sel {
                        *selected = Some(e.key.clone());
                        out.selection_changed = true;
                    }
                }
            });
        if let Some(g) = foot {
            let fy = rect.max.y - g.size().y - 2.0;
            ui.painter()
                .galley(pos2(rect.min.x, fy), g, pal.text_subtle);
        }
        ui.allocate_rect(rect, Sense::hover());
        out
    }
}

/// What an entry row is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The "all" entry (medium weight).
    All,
    /// A group.
    Entry,
    /// A sub-entry (indented, with a guide line).
    Child,
}

/// One entry row in `r`: label left, count right (tabular).
#[allow(clippy::too_many_arguments)]
fn entry(
    ui: &mut Ui,
    id: Id,
    r: Rect,
    label: &str,
    count: u64,
    selected: bool,
    kind: Kind,
    pal: &Palette,
    tool: bool,
) -> egui::Response {
    let resp = ui
        .interact(r, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    crate::design::a11y::selectable(
        &resp,
        Role::ListItem,
        &format!("{label}, {}", group_digits(count)),
        selected,
    );
    if selected {
        ui.painter().rect_filled(r, radius::CONTROL, pal.active);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(r, radius::CONTROL, pal.hover.gamma_multiply(0.7));
    }
    let cg = ui.painter().layout_no_wrap(
        group_digits(count),
        mono_font(text::CAPTION, Weight::Regular),
        pal.text_subtle,
    );
    let cw = cg.size().x;
    ui.painter().galley(
        pos2(r.max.x - 6.0 - cw, r.center().y - cg.size().y / 2.0),
        cg,
        pal.text_subtle,
    );
    let weight = if kind == Kind::All || (selected && kind == Kind::Entry) {
        Weight::Medium
    } else {
        Weight::Regular
    };
    let color = if selected { pal.text } else { pal.text_muted };
    let indent = if kind == Kind::Child {
        // Guide line from the parent down the sub-entries.
        let x = r.min.x + 9.5;
        ui.painter().vline(
            x,
            (r.min.y + 4.0)..=(r.max.y - 4.0),
            Stroke::new(1.0, pal.border),
        );
        CHILD_INDENT
    } else if tool {
        crate::pages::shared::tool_label::ROOM
    } else {
        0.0
    };
    let g = elided(
        ui,
        label,
        font(text::SMALL, weight),
        color,
        r.width() - cw - 20.0 - indent,
    );
    ui.painter().galley(
        pos2(r.min.x + 6.0 + indent, r.center().y - g.size().y / 2.0),
        g,
        color,
    );
    resp
}

fn chevron(ui: &mut Ui, id: Id, r: Rect, icon: Icon, name: &str, pal: &Palette) -> egui::Response {
    let resp = ui
        .interact(r, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    if resp.hovered() {
        ui.painter().rect_filled(r, radius::CONTROL, pal.hover);
    }
    let ir = Rect::from_center_size(r.center(), vec2(14.0, 14.0));
    icons::paint(ui.painter(), ir, icon, pal.text_muted);
    resp
}

/// `12,345`.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
