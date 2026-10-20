//! Plan notices in dialogs and in the switch preview (UI spec rule 8).
//!
//! A plan can carry dozens of notices: turning the router off on a relay that
//! lists 32 model names the app's Codex has no built-in entry for gave one line
//! per model (user bug 103). [`group`] drops exact duplicates, folds notices of
//! the same kind (catalog key and severity) into one row and sorts the rows by
//! severity, blocking first, then warnings, then information, each kind in the
//! order the planner produced it. [`notice_list`] draws them: warnings and
//! blocking notices always show, information sits behind one collapsed
//! disclosure, and a row that stands for several notices says how many with an
//! expander for the full list.

use egui::{Id, Rect, Sense, Ui, vec2};
use superba_core::{LocalizedText, Notice, Severity};

use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{radius, size, space, text};
use crate::design::widgets::focus::{focus_ring, show_focus};
use crate::design::widgets::{Text, link_button};
use crate::pages::Cx;

/// Notices of one kind (same catalog key and severity), in planner order,
/// without exact duplicates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeGroup {
    /// Severity shared by the items.
    pub severity: Severity,
    /// The catalog key shared by the items.
    pub key: String,
    /// The notices' texts.
    pub items: Vec<LocalizedText>,
}

impl NoticeGroup {
    /// The one argument the items differ in while every other argument is the
    /// same (the model of "{model}: no catalog entry ..."): its name and the
    /// values in order. `None` for a single item or items that differ in more.
    pub fn varying_arg(&self) -> Option<(&str, Vec<&str>)> {
        let [first, rest @ ..] = self.items.as_slice() else {
            return None;
        };
        if rest.is_empty() {
            return None;
        }
        let same_shape = |t: &LocalizedText| {
            t.nested == first.nested
                && t.args.len() == first.args.len()
                && t.args.iter().zip(&first.args).all(|(a, b)| a.0 == b.0)
        };
        if !rest.iter().all(same_shape) {
            return None;
        }
        let mut varying =
            (0..first.args.len()).filter(|&i| rest.iter().any(|t| t.args[i].1 != first.args[i].1));
        let (Some(i), None) = (varying.next(), varying.next()) else {
            return None;
        };
        Some((
            first.args[i].0.as_str(),
            self.items.iter().map(|t| t.args[i].1.as_str()).collect(),
        ))
    }
}

/// Group `notices` by kind and sort the groups by severity (most severe first,
/// planner order within a severity). Exact duplicates are dropped. A notice
/// without a catalog key is a kind of its own.
pub fn group(notices: &[Notice]) -> Vec<NoticeGroup> {
    let mut groups: Vec<NoticeGroup> = Vec::new();
    for n in notices {
        let same_kind = |g: &&mut NoticeGroup| {
            g.severity == n.severity
                && if n.text.key.is_empty() {
                    g.items.first() == Some(&n.text)
                } else {
                    g.key == n.text.key
                }
        };
        match groups.iter_mut().find(same_kind) {
            Some(g) => {
                if !g.items.contains(&n.text) {
                    g.items.push(n.text.clone());
                }
            }
            None => groups.push(NoticeGroup {
                severity: n.severity,
                key: n.text.key.clone(),
                items: vec![n.text.clone()],
            }),
        }
    }
    // Stable: planner order within a severity.
    groups.sort_by_key(|g| std::cmp::Reverse(g.severity));
    groups
}

/// The text of a group's row: the notice itself for one item, else the
/// kind's own count sentence (`notice.group.<key>` with `{n}`) or the first
/// notice with "and n more like it".
pub fn summary(cx: &Cx<'_>, g: &NoticeGroup) -> String {
    let first = cx.localize(&g.items[0]);
    let n = g.items.len();
    if n == 1 {
        return first;
    }
    let key = format!("notice.group.{}", g.key);
    if cx.tr.has(&key) {
        let text = LocalizedText::new(key, "").arg("n", n);
        return cx.tr.text(&text);
    }
    cx.tr.f(
        "notice.group.more",
        &[
            ("text", &first.trim_end_matches(['。', '.'])),
            ("n", &(n - 1)),
        ],
    )
}

/// The lines an expanded group lists: the varying values on one wrapped line
/// (model names), else every notice's own text.
pub fn details(cx: &Cx<'_>, g: &NoticeGroup) -> Details {
    match g.varying_arg() {
        Some((_, values)) => Details::Values(values.join(", ")),
        None => Details::Texts(g.items.iter().map(|t| cx.localize(t)).collect()),
    }
}

/// What an expanded group shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Details {
    /// The values the notices differ in, comma-separated (ids: mono).
    Values(String),
    /// Every notice's text.
    Texts(Vec<String>),
}

fn group_id(id: Id, g: &NoticeGroup) -> Id {
    id.with(("group", g.severity, &g.key))
}

