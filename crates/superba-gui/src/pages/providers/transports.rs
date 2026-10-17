//! WebSocket and zstd support per line on the Connection tab (user request
//! 14): two small badges per line (supported / not / unknown) with a tooltip
//! that says what the line answered and when it was checked, and a caption
//! under the lines. Results come from `SwitchService::line_transports`
//! (cached per line and key); opening a provider checks the lines without a
//! recent result, and Test, Refresh or a key check checks them all again.
//!
//! The state lives in egui's temporary data per provider (no page state
//! fields), polled while the tab is drawn.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use superba_core::{
    LineId, LineTransports, LineTransportsRequest, Profile, TransportCheck, TransportRefresh,
    TransportSupport,
};

use super::State;
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{radius, text};
use crate::design::widgets::{Status, StatusDot, Text};
use crate::pages::Cx;
use crate::pages::shared::providers::Provider;
use crate::runtime::ReadTask;

/// Transport checks of one provider.
#[derive(Debug, Default)]
struct Checks {
    task: Option<ReadTask<Vec<LineTransports>>>,
    /// A refresh asked for while a check ran.
    queued: Option<TransportRefresh>,
    /// The first (automatic) load was started.
    started: bool,
    value: Vec<LineTransports>,
    error: Option<String>,
    /// The provider's key check was running at the last frame.
    was_checking: bool,
}

type Shared = Arc<Mutex<Checks>>;

fn checks(ui: &Ui, profile: &Profile) -> Shared {
    let id = egui::Id::unique(("provider-transports", profile.id.as_str()));
    ui.ctx()
        .data_mut(|d| d.get_temp_mut_or_insert_with(id, Shared::default).clone())
}

/// The provider's profile, when its lines can be checked (a key is stored).
fn checkable(p: &Provider) -> Option<&Profile> {
    p.profile
        .as_ref()
        .filter(|pr| pr.key_ref.is_some() && !p.site.lines.is_empty())
}

fn start(c: &mut Checks, cx: &Cx<'_>, profile: &Profile, refresh: TransportRefresh) {
    if c.task.is_some() {
        // Never two checks of one provider at once; the stronger request wins.
        if c.queued != Some(TransportRefresh::All) {
            c.queued = Some(refresh);
        }
        return;
    }
    let service = cx.service.clone();
    let request = LineTransportsRequest::cached(profile.id.clone()).with_refresh(refresh);
    c.started = true;
    c.task = Some(crate::runtime::task(async move {
        service.line_transports(request).await
    }));
}

/// Poll the checks of `p` and start the automatic and the follow-up ones:
/// the first draw checks lines without a recent result; a key check of the
/// provider (Test, "Check on all lines") checks every line again.
pub(super) fn poll(ui: &Ui, cx: &Cx<'_>, state: &State, p: &Provider) {
    poll_inner(ui, cx, state, p, true);
}

/// [`poll`] without the automatic first check (another tab of the provider:
/// Test there still refreshes the checks).
pub(super) fn follow(ui: &Ui, cx: &Cx<'_>, state: &State, p: &Provider) {
    poll_inner(ui, cx, state, p, false);
}

fn poll_inner(ui: &Ui, cx: &Cx<'_>, state: &State, p: &Provider, auto: bool) {
    let Some(profile) = checkable(p) else {
        return;
    };
    let shared = checks(ui, profile);
    let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
    if auto && !c.started {
        start(&mut c, cx, profile, TransportRefresh::Missing);
    }
    let checking = state.check_task.as_ref().is_some_and(|(id, _)| id == &p.id);
    if checking && !c.was_checking {
        start(&mut c, cx, profile, TransportRefresh::All);
    }
    c.was_checking = checking;
    if let Some(task) = &c.task {
        match task.poll() {
            Some(result) => {
                c.task = None;
                match result {
                    Ok(v) => {
                        c.value = v;
                        c.error = None;
                    }
                    Err(e) => c.error = Some(crate::pages::error_text(cx.tr, &e)),
                }
                if let Some(next) = c.queued.take() {
                    start(&mut c, cx, profile, next);
                }
            }
            None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
        }
    }
}

/// Check every line of `p` again (Refresh).
pub(super) fn refresh(ui: &Ui, cx: &Cx<'_>, p: &Provider) {
    if let Some(profile) = checkable(p) {
        let shared = checks(ui, profile);
        let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
        start(&mut c, cx, profile, TransportRefresh::All);
    }
}

/// Forget the results of `p` (its key changed): the next draw checks again.
pub(super) fn forget(ui: &Ui, p: &Provider) {
    if let Some(profile) = p.profile.as_ref() {
        let shared = checks(ui, profile);
        let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
        *c = Checks {
            was_checking: c.was_checking,
            ..Checks::default()
        };
    }
}

