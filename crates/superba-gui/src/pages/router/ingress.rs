//! Local entry points, token readiness and recorded traffic, without exposing credentials.

use std::time::Duration;

use egui::Ui;
use superba_core::{
    RequestHarness, RequestProtocol, RequestSource, StatsQuery, SwitchService, Timestamp,
};

use super::{Op, State};
use crate::design::elide::Shape;
use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::{size, space, text};
use crate::design::widgets::value_chip::ValueChip;
use crate::design::widgets::{Badge, IconButton, Status, Table, Text, Toggle};
use crate::pages::{Cx, section_with};

/// Client base URLs of the four entry points shipped in this build.
pub const PATHS: [&str; 4] = ["/backend-api/codex", "/claude", "/openai/v1", "/gemini"];

/// Absolute local URL, only when a port is configured.
pub fn address(port: Option<u16>, index: usize) -> Option<String> {
    Some(format!("http://127.0.0.1:{}{}", port?, PATHS.get(index)?))
}

/// Recorded traffic in the last 24 hours. Protocol identifies Messages/Gemini; tool
/// attribution separates Codex from other Responses clients. Counts describe
/// recorded tool/protocol traffic, since old request records do not retain paths.
pub async fn recent_counts(
    service: &dyn SwitchService,
    now: Timestamp,
) -> superba_core::Result<[u64; 4]> {
    let base = StatsQuery {
        from: Some(now.saturating_sub(Duration::from_secs(86_400))),
        to: Some(now),
        source: Some(RequestSource::Router),
        ..StatsQuery::default()
    };
    let claude = service
        .router_stats(StatsQuery {
            protocol: Some(RequestProtocol::Messages),
            ..base.clone()
        })
        .await?
        .totals
        .requests;
    let mut codex = 0;
    let mut openai = 0;
    for protocol in [RequestProtocol::Responses, RequestProtocol::ChatCompletions] {
        let all = service
            .router_stats(StatsQuery {
                protocol: Some(protocol),
                ..base.clone()
            })
            .await?
            .totals
            .requests;
        let own = service
            .router_stats(StatsQuery {
                protocol: Some(protocol),
                harnesses: vec![RequestHarness::Codex],
                ..base.clone()
            })
            .await?
            .totals
            .requests;
        codex += own;
        openai += all.saturating_sub(own);
    }
    let gemini = service
        .router_stats(StatsQuery {
            protocol: Some(RequestProtocol::Gemini),
            ..base
        })
        .await?
        .totals
        .requests;
    Ok([codex, claude, openai, gemini])
}

/// 入口 / Entry points: whether the router's token is ready, then a table of
/// the four entry points — name, address (elided in the middle, the whole URL
/// on hover), requests recorded in the last 24 hours (right-aligned) and a copy
/// action in the last column — and the protocol conversion switches.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    state.ingress.poll();
    if state.ingress.due(Some(Duration::from_secs(10))) {
        let service = cx.service.clone();
        let now = cx.now;
        state
            .ingress
            .start(async move { recent_counts(service.as_ref(), now).await });
    }
    let snapshot = state.snapshot.as_ref();
    let port = snapshot.and_then(|s| s.port);
    let (token_status, token) = match snapshot {
        Some(s) if s.token_ready => (Status::Ok, "router.ingress.token_ready"),
        Some(_) => (Status::Warn, "router.ingress.token_missing"),
        None => (Status::Idle, "router.ingress.token_unknown"),
    };
    let names = [
        cx.tr.text(&RequestHarness::Codex.text()),
        cx.tr.text(&RequestHarness::ClaudeCode.text()),
        cx.tr.t("router.ingress.openai"),
        cx.tr.text(&RequestProtocol::Gemini.text()),
    ];
    section_with(ui, cx, &cx.tr.t("router.ingress.title"), None, |ui| {
        ui.add(Badge::new(cx.tr.t(token)).status(token_status));
    });
    let headers = [
        cx.tr.t("router.ingress.col.name"),
        cx.tr.t("router.ingress.col.address"),
        cx.tr.t("router.ingress.col.recent"),
        String::new(),
    ];
    // The name and count columns are as wide as their widest text.
    let width = |text: &str, f: egui::FontId| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), f, egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    let name_w = names
        .iter()
        .map(|n| width(n, font(text::BODY, Weight::Medium)))
        .fold(
            width(&headers[0], font(text::CAPTION, Weight::Medium)),
            f32::max,
        )
        + space::M * 2.0
        + crate::pages::shared::tool_label::ROOM;
    let count_w = width(&headers[2], font(text::CAPTION, Weight::Medium))
        .max(width("999,999", font(text::SMALL, Weight::Regular)))
        + space::M * 2.0;
    let table = Table::new(&[0.0, 1.0, 0.0, 0.0])
        .min_widths(&[name_w, 0.0, count_w, size::CONTROL_SM + space::M])
        .right_aligned(&[2, 3]);
    table.header(ui, &headers);
    let counts = state
        .ingress
        .value
        .filter(|_| state.ingress.error.is_none());
    for (i, name) in names.iter().enumerate() {
        let url = address(port, i);
        ui.push_id(i, |ui| {
            table.row(ui, size::ROW, i + 1 == names.len(), |ui, col| match col {
                0 => {
                    // Codex, Claude Code and Gemini with their icon; the
                    // OpenAI-compatible entry has no single tool.
                    let tool = [Some("cx"), Some("cc"), None, Some("gm")][i];
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(
                                crate::pages::shared::tool_label::TILE,
                                crate::pages::shared::tool_label::TILE,
                            ),
                            egui::Sense::hover(),
                        );
                        if let Some(m) = tool {
                            crate::pages::shared::tool_label::paint(ui, m, r.center());
                        }
                        ui.label(Text::strong(name, cx.pal));
                    });
                }
                1 => match &url {
                    // An address: monospace, shortened in the middle.
                    Some(url) => {
                        ui.add(
                            ValueChip::new(url, Shape::Url)
                                .plain()
                                .size(text::SMALL)
                                .max_width(ui.available_width()),
                        );
                    }
                    None => {
                        ui.label(Text::small(cx.tr.t("router.ingress.no_address"), cx.pal))
                            .on_hover_text(PATHS[i]);
                    }
                },
                2 => {
                    let count = counts.map_or_else(
                        || "—".to_owned(),
                        |c| crate::pages::shared::format::group_thousands(c[i]),
                    );
                    ui.label(Text::small(count, cx.pal).color(cx.pal.text))
                        .on_hover_text(cx.tr.t("router.ingress.count_hint"));
                }
                _ => {
                    let copy = cx.tr.f("router.ingress.copy", &[("name", name)]);
                    if ui
                        .add(
                            IconButton::new(Icon::Copy, copy)
                                .size(size::CONTROL_SM)
                                .enabled(url.is_some()),
                        )
                        .clicked()
                        && let Some(url) = &url
                    {
                        ui.ctx().copy_text(url.clone());
                    }
                }
            });
        });
    }
}

