//! Drawing of the Codex settings page (UX §3.6): the strip (this computer,
//! provider, search, Advanced, legacy keys), an optional notice line, the three
//! panes (rail, paged list, detail — a sheet over the list on narrow windows) and
//! the footer (pager under the list, pending bar), plus the diff and the long
//! explanation dialogs.

use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use superba_core::Locale;

use super::State;
use super::model::{Availability, Entry, Model, Stage, Value, diff};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::layout::{self, Track};
use crate::design::tokens::{radius, size, text};
use crate::design::widgets::{
    Badge, Button, Dropdown, PagedTable, Pager, PendingAction, PendingBar, PendingLabels, Status,
    StatusDot, Table, Text, Toggle, dialog, dialog_body,
};
use crate::pages::shared::{loading, providers, values};
use crate::pages::{Action, Cx};

/// Rail width.
pub(super) const RAIL_W: f32 = 176.0;
/// Docked detail pane width.
const DETAIL_W: f32 = 372.0;
/// List width of the 1280 px layout; the rail grows only beyond it.
const LIST_MIN: f32 = 424.0;
/// Width of the side sheet on narrow windows.
const SHEET_W: f32 = 420.0;
/// Page width from which the detail pane docks (a 1120 px window).
const DOCK_MIN: f32 = 836.0;
/// Gap between panes.
const GAP: f32 = 12.0;
/// Widest value chip of a list row (user bug 15: it never grows past this).
const CHIP_W: f32 = 150.0;

/// Placeholder while the first load runs.
pub(super) fn loading(ui: &mut Ui, cx: &Cx<'_>) {
    loading::spinner_row(ui, cx, &cx.tr.t("codex_settings.loading"));
}

/// What the page asks its state to do after drawing.
#[derive(Debug, Default)]
pub(super) struct Events {
    /// Save (and apply) the pending edits.
    pub save: bool,
    /// Edit another profile.
    pub profile: Option<superba_core::ProfileId>,
    /// Build the model again (Retry after a failure).
    pub retry: bool,
}

/// No model to show yet (user bug 83): the first load, or another provider
/// was chosen. With a chosen provider the picker shows it at once (busy while
/// its model is built, and still usable), then the loading state or the
/// failure with Retry in place of the panes; never the previous provider's
/// settings.
pub(super) fn waiting(ui: &mut Ui, cx: &Cx<'_>, state: &State) -> Events {
    let mut events = Events::default();
    let chosen = state.profile.as_ref();
    let name = chosen.map(|id| {
        cx.data
            .profile(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string())
    });
    if chosen.is_some() {
        ui.horizontal(|ui| {
            ui.set_min_height(size::CONTROL);
            ui.spacing_mut().item_spacing.x = 8.0;
            // The strip as it will be, so the picker stays where it was.
            scope_mark(ui, cx, false);
            provider_picker(ui, cx, chosen, state.load.loading(), &mut events);
        });
        ui.add_space(6.0);
    }
    match (state.load.failure(), &name) {
        (Some(f), Some(name)) => {
            let title = cx
                .tr
                .f("codex_settings.load_failed_for", &[("provider", name)]);
            events.retry = loading::failed_card(ui, cx, &title, &f.text(cx));
        }
        (Some(f), None) => {
            let title = cx.tr.t("codex_settings.load_failed");
            events.retry = loading::failed_card(ui, cx, &title, &f.text(cx));
        }
        (None, Some(name)) => {
            loading::spinner_row(
                ui,
                cx,
                &cx.tr.f("codex_settings.loading_for", &[("provider", name)]),
            );
        }
        (None, None) => loading(ui, cx),
    }
    events
}

