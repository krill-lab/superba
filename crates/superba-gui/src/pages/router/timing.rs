//! 计时 (Timing) tab of the request detail pane (UX §3.5).
//!
//! The left column is a waterfall with one line per timing mark — 收到请求,
//! 发往上游, 上游响应头, 首个事件, 首字, 最后输出, 完成 — showing the mark's offset
//! from `t0` and a bar for the phase that ends at it, on one time axis. Marks
//! come normalized ([`RequestTiming::normalized`]: never out of order), and a
//! 0 ms phase is a real measurement, not an error: the first event read together
//! with the response headers is drawn as a tick and labelled 与响应头同时到达.
//! Because every mark has its own line, marks at the same time never draw over
//! each other.
//!
//! The right column repeats the speed numbers of the table and the stats line
//! (首字, 完成时间 and TPS of the row: the same stored values), explains a TPS
//! of "—" (window under 0.2 s or fewer than 10 output tokens), then the
//! attempts in the UI language from their fields (provider and line names as
//! elsewhere), and the router's raw log lines (English) behind a toggle.

use egui::{Rect, ScrollArea, Sense, Stroke, Ui, pos2, vec2};
use superba_core::{
    RequestAttempt, RequestDetail, RequestRow, RequestStatus, RequestTiming, RequestTransport,
    TPS_MIN_OUTPUT, TPS_MIN_WINDOW_MS,
};

use super::detail::stats_line_note;
use super::requests::{DASH, ProviderNames, seconds, tps};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::radius;
use crate::design::widgets::Text;
use crate::i18n::Tr;
use crate::pages::Cx;
use crate::pages::shared::format::group_thousands;

/// A timing mark, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    /// `t0`: the router received the request.
    Received,
    /// The request went upstream.
    Sent,
    /// Response headers.
    Headers,
    /// First Responses event.
    FirstEvent,
    /// First output (首字).
    FirstToken,
    /// Last output delta.
    LastDelta,
    /// End (完成).
    Done,
}

impl Mark {
    /// Every mark, in order.
    pub const ALL: [Mark; 7] = [
        Mark::Received,
        Mark::Sent,
        Mark::Headers,
        Mark::FirstEvent,
        Mark::FirstToken,
        Mark::LastDelta,
        Mark::Done,
    ];

    fn name(self) -> &'static str {
        match self {
            Mark::Received => "received",
            Mark::Sent => "sent",
            Mark::Headers => "headers",
            Mark::FirstEvent => "first_event",
            Mark::FirstToken => "first_token",
            Mark::LastDelta => "last_delta",
            Mark::Done => "done",
        }
    }

    /// Locale key of the mark's label.
    pub fn key(self) -> String {
        format!("requests.detail.mark.{}", self.name())
    }

    /// Locale key of the note on a 0 ms phase that starts at this mark
    /// ("arrived with the headers").
    pub fn same_key(self) -> String {
        format!("requests.detail.same.{}", self.name())
    }

    fn of(self, t: &RequestTiming) -> Option<u64> {
        match self {
            Mark::Received => Some(0),
            Mark::Sent => t.sent_ms,
            Mark::Headers => t.headers_ms,
            Mark::FirstEvent => t.first_event_ms,
            Mark::FirstToken => t.first_token_ms,
            Mark::LastDelta => t.last_delta_ms,
            Mark::Done => t.end_ms,
        }
    }
}

/// Why a mark has no time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// A WebSocket request has no response headers of its own.
    WebSocketHeaders,
    /// No event arrived (an error answer, or no answer at all).
    NoEvents,
    /// Events arrived but no output.
    NoOutput,
    /// Not recorded (not reached).
    Unknown,
}

/// One line of the waterfall.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The mark.
    pub mark: Mark,
    /// Its offset from `t0` in ms (`None`: see `missing`).
    pub at_ms: Option<u64>,
    /// Where the phase ending at this mark starts: the latest earlier mark with
    /// a time.
    pub from: Option<(Mark, u64)>,
    /// Why there is no time.
    pub missing: Option<Missing>,
}

impl Step {
    /// Length of the phase ending at this mark.
    pub fn phase_ms(&self) -> Option<u64> {
        Some(self.at_ms?.saturating_sub(self.from?.1))
    }

    /// The earlier mark this one arrived together with (a 0 ms phase).
    pub fn same_as(&self) -> Option<Mark> {
        (self.phase_ms()? == 0)
            .then(|| self.from.map(|(m, _)| m))
            .flatten()
    }
}

