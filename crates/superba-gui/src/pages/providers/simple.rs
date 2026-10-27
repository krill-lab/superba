//! Codex's simple provider view: select, inspect lines, and save an API key.

use egui::{Align, Layout, ScrollArea, Ui};
use superba_core::{KeyInput, Secret, SiteKind};

use super::{KeyEdit, State, account, detail, list, mixed};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, space};
use crate::design::widgets::{
    Badge, Button, Card, KeyInput as KeyField, Latency, LatencyBadge, PagedTable, Status,
    StatusDot, Table, Text, paint_row,
};
use crate::pages::shared::providers::{self, Provider, ProviderId, ProviderKind};
use crate::pages::shared::{new_profile, ordered_lines};
use crate::pages::{Action, Cx, Page, elided, error_text};

/// Draw the limited view until its explicit advanced-settings action is used.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if state.simple_advanced {
        super::show(ui, cx, state, out);
        return;
    }
    if state.poll(cx, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    ui.horizontal(|ui| {
        ui.label(Text::page_title(
            cx.tr.t(Page::Providers.title_key()),
            cx.pal,
        ));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("providers.simple.advanced"))
                        .small()
                        .enabled(!cx.busy),
                )
                .clicked()
            {
                state.simple_advanced = true;
            }
        });
    });
    ui.add_space(12.0);
    let all = providers::list(cx.data);
    let body = ui.available_rect_before_wrap();
    let cols = layout::column_rects(body, &[Track::Fixed(240.0), Track::Fill], 24.0);
    layout::fixed(ui, cols[0], |ui| provider_list(ui, cx, state, &all));
    ui.painter().vline(
        cols[1].min.x - 12.0,
        cols[1].y_range(),
        egui::Stroke::new(1.0, cx.pal.border),
    );
    let current = current(state, &all).cloned();
    layout::fixed(ui, cols[1], |ui| {
        ScrollArea::vertical()
            .id_salt("simple-provider-detail")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(p) = &current {
                    provider_detail(ui, cx, state, p, out);
                }
            });
    });
}

fn provider_list(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, all: &[Provider]) {
    let current = current(state, all).and_then(|p| all.iter().position(|x| x.id == p.id));
    if state.list.selected != current {
        state.list.selected = current;
        if let Some(i) = current {
            state.list.select(i, all.len());
        }
    }
    let response = PagedTable::new(
        "simple-providers",
        Table::new(&[1.0]),
        all.len(),
        &mut state.list,
    )
    .label(cx.tr.t(Page::Providers.title_key()))
    .row_name(|row| simple_row(cx, &all[row]).accessible_name())
    .row_height(size::ROW_LIST2)
    .tiles(space::XS)
    .show(ui, |ui, row, _| {
        let rect = ui.max_rect();
        paint_row(ui, rect, &simple_row(cx, &all[row]));
        ui.allocate_rect(rect, egui::Sense::hover());
    });
    if response.selection_changed
        && let Some(p) = state.list.selected.and_then(|i| all.get(i))
    {
        state.select(p.id.clone());
    }
}

/// Saving the first key turns a site entry into a profile entry. Keep the site
/// selected while the backend is saving and the updated profile list is pending.
fn current<'a>(state: &State, all: &'a [Provider]) -> Option<&'a Provider> {
    if let Some(ProviderId::Profile(id)) = &state.selected
        && !all.iter().any(|provider| provider.profile_id() == Some(id))
        && let Some(site) = all
            .iter()
            .find(|provider| provider.profile.is_none() && provider.site.id.as_str() == id.as_str())
    {
        return Some(site);
    }
    state.current(all)
}

fn simple_row(cx: &Cx<'_>, p: &Provider) -> crate::design::widgets::RowContent {
    let mut row = list::row_content(cx, p);
    row.subtitle = (!p.has_key()).then(|| cx.tr.t("providers.no_key"));
    row.secondary = None;
    row.value = p.applied.then(|| cx.tr.t("providers.applied"));
    row
}

fn provider_detail(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    p: &Provider,
    out: &mut Vec<Action>,
) {
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.horizontal_wrapped(|ui| {
        ui.label(Text::section(list::name(cx, p), cx.pal));
        if p.applied {
            ui.add(Badge::new(cx.tr.t("providers.applied")).status(Status::Ok));
        } else {
            let label = cx.tr.f(
                "providers.simple.switch_to",
                &[("name", &list::name(cx, p))],
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        Button::primary(label)
                            .small()
                            .enabled(!cx.busy && p.has_key()),
                    )
                    .clicked()
                {
                    if let Some(profile) = &p.profile {
                        out.push(Action::Switch {
                            profile: profile.id.clone(),
                            line: None,
                            auth: None,
                        });
                    }
                }
            });
        }
    });
    ui.add_space(6.0);
    if p.kind == ProviderKind::Official {
        ui.label(Text::body(cx.tr.t("providers.simple.official"), cx.pal));
        return;
    }
    Card::new()
        .padding(12)
        .show(ui, |ui| line_status(ui, cx, state, p, out));
    Card::new()
        .padding(12)
        .show(ui, |ui| account::show_simple(ui, cx, state, p));
    if p.site.kind == SiteKind::Krill {
        Card::new().padding(12).show(ui, |ui| {
            if cx.data.router_enabled() {
                mixed::show_simple(
                    ui,
                    cx,
                    &mut state.mixed,
                    out,
                    "providers.simple.remote_on",
                    None,
                );
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label(Text::body(cx.tr.t("providers.simple.remote_off"), cx.pal));
                    if ui
                        .add(Button::new(cx.tr.t("providers.simple.enable_router")).small())
                        .clicked()
                    {
                        out.push(Action::Go(Page::Router));
                    }
                });
            }
        });
    }
    Card::new()
        .padding(12)
        .show(ui, |ui| key(ui, cx, state, p, out));
}

