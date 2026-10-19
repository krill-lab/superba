//! Local entry points, token readiness and recorded traffic, without exposing credentials.

use std::time::Duration;

use egui::{Align, Layout, Ui};
use superba_core::{
    RequestHarness, RequestProtocol, RequestSource, StatsQuery, SwitchService, Timestamp,
};

use super::State;
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Card, Text, Toggle};
use crate::pages::Cx;

/// Entry points shipped in this build. Gemini remains a visible placeholder.
pub const PATHS: [&str; 3] = ["/backend-api/codex", "/claude", "/openai/v1"];

/// Absolute local URL, only when a port is configured.
pub fn address(port: Option<u16>, index: usize) -> Option<String> {
    Some(format!("http://127.0.0.1:{}{}", port?, PATHS.get(index)?))
}

/// Recorded traffic in the last 24 hours. Protocol identifies Messages; tool
/// attribution separates Codex from other Responses clients. Counts describe
/// recorded tool/protocol traffic, since old request records do not retain paths.
pub async fn recent_counts(
    service: &dyn SwitchService,
    now: Timestamp,
) -> superba_core::Result<[u64; 3]> {
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
    Ok([codex, claude, openai])
}

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
    let token = cx.tr.t(match snapshot {
        Some(s) if s.token_ready => "router.ingress.token_ready",
        Some(_) => "router.ingress.token_missing",
        None => "router.ingress.token_unknown",
    });
    let names = [
        cx.tr.text(&RequestHarness::Codex.text()),
        cx.tr.text(&RequestHarness::ClaudeCode.text()),
        cx.tr.t("router.ingress.openai"),
    ];
    Card::new().padding(12).show(ui, |ui| {
        ui.label(Text::section(cx.tr.t("router.ingress.title"), cx.pal));
        for (i, name) in names.iter().enumerate() {
            ui.push_id(i, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(Text::small(name, cx.pal));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(Text::caption(&token, cx.pal));
                    });
                });
                let url = address(port, i);
                ui.horizontal(|ui| {
                    let copy = cx.tr.f("router.ingress.copy", &[("name", name)]);
                    let button = ui
                        .add(
                            Button::ghost(cx.tr.t("requests.detail.copy"))
                                .icon(Icon::Copy)
                                .small()
                                .enabled(url.is_some()),
                        )
                        .accessible_name(copy);
                    if button.clicked()
                        && let Some(url) = &url
                    {
                        ui.ctx().copy_text(url.clone());
                    }
                    ui.add(
                        egui::Label::new(Text::small(
                            url.clone()
                                .unwrap_or_else(|| cx.tr.t("router.ingress.no_address")),
                            cx.pal,
                        ))
                        .truncate(),
                    )
                    .on_hover_text(url.unwrap_or_else(|| PATHS[i].to_owned()));
                });
                let count = state
                    .ingress
                    .value
                    .as_ref()
                    .filter(|_| state.ingress.error.is_none())
                    .map(|counts| cx.tr.f("router.ingress.recent", &[("n", &counts[i])]))
                    .unwrap_or_else(|| cx.tr.t("router.ingress.recent_unknown"));
                ui.label(Text::caption(count, cx.pal))
                    .on_hover_text(cx.tr.t("router.ingress.count_hint"));
            });
        }
        ui.add_space(8.0);
        ui.label(Text::caption(
            format!("{} · /gemini", cx.tr.t("router.ingress.gemini_pending")),
            cx.pal,
        ));
        ui.horizontal(|ui| {
            let mut enabled = false;
            ui.add(Toggle::new(&mut enabled, cx.tr.t("router.ingress.translation")).enabled(false));
            ui.label(Text::small(cx.tr.t("router.ingress.translation"), cx.pal));
        });
        ui.add(
            egui::Label::new(Text::caption(
                cx.tr.t("router.ingress.translation_pending"),
                cx.pal,
            ))
            .wrap(),
        );
    });
}