/// The timing of `row` from its detail, with the row's own numbers filling in
/// what the detail lacks, normalized (no mark before an earlier one).
pub fn timing_of(row: &RequestRow, detail: Option<&RequestDetail>) -> RequestTiming {
    let t = detail.map(|d| d.timing.clone()).unwrap_or_default();
    let websocket = row.transport == RequestTransport::WebSocket;
    RequestTiming {
        headers_ms: t
            .headers_ms
            .or(row.ttfb_ms)
            // For a WebSocket answer the row's ttfb is its first frame, not headers.
            .filter(|_| !websocket),
        first_token_ms: t.first_token_ms.or(row.first_token_ms),
        end_ms: t.end_ms.or(row.duration_ms),
        ..t
    }
    .normalized()
}

/// The waterfall lines of `row`.
pub fn steps(row: &RequestRow, detail: Option<&RequestDetail>) -> Vec<Step> {
    let t = timing_of(row, detail);
    let mut prev: Option<(Mark, u64)> = None;
    Mark::ALL
        .iter()
        .map(|&mark| {
            let at_ms = mark.of(&t);
            let from = if mark == Mark::Received { None } else { prev };
            let missing = match (at_ms, mark) {
                (Some(_), _) => None,
                (None, Mark::Headers) if row.transport == RequestTransport::WebSocket => {
                    Some(Missing::WebSocketHeaders)
                }
                (None, Mark::FirstEvent) if row.status != RequestStatus::Ok => {
                    Some(Missing::NoEvents)
                }
                (None, Mark::FirstToken | Mark::LastDelta) if t.first_event_ms.is_some() => {
                    Some(if t.first_token_ms.is_some() {
                        Missing::Unknown
                    } else {
                        Missing::NoOutput
                    })
                }
                _ => Some(Missing::Unknown),
            };
            if let Some(ms) = at_ms {
                prev = Some((mark, ms));
            }
            Step {
                mark,
                at_ms,
                from,
                missing,
            }
        })
        .collect()
}

/// `640 ms`, `12,480 ms`, or `2m 05s` past 100 s.
pub fn offset_text(ms: u64) -> String {
    if ms < 100_000 {
        format!("{} ms", group_thousands(ms))
    } else {
        seconds(Some(ms))
    }
}

/// The text after a phase bar: `+28 ms`, `+0 ms · 与响应头同时到达`, or why
/// the mark has no time.
pub fn phase_text(tr: &Tr, step: &Step) -> String {
    if let Some(m) = step.missing {
        return match m {
            Missing::WebSocketHeaders => tr.t("requests.detail.missing.ws_headers"),
            Missing::NoEvents => tr.t("requests.detail.missing.no_events"),
            Missing::NoOutput => tr.t("requests.detail.missing.no_output"),
            Missing::Unknown => String::new(),
        };
    }
    let Some(ms) = step.phase_ms() else {
        return String::new();
    };
    let len = format!("+{}", offset_text(ms));
    match step.same_as() {
        Some(prev) => format!("{len} · {}", tr.t(&prev.same_key())),
        None => len,
    }
}

/// The lines under the speed numbers about TPS: its window, or why it is "—".
pub fn tps_note(tr: &Tr, row: &RequestRow, timing: &RequestTiming) -> Option<String> {
    let output = row.output_tokens?;
    let window = timing.tps_window_ms();
    if row.tps_milli.is_some() {
        let window = window?;
        return Some(tr.f(
            "requests.detail.tps.window",
            &[
                ("output", &group_thousands(output)),
                ("window", &offset_text(window)),
            ],
        ));
    }
    match window {
        Some(w) if w < TPS_MIN_WINDOW_MS => {
            Some(tr.f("requests.detail.tps.short", &[("window", &offset_text(w))]))
        }
        Some(_) if output < TPS_MIN_OUTPUT => Some(tr.t("requests.detail.tps.few")),
        _ => None,
    }
}

/// What happened to an attempt, in the UI language (the router stores a short
/// English reason: `answered`, `connect failed: …`, `HTTP 502`, …; anything
/// else is shown as stored).
pub fn outcome_text(tr: &Tr, outcome: &str) -> String {
    let key = match outcome {
        "answered" => Some("answered"),
        o if o.starts_with("zstd body refused") => Some("zstd_refused"),
        o if o.starts_with("connect timeout") => Some("connect_timeout"),
        o if o.starts_with("connect failed") => Some("connect_failed"),
        o if o.starts_with("no response") => Some("no_response"),
        o if o.starts_with("intercepted by the line's edge") => Some("intercepted"),
        o if o.starts_with("HTTP ") => Some("upstream_error"),
        _ => None,
    };
    key.map_or_else(
        || outcome.to_owned(),
        |k| tr.t(&format!("requests.detail.outcome.{k}")),
    )
}