/// The two badges of one line (nothing for a provider without a key).
pub(super) fn badges(ui: &mut Ui, cx: &Cx<'_>, p: &Provider, line: &LineId) {
    let Some(profile) = checkable(p) else {
        return;
    };
    let shared = checks(ui, profile);
    let c = shared.lock().unwrap_or_else(|e| e.into_inner());
    let result = c.value.iter().find(|r| &r.line_id == line);
    let busy = c.task.is_some();
    let when = result.map(|r| cx.ago(r.checked_at));
    let tip = |ws: bool, check: Option<&TransportCheck>| {
        let headline = match (ws, check.map(|c| c.support)) {
            (true, Some(TransportSupport::Supported)) => "providers.transport.ws.supported",
            (true, Some(TransportSupport::Unsupported)) => "providers.transport.ws.unsupported",
            (true, Some(_)) => "providers.transport.ws.unknown",
            (true, None) => "providers.transport.ws.title",
            (false, Some(TransportSupport::Supported)) => "providers.transport.zstd.supported",
            (false, Some(TransportSupport::Unsupported)) => "providers.transport.zstd.unsupported",
            (false, Some(_)) => "providers.transport.zstd.unknown",
            (false, None) => "providers.transport.zstd.title",
        };
        let mut lines = vec![cx.tr.t(headline)];
        match check {
            Some(check) => {
                if let Some(d) = &check.detail {
                    lines.push(cx.tr.f("providers.transport.answer", &[("detail", d)]));
                }
                if let Some(w) = &when {
                    lines.push(cx.tr.f("providers.transport.checked", &[("when", w)]));
                }
            }
            None => lines.push(cx.tr.t(if busy {
                "providers.transport.checking"
            } else {
                "providers.transport.not_checked"
            })),
        }
        lines.join("\n")
    };
    let ws = result.map(|r| &r.websocket);
    let zstd = result.map(|r| &r.zstd);
    pill(ui, cx, "WS", ws.map(|c| c.support), busy).on_hover_text(tip(true, ws));
    pill(ui, cx, "zstd", zstd.map(|c| c.support), busy).on_hover_text(tip(false, zstd));
}

/// One badge: outline pill with a drawn status icon and the name.
fn pill(
    ui: &mut Ui,
    cx: &Cx<'_>,
    label: &str,
    support: Option<TransportSupport>,
    busy: bool,
) -> Response {
    const ICON: f32 = 11.0;
    let font_id = font(text::CAPTION, Weight::Medium);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font_id, egui::Color32::PLACEHOLDER);
    let size = vec2(galley.size().x + ICON + 18.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let pal = cx.pal;
        let (icon, color) = match support {
            Some(TransportSupport::Supported) => (Icon::Check, pal.green),
            Some(TransportSupport::Unsupported) => (Icon::Close, pal.text_subtle),
            _ if busy => (Icon::Refresh, pal.text_subtle),
            _ => (Icon::Info, pal.text_subtle),
        };
        let painter = ui.painter();
        // The stroke stays inside the allocated rect (never clipped).
        painter.rect_stroke(
            rect,
            radius::PILL,
            Stroke::new(1.0, pal.border),
            StrokeKind::Inside,
        );
        let icon_rect = Rect::from_center_size(
            pos2(rect.min.x + 7.0 + ICON / 2.0, rect.center().y),
            vec2(ICON, ICON),
        );
        icons::paint(painter, icon_rect, icon, color);
        let text_color = if support == Some(TransportSupport::Supported) {
            pal.text
        } else {
            pal.text_muted
        };
        painter.galley(
            pos2(
                icon_rect.max.x + 4.0,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            text_color,
        );
    }
    response
}

/// The caption under the lines: when the lines were checked, a check in
/// progress, or why it failed.
pub(super) fn footer(ui: &mut Ui, cx: &Cx<'_>, p: &Provider) {
    let Some(profile) = checkable(p) else {
        return;
    };
    let shared = checks(ui, profile);
    let c = shared.lock().unwrap_or_else(|e| e.into_inner());
    let (status, line) = if c.task.is_some() {
        (Status::Busy, cx.tr.t("providers.transport.checking"))
    } else if let Some(e) = &c.error {
        (
            Status::Warn,
            cx.tr.f("providers.transport.failed", &[("error", e)]),
        )
    } else if let Some(oldest) = c.value.iter().map(|r| r.checked_at).min() {
        (
            Status::Idle,
            cx.tr
                .f("providers.transport.footer", &[("when", &cx.ago(oldest))]),
        )
    } else {
        (Status::Idle, cx.tr.t("providers.transport.not_checked"))
    };
    drop(c);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add(StatusDot::new(status));
        ui.add(egui::Label::new(Text::caption(line.clone(), cx.pal)).truncate())
            .on_hover_text(cx.tr.t("providers.transport.footer_hint"));
    });
}
