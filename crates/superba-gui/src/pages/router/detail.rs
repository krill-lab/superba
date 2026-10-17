//! Detail pane of the selected request (UX §3.5): tabs 通用 / 计时 (request
//! content is never recorded, user decision 22). 通用 shows small cards (请求,
//! 用量, 速度, 路由, 会话, 费用) in two rows of three, and the pane is sized to
//! them ([`pane_height`]); 计时 a timeline and the router's log lines for the
//! request (for a row from Codex's session files, the derived duration).

use std::collections::BTreeMap;

use egui::{Rect, ScrollArea, Sense, Ui, pos2, vec2};
use superba_core::{
    RequestClient, RequestDetail, RequestKind, RequestRow, RequestSource, RequestTransport,
    SessionTitle,
};

use super::requests::{
    self, kind_text, line_applicable, provider_cell, seconds, session_cell, session_key, tokens,
    tps,
};
use crate::design::widgets::info_card::{self, CopyLink, InfoCard, InfoLine};
use crate::design::widgets::{Text, detail_pane};
use crate::i18n::Tr;
use crate::pages::{Action, Cx};

/// Tab label keys, in order.
pub const TABS: [&str; 2] = ["requests.detail.general", "requests.detail.timing"];

/// Lines each row of 通用 cards is laid out for, so the pane keeps its height
/// while the selection moves; no card has more today (one that had would grow
/// its row, and the pane with it).
pub const CARD_LINES: usize = 3;
/// Gap between the 通用 cards.
pub const CARD_GAP: f32 = 6.0;
/// Space under the last row of cards.
const CARDS_BOTTOM: f32 = 2.0;

/// `会话 · 模型 · 供应商 · 线路` for the pane's title line.
pub fn title_line(
    cx: &Cx<'_>,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
) -> String {
    let mut parts = vec![session_cell(cx.tr, titles, row)];
    if let Some(m) = &row.model {
        parts.push(m.clone());
    }
    parts.push(provider_cell(row));
    parts.join(" · ")
}

/// One card of 通用.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// Title.
    pub title: String,
    /// Lines, most important first (all of them are shown; ids end in a copy link).
    pub lines: Vec<InfoLine>,
}

fn line(label: Option<String>, value: String) -> InfoLine {
    InfoLine {
        label,
        value,
        ..InfoLine::default()
    }
}

fn mono_line(label: String, value: String) -> InfoLine {
    InfoLine {
        label: Some(label),
        value,
        mono: true,
        ..InfoLine::default()
    }
}

/// A "复制" link copying `text`, with `key` ("复制缓存键") as its hover text.
fn copy_link(tr: &Tr, key: &str, text: &str) -> CopyLink {
    CopyLink {
        label: tr.t("requests.detail.copy"),
        tooltip: tr.t(key),
        text: text.to_owned(),
    }
}

/// The failover chain `国内极速 502 → 海外线路 200` from the attempts (an
/// attempt without a status by what happened, in the UI language).
pub fn chain(tr: &Tr, detail: &RequestDetail) -> Option<String> {
    (detail.attempts.len() > 1).then(|| {
        detail
            .attempts
            .iter()
            .map(|a| {
                let name = a
                    .line_name
                    .clone()
                    .or_else(|| a.upstream_host.clone())
                    .unwrap_or_else(|| a.member_id.to_string());
                match a.status {
                    Some(s) => format!("{name} {s}"),
                    None => format!("{name} {}", super::timing::outcome_text(tr, &a.outcome)),
                }
            })
            .collect::<Vec<_>>()
            .join(" → ")
    })
}

