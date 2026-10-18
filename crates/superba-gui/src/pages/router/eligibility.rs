//! "Can the router be turned on?" (user bug 28). Router members need a key the
//! router can send as an API key (whatever auth the provider uses when switched
//! directly, user bug 78); when no member has one, 开启 is disabled and this
//! panel says why, right under the banner, with the direct fixes: add an
//! eligible provider as a member, or enter an API key for a provider that has
//! none, which opens the Providers page key field.
//! The rules are the switch service's ([`superba_switch::router_doctor`]), so
//! the page, `spb router enable` and `spb router doctor` say the same thing.
//!
//! The facts come from the page's own snapshot, loaded together with the member
//! list it shows (user bug 39: the gate read the shell's copy of the profile
//! set, loaded at start and not after a member was added here or with `spb`,
//! so Krill was listed as member #1 while the panel offered to add it and 开启
//! stayed disabled). The shell's copy is only the fallback before the first
//! snapshot.

use egui::{Align, Layout, Ui};
use superba_core::SiteKind;
use superba_switch::router_doctor::{RouterCandidate, router_candidates};

use super::State;
use super::model::{Candidate, MemberKind};
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Status, StatusDot, Text};
use crate::pages::{Action, Cx, Page};

/// Every provider as a possible member: the snapshot's (loaded with the member
/// list), else from the shell's profiles and sites (empty while they load).
pub fn candidates(cx: &Cx<'_>, state: &State) -> Vec<RouterCandidate> {
    if let Some(s) = &state.snapshot {
        return s.eligibility.clone();
    }
    cx.data
        .profiles
        .value
        .as_ref()
        .map(|set| router_candidates(set, &cx.data.sites))
        .unwrap_or_default()
}

/// At least one member can carry traffic (the one rule:
/// [`superba_switch::router_doctor::has_eligible_member`]). `true` while
/// nothing is loaded yet, so nothing flickers disabled at start.
pub fn has_eligible_member(cx: &Cx<'_>, state: &State) -> bool {
    (state.snapshot.is_none() && cx.data.profiles.value.is_none())
        || superba_switch::router_doctor::has_eligible_member(&candidates(cx, state))
}

/// The router has members (the snapshot's list, else the shell's settings).
pub fn has_members(cx: &Cx<'_>, state: &State) -> bool {
    match &state.snapshot {
        Some(s) => !s.members.is_empty(),
        None => cx
            .data
            .profiles
            .value
            .as_ref()
            .is_some_and(|p| !p.settings.router.members.is_empty()),
    }
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
    if has_eligible_member(cx, state) {
        return;
    }
    let all = candidates(cx, state);
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