/// The loaded page in the fixed rectangle below the header.
pub(super) fn page(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) -> Events {
    let mut events = Events::default();
    let Some(model) = state.model.clone() else {
        return events;
    };
    let notice = notice(cx, &model, state);
    let body = ui.available_rect_before_wrap();
    let mut tracks = vec![Track::Fixed(size::CONTROL)];
    if notice.is_some() {
        tracks.push(Track::Fixed(20.0));
    }
    tracks.push(Track::Fill);
    tracks.push(Track::Fixed(size::FOOTER));
    let rects = layout::row_rects(body, &tracks, 6.0);
    let (strip_r, notice_r, panes_r, footer_r) = if notice.is_some() {
        (rects[0], Some(rects[1]), rects[2], rects[3])
    } else {
        (rects[0], None, rects[1], rects[2])
    };
    layout::fixed(ui, strip_r, |ui| strip(ui, cx, state, &model, &mut events));
    if let (Some(r), Some(n)) = (notice_r, notice) {
        layout::fixed(ui, r, |ui| notice_line(ui, cx, &model, n, out));
    }
    // A new filter starts at its first row (selected, page 1).
    if state.shown_filter.as_ref() != Some(&state.filter) {
        state.shown_filter = Some(state.filter.clone());
        state.table.page = 0;
        state.table.selected = (!state.rows().is_empty()).then_some(0);
    }
    let docked = body.width() >= DOCK_MIN;
    // The rail takes what its longest label needs from the width beyond the
    // 1280 px layout (rail 176 · list 424 · detail 372); narrower, long labels
    // wrap to two lines.
    let spare = (panes_r.width() - (RAIL_W + GAP + LIST_MIN + GAP + DETAIL_W)).max(0.0);
    let rail_w = super::rail::needed_width(ui, cx, &model).clamp(RAIL_W, RAIL_W + spare);
    let cols = layout::column_rects(panes_r, &[Track::Fixed(rail_w), Track::Fill], GAP);
    layout::fixed(ui, cols[0], |ui| super::rail::show(ui, cx, state, &model));
    let main = cols[1];
    let mut list_x = main.x_range();
    if docked {
        let parts = layout::column_rects(main, &[Track::Fill, Track::Fixed(DETAIL_W)], GAP);
        list_x = parts[0].x_range();
        layout::fixed(ui, parts[0], |ui| list(ui, cx, state, &model, docked));
        ui.painter().vline(
            parts[1].min.x - GAP / 2.0,
            parts[1].y_range(),
            egui::Stroke::new(1.0, cx.pal.border),
        );
        layout::fixed(ui, parts[1], |ui| {
            super::detail::show(ui, cx, state, &model, out);
        });
    } else {
        layout::fixed(ui, main, |ui| list(ui, cx, state, &model, docked));
        if state.sheet_open && state.selected().is_some() {
            sheet(ui.ctx(), cx, state, &model, main, out);
        }
    }
    layout::fixed(ui, footer_r, |ui| {
        events.save = footer(ui, cx, state, &model, list_x);
    });
    if state.diff_open {
        diff_dialog(ui.ctx(), cx, state, &model);
    }
    if state.expanded.is_some() {
        super::detail::expanded_dialog(ui.ctx(), cx, state, &model);
    }
    super::editor::dialogs(ui.ctx(), cx, state, &model);
    events
}

/// The detail pane as a sheet over the right edge of the list (narrow
/// windows); Escape or the close button hides it.
fn sheet(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    state: &mut State,
    model: &Model,
    main: egui::Rect,
    out: &mut Vec<Action>,
) {
    let w = SHEET_W.min(main.width());
    let rect = egui::Rect::from_min_max(egui::pos2(main.max.x - w, main.min.y), main.max);
    let mut close = false;
    egui::Area::new(egui::Id::unique("codex-settings-sheet"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(cx.pal.surface)
                .stroke(egui::Stroke::new(1.0, cx.pal.border))
                .corner_radius(radius::CARD)
                .inner_margin(egui::Margin::same(16))
                .shadow(egui::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: cx.pal.shadow,
                })
                .show(ui, |ui| {
                    ui.set_width(w - 32.0);
                    ui.set_height(rect.height() - 32.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if ui
                            .add(crate::design::widgets::IconButton::new(
                                Icon::Close,
                                cx.tr.t("codex_settings.pending.close"),
                            ))
                            .clicked()
                        {
                            close = true;
                        }
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            super::detail::show(ui, cx, state, model, out);
                        });
                    });
                });
        });
    if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.sheet_open = false;
    }
}