/// One attempt from its fields: `1. 海外线路 · api.example.com · HTTP 200 ·
/// 已应答 · 520 ms`, with the current line (else provider) name.
pub fn attempt_line(tr: &Tr, names: &ProviderNames, i: usize, a: &RequestAttempt) -> String {
    let member = a.member_id.as_str();
    let name = match &a.line_name {
        Some(line) => names.line(member, line),
        None => names.provider(member, member),
    };
    let mut parts = vec![format!("{}. {name}", i + 1)];
    if let Some(h) = &a.upstream_host {
        parts.push(h.clone());
    }
    if let Some(s) = a.status {
        parts.push(format!("HTTP {s}"));
    }
    if !a.outcome.is_empty() {
        parts.push(outcome_text(tr, &a.outcome));
    }
    if let Some(ms) = a.elapsed_ms {
        parts.push(offset_text(ms));
    }
    // How the request body went to this line (HTTP).
    if let Some(c) = a.compression {
        parts.push(tr.text(&c.text()));
    }
    parts.join(" · ")
}

/// The router's log lines worth showing raw: the ones the localized notes
/// above do not already say (the stats line added or stripped).
pub fn raw_log(d: &RequestDetail) -> Vec<&str> {
    d.log
        .iter()
        .map(String::as_str)
        .filter(|l| *l != "stats line added" && !l.ends_with("removed from the input"))
        .collect()
}

/// Height of one waterfall line.
pub const LINE_H: f32 = 18.0;
/// Width from which the notes go into a column right of the waterfall.
const TWO_COLUMNS: f32 = 620.0;

/// Draw the tab.
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    row: &RequestRow,
    detail: Option<&RequestDetail>,
    loading: bool,
) {
    let Some(d) = detail else {
        ui.label(Text::small(
            cx.tr.t(if loading {
                "requests.loading"
            } else {
                "requests.detail.unavailable"
            }),
            cx.pal,
        ));
        return;
    };
    let steps = steps(row, Some(d));
    let rect = ui.available_rect_before_wrap();
    let waterfall_h = LINE_H * steps.len() as f32;
    if rect.width() >= TWO_COLUMNS && rect.height() >= waterfall_h {
        let left_w = (rect.width() * 0.62).floor();
        let left = Rect::from_min_size(rect.min, vec2(left_w, waterfall_h));
        let right = Rect::from_min_max(pos2(rect.min.x + left_w + 20.0, rect.min.y), rect.max);
        waterfall(ui, cx, &steps, left);
        crate::design::layout::fixed(ui, right, |ui| notes(ui, cx, row, d));
        ui.allocate_rect(rect, Sense::hover());
    } else {
        ScrollArea::vertical()
            .id_salt("router-detail-timing")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), waterfall_h), Sense::hover());
                waterfall(ui, cx, &steps, r);
                ui.add_space(6.0);
                notes_body(ui, cx, row, d);
            });
    }
}

