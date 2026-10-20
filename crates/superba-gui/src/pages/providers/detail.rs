//! The detail of the selected provider (UX §3.3): header (name, kind, in use,
//! homepage, "switch to this provider", Test and Delete as buttons; rename and
//! duplicate live in the list's right-click menu), the tabs of a key-based
//! provider, the official login's own detail, and the delete confirmation.

use egui::{Align, Layout, RichText, Ui};
use superba_core::{KeyInput, Profile, ProfileId, RestoreTarget};

use super::{State, Tab, actions, form, ops};
use crate::design::fonts::{Weight, font};
use crate::design::layout;
use crate::design::tokens::{size, text};
use crate::design::widgets::{
    Badge, Button, Segmented, Status, Text, dialog, dialog_body, link_button,
};
use crate::pages::shared::providers::{Provider, ProviderKind};
use crate::pages::shared::{auto_facts, decision_with, ordered_lines};
use crate::pages::{Action, Cx, section_gap};

/// What "switch to this provider" does (`None` without a key).
pub(super) fn switch_action(p: &Provider) -> Option<Action> {
    if p.kind == ProviderKind::Official {
        return Some(Action::Restore(RestoreTarget::Official));
    }
    let profile = p.profile.as_ref().filter(|p| p.key_ref.is_some())?;
    Some(Action::Switch {
        profile: profile.id.clone(),
        line: None,
        auth: None,
    })
}

/// Draw the detail of `p`.
pub(super) fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    p: &Provider,
    out: &mut Vec<Action>,
) {
    header(ui, cx, state, p, out);
    ui.add_space(10.0);
    if p.kind == ProviderKind::Official {
        official(ui, cx, out);
        return;
    }
    let account = p.has_key() && p.profile.is_some();
    if !account {
        state.tab = Tab::Connection;
    }
    ui.horizontal(|ui| {
        let mut tab = state.tab;
        let mut seg = Segmented::new("providers-tab", &mut tab)
            .label(super::list::name(cx, p))
            .option(Tab::Connection, cx.tr.t("providers.tab.connection"))
            .small();
        if account {
            seg = seg.option(Tab::Account, cx.tr.t("providers.tab.account"));
        }
        seg.show(ui);
        state.tab = tab;
    });
    ui.add_space(12.0);
    let id = egui::Id::unique(("providers-detail-tab", p.id.command(), state.tab));
    // The Account tab fills its height (a paged table): it keeps the height it
    // has at the Fit minimum and scrolls below that; the Connection tab scrolls
    // when its rows are taller than the pane.
    let min = match state.tab {
        Tab::Connection => 0.0,
        Tab::Account => size::FIT_MIN_BODY - (ui.cursor().min.y - ui.max_rect().min.y),
    };
    overflow_scroll(ui, id, min, |ui| match state.tab {
        Tab::Connection => super::connection::show(ui, cx, state, p, out),
        Tab::Account => super::account::show(ui, cx, state, p),
    });
}

/// How tall a tab was and whether it scrolled (egui temp data per tab).
#[derive(Clone, Copy, Debug, Default)]
struct Overflow {
    height: f32,
    scrolling: bool,
}

/// Draw `add` in the rest of the pane; in a vertical scroll area only while it
/// needs more than that: its height last frame, or `min` for a tab that fills
/// what it gets (it then gets `min`). The Fit sizes keep a pane without a
/// scroll area; a short window (960×640) cuts nothing off at the bottom.
fn overflow_scroll(ui: &mut Ui, id: egui::Id, min: f32, add: impl FnOnce(&mut Ui)) {
    let room = ui.available_height();
    let last: Overflow = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    let need = last.height.max(min);
    // A few points of hysteresis: a tab just at the edge does not flip.
    let scrolling = if last.scrolling {
        need > room - 8.0
    } else {
        need > room + 0.5
    };
    let height = if scrolling {
        egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if min > 0.0 {
                    let rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(ui.available_width(), min.max(room)),
                    );
                    layout::fixed(ui, rect, add);
                } else {
                    add(ui);
                }
            })
            .content_size
            .y
    } else {
        ui.scope(add).response.rect.height()
    };
    let flips = if scrolling {
        height.max(min) <= room - 8.0
    } else {
        height.max(min) > room + 0.5
    };
    if flips {
        ui.ctx().request_repaint();
    }
    ui.ctx()
        .data_mut(|d| d.insert_temp(id, Overflow { height, scrolling }));
}