/// The strip: "This computer ⓘ", provider picker, search, Advanced and the
/// legacy-key notice.
fn strip(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, model: &Model, events: &mut Events) {
    ui.horizontal(|ui| {
        ui.set_min_height(size::CONTROL);
        ui.spacing_mut().item_spacing.x = 8.0;
        scope_label(ui, cx, model);
        provider_picker(ui, cx, model.profile_id.as_ref(), false, events);
        let search = crate::design::widgets::SearchField::new(
            &mut state.filter.query,
            "codex-settings-search",
            cx.tr.t("codex_settings.search"),
            cx.tr.t("action.clear_search"),
        )
        .width(220.0)
        .margin(vec2(10.0, 6.0))
        .show(ui);
        if state.focus_search {
            search.request_focus();
            state.focus_search = false;
        }
        ui.add(Toggle::new(
            &mut state.filter.advanced,
            cx.tr.t("codex_settings.advanced"),
        ))
        .on_hover_text(cx.tr.t("codex_settings.advanced_hint"));
        ui.label(Text::small(cx.tr.t("codex_settings.advanced"), cx.pal))
            .on_hover_text(cx.tr.t("codex_settings.advanced_hint"));
        let legacy: Vec<&Entry> = model
            .migrations()
            .filter(|e| state.pending.get(&e.path).is_none())
            .collect();
        if !legacy.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::ghost(cx.tr.t("codex_settings.migrate.button"))
                            .small()
                            .enabled(model.config_error.is_none()),
                    )
                    .on_hover_text(cx.tr.t("codex_settings.migrate.restore_note"))
                    .clicked()
                {
                    for e in &legacy {
                        for (path, change) in model.migration_changes(e) {
                            state.pending.set_path(model, &path, change);
                        }
                    }
                    state.diff_open = true;
                }
                ui.label(Text::small(
                    cx.tr
                        .f("codex_settings.migrate.short", &[("n", &legacy.len())]),
                    cx.pal,
                ))
                .on_hover_text(
                    cx.tr
                        .f("codex_settings.migrate.notice", &[("n", &legacy.len())]),
                );
                ui.add(StatusDot::new(Status::Warn));
            });
        }
    });
}

/// "本机 ⓘ": the local consumers and the catalog version in the tooltip; amber
/// when the catalog is approximate or older than a local Codex.
fn scope_label(ui: &mut Ui, cx: &Cx<'_>, model: &Model) {
    let version = |key: super::model::TargetKey| {
        model
            .targets
            .iter()
            .find(|t| t.key == key)
            .map(|t| {
                t.version
                    .clone()
                    .unwrap_or_else(|| cx.tr.t("codex_settings.version_unknown"))
            })
            .unwrap_or_else(|| "—".into())
    };
    let warn = model
        .targets
        .iter()
        .any(|t| t.approximate || t.catalog_outdated);
    let mut tip = vec![cx.tr.f(
        "codex_settings.scope.consumers",
        &[
            ("app", &version(super::model::TargetKey::App)),
            ("cli", &version(super::model::TargetKey::Cli)),
            (
                "catalog",
                &model.catalog_version.clone().unwrap_or_default(),
            ),
        ],
    )];
    if model.targets.iter().any(|t| t.catalog_outdated) {
        tip.push(cx.tr.t("codex_settings.catalog_outdated"));
    } else if model.targets.iter().any(|t| t.approximate) {
        tip.push(cx.tr.t("codex_settings.catalog_approximate"));
    }
    if model.targets.is_empty() {
        tip.push(cx.tr.t("codex_settings.no_targets"));
    }
    scope_mark(ui, cx, warn).on_hover_text(tip.join("\n"));
}

/// "本机 ⓘ" (amber when `warn`).
fn scope_mark(ui: &mut Ui, cx: &Cx<'_>, warn: bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(Text::strong(cx.tr.t("codex_settings.scope.label"), cx.pal));
        let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
        let color = if warn {
            cx.pal.status(Status::Warn)
        } else {
            cx.pal.text_subtle
        };
        icons::paint(ui.painter(), rect, Icon::Info, color);
    })
    .response
}

/// Provider picker (key-based providers; the applied one marked "in use")
/// showing `current`, the chosen one; `busy` while its model is built.
fn provider_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    current: Option<&superba_core::ProfileId>,
    busy: bool,
    events: &mut Events,
) {
    let list: Vec<providers::Provider> = providers::with_profiles(cx.data)
        .into_iter()
        .filter(|p| p.kind != providers::ProviderKind::Official)
        .collect();
    ui.label(Text::small(cx.tr.t("codex_settings.provider"), cx.pal));
    let label = |p: &providers::Provider| {
        let name = p
            .profile
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        if p.applied {
            cx.tr
                .f("codex_settings.provider_applied", &[("name", &name)])
        } else {
            name
        }
    };
    let selected = list
        .iter()
        .find(|p| p.profile_id() == current)
        .map(label)
        .unwrap_or_else(|| cx.tr.t("codex_settings.no_profile_short"));
    ui.add_enabled_ui(list.len() > 1, |ui| {
        Dropdown::new("codex-settings-profile")
            .label(cx.tr.t("codex_settings.provider"))
            .selected_text(selected)
            .width(150.0)
            .loading(busy)
            .show_ui(ui, |ui| {
                for p in &list {
                    let Some(id) = p.profile_id() else { continue };
                    let on = current == Some(id);
                    if ui.selectable_label(on, label(p)).clicked() && !on {
                        events.profile = Some(id.clone());
                    }
                }
            })
            .response
            .on_disabled_hover_text(cx.tr.t("codex_settings.scope.official"));
    });
}