/// Open every disclosure of the list `id` showing `notices` (screenshots,
/// tests).
pub fn expand_all(ctx: &egui::Context, id: Id, notices: &[Notice]) {
    ctx.data_mut(|d| {
        d.insert_temp(id.with("info"), true);
        for g in group(notices) {
            d.insert_temp(group_id(id, &g), true);
        }
    });
}

fn is_open(ui: &Ui, id: Id) -> bool {
    ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false)
}

fn toggle(ui: &Ui, id: Id) {
    let open = is_open(ui, id);
    ui.data_mut(|d| d.insert_temp(id, !open));
}

/// Severity icon and its color.
fn marker(cx: &Cx<'_>, severity: Severity) -> (Icon, egui::Color32) {
    match severity {
        Severity::Info => (Icon::Info, cx.pal.text_subtle),
        Severity::Warning => (Icon::Warning, cx.pal.amber),
        Severity::Danger => (Icon::Warning, cx.pal.red),
    }
}

/// Icon column width (the details of a group indent by it).
const ICON_COLUMN: f32 = 16.0;

/// One notice row: the severity icon, then `add` (the wrapping text).
pub fn notice_row<R>(
    ui: &mut Ui,
    cx: &Cx<'_>,
    severity: Severity,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(ICON_COLUMN, 18.0), Sense::hover());
        let (icon, color) = marker(cx, severity);
        icons::paint(
            ui.painter(),
            Rect::from_center_size(r.center(), vec2(14.0, 14.0)),
            icon,
            color,
        );
        ui.vertical(add).inner
    })
    .inner
}

fn group_row(ui: &mut Ui, cx: &Cx<'_>, id: Id, g: &NoticeGroup) {
    let text = summary(cx, g);
    if g.items.len() == 1 {
        notice_row(ui, cx, g.severity, |ui| {
            ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
        });
        return;
    }
    let open_id = group_id(id, g);
    let open = is_open(ui, open_id);
    notice_row(ui, cx, g.severity, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            ui.add(egui::Label::new(Text::small(format!("{text} ·"), cx.pal)).wrap());
            let label = cx.tr.t(if open {
                "notice.collapse"
            } else {
                "notice.expand"
            });
            if link_button(ui, &label, false).clicked() {
                toggle(ui, open_id);
            }
        });
        if open {
            match details(cx, g) {
                Details::Values(values) => {
                    ui.add(egui::Label::new(Text::mono_muted(values, cx.pal)).wrap());
                }
                Details::Texts(texts) => {
                    for t in texts {
                        ui.add(egui::Label::new(Text::small(t, cx.pal)).wrap());
                    }
                }
            }
        }
    });
}

/// The "n notes" row: a chevron in the icon column and the label, the whole
/// row clickable (a button for screen readers, named by its label).
fn disclosure_row(ui: &mut Ui, cx: &Cx<'_>, label: &str, open: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font(text::SMALL, Weight::Medium),
        egui::Color32::PLACEHOLDER,
    );
    let height = size::CONTROL_SM;
    let width = ICON_COLUMN + ui.spacing().item_spacing.x + galley.size().x;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let color = if response.hovered() {
        cx.pal.text
    } else {
        cx.pal.text_muted
    };
    let icon = Rect::from_center_size(
        egui::pos2(rect.min.x + ICON_COLUMN / 2.0, rect.center().y),
        vec2(14.0, 14.0),
    );
    icons::paint(
        ui.painter(),
        icon,
        if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        },
        color,
    );
    let text_pos = egui::pos2(
        rect.min.x + ICON_COLUMN + ui.spacing().item_spacing.x,
        rect.center().y - galley.size().y / 2.0,
    );
    if show_focus(&response) {
        focus_ring(ui.painter(), rect, radius::INNER, cx.pal);
    }
    ui.painter().galley(text_pos, galley, color);
    let name = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    response
}

/// Draw `notices` grouped and sorted ([`group`]): blocking notices and
/// warnings, then one collapsed "n notes" disclosure holding the information.
/// `id` keeps which disclosures are open (one per dialog or preview).
pub fn notice_list(ui: &mut Ui, cx: &Cx<'_>, id: Id, notices: &[Notice]) {
    let groups = group(notices);
    let (info, urgent): (Vec<&NoticeGroup>, Vec<&NoticeGroup>) =
        groups.iter().partition(|g| g.severity == Severity::Info);
    for g in urgent {
        group_row(ui, cx, id, g);
    }
    if info.is_empty() {
        return;
    }
    let open_id = id.with("info");
    let open = is_open(ui, open_id);
    let count: usize = info.iter().map(|g| g.items.len()).sum();
    let label = cx.tr.f(
        if open {
            "notice.info_hide"
        } else {
            "notice.info_show"
        },
        &[("n", &count)],
    );
    if disclosure_row(ui, cx, &label, open).clicked() {
        toggle(ui, open_id);
    }
    if is_open(ui, open_id) {
        for g in info {
            group_row(ui, cx, id, g);
        }
    }
}
