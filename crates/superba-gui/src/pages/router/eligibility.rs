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

use egui::Ui;
use superba_core::SiteKind;
use superba_switch::router_doctor::{RouterCandidate, router_candidates};

use super::State;
use super::model::{Candidate, MemberKind};
use crate::design::icons::Icon;
use crate::design::tokens::space;
use crate::design::widgets::{Button, FormRow, Status, StatusDot, Text};
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

/// Which of the `fixable` providers gets the primary button: the first one
/// that can be added as it is (ui-spec rule 5: one primary per view; the
/// others and every "enter a key" fix are secondary).
pub fn primary_fix(fixable: &[&RouterCandidate]) -> Option<usize> {
    fixable.iter().position(|c| c.addable())
}

/// The explanation and the fixes, in the members section; draws nothing when
/// a member is eligible. One row per provider that can be fixed: what is
/// missing on the left, the fix on the right — 添加 … 为成员 (the first one is
/// the page's primary action) or 为 … 填写 API key, which opens the Providers
/// page key field. Without any such provider, 添加供应商.
pub fn panel(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if has_eligible_member(cx, state) {
        return;
    }
    let all = candidates(cx, state);
    let busy = cx.busy || state.op.is_some();
    ui.add_space(space::S);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        ui.add(StatusDot::new(Status::Warn));
        ui.add(egui::Label::new(Text::strong(cx.tr.t("router.eligibility.none"), cx.pal)).wrap());
    });
    let fixable: Vec<&RouterCandidate> = all
        .iter()
        .filter(|c| c.addable() || c.eligibility.needs_key())
        .collect();
    if fixable.is_empty() {
        fix_row(
            ui,
            cx,
            &cx.tr.t("router.eligibility.no_candidates"),
            Button::primary(cx.tr.t("router.eligibility.add_provider"))
                .icon(Icon::Plus)
                .small(),
            || {
                out.push(Action::PageCommand {
                    page: Page::Providers,
                    command: "add".into(),
                });
            },
        );
        return;
    }
    let first_add = primary_fix(&fixable);
    for (i, c) in fixable.iter().enumerate() {
        let name = cx.tr.text(&c.name);
        let reason = cx.tr.f(
            "router.eligibility.reason",
            &[
                ("name", &name),
                ("reason", &cx.tr.text(&c.eligibility.text())),
            ],
        );
        if c.addable() {
            let label = cx.tr.f("router.eligibility.add", &[("name", &name)]);
            let button = if Some(i) == first_add {
                Button::primary(label)
            } else {
                Button::new(label)
            };
            let member = as_member(cx, c);
            fix_row(
                ui,
                cx,
                &reason,
                button.icon(Icon::Plus).small().enabled(!busy),
                || {
                    if let Some(member) = member {
                        state.add(cx, member);
                    }
                },
            );
        } else {
            let command = key_command(c);
            fix_row(
                ui,
                cx,
                &reason,
                Button::new(cx.tr.f("router.eligibility.fill_key", &[("name", &name)]))
                    .icon(Icon::Key)
                    .small(),
                || {
                    out.push(Action::PageCommand {
                        page: Page::Providers,
                        command,
                    });
                },
            );
        }
    }
    ui.add_space(space::XS);
}

/// One fix: the reason (shortened with its full text on hover when narrow) and
/// its button on the right, then a hairline.
fn fix_row(ui: &mut Ui, cx: &Cx<'_>, reason: &str, button: Button, clicked: impl FnOnce()) {
    let button_w = button.desired_size(ui).x;
    let label_w = (ui.available_width() - button_w - space::M).max(0.0);
    FormRow::default().label_width(label_w).right().show_with(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                ui.add_space(space::L + space::XS);
                ui.add(egui::Label::new(Text::small(reason, cx.pal).color(cx.pal.text)).truncate())
                    .on_hover_text(reason);
            });
        },
        |ui| {
            if ui.add(button).clicked() {
                clicked();
            }
        },
    );
    crate::pages::divider(ui, cx);
}