/// The one-line notice above the panes (read-only reason, config error, a
/// failed reload, saved-but-unapplied settings, catalog notes).
#[derive(Clone, Debug)]
pub(super) enum Notice {
    /// config.toml cannot be parsed.
    ConfigError(String),
    /// Reading the settings again failed (the shown ones stay).
    ReloadFailed(String),
    /// No key-based profile.
    ReadOnly(&'static str),
    /// Saved settings no local Codex knows.
    Blocked(usize),
    /// Saved settings waiting for the next switch.
    Waiting(usize),
    /// Catalog notes.
    Notes(Vec<String>),
}

fn notice(cx: &Cx<'_>, model: &Model, state: &State) -> Option<Notice> {
    if let Some(e) = &model.config_error {
        return Some(Notice::ConfigError(e.clone()));
    }
    if let Some(f) = state.load.failure() {
        return Some(Notice::ReloadFailed(f.text(cx)));
    }
    if let Some(key) = model.read_only_reason {
        return Some(Notice::ReadOnly(key));
    }
    let (blocked, waiting): (Vec<&Entry>, Vec<&Entry>) = model
        .entries
        .iter()
        .filter(|e| e.unapplied(model.profile_applied) && state.pending.get(&e.path).is_none())
        .partition(|e| e.version_blocked());
    if !waiting.is_empty() {
        return Some(Notice::Waiting(waiting.len()));
    }
    if !blocked.is_empty() {
        return Some(Notice::Blocked(blocked.len()));
    }
    let notes: Vec<String> = model.notes.iter().map(|n| cx.localize(n)).collect();
    (!notes.is_empty()).then_some(Notice::Notes(notes))
}

fn notice_line(ui: &mut Ui, cx: &Cx<'_>, model: &Model, n: Notice, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (status, text, tip) = match &n {
            Notice::ConfigError(e) => (
                Status::Error,
                cx.tr.f("codex_settings.config_error", &[("error", e)]),
                None,
            ),
            Notice::ReloadFailed(reason) => (
                Status::Error,
                cx.tr
                    .f("codex_settings.reload_failed", &[("reason", reason)]),
                Some(reason.clone()),
            ),
            Notice::ReadOnly(key) => (Status::Idle, cx.tr.t(key), None),
            Notice::Blocked(n) => (
                Status::Warn,
                cx.tr.f("codex_settings.unapplied", &[("n", n)]),
                None,
            ),
            Notice::Waiting(n) => (
                Status::Warn,
                cx.tr.f("codex_settings.unapplied_waiting", &[("n", n)]),
                None,
            ),
            Notice::Notes(notes) => (
                Status::Idle,
                notes.first().cloned().unwrap_or_default(),
                (notes.len() > 1).then(|| notes.join("\n")),
            ),
        };
        ui.add(StatusDot::new(status));
        if let (Notice::Waiting(_), Some(profile)) = (&n, &model.profile_id) {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::ghost(cx.tr.t("codex_settings.apply_now"))
                            .small()
                            .enabled(!cx.busy),
                    )
                    .clicked()
                {
                    out.push(Action::Switch {
                        profile: profile.clone(),
                        line: None,
                        auth: None,
                    });
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(Text::small(&text, cx.pal)).truncate());
                });
            });
            return;
        }
        let r = ui.add(egui::Label::new(Text::small(&text, cx.pal)).truncate());
        r.on_hover_text(tip.unwrap_or(text));
    });
}

