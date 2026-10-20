//! Remote compaction of a provider on the Connection tab (GitHub issue 3):
//! one row with the effective support (native / not supported / not checked),
//! where it comes from (Superba's check and when, Krill's known support, the
//! user's choice), a Check button (one tiny compaction request: a few tokens)
//! and the choice (Automatic / Native / Not supported) under Change; when the
//! provider cannot compact, a note says what that means and what to do.
//! Results come from `SwitchService::provider_compaction` (cached per
//! provider and key); opening a provider checks one that nothing is known
//! about yet (a custom site), at most once a day.
//!
//! The state lives in egui's temporary data per provider, polled while the
//! tab is drawn.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{RichText, Ui};
use superba_core::{
    CompactionChoice, CompactionRequest, CompactionSource, CompactionSupport, Profile,
    ProviderCompaction, TransportRefresh,
};

use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Button, Segmented, Status, StatusDot, Text, link_button};
use crate::pages::shared::providers::Provider;
use crate::pages::{Action, Cx, note};
use crate::runtime::ReadTask;

/// The compaction record of one provider.
#[derive(Debug, Default)]
struct Check {
    task: Option<ReadTask<ProviderCompaction>>,
    /// The task sends a request (not only the cache).
    checking: bool,
    started: bool,
    value: Option<ProviderCompaction>,
    error: Option<String>,
    /// The choice is shown.
    open: bool,
}

type Shared = Arc<Mutex<Check>>;

fn state_of(ui: &Ui, profile: &Profile) -> Shared {
    let id = egui::Id::unique(("provider-compaction", profile.id.as_str()));
    ui.ctx()
        .data_mut(|d| d.get_temp_mut_or_insert_with(id, Shared::default).clone())
}

/// The provider's profile when it can be checked (a key is stored).
fn checkable(p: &Provider) -> Option<&Profile> {
    p.profile
        .as_ref()
        .filter(|pr| !pr.is_builtin() && !p.site.lines.is_empty())
}

fn start(c: &mut Check, cx: &Cx<'_>, profile: &Profile, refresh: TransportRefresh) {
    if c.task.is_some() {
        return;
    }
    let service = cx.service.clone();
    let request = CompactionRequest::cached(profile.id.clone()).with_refresh(refresh);
    c.started = true;
    c.checking = refresh != TransportRefresh::Cached;
    c.task = Some(crate::runtime::task(async move {
        service.provider_compaction(request).await
    }));
}

/// Load the record (cache only), then check a provider nothing is known about
/// (never checked, no built-in fact, no choice) or whose check is stale.
pub(super) fn poll(ui: &Ui, cx: &Cx<'_>, p: &Provider) {
    let Some(profile) = checkable(p) else {
        return;
    };
    let shared = state_of(ui, profile);
    let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
    if !c.started {
        start(&mut c, cx, profile, TransportRefresh::Cached);
    }
    if let Some(task) = &c.task {
        match task.poll() {
            Some(result) => {
                let was_checking = c.checking;
                c.task = None;
                c.checking = false;
                match result {
                    Ok(v) => {
                        let due = !was_checking
                            && profile.key_ref.is_some()
                            && v.choice.is_auto()
                            && v.source != CompactionSource::Builtin
                            && v.is_stale(cx.now);
                        c.value = Some(v);
                        c.error = None;
                        if due {
                            start(&mut c, cx, profile, TransportRefresh::Missing);
                        }
                    }
                    Err(e) => c.error = Some(crate::pages::error_text(cx.tr, &e)),
                }
            }
            None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
        }
    }
}

/// Forget the record of `p` (its key or choice changed): the next draw loads
/// it again.
pub(super) fn forget(ui: &Ui, p: &Provider) {
    if let Some(profile) = p.profile.as_ref() {
        let shared = state_of(ui, profile);
        let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
        let open = c.open;
        *c = Check {
            open,
            ..Check::default()
        };
    }
}

fn status_of(s: CompactionSupport) -> Status {
    match s {
        CompactionSupport::Native => Status::Ok,
        CompactionSupport::Unsupported => Status::Warn,
        _ => Status::Idle,
    }
}