fn line_status(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider, out: &mut Vec<Action>) {
    let current = if cx.data.router_failover_enabled() {
        state.router.tick(ui, cx);
        p.profile_id()
            .and_then(|id| state.router.route(id))
            .map(|route| superba_core::LineId::new(&route.line_id))
    } else {
        p.profile
            .as_ref()
            .and_then(|profile| crate::pages::shared::applied_line(cx.data, profile))
    };
    let probing = cx.data.probing_site(&p.site.id);
    if !p.site.lines.is_empty()
        && !cx.data.caps.contains_key(&p.site.id)
        && state.probed.insert(p.site.id.clone())
    {
        out.push(Action::Probe(p.site.id.clone()));
    }
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("providers.simple.refresh"))
                        .icon(Icon::Refresh)
                        .small()
                        .loading(probing || state.check_task.is_some())
                        .enabled(!cx.busy && !probing && state.check_task.is_none()),
                )
                .on_hover_text(cx.tr.t("lines.key.cost"))
                .clicked()
            {
                out.push(Action::Probe(p.site.id.clone()));
                if let Some(profile) = p.profile.as_ref().filter(|pr| pr.key_ref.is_some()) {
                    detail::start_checks(state, cx, p, KeyInput::Profile(profile.id.clone()));
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(Text::strong(cx.tr.t("providers.simple.lines"), cx.pal));
            });
        });
    });
    if let Some(error) = cx
        .data
        .caps
        .get(&p.site.id)
        .and_then(|load| load.error.as_ref())
    {
        crate::pages::note(ui, cx, Status::Error, error_text(cx.tr, error));
    }
    for (index, line) in ordered_lines(&p.site, p.profile.as_ref())
        .into_iter()
        .enumerate()
    {
        let caps = cx.data.site_caps(&p.site.id, &line.id);
        let latency = if probing {
            Latency::Probing
        } else {
            match caps {
                Some(caps) if !caps.reachable => Latency::Unreachable,
                Some(caps) => caps.latency_ms.map(Latency::Ms).unwrap_or(Latency::Unknown),
                None => Latency::Unknown,
            }
        };
        if index > 0 {
            crate::pages::divider(ui, cx);
        }
        let active = p.applied && current.as_ref() == Some(&line.id);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let row = egui::Rect::from_min_size(
                    ui.cursor().min,
                    egui::vec2(ui.available_width(), 58.0),
                );
                let columns = layout::column_rects(row, &[Track::Fill, Track::Fixed(90.0)], 12.0);
                layout::fixed(ui, columns[1], |ui| {
                    ui.add(LatencyBadge::new(latency));
                    if active {
                        ui.add(Badge::new(cx.tr.t("connection.lines.current")).status(Status::Ok));
                    } else if ui
                        .add(
                            Button::new(cx.tr.t("providers.simple.switch"))
                                .small()
                                .enabled(
                                    !cx.busy
                                        && p.has_key()
                                        && !p
                                            .profile
                                            .as_ref()
                                            .is_some_and(|pr| pr.is_line_disabled(&line.id)),
                                ),
                        )
                        .on_hover_text(cx.tr.f(
                            "providers.simple.switch_to",
                            &[("name", &cx.tr.text(&line.name))],
                        ))
                        .clicked()
                    {
                        if let Some(profile) = &p.profile {
                            out.push(if p.applied {
                                crate::pages::shared::switch_action(cx.data, profile, &line.id)
                            } else {
                                Action::Switch {
                                    profile: profile.id.clone(),
                                    line: Some(line.id.clone()),
                                    auth: None,
                                }
                            });
                        }
                    }
                });
                layout::fixed(ui, columns[0], |ui| {
                    elided(ui, Text::strong(cx.tr.text(&line.name), cx.pal));
                    line_health(ui, cx, state, p, &line.id, caps);
                });
            });
    }
}