/// Human rendering of a value: on/off for booleans, bare strings.
pub(super) fn value_text(cx: &Cx<'_>, v: &Value) -> String {
    match v {
        Value::Bool(b) => cx.tr.t(if *b {
            "codex_settings.value.on"
        } else {
            "codex_settings.value.off"
        }),
        Value::Str(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The paged list of two-line rows ("New in latest" is grouped by release).
fn list(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, model: &Model, docked: bool) {
    if state.filter.rail == super::model::Rail::New && !state.filter.searching() {
        if super::news::show(ui, cx, state, model) && !docked {
            state.sheet_open = true;
        }
        return;
    }
    let rows: Vec<Entry> = state.rows().into_iter().cloned().collect();
    if rows.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(Text::strong(cx.tr.t("codex_settings.empty.title"), cx.pal));
            ui.label(Text::small(cx.tr.t("codex_settings.empty.body"), cx.pal));
        });
        return;
    }
    let zh = cx.tr.locale() == Locale::ZhCn;
    let total = rows.len();
    let columns = Table::new(&[1.0, 0.0]).min_widths(&[0.0, CHIP_W]);
    let pending = state.pending.clone();
    let r = PagedTable::new("codex-settings-list", columns, total, &mut state.table)
        .label(cx.tr.t(crate::pages::Page::Settings.title_key()))
        .row_height(size::ROW_LIST2)
        .without_pager()
        .keys_when_idle(true)
        .show(ui, |ui, row, col| {
            let e = &rows[row];
            if col == 0 {
                row_text(ui, cx, model, e, &pending, zh);
            } else {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    value_pill(ui, cx, e, &pending);
                });
            }
        });
    if !docked && (r.activated.is_some() || (r.selection_changed && r.response.has_focus())) {
        state.sheet_open = true;
    }
}

/// Marker, title (+ exceptional badges) and the second line of a row.
fn row_text(
    ui: &mut Ui,
    cx: &Cx<'_>,
    model: &Model,
    e: &Entry,
    pending: &super::model::Pending,
    zh: bool,
) {
    let (marker, _) = ui.allocate_exact_size(vec2(8.0, 40.0), Sense::hover());
    if pending.get(&e.path).is_some() {
        ui.painter()
            .circle_filled(marker.center(), 3.0, cx.pal.status(Status::Warn));
    } else if e.managed {
        ui.painter()
            .circle_filled(marker.center(), 3.0, cx.pal.text);
    }
    let title = e.title_for(zh);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let t = if title.is_path {
                RichText::new(title.text)
                    .font(mono_font(text::SMALL, Weight::Medium))
                    .color(cx.pal.text)
            } else {
                RichText::new(title.text)
                    .font(font(text::BODY, Weight::Medium))
                    .color(cx.pal.text)
            };
            ui.add(egui::Label::new(t).truncate());
            if title.zh_missing {
                ui.add(Badge::new(cx.tr.t("codex_settings.zh_missing")).outline());
            }
            if e.stage != Stage::Stable {
                ui.add(Badge::new(cx.tr.t(e.stage.label_key())).outline());
            }
            if let Some(v) = e.needs_version() {
                ui.add(
                    Badge::new(cx.tr.f("codex_settings.needs_version", &[("version", &v)]))
                        .status(Status::Warn),
                );
            }
            if let Some(os) = &e.platform {
                ui.add(Badge::new(cx.tr.f("codex_settings.only_on", &[("os", os)])).outline());
            }
        });
        if let Some(sub) = e.subtitle_for(zh) {
            let t = if title.is_path {
                Text::caption(sub, cx.pal)
            } else {
                RichText::new(sub)
                    .font(mono_font(text::CAPTION, Weight::Regular))
                    .color(cx.pal.text_subtle)
            };
            ui.add(egui::Label::new(t).truncate());
        }
    });
    let _ = model;
}

/// The effective value as a compact chip (muted when it is the default): one
/// line of at most [`CHIP_W`] points, shortened by the value's shape, "+N lines"
/// / "+N more" for the rest, the whole value in the tooltip.
fn value_pill(ui: &mut Ui, cx: &Cx<'_>, e: &Entry, pending: &super::model::Pending) {
    let configured = e.value_with(pending);
    let (shown, muted) = match (configured, &e.default) {
        (Some(v), _) => (super::editor::shown(cx, v), false),
        (None, Some(d)) => (super::editor::shown(cx, d), true),
        (None, None) => (
            values::Shown::label(cx.tr.t("codex_settings.value.unset")),
            true,
        ),
    };
    values::chip(ui, cx, &shown, CHIP_W, muted);
}