/// Name, badges, homepage and the actions.
fn header(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider, out: &mut Vec<Action>) {
    let name = super::list::name(cx, p);
    let narrow = ui.available_width() < 600.0;
    ui.horizontal(|ui| {
        ui.set_min_height(32.0);
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.add(
            egui::Label::new(
                RichText::new(&name)
                    .font(font(text::H2, Weight::Semibold))
                    .color(cx.pal.text),
            )
            .truncate(),
        );
        ui.add(Badge::new(cx.tr.t(p.kind.badge_key())).outline());
        if p.applied {
            ui.add(Badge::new(cx.tr.t("providers.applied")).status(Status::Ok));
        }
        if !narrow {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                header_actions(ui, cx, state, p, out);
            });
        }
    });
    if narrow {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                header_actions(ui, cx, state, p, out);
            });
        });
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if let Some(home) = &p.site.homepage {
            let host = home.host_str().unwrap_or(home.as_str()).to_owned();
            if link_button(ui, &host, true).clicked() {
                out.push(Action::OpenUrl(home.to_string()));
            }
            ui.label(Text::small("·", cx.pal));
        }
        let what = match p.kind {
            ProviderKind::Preset => cx
                .tr
                .f("providers.builtin_lines", &[("n", &p.site.lines.len())]),
            ProviderKind::Custom => form::base_url(&p.site),
            ProviderKind::Official => cx.tr.t("providers.official.what"),
        };
        ui.add(egui::Label::new(Text::small(what, cx.pal)).truncate());
    });
}

/// Header actions get a separate line when the detail column is narrow.
fn header_actions(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    p: &Provider,
    out: &mut Vec<Action>,
) {
    buttons(ui, cx, state, p, out);
    let label = if p.kind == ProviderKind::Official {
        cx.tr.t("providers.official.use")
    } else {
        cx.tr.t("providers.use")
    };
    let action = switch_action(p);
    let r = ui.add(
        Button::primary(label)
            .small()
            .enabled(action.is_some() && !cx.busy && !p.applied),
    );
    let r = if p.applied {
        r.on_disabled_hover_text(cx.tr.t("providers.use_applied"))
    } else if action.is_none() {
        r.on_disabled_hover_text(cx.tr.t("providers.use_needs_key"))
    } else {
        r
    };
    if r.clicked()
        && let Some(a) = action
    {
        out.push(a);
    }
}

/// Test and Delete (right to left: Delete is the outermost). Delete opens the
/// confirmation; a refused delete is disabled and says why on hover. The
/// official login has neither.
fn buttons(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, p: &Provider, out: &mut Vec<Action>) {
    if p.kind == ProviderKind::Official {
        return;
    }
    let refusal = actions::delete_refusal(cx, p);
    let delete = ui.add(
        Button::danger(cx.tr.t("providers.delete.button"))
            .small()
            .enabled(refusal.is_none() && !cx.busy && !state.is_modal_open()),
    );
    let delete = match &refusal {
        Some(why) => delete.on_disabled_hover_text(why),
        None => delete,
    };
    if delete.clicked() {
        actions::open_remove(state, p);
    }
    let testing = actions::testing(state, cx, p);
    if ui
        .add(
            Button::new(cx.tr.t("providers.test.button"))
                .small()
                .loading(testing),
        )
        .on_hover_text(cx.tr.t("providers.menu.test"))
        .clicked()
        && !testing
    {
        actions::test(state, cx, p, out);
    }
}