fn line_health(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &State,
    p: &Provider,
    line: &superba_core::LineId,
    caps: Option<&superba_core::LineCaps>,
) {
    if p.profile
        .as_ref()
        .is_some_and(|profile| profile.is_line_disabled(line))
    {
        ui.add(Badge::new(cx.tr.t("connection.lines.disabled")).outline());
        return;
    }
    let checked = state
        .checks
        .get(&p.id)
        .and_then(|checks| checks.iter().find(|(id, _, _)| id == line))
        .map(|(_, _, result)| result);
    let (status, label) = if state.check_task.as_ref().is_some_and(|(id, _)| id == &p.id) {
        (Status::Busy, cx.tr.t("lines.key.checking"))
    } else {
        match checked {
            Some(Ok(check)) if check.access.is_ok() => {
                (Status::Ok, cx.tr.t("providers.simple.ready"))
            }
            Some(Ok(check)) => (Status::Error, cx.tr.text(&check.access.text())),
            Some(Err(error)) => (Status::Error, error_text(cx.tr, error)),
            None if caps.is_some_and(|caps| caps.reachable && caps.codex_path.is_some()) => {
                (Status::Ok, cx.tr.t("providers.simple.ready"))
            }
            None if caps.is_some() => (Status::Error, cx.tr.t("providers.simple.unavailable")),
            None => (Status::Idle, cx.tr.t("providers.simple.not_tested")),
        }
    };
    ui.horizontal(|ui| {
        ui.add(StatusDot::new(status));
        elided(ui, Text::small(label, cx.pal));
    });
}

fn key(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider, out: &mut Vec<Action>) {
    let editor_top = ui.cursor().min;
    let reveal_id = ui.scope_id().with(("simple-key-reveal", p.id.command()));
    ui.label(Text::strong(cx.tr.t("providers.row.key"), cx.pal));
    ui.add_space(8.0);
    let profile = p.profile.as_ref();
    let stored = profile.and_then(|profile| profile.key_ref.as_ref());
    let editing = state
        .key_edit
        .as_ref()
        .is_some_and(|edit| edit.provider.as_ref() == Some(&p.id));
    if stored.is_some() && !editing {
        ui.horizontal_wrapped(|ui| {
            let suffix = crate::pages::profile_key_suffix(ui, cx, profile);
            let label = suffix
                .map(|suffix| format!("•••• {suffix}"))
                .unwrap_or_else(|| cx.tr.t("providers.simple.key_saved"));
            ui.label(Text::body(label, cx.pal));
            if ui
                .add(
                    Button::new(cx.tr.t("providers.key.replace"))
                        .small()
                        .enabled(!cx.busy),
                )
                .clicked()
            {
                state.key_edit = Some(KeyEdit {
                    provider: Some(p.id.clone()),
                    ..KeyEdit::default()
                });
                ui.ctx().data_mut(|data| data.insert_temp(reveal_id, true));
            }
        });
        return;
    }
    if !editing {
        state.key_edit = Some(KeyEdit {
            provider: Some(p.id.clone()),
            ..KeyEdit::default()
        });
    }
    let placeholder = if p.site.kind == SiteKind::Krill {
        "nb_…".to_owned()
    } else {
        cx.tr.t("connection.key.placeholder_compatible")
    };
    let edit = state.key_edit.as_mut().expect("key editor initialized");
    let response = ui
        .add_enabled_ui(!cx.busy, |ui| {
            KeyField::new(
                ("simple-provider-key", p.id.command()),
                &mut edit.key,
                &mut edit.reveal,
                placeholder,
                cx.tr.t("lines.key.reveal"),
            )
            .show(ui)
        })
        .inner;
    let reveal = ui
        .ctx()
        .data(|data| data.get_temp::<bool>(reveal_id))
        .unwrap_or(false);
    if reveal {
        response.request_focus();
        ui.ctx().data_mut(|data| data.insert_temp(reveal_id, false));
    }
    let has_text = !edit.key.trim().is_empty();
    let mut save = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let mut check = false;
    let mut cancel = false;
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        save |= ui
            .add(
                Button::primary(cx.tr.t("lines.key.save"))
                    .small()
                    .enabled(has_text && !cx.busy),
            )
            .clicked();
        check = ui
            .add(
                Button::ghost(cx.tr.t("providers.key.check"))
                    .small()
                    .loading(state.check_task.is_some())
                    .enabled(has_text && !cx.busy && state.check_task.is_none()),
            )
            .on_hover_text(cx.tr.t("lines.key.cost"))
            .clicked();
        if stored.is_some() {
            cancel = ui
                .add(Button::ghost(cx.tr.t("action.cancel")).small())
                .clicked();
        }
    });
    if reveal || response.has_focus() {
        ui.scroll_to_rect(
            egui::Rect::from_min_max(editor_top, ui.min_rect().max),
            Some(Align::Max),
        );
    }
    let typed = Secret::new(edit.key.trim().to_owned());
    if check {
        detail::start_checks(state, cx, p, KeyInput::Secret(typed.clone()));
    }
    if save && has_text && !cx.busy {
        let profile = profile
            .cloned()
            .unwrap_or_else(|| new_profile(&p.site, cx.data.site_name(cx.tr, &p.site)));
        state.simple_accounts.remove(&profile.id);
        state.selected = Some(ProviderId::Profile(profile.id.clone()));
        out.push(Action::SaveProfile {
            profile: Box::new(profile),
            key: Some(typed),
        });
        state.key_edit = None;
        // A check in flight used the old or a previously typed credential.
        state.check_task = None;
        state.checks.remove(&p.id);
        super::transports::forget(ui, p);
    } else if cancel {
        state.key_edit = None;
    }
}