/// Footer: pager under the list column, pending bar on the right. Returns true
/// when "save" was chosen.
fn footer(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    model: &Model,
    list_x: egui::Rangef,
) -> bool {
    let rect = ui.max_rect();
    let total = state.rows().len();
    let per = state.table.per_page.max(1);
    let narrow = rect.width() < 900.0;
    let pager_rect = egui::Rect::from_x_y_ranges(list_x.min..=list_x.max, rect.y_range());
    let mut page = state.table.page;
    // "New in latest" scrolls its grouped list instead of paging.
    let grouped = state.filter.rail == super::model::Rail::New && !state.filter.searching();
    let changed = !grouped
        && layout::fixed(ui, pager_rect, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let mut p = Pager::new("codex-settings-pager", &mut page, total, per).labels(
                    cx.tr.t("codex_settings.pager.prev"),
                    cx.tr.t("codex_settings.pager.next"),
                );
                if narrow {
                    p = p.compact();
                }
                p.show(ui).changed()
            })
            .inner
        });
    if changed {
        state.table.page = page;
        let visible = state.table.visible(total);
        if state.table.selected.is_some_and(|s| !visible.contains(&s)) {
            state.table.selected = (total > 0).then_some(visible.start);
        }
    }
    let count = state.pending.len();
    if count == 0 {
        return false;
    }
    let blocked = super::source::blocked_reason(cx, model);
    let labels = PendingLabels {
        count: cx.tr.f("codex_settings.pending.count", &[("n", &count)]),
        diff: cx.tr.t("codex_settings.pending.diff"),
        discard: cx.tr.t("codex_settings.pending.discard"),
        save: cx.tr.t(if model.profile_applied {
            "codex_settings.pending.save_apply"
        } else {
            "codex_settings.pending.save"
        }),
    };
    let bar_rect = egui::Rect::from_x_y_ranges(
        (list_x.max.min(rect.max.x - 360.0)).max(rect.min.x)..=rect.max.x,
        rect.y_range(),
    );
    let action = layout::fixed(ui, bar_rect, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            PendingBar::new(count, labels)
                .saving(state.saving.is_some())
                .enabled(blocked.is_none())
                .error(state.save_error.clone())
                .show(ui)
        })
        .inner
    });
    match action {
        PendingAction::Save => return true,
        PendingAction::Discard => {
            state.pending.clear();
            state.buffers.clear();
            state.invalid.clear();
            state.save_error = None;
        }
        PendingAction::Diff => state.diff_open = true,
        PendingAction::None => {}
    }
    false
}

