//! Local entry points, token readiness and recorded traffic, without exposing credentials.

use std::time::Duration;

use egui::{Align, Layout, Ui};
use superba_core::{
    RequestHarness, RequestProtocol, RequestSource, StatsQuery, SwitchService, Timestamp,
};

use super::{Op, State};
use crate::design::icons::Icon;
use crate::design::widgets::{Card, FormRow, IconButton, Text, Toggle};
use crate::pages::Cx;

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
        cx.tr.text(&RequestProtocol::Gemini.text()),
    ];
    ui.scope(|ui| {
        // Other controls can widen a scrolling column beyond its visible pane.
        // Keep this card and its wrapped conversion labels inside that pane.
        let width = ui
            .available_width()
            .min((ui.clip_rect().right() - ui.cursor().left()).max(0.0));
        ui.set_width(width);
        Card::new().padding(16).show(ui, |ui| {
            FormRow::new(Text::section(cx.tr.t("router.ingress.title"), cx.pal))
                .label_width(0.0)
                .right()
                .show(ui, |ui| {
                    ui.label(Text::caption(&token, cx.pal));
                });
            for (i, name) in names.iter().enumerate() {
                ui.push_id(i, |ui| {
                    let url = address(port, i);
                    let count = state
                        .ingress
                        .value
                        .as_ref()
                        .filter(|_| state.ingress.error.is_none())
                        .map(|counts| cx.tr.f("router.ingress.recent", &[("n", &counts[i])]))
                        .unwrap_or_else(|| cx.tr.t("router.ingress.recent_unknown"));
                    let compact = ui.available_width() < 720.0;
                    if compact {
                        FormRow::new(Text::strong(name, cx.pal))
                            .label_width(0.0)
                            .right()
                            .show(ui, |ui| {
                                ui.label(Text::caption(&count, cx.pal))
                                    .on_hover_text(cx.tr.t("router.ingress.count_hint"));
                            });
                    }
                    let row = if compact {
                        FormRow::unlabeled()
                    } else {
                        FormRow::new(Text::strong(name, cx.pal)).label_width(140.0)
                    };
                    row.show(ui, |ui| {
                        // Reserve trailing controls so the URL cannot push them offscreen.
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let copy = cx.tr.f("router.ingress.copy", &[("name", name)]);
                            if ui
                                .add(IconButton::new(Icon::Copy, copy).enabled(url.is_some()))
                                .clicked()
                                && let Some(url) = &url
                            {
                                ui.ctx().copy_text(url.clone());
                            }
                            if !compact {
                                ui.label(Text::caption(&count, cx.pal))
                                    .on_hover_text(cx.tr.t("router.ingress.count_hint"));
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new(Text::mono_muted(
                                        url.clone().unwrap_or_else(|| {
                                            cx.tr.t("router.ingress.no_address")
                                        }),
                                        cx.pal,
                                    ))
                                    .truncate(),
                                )
                                .on_hover_text(url.unwrap_or_else(|| PATHS[i].to_owned()));
                            });
                        });
                    });
                    if i + 1 < names.len() {
                        crate::pages::divider(ui, cx);
                    }
                });
            }
            ui.add_space(8.0);
            translation(ui, cx, state);
        });
    });
}

fn translation(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    if let Some(error) = &state.conversion_error {
        crate::pages::note(ui, cx, crate::design::widgets::Status::Error, error);
    }
    let conversions = state.snapshot.as_ref().and_then(|s| s.translation.clone());
    let Some(conversions) = conversions else {
        ui.label(Text::small(cx.tr.t("router.ingress.translation"), cx.pal));
        ui.label(Text::caption(
            cx.tr.t("router.ingress.translation_unknown"),
            cx.pal,
        ));
        return;
    };
    let enabled = conversions.iter().filter(|c| c.enabled).count();
    let title = cx.tr.f(
        "router.ingress.translation_state",
        &[("enabled", &enabled), ("total", &conversions.len())],
    );
    egui::CollapsingHeader::new(title)
        .id_salt("router-conversion-directions")
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(Text::caption(
                    cx.tr.t("router.ingress.translation_hint"),
                    cx.pal,
                ))
                .wrap(),
            );
            for mut conversion in conversions {
                let label = format!(
                    "{} → {}",
                    cx.tr.text(&conversion.from.text()),
                    cx.tr.text(&conversion.to.text()),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            Toggle::new(&mut conversion.enabled, &label)
                                .enabled(state.op.is_none() && state.error.is_none()),
                        )
                        .changed()
                    {
                        state.start(cx, Op::Conversion, |b| b.set_conversion(conversion));
                    }
                    ui.add(egui::Label::new(Text::small(label, cx.pal)).wrap());
                });
            }
        });
}