/// A copy of `profile` as a new provider (same site, lines, auth choice, model
/// defaults and Codex settings; no key yet).
pub(super) fn duplicate(cx: &Cx<'_>, profile: &Profile) -> Profile {
    let set = cx.data.profiles.value.as_ref();
    let names: Vec<&str> = set
        .map(|s| s.profiles.iter().map(|p| p.name.as_str()).collect())
        .unwrap_or_default();
    let ids: Vec<&str> = set
        .map(|s| {
            s.profiles
                .iter()
                .map(|p| p.id.as_str())
                .chain(s.custom_sites.iter().map(|s| s.id.as_str()))
                .chain(cx.data.sites.iter().map(|s| s.id.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let name = form::unique_name(
        &cx.tr
            .f("providers.duplicate_name", &[("name", &profile.name)]),
        &names,
    );
    let mut copy = profile.clone();
    copy.id = ProfileId::new(form::slug_id(&name, &ids));
    copy.name = name;
    copy.key_ref = None;
    copy.builtin = None;
    copy
}

/// Probe the provider's lines and check its key on every line (custom sites
/// also get the site test: models, balance).
pub(super) fn test_connection(state: &mut State, cx: &Cx<'_>, p: &Provider, out: &mut Vec<Action>) {
    if p.kind == ProviderKind::Official {
        return;
    }
    out.push(Action::Probe(p.site.id.clone()));
    let Some(profile) = p.profile.as_ref().filter(|p| p.key_ref.is_some()) else {
        return;
    };
    start_checks(state, cx, p, KeyInput::Profile(profile.id.clone()));
    if p.kind == ProviderKind::Custom && !state.tests.contains_key(&p.site.id) {
        let service = cx.service.clone();
        let (site, id) = (p.site.id.clone(), profile.id.clone());
        state.tests.insert(
            p.site.id.clone(),
            crate::runtime::task(async move { Ok(ops::test_saved(service, site, id).await) }),
        );
    }
}

/// Check `key` on every line of `p` with the mode each line resolves to.
pub(super) fn start_checks(state: &mut State, cx: &Cx<'_>, p: &Provider, key: KeyInput) {
    if state.check_task.is_some() {
        return;
    }
    let lines = ordered_lines(&p.site, p.profile.as_ref())
        .into_iter()
        .map(|line| {
            let caps = cx.data.site_caps(&p.site.id, &line.id);
            let mode = decision_with(
                &p.site,
                Some(&line),
                caps,
                p.profile.as_ref(),
                auto_facts(cx.data),
            )
            .mode
            .unwrap_or(superba_core::AuthMode::ApiKey);
            (line.id, mode)
        })
        .collect();
    let service = cx.service.clone();
    let site = p.site.id.clone();
    state.check_task = Some((
        p.id.clone(),
        crate::runtime::task(ops::check_lines(service, site, lines, key)),
    ));
}

/// The official login: what it does, switch to it, restore the original.
fn official(ui: &mut Ui, cx: &Cx<'_>, out: &mut Vec<Action>) {
    ui.add(egui::Label::new(Text::body(cx.tr.t("providers.official.body"), cx.pal)).wrap());
    ui.add_space(8.0);
    let stash = match cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.login_stash.as_ref())
    {
        Some(info) => cx.tr.f(
            "providers.official.stash_saved",
            &[("when", &cx.ago(info.stashed_at))],
        ),
        None => cx.tr.t("providers.official.stash_none"),
    };
    ui.add(egui::Label::new(Text::small(stash, cx.pal)).wrap());
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        if cx
            .data
            .overview
            .value
            .as_ref()
            .is_some_and(|o| o.baseline_captured)
            && ui
                .add(
                    Button::new(cx.tr.t("providers.official.restore_original"))
                        .small()
                        .enabled(!cx.busy),
                )
                .clicked()
        {
            out.push(Action::restore_original());
        }
    });
    section_gap(ui, cx);
    ui.add(egui::Label::new(Text::small(cx.tr.t("providers.official.note"), cx.pal)).wrap());
}

/// Delete confirmation: what goes away, refused while the provider is in use.
pub(super) fn remove_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State) {
    let Some(r) = state.remove.as_mut() else {
        return;
    };
    let busy = r.task.is_some();
    let mut close = false;
    let name = super::list::name(cx, &r.provider);
    // The rules are checked again on the current data (a switch may have made
    // the provider the one in use since the dialog opened).
    let refusal = if busy {
        None
    } else {
        let now = crate::pages::shared::providers::list(cx.data)
            .into_iter()
            .find(|q| q.id == r.provider.id);
        actions::delete_refusal(cx, now.as_ref().unwrap_or(&r.provider))
    };
    let response = dialog(ctx, egui::Id::unique("providers-remove"), 480.0, |ui| {
        ui.label(Text::h2(
            cx.tr.f("providers.delete.title", &[("name", &name)]),
            cx.pal,
        ));
        dialog_body(ui, |ui| {
            let body = if r.provider.kind == ProviderKind::Custom {
                cx.tr.f("providers.delete.custom", &[("name", &name)])
            } else {
                cx.tr.f("providers.delete.confirm", &[("name", &name)])
            };
            ui.add(egui::Label::new(Text::muted(body, cx.pal)).wrap());
            if r.provider.kind == ProviderKind::Custom {
                ui.add(
                    egui::Label::new(Text::mono_muted(form::base_url(&r.provider.site), cx.pal))
                        .wrap(),
                );
            }
            if let Some(e) = refusal.as_ref().or(r.error.as_ref()) {
                ui.add(egui::Label::new(Text::small(e, cx.pal).color(cx.pal.red)).wrap());
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    Button::danger(
                        cx.tr
                            .f("providers.delete.confirm_button", &[("name", &name)]),
                    )
                    .loading(busy)
                    .enabled(!cx.busy && !busy && refusal.is_none()),
                )
                .clicked()
            {
                r.error = None;
                r.task = Some(crate::runtime::task(ops::remove_provider(
                    cx.service.clone(),
                    r.provider.profile_id().cloned(),
                    r.provider.site.id.clone(),
                    r.provider.kind == ProviderKind::Custom,
                )));
            }
            if ui
                .add(
                    Button::ghost(cx.tr.t("action.cancel"))
                        .kbd("Esc")
                        .enabled(!busy),
                )
                .clicked()
            {
                close = true;
            }
        });
    });
    if !busy && (close || response.should_close()) {
        state.remove = None;
    }
}