/// The cards of 通用.
pub fn cards(
    tr: &Tr,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
    detail: Option<&RequestDetail>,
) -> Vec<Card> {
    let client = tr.text(&row.client.text());
    let transport = requests::transport_text(tr, row.transport);
    let path = match (row.transport, row.request_kind) {
        (RequestTransport::WebSocket, _) => "WS response.create".to_owned(),
        (_, Some(RequestKind::Compaction)) => "POST /responses/compact".to_owned(),
        _ => "POST /responses".to_owned(),
    };
    // Rollout rows: where they come from instead of the transport the rollout
    // does not know.
    let mut request = vec![if row.source == RequestSource::Rollout {
        line(
            Some(tr.t("requests.detail.source")),
            tr.text(&row.source.text()),
        )
    } else {
        line(None, format!("{path} · {transport}"))
    }];
    // How the body went upstream (the answering attempt; HTTP only).
    if let Some(c) = detail
        .and_then(|d| d.attempts.last())
        .and_then(|a| a.compression)
    {
        request.push(line(
            Some(tr.t("requests.compression.label")),
            tr.text(&c.text()),
        ));
    }
    if let Some(m) = &row.model {
        let model = match &row.upstream_model {
            Some(u) if u != m => format!("{m} → {u}"),
            _ => m.clone(),
        };
        request.push(line(Some(tr.t("requests.detail.model")), model));
    }
    request.push(line(Some(tr.t("requests.detail.client")), client));

    let mut usage = vec![
        line(
            None,
            tr.f(
                "requests.detail.usage_in",
                &[
                    ("input", &tokens(row.fresh_input_tokens())),
                    ("cached", &tokens(row.cached_tokens)),
                ],
            ),
        ),
        line(
            None,
            tr.f(
                "requests.detail.usage_out",
                &[
                    ("write", &tokens(row.cache_write_tokens)),
                    ("output", &tokens(row.output_tokens)),
                    ("reasoning", &tokens(row.reasoning_tokens)),
                ],
            ),
        ),
    ];
    if let Some(raw) = row.input_tokens {
        usage.push(line(
            None,
            tr.f("requests.tip.input", &[("raw", &tokens(Some(raw)))]),
        ));
    }

    let mut speed = vec![
        line(
            None,
            tr.f(
                "requests.detail.speed_times",
                &[
                    ("ttft", &seconds(row.first_token_ms)),
                    ("duration", &seconds(row.duration_ms)),
                ],
            ),
        ),
        line(
            None,
            tr.f("requests.detail.speed_tps", &[("tps", &tps(row.tps_milli))]),
        ),
    ];
    if row.ttfb_ms.is_some() {
        speed.push(line(
            Some(tr.t("requests.detail.ttfb")),
            seconds(row.ttfb_ms),
        ));
    }
    // The speed tier by its name (标准 / Fast / Ultrafast), not the wire value
    // (ux-visual #9).
    if let Some(t) = requests::tier_text(tr, row) {
        speed.push(line(Some(tr.t("requests.detail.tier_label")), t));
    }
    if row.source == RequestSource::Rollout {
        speed.push(line(None, tr.t("requests.tip.router_only")));
    }

    let failover = match detail.and_then(|d| chain(tr, d)) {
        Some(c) => c,
        None if row.failovers > 0 => tr.f("requests.detail.failovers", &[("n", &row.failovers)]),
        None => tr.t("requests.detail.no_failover"),
    };
    let mut route = vec![if row.source == RequestSource::Rollout {
        line(
            None,
            format!(
                "{} · {}",
                provider_cell(row),
                tr.t("requests.detail.line_router_only")
            ),
        )
    } else {
        line(None, format!("{} · {failover}", provider_cell(row)))
    }];
    if let Some(id) = &row.upstream_request_id {
        route.push(mono_line(tr.t("requests.detail.upstream_id"), id.clone()));
    }
    if let Some(h) = &row.upstream_host {
        route.push(mono_line(tr.t("requests.detail.host"), h.clone()));
    }

    let mut session = Vec::new();
    session.push(line(None, session_cell(tr, titles, row)));
    let thread = row
        .thread_id
        .clone()
        .or_else(|| session_key(row).and_then(|k| titles.get(k).and_then(|t| t.thread_id.clone())));
    let mut thread_line = line(
        None,
        match &thread {
            Some(t) => format!(
                "thread {} · {}",
                requests::short_cache_key(t),
                kind_text(tr, row)
            ),
            None => kind_text(tr, row),
        },
    );
    thread_line.copy = thread
        .as_deref()
        .map(|t| copy_link(tr, "requests.detail.copy_thread", t));
    session.push(thread_line);
    if let Some(k) = &row.prompt_cache_key {
        let mut key_line = mono_line(tr.t("requests.col.cache_key"), k.clone());
        key_line.copy = Some(copy_link(tr, "requests.detail.copy_key", k));
        session.push(key_line);
    }

    let cost = super::prices::cost_lines(tr, row);

    vec![
        Card {
            title: tr.t("requests.detail.card.request"),
            lines: request,
        },
        Card {
            title: tr.t("requests.detail.card.usage"),
            lines: usage,
        },
        Card {
            title: tr.t("requests.detail.card.speed"),
            lines: speed,
        },
        Card {
            title: tr.t("requests.detail.card.route"),
            lines: route,
        },
        Card {
            title: tr.t("requests.detail.card.session"),
            lines: session,
        },
        Card {
            title: tr.t("requests.detail.card.cost"),
            lines: cost,
        },
    ]
}

/// What the 计时 tab says about the stats line for this request.
pub fn stats_line_note(tr: &Tr, row: &RequestRow) -> String {
    if !line_applicable(row) {
        return tr.t(if row.client == RequestClient::Exec {
            "requests.detail.line_exec"
        } else {
            "requests.detail.line_na"
        });
    }
    tr.t(if row.injected {
        "requests.detail.line_injected"
    } else {
        "requests.detail.line_not_injected"
    })
}