/// The waterfall in `rect`: label, offset, phase bar on the time axis, phase
/// length (or why there is no time) per line. Each mark has its own line, so
/// marks at the same time never overlap; the phase texts get a column of their
/// own right of the longest bar.
fn waterfall(ui: &Ui, cx: &Cx<'_>, steps: &[Step], rect: Rect) {
    let painter = ui.painter_at(rect.expand2(vec2(0.0, 2.0)));
    let pal = cx.pal;
    let widest = |gs: &[std::sync::Arc<egui::Galley>]| {
        gs.iter().map(|g| g.size().x).fold(0.0, f32::max).ceil()
    };
    let labels: Vec<_> = steps
        .iter()
        .map(|s| {
            let color = if s.at_ms.is_some() {
                pal.text_muted
            } else {
                pal.text_subtle
            };
            painter.layout_no_wrap(cx.tr.t(&s.mark.key()), font(12.0, Weight::Medium), color)
        })
        .collect();
    let offsets: Vec<_> = steps
        .iter()
        .map(|s| {
            let text = s.at_ms.map_or_else(|| DASH.to_owned(), offset_text);
            painter.layout_no_wrap(text, mono_font(11.0, Weight::Regular), pal.text)
        })
        .collect();
    let phases: Vec<_> = steps
        .iter()
        .map(|s| {
            painter.layout_no_wrap(
                phase_text(cx.tr, s),
                font(11.0, Weight::Regular),
                pal.text_subtle,
            )
        })
        .collect();
    let (label_w, offset_w) = (widest(&labels), widest(&offsets));
    let track_left = rect.min.x + label_w + 12.0 + offset_w + 14.0;
    let track_right = (rect.max.x - widest(&phases) - 8.0).max(track_left + 60.0);
    let span = steps
        .iter()
        .filter_map(|s| s.at_ms)
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let x_of = |ms: u64| track_left + (track_right - track_left) * (ms as f32 / span).min(1.0);
    for (i, step) in steps.iter().enumerate() {
        let mid = rect.min.y + i as f32 * LINE_H + LINE_H / 2.0;
        let label = labels[i].clone();
        painter.galley(
            pos2(rect.min.x, mid - label.size().y / 2.0),
            label,
            pal.text,
        );
        let offset = offsets[i].clone();
        let ox = rect.min.x + label_w + 12.0 + offset_w - offset.size().x;
        painter.galley(pos2(ox, mid - offset.size().y / 2.0), offset, pal.text);
        // The phase ending here: a bar from the previous mark, or a tick for a
        // 0 ms (or sub-pixel) phase, never a bar of no or negative width.
        let shape = match (step.at_ms, step.from) {
            (Some(at), Some((_, from))) => {
                let (x0, x1) = (x_of(from), x_of(at));
                Some(if x1 - x0 < 2.0 {
                    let x = x0.max(track_left + 1.0);
                    (Rect::from_center_size(pos2(x, mid), vec2(2.0, 10.0)), 1)
                } else {
                    (
                        Rect::from_min_max(pos2(x0, mid - 4.0), pos2(x1, mid + 4.0)),
                        radius::CONTROL / 3,
                    )
                })
            }
            _ => None,
        };
        let text_x = shape.map_or(track_left + 6.0, |(r, _)| r.max.x + 6.0);
        let g = phases[i].clone();
        // The axis, interrupted where the phase text sits.
        let axis = Stroke::new(1.0, pal.border);
        if g.size().x > 0.0 {
            if text_x - 4.0 > track_left {
                painter.hline(track_left..=text_x - 4.0, mid, axis);
            }
            if text_x + g.size().x + 4.0 < track_right {
                painter.hline(text_x + g.size().x + 4.0..=track_right, mid, axis);
            }
        } else {
            painter.hline(track_left..=track_right, mid, axis);
        }
        match shape {
            Some((r, corner)) => {
                let color = if step.mark == Mark::LastDelta {
                    pal.link
                } else {
                    pal.text_subtle
                };
                painter.rect_filled(r, corner, color);
            }
            None if step.at_ms.is_some() => {
                painter.circle_filled(pos2(track_left, mid), 3.0, pal.text_subtle);
            }
            None => {}
        }
        if g.size().x > 0.0 {
            painter.galley(pos2(text_x, mid - g.size().y / 2.0), g, pal.text_subtle);
        }
    }
}

/// The right column: speed numbers, TPS window, stats line note, attempts and
/// log lines (scrolling when long).
fn notes(ui: &mut Ui, cx: &Cx<'_>, row: &RequestRow, d: &RequestDetail) {
    ScrollArea::vertical()
        .id_salt("router-detail-timing-notes")
        .auto_shrink([false, false])
        .show(ui, |ui| notes_body(ui, cx, row, d));
}

fn notes_body(ui: &mut Ui, cx: &Cx<'_>, row: &RequestRow, d: &RequestDetail) {
    let tr = cx.tr;
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.add(
        egui::Label::new(Text::small(
            format!(
                "{} · {}",
                tr.f(
                    "requests.detail.speed_times",
                    &[
                        ("ttft", &seconds(row.first_token_ms)),
                        ("duration", &seconds(row.duration_ms)),
                    ],
                ),
                tr.f("requests.detail.speed_tps", &[("tps", &tps(row.tps_milli))]),
            ),
            cx.pal,
        ))
        .wrap(),
    );
    if let Some(note) = tps_note(tr, row, &timing_of(row, Some(d))) {
        ui.add(egui::Label::new(Text::caption(note, cx.pal)).wrap());
    }
    ui.add_space(4.0);
    ui.add(egui::Label::new(Text::caption(stats_line_note(tr, row), cx.pal)).wrap());
    if row.stripped > 0 {
        ui.add(
            egui::Label::new(Text::caption(
                tr.f("requests.detail.stripped", &[("n", &row.stripped)]),
                cx.pal,
            ))
            .wrap(),
        );
    }
    if !d.attempts.is_empty() {
        ui.add_space(4.0);
        let names = ProviderNames::new(tr, cx.data.profiles.value.as_ref(), &cx.data.sites);
        for (i, a) in d.attempts.iter().enumerate() {
            ui.add(egui::Label::new(Text::caption(attempt_line(tr, &names, i, a), cx.pal)).wrap());
        }
    }
    // The router's own words (English), for bug reports: folded away.
    let raw = raw_log(d);
    if !raw.is_empty() {
        ui.add_space(4.0);
        egui::CollapsingHeader::new(Text::caption(tr.t("requests.detail.raw_log"), cx.pal))
            .id_salt(("router-detail-raw-log", row.id))
            .default_open(false)
            .show(ui, |ui| {
                for l in raw {
                    ui.add(egui::Label::new(Text::mono_muted(l, cx.pal)).wrap());
                }
            });
    }
}