/// 「协议转换」 / "Protocol conversion": a plain section like 「入口」 (user bug
/// 116): the title with how many directions are on at its right, what the
/// switches mean, then the matrix.
pub(super) fn conversion_section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let conversions = state.snapshot.as_ref().and_then(|s| s.translation.clone());
    let count = conversions.as_ref().map(|c| {
        cx.tr.f(
            "router.ingress.translation_count",
            &[
                ("enabled", &c.iter().filter(|c| c.enabled).count()),
                ("total", &c.len()),
            ],
        )
    });
    section_with(
        ui,
        cx,
        &cx.tr.t("router.ingress.translation"),
        Some(&cx.tr.t("router.ingress.translation_hint")),
        |ui| {
            if let Some(count) = &count {
                ui.label(Text::small(count, cx.pal));
            }
        },
    );
    if let Some(error) = &state.conversion_error {
        crate::pages::note(ui, cx, Status::Error, error);
    }
    match conversions {
        Some(conversions) => conversion_matrix(ui, cx, state, &conversions),
        None => {
            ui.label(
                Text::small(cx.tr.t("router.ingress.translation_unknown"), cx.pal)
                    .color(cx.pal.text_subtle),
            );
        }
    }
}

/// The conversion directions as a matrix: a row per client protocol, a column
/// per upstream protocol, a switch where a conversion exists ("—" elsewhere).
/// Each switch is named "client → upstream", as the list it replaces was.
fn conversion_matrix(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    conversions: &[superba_core::RouterConversion],
) {
    let axes = matrix_axes(conversions);
    let (from, to) = (&axes, &axes);
    let mut headers = vec![cx.tr.t("router.ingress.col.client_protocol")];
    headers.extend(to.iter().map(|p| cx.tr.text(&p.text())));
    let mut weights = vec![1.4];
    weights.extend(to.iter().map(|_| 1.0));
    // Every column at least as wide as its header ("Anthropic Messages" ran
    // into "Gemini" at 960 pt) and the first as its row names; the rest is shared.
    let width = |text: &str, f: egui::FontId| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), f, egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    let caption = font(text::CAPTION, Weight::Medium);
    let mut min_widths: Vec<f32> = headers
        .iter()
        .map(|h| width(h, caption.clone()) + space::M)
        .collect();
    min_widths[0] = from
        .iter()
        .map(|p| width(&cx.tr.text(&p.text()), font(text::SMALL, Weight::Regular)) + space::M)
        .fold(min_widths[0], f32::max);
    let table = Table::new(&weights).min_widths(&min_widths);
    table.header(ui, &headers);
    let enabled = state.op.is_none() && state.error.is_none();
    for (r, client) in from.iter().enumerate() {
        ui.push_id(("conversion", r), |ui| {
            table.row(ui, size::ROW, r + 1 == from.len(), |ui, col| {
                if col == 0 {
                    ui.label(Text::small(cx.tr.text(&client.text()), cx.pal).color(cx.pal.text));
                    return;
                }
                let upstream = to[col - 1];
                match conversions
                    .iter()
                    .find(|c| c.from == *client && c.to == upstream)
                {
                    Some(c) => {
                        let mut c = *c;
                        let label = format!(
                            "{} → {}",
                            cx.tr.text(&c.from.text()),
                            cx.tr.text(&c.to.text()),
                        );
                        if ui
                            .add(Toggle::new(&mut c.enabled, &label).enabled(enabled))
                            .changed()
                        {
                            state.start(cx, Op::Conversion, |b| b.set_conversion(c));
                        }
                    }
                    None => {
                        ui.label(Text::small("—", cx.pal));
                    }
                }
            });
        });
    }
}

/// The protocols of the conversion matrix, the same list for its rows (client
/// protocols) and its columns (upstream protocols) so that "no conversion"
/// runs down the diagonal: Responses, Chat Completions, Messages, Gemini, the
/// ones that appear in `conversions`.
pub fn matrix_axes(conversions: &[superba_core::RouterConversion]) -> Vec<RequestProtocol> {
    [
        RequestProtocol::Responses,
        RequestProtocol::ChatCompletions,
        RequestProtocol::Messages,
        RequestProtocol::Gemini,
    ]
    .into_iter()
    .filter(|p| conversions.iter().any(|c| c.from == *p || c.to == *p))
    .collect()
}