/// Draw the body of `tab`.
#[allow(clippy::too_many_arguments)]
pub fn body(
    ui: &mut Ui,
    cx: &Cx<'_>,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
    detail: Option<&RequestDetail>,
    loading: bool,
    tab: usize,
    out: &mut Vec<Action>,
) {
    match tab {
        0 => general(ui, cx, titles, row, detail, out),
        // Codex's session files have no timing marks: what was derived.
        1 if row.source == RequestSource::Rollout => rollout_timing(ui, cx, row),
        _ => super::timing::show(ui, cx, row, detail, loading),
    }
}

/// The 计时 tab of a row from Codex's session files: the derived duration and
/// why there is no waterfall.
fn rollout_timing(ui: &mut Ui, cx: &Cx<'_>, row: &RequestRow) {
    ui.add(
        egui::Label::new(Text::small(
            cx.tr.f(
                "requests.detail.rollout_timing",
                &[("duration", &seconds(row.duration_ms))],
            ),
            cx.pal,
        ))
        .wrap(),
    );
    ui.add(
        egui::Label::new(Text::caption(
            cx.tr.t("requests.detail.rollout_timing_note"),
            cx.pal,
        ))
        .wrap(),
    );
}

/// Heights of the rows of three 通用 cards: each as tall as a card with the
/// most lines in it ([`CARD_LINES`] at least).
pub fn card_rows(cards: &[Card]) -> Vec<f32> {
    cards
        .chunks(3)
        .map(|row| {
            let lines = row.iter().map(|c| c.lines.len()).max().unwrap_or(0);
            info_card::height(lines.max(CARD_LINES))
        })
        .collect()
}

/// Height the 通用 cards need: their rows, the gaps between them and a little
/// room under the last one.
pub fn general_height(cards: &[Card]) -> f32 {
    let rows = card_rows(cards);
    rows.iter().sum::<f32>() + CARD_GAP * rows.len().saturating_sub(1) as f32 + CARDS_BOTTOM
}

/// Height of the detail pane of `row` sized to its content: the title line, the
/// tabs and the 通用 cards. The other tabs get the same height (their lists and
/// bodies scroll), so switching tabs never moves the split.
pub fn pane_height(
    tr: &Tr,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
    detail: Option<&RequestDetail>,
) -> f32 {
    detail_pane::CHROME + general_height(&cards(tr, titles, row, detail))
}

fn general(
    ui: &mut Ui,
    cx: &Cx<'_>,
    titles: &BTreeMap<String, SessionTitle>,
    row: &RequestRow,
    detail: Option<&RequestDetail>,
    out: &mut Vec<Action>,
) {
    let cards = cards(cx.tr, titles, row, detail);
    let rows = card_rows(&cards);
    // The pane is normally as tall as its cards; one made smaller by hand (or a
    // very short window) scrolls rather than cutting the cards off.
    if general_height(&cards) <= ui.available_height() + 0.5 {
        card_grid(ui, cards, &rows, out);
    } else {
        ScrollArea::vertical()
            .id_salt("router-detail-general")
            .auto_shrink([false, false])
            .show(ui, |ui| card_grid(ui, cards, &rows, out));
    }
}

/// The cards in rows of three at the available width, each row `rows[i]` tall.
fn card_grid(ui: &mut Ui, cards: Vec<Card>, rows: &[f32], out: &mut Vec<Action>) {
    let origin = ui.available_rect_before_wrap().min;
    let width = ui.available_width();
    let card_w = ((width - CARD_GAP * 2.0) / 3.0).floor();
    let mut y = origin.y;
    for (i, card) in cards.into_iter().enumerate() {
        let h = rows.get(i / 3).copied().unwrap_or_default();
        let x = origin.x + (i % 3) as f32 * (card_w + CARD_GAP);
        let r = Rect::from_min_size(pos2(x, y), vec2(card_w, h));
        crate::design::layout::fixed(ui, r, |ui| {
            let mut c = InfoCard::new(card.title).min_height(h);
            for l in card.lines {
                c = c.push(l);
            }
            if let Some(text) = c.show(ui).copied {
                out.push(Action::Copy(text));
            }
        });
        if i % 3 == 2 {
            y += h + CARD_GAP;
        }
    }
    let total = rows.iter().sum::<f32>() + CARD_GAP * rows.len().saturating_sub(1) as f32;
    ui.allocate_rect(
        Rect::from_min_size(origin, vec2(width, total + CARDS_BOTTOM)),
        Sense::hover(),
    );
}
