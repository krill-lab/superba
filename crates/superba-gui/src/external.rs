//! GH#5: Codex's configuration changed outside Superba (cc-switch, AI Toolbox,
//! Codex itself, a hand edit). The overview's to-do names it; this dialog shows
//! who most likely made the change, each changed key and the credential, and
//! the three answers: take it over again (re-apply the applied profile, through
//! the normal plan and countdown), let the other tool manage Codex (a
//! hand-off), or not now. It opens by itself once for each new state of the
//! files (start-up, or a change seen by the overview's polling); "Not now" is
//! remembered for that state only.

use egui::Ui;
use superba_core::{
    AUTH_JSON_PATH, ChangeSource, ExternalChange, ProfileId, SwitchIntent, SwitchRequest,
    auth_shape_label,
};

use crate::design::widgets::{Button, Text, dialog};
use crate::pages::{Action, Cx};

/// Dialog state between frames.
#[derive(Debug, Default)]
pub struct ExternalDialog {
    /// Fingerprint of the change being shown (`None` = closed).
    open: Option<String>,
    /// Changes the user answered "Not now" (or acted on) for.
    dismissed: Vec<String>,
}

impl ExternalDialog {
    /// Open for the overview's current change (the to-do's button).
    pub fn open(&mut self, change: &ExternalChange) {
        self.open = Some(change.fingerprint.clone());
    }

    /// Open by itself for a change not answered yet (`allowed`: nothing else is
    /// on screen: no plan, no wizard, no other dialog).
    pub fn offer(&mut self, change: Option<&ExternalChange>, allowed: bool) {
        let Some(change) = change else {
            self.open = None;
            return;
        };
        if self.open.is_none() && allowed && !self.dismissed.contains(&change.fingerprint) {
            self.open = Some(change.fingerprint.clone());
        }
    }

    /// Shown now.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn close(&mut self) {
        if let Some(f) = self.open.take()
            && !self.dismissed.contains(&f)
        {
            self.dismissed.push(f);
        }
    }
}

/// The plan request that takes everything over again: the applied profile,
/// every change answered `Overwrite`.
pub fn take_over_request(profile: &ProfileId) -> SwitchRequest {
    let mut r = SwitchRequest::profile(profile.clone());
    superba_switch::ownership::take_over(&mut r);
    r
}

/// The hand-off request.
pub fn hand_off_request(to: &ChangeSource) -> SwitchRequest {
    let mut r = SwitchRequest::profile(ProfileId::new(""));
    r.intent = SwitchIntent::HandOff { to: to.clone() };
    r
}

/// "cc-switch" / "AI Toolbox" / "another tool", for buttons.
pub fn tool_name(cx: &Cx<'_>, source: &ChangeSource) -> String {
    match source.tool() {
        Some(t) => t.to_owned(),
        None => cx.tr.t("external.other_tool"),
    }
}

/// The changed keys, the credential and environment values as diff rows.
pub fn rows(ui: &mut Ui, cx: &Cx<'_>, id: &str, change: &ExternalChange) {
    let unset = cx.tr.t("external.unset");
    for k in &change.keys {
        crate::pages::shared::values::change_row(
            ui,
            cx,
            egui::Id::unique(("external-key", id, &k.path)),
            None,
            &k.path,
            k.before.as_deref(),
            k.after.as_deref(),
            None,
            &unset,
        );
    }
    if let Some(a) = &change.auth {
        auth_row(ui, cx, id, a);
    }
    for e in &change.env {
        crate::pages::shared::values::change_row(
            ui,
            cx,
            egui::Id::unique(("external-env", id, format!("{:?}", e.location))),
            None,
            &e.var,
            e.written.as_deref(),
            e.live.as_deref(),
            None,
            &unset,
        );
    }
}

/// The credential row (shapes only, never a key).
pub fn auth_row(ui: &mut Ui, cx: &Cx<'_>, id: &str, a: &superba_core::AuthDrift) {
    let before = cx.tr.text(&auth_shape_label(&a.written));
    let after = cx.tr.text(&auth_shape_label(&a.live));
    crate::pages::shared::values::change_row(
        ui,
        cx,
        egui::Id::unique(("external-auth", id)),
        None,
        AUTH_JSON_PATH,
        Some(&before),
        Some(&after),
        None,
        "—",
    );
}

/// Draw the dialog when it is open.
pub fn show(ctx: &egui::Context, cx: &Cx<'_>, state: &mut ExternalDialog, out: &mut Vec<Action>) {
    let Some(open) = state.open.clone() else {
        return;
    };
    let Some(change) = cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.external.as_ref())
        .filter(|x| x.fingerprint == open)
    else {
        // Answered elsewhere, or the files changed again: the next poll offers
        // the new state.
        state.open = None;
        return;
    };
    let pal = cx.pal;
    let tr = cx.tr;
    let source = cx.localize(&change.source.text());
    let tool = tool_name(cx, &change.source);
    let profile = cx
        .data
        .active_state()
        .and_then(|a| a.profile_id.clone())
        .filter(|id| cx.data.profile(id).is_some_and(|p| !p.is_builtin()));
    let mut close = false;
    let response = dialog(ctx, egui::Id::unique("external-change"), 620.0, |ui| {
        ui.label(Text::h2(tr.t("external.title"), pal));
        ui.add(
            egui::Label::new(Text::body(
                tr.f(
                    "external.body",
                    &[("source", &source), ("n", &change.count())],
                ),
                pal,
            ))
            .wrap(),
        );
        for e in &change.evidence {
            ui.add(egui::Label::new(Text::small(cx.localize(e), pal).color(pal.text_muted)).wrap());
        }
        ui.add_space(4.0);
        rows(ui, cx, "dialog", change);
        ui.add_space(4.0);
        ui.add(egui::Label::new(Text::muted(tr.t("external.promise"), pal)).wrap());
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if let Some(p) = &profile
                && ui
                    .add(Button::primary(tr.t("external.take_over")).enabled(!cx.busy))
                    .on_hover_text(tr.t("external.take_over_tip"))
                    .clicked()
            {
                out.push(Action::Plan(Box::new(take_over_request(p))));
                close = true;
            }
            if ui
                .add(Button::new(tr.f("external.hand_off", &[("tool", &tool)])).enabled(!cx.busy))
                .on_hover_text(tr.t("external.hand_off_tip"))
                .clicked()
            {
                out.push(Action::Plan(Box::new(hand_off_request(&change.source))));
                close = true;
            }
            if ui
                .add(Button::ghost(tr.t("external.later")).kbd("Esc"))
                .clicked()
            {
                close = true;
            }
        });
    });
    if close || response.should_close() {
        state.close();
    }
}
