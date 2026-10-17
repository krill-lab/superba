//! "Can the router be turned on?" (user bug 28). Router members need a key the
//! router can send as an API key; when no member has one, 开启 is disabled and
//! this panel says why, right under the banner, with the direct fixes: add an
//! eligible provider as a member, or enter an API key for a provider that has
//! none (or is set to PAT only), which opens the Providers page key field.
//! The rules are the switch service's ([`superba_switch::router_doctor`]), so
//! the page, `spb router enable` and `spb router doctor` say the same thing.

use egui::{Align, Layout, Ui};
use superba_core::SiteKind;
use superba_switch::router_doctor::{RouterCandidate, router_candidates};

use super::State;
use super::model::{Candidate, MemberKind};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Status, StatusDot, Text};
use crate::pages::{Action, Cx, Page};

/// Every provider as a possible member, from the loaded profiles and sites
/// (empty while the profiles load).
pub fn candidates(cx: &Cx<'_>) -> Vec<RouterCandidate> {
    cx.data
        .profiles
        .value
        .as_ref()
        .map(|set| router_candidates(set, &cx.data.sites))
        .unwrap_or_default()
}

/// At least one member can carry traffic. `true` while the profiles load, so
/// nothing flickers disabled at start.
pub fn has_eligible_member(cx: &Cx<'_>) -> bool {
    cx.data.profiles.value.is_none()
        || superba_switch::router_doctor::has_eligible_member(&candidates(cx))
}

/// The page command that opens the key field for `c` on the Providers page.
pub fn key_command(c: &RouterCandidate) -> String {
    match &c.profile_id {
        Some(id) => format!("key:{id}"),
        None => format!("site:{}", c.site_id),
    }
}

fn as_member(cx: &Cx<'_>, c: &RouterCandidate) -> Option<Candidate> {
    Some(Candidate {
        site_id: c.site_id.to_string(),
        profile_id: c.profile_id.as_ref()?.to_string(),
        name: cx.tr.text(&c.name),
        kind: match c.site_kind {
            Some(SiteKind::Krill) => MemberKind::Krill,
            _ => MemberKind::Compatible,
        },
    })
}

/// The explanation and the fixes; draws nothing when a member is eligible.
pub fn panel(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if has_eligible_member(cx) {
        return;
    }
    let all = candidates(cx);
    let busy = cx.busy || state.op.is_some();
    egui::Frame::new()
        .fill(cx.pal.surface)
        .stroke(egui::Stroke::new(1.0, cx.pal.border))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Warn));
                ui.label(Text::strong(cx.tr.t("router.eligibility.none"), cx.pal));
            });
            let fixable: Vec<&RouterCandidate> = all
                .iter()
                .filter(|c| c.addable() || c.eligibility.needs_key())
                .collect();
            if fixable.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(Text::small(
                        cx.tr.t("router.eligibility.no_candidates"),
                        cx.pal,
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                Button::new(cx.tr.t("router.eligibility.add_provider"))
                                    .icon(Icon::Plus)
                                    .small(),
                            )
                            .clicked()
                        {
                            out.push(Action::PageCommand {
                                page: Page::Providers,
                                command: "add".into(),
                            });
                        }
                    });
                });
                return;
            }
            for c in fixable {
                let name = cx.tr.text(&c.name);
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    let reason = cx.tr.f(
                        "router.eligibility.reason",
                        &[
                            ("name", &name),
                            ("reason", &cx.tr.text(&c.eligibility.text())),
                        ],
                    );
                    ui.add(egui::Label::new(Text::small(reason.clone(), cx.pal)).truncate())
                        .on_hover_text(reason);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if c.addable() {
                            let r = ui.add(
                                Button::primary(
                                    cx.tr.f("router.eligibility.add", &[("name", &name)]),
                                )
                                .icon(Icon::Plus)
                                .small()
                                .enabled(!busy),
                            );
                            if r.clicked()
                                && let Some(member) = as_member(cx, c)
                            {
                                state.add(cx, member);
                            }
                        } else if ui
                            .add(
                                Button::new(
                                    cx.tr.f("router.eligibility.fill_key", &[("name", &name)]),
                                )
                                .icon(Icon::Key)
                                .small(),
                            )
                            .clicked()
                        {
                            out.push(Action::PageCommand {
                                page: Page::Providers,
                                command: key_command(c),
                            });
                        }
                    });
                });
            }
        });
    ui.add_space(6.0);
}