/// The row (label, support, source, Check, Change) and its note.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, p: &Provider, out: &mut Vec<Action>) {
    let Some(profile) = checkable(p) else {
        return;
    };
    let shared = state_of(ui, profile);
    let (value, checking, error, open) = {
        let c = shared.lock().unwrap_or_else(|e| e.into_inner());
        (
            c.value.clone(),
            c.task.is_some() && c.checking,
            c.error.clone(),
            c.open,
        )
    };
    let mut toggle = false;
    let mut check = false;
    let label = cx.tr.t("providers.row.compaction");
    // Change and Check keep their size; the source note is shortened.
    super::connection::row_split(
        ui,
        cx,
        &label,
        |ui: &mut Ui| {
            let label = if open {
                cx.tr.t("providers.compaction.hide_choice")
            } else {
                cx.tr.t("providers.compaction.change")
            };
            if link_button(ui, &label, false).clicked() {
                toggle = true;
            }
            let can_check = profile.key_ref.is_some() && !cx.busy;
            if ui
                .add(
                    Button::ghost(cx.tr.t("providers.compaction.check"))
                        .icon(Icon::Refresh)
                        .small()
                        .loading(checking)
                        .enabled(can_check && !checking),
                )
                .on_hover_text(cx.tr.t("providers.compaction.check_tip"))
                .clicked()
            {
                check = true;
            }
        },
        |ui: &mut Ui| {
            let support = value
                .as_ref()
                .map_or(CompactionSupport::Unknown, |v| v.support);
            ui.add(StatusDot::new(if checking {
                Status::Busy
            } else {
                status_of(support)
            }));
            ui.label(
                RichText::new(cx.tr.text(&support.text()))
                    .font(font(text::SMALL, Weight::Medium))
                    .color(cx.pal.text),
            );
            let source = value
                .as_ref()
                .map(|v| source_text(cx, v))
                .unwrap_or_default();
            if !source.is_empty() {
                let tip = value.as_ref().and_then(|v| tooltip(cx, v));
                let r =
                    ui.add(egui::Label::new(Text::small(format!("· {source}"), cx.pal)).truncate());
                if let Some(tip) = tip {
                    r.on_hover_text(tip);
                }
            }
        },
    );
    if toggle || check {
        let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
        if toggle {
            c.open = !c.open;
        }
        if check {
            start(&mut c, cx, profile, TransportRefresh::All);
        }
    }
    if open {
        let mut choice = profile.compaction;
        let before = choice;
        ui.horizontal(|ui| {
            ui.add_space(80.0);
            ui.add_enabled_ui(!cx.busy, |ui| {
                Segmented::new("providers-compaction", &mut choice)
                    .small()
                    .option(
                        CompactionChoice::Auto,
                        cx.tr.text(&CompactionChoice::Auto.text()),
                    )
                    .option(
                        CompactionChoice::Native,
                        cx.tr.text(&CompactionChoice::Native.text()),
                    )
                    .option(
                        CompactionChoice::Unsupported,
                        cx.tr.text(&CompactionChoice::Unsupported.text()),
                    )
                    .show(ui);
            });
            ui.add(
                egui::Label::new(Text::caption(
                    cx.tr.t("providers.compaction.choice_body"),
                    cx.pal,
                ))
                .truncate(),
            )
            .on_hover_text(cx.tr.t("providers.compaction.choice_body"));
        });
        if choice != before {
            let mut changed = profile.clone();
            changed.compaction = choice;
            out.push(Action::SaveProfile {
                profile: Box::new(changed),
                key: None,
            });
            forget(ui, p);
        }
    }
    if let Some(e) = error {
        note(
            ui,
            cx,
            Status::Warn,
            cx.tr.f("providers.compaction.failed", &[("error", &e)]),
        );
    } else if value
        .as_ref()
        .is_some_and(|v| v.support == CompactionSupport::Unsupported)
    {
        note(
            ui,
            cx,
            Status::Warn,
            cx.tr.t("providers.compaction.unsupported_note"),
        );
    }
}

/// Where the value comes from, in a few words.
fn source_text(cx: &Cx<'_>, v: &ProviderCompaction) -> String {
    match v.source {
        CompactionSource::Override => cx.tr.t("providers.compaction.source_override"),
        CompactionSource::Builtin => cx.tr.t("providers.compaction.source_builtin"),
        CompactionSource::Check => match v.checked_at {
            Some(at) => cx.tr.f(
                "providers.compaction.source_check",
                &[("when", &cx.ago(at))],
            ),
            None => String::new(),
        },
        _ => cx.tr.t("providers.compaction.source_none"),
    }
}

/// The last check's answer, model and time.
fn tooltip(cx: &Cx<'_>, v: &ProviderCompaction) -> Option<String> {
    let mut lines = vec![cx.tr.t("providers.compaction.tip")];
    if let (Some(c), Some(at)) = (&v.check, v.checked_at) {
        lines.push(cx.tr.f(
            "providers.compaction.tip_check",
            &[
                ("when", &cx.ago(at)),
                ("model", &v.model.as_deref().unwrap_or("-")),
                ("result", &cx.tr.text(&c.support.text())),
            ],
        ));
        if let Some(d) = &c.detail {
            lines.push(cx.tr.f("providers.transport.answer", &[("detail", d)]));
        }
    }
    Some(lines.join("\n"))
}