/// Diff dialog: key, before → after, and the local Codex versions that don't
/// know a key.
fn diff_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, model: &Model) {
    let lines = diff(model, &state.pending);
    let mut close = false;
    let mut save = false;
    let mut drop: Option<String> = None;
    let r = dialog(ctx, egui::Id::unique("codex-settings-diff"), 600.0, |ui| {
        ui.label(Text::h2(
            cx.tr
                .f("codex_settings.pending.title", &[("n", &lines.len())]),
            cx.pal,
        ));
        dialog_body(ui, |ui| {
            ui.add(
                egui::Label::new(Text::small(cx.tr.t("codex_settings.pending.note"), cx.pal))
                    .wrap(),
            );
            for line in &lines {
                if diff_row(ui, cx, model, line) {
                    drop = Some(line.path.clone());
                }
            }
            let migrating = lines.iter().any(|l| {
                l.after.is_none() && model.entry(&l.path).is_some_and(|e| e.legacy.is_some())
            });
            if migrating {
                ui.add(
                    egui::Label::new(Text::caption(
                        cx.tr.t("codex_settings.migrate.restore_note"),
                        cx.pal,
                    ))
                    .wrap(),
                );
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let blocked = super::source::blocked_reason(cx, model);
            if ui
                .add(
                    Button::primary(cx.tr.t(if model.profile_applied {
                        "codex_settings.pending.save_apply"
                    } else {
                        "codex_settings.pending.save"
                    }))
                    .small()
                    .enabled(blocked.is_none() && !lines.is_empty()),
                )
                .clicked()
            {
                save = true;
            }
            if ui
                .add(
                    Button::ghost(cx.tr.t("codex_settings.pending.close"))
                        .small()
                        .kbd("Esc"),
                )
                .clicked()
            {
                close = true;
            }
        });
    });
    if let Some(path) = drop {
        state.pending.discard(&path);
        state.buffers.remove(&path);
    }
    if save {
        state.diff_open = false;
        state.save(cx);
    } else if close || r.should_close() {
        state.diff_open = false;
    }
}

/// One row of the diff dialog in fixed columns — path, `before → after` (each
/// shortened to its half, never wider), a warning mark when a local Codex does
/// not know the key, the chevron that opens the full change below the row, and
/// "drop this edit". Returns true when the edit is dropped.
fn diff_row(ui: &mut Ui, cx: &Cx<'_>, model: &Model, line: &super::model::DiffLine) -> bool {
    let unset = values::Shown::label(cx.tr.t("codex_settings.value.unset"));
    let shown = |v: &Option<Value>| {
        v.as_ref()
            .map(|v| super::editor::shown(cx, v))
            .unwrap_or_else(|| unset.clone())
    };
    let (before, after) = (shown(&line.before), shown(&line.after));
    let names: Vec<String> = line
        .unsupported
        .iter()
        .map(|k| {
            model
                .targets
                .iter()
                .find(|t| &t.key == k)
                .map(|t| t.label.clone())
                .unwrap_or_default()
        })
        .collect();
    let id = egui::Id::unique(("codex-settings-diff-open", &line.path));
    let width = ui.available_width();
    let mut dropped = false;
    let (row, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 4.0;
    if child
        .add(
            crate::design::widgets::IconButton::new(
                Icon::Close,
                cx.tr.t("codex_settings.pending.drop"),
            )
            .size(24.0),
        )
        .clicked()
    {
        dropped = true;
    }
    let path_w = (width * 0.34).min(200.0);
    // Drop button, chevron, warning mark and their gaps on the right.
    let change_w = width - path_w - (24.0 + 22.0 + 20.0 + 4.0 * 4.0) - 12.0;
    let side = ((change_w - 22.0) / 2.0).max(24.0);
    let cut = values::cut(&child, &before, side - 2.0) || values::cut(&child, &after, side - 2.0);
    if cut || values::is_open(&child, id) {
        values::disclosure(&mut child, cx, id);
    } else {
        child.add_space(22.0);
    }
    let (mark, mark_r) = child.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    if !names.is_empty() {
        icons::paint(
            child.painter(),
            egui::Rect::from_center_size(mark.center(), vec2(14.0, 14.0)),
            Icon::Warning,
            cx.pal.status(Status::Warn),
        );
        mark_r.on_hover_text(cx.tr.f(
            "codex_settings.pending.unsupported",
            &[("targets", &names.join(", "))],
        ));
    }
    child.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let f = mono_font(text::CAPTION, Weight::Regular);
        let p = crate::design::widgets::value_chip::fit(
            ui,
            &line.path,
            &f,
            path_w,
            crate::design::elide::Ellipsis::Middle,
        );
        let (r, resp) = ui.allocate_exact_size(vec2(path_w, 20.0), Sense::hover());
        ui.painter().text(
            egui::pos2(r.min.x, r.center().y),
            egui::Align2::LEFT_CENTER,
            &p,
            f,
            cx.pal.text,
        );
        if p != line.path {
            resp.on_hover_text(&line.path);
        }
        values::change(ui, cx, &before, &after, change_w);
    });
    if values::is_open(ui, id) {
        if !names.is_empty() {
            ui.add(
                Badge::new(cx.tr.f(
                    "codex_settings.pending.unsupported",
                    &[("targets", &names.join(", "))],
                ))
                .status(Status::Warn),
            );
        }
        values::full_diff(
            ui,
            cx,
            id.with("full"),
            line.before.as_ref().map(|_| &before),
            line.after.as_ref().map(|_| &after),
            &cx.tr.t("codex_settings.value.unset"),
            180.0,
        );
        ui.add_space(4.0);
    }
    dropped
}

/// Availability icon of one local consumer or host: a check or a cross, `None`
/// when unknown (shown as "?"). Drawn, since the bundled fonts have no check mark.
pub(super) fn avail_icon(a: Availability) -> Option<Icon> {
    match a {
        Availability::Yes => Some(Icon::Check),
        Availability::No => Some(Icon::Close),
        Availability::Unknown => None,
    }
}

/// The availability mark inline with small text: [`avail_icon`], or "?".
pub(super) fn avail_mark(ui: &mut Ui, cx: &Cx<'_>, a: Availability) {
    match avail_icon(a) {
        Some(icon) => {
            let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            icons::paint(ui.painter(), rect, icon, cx.pal.text_muted);
        }
        None => {
            ui.label(Text::small("?", cx.pal));
        }
    }
}
