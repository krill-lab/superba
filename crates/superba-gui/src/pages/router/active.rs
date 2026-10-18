//! 活动请求 (UX §3.5): requests in flight through the local router, from its
//! admin API, refreshed every second while visible. Finished requests move to
//! 最近请求.

use std::time::Duration;

use egui::Ui;
use superba_core::InflightRequest;

use super::db::Loader;
use super::requests::{self, ActiveCol};
use super::{State, mode};
use crate::design::layout;
use crate::design::widgets::Text;
use crate::design::widgets::log_table::LogTable;
use crate::pages::{Action, Cx, error_text};

/// Refresh interval.
const EVERY: Duration = Duration::from_secs(1);

/// State of 活动请求.
#[derive(Debug, Default)]
pub struct Active {
    /// Requests in flight.
    pub inflight: Loader<Vec<InflightRequest>>,
    /// Selected router-local id.
    pub selected: Option<u64>,
}

impl Active {
    /// A read is running.
    pub fn loading(&self) -> bool {
        self.inflight.loading()
    }

    /// The rows after the search filter.
    pub fn rows<'a>(
        &'a self,
        titles: &std::collections::BTreeMap<String, superba_core::SessionTitle>,
        search: &str,
    ) -> Vec<&'a InflightRequest> {
        let needle = search.trim().to_lowercase();
        self.inflight
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|r| {
                needle.is_empty()
                    || [
                        r.prompt_cache_key.as_deref(),
                        Some(r.member_name.as_str()),
                        r.line_name.as_deref(),
                        r.model.as_deref(),
                        r.prompt_cache_key
                            .as_deref()
                            .and_then(|k| titles.get(k))
                            .map(|t| t.title.as_str()),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|f| f.to_lowercase().contains(&needle))
            })
            .collect()
    }
}

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let routed = mode(cx).is_router();
    let a = &mut state.active;
    a.inflight.poll();
    if a.inflight.due(Some(EVERY)) {
        let service = cx.service.clone();
        a.inflight
            .start(async move { service.router_inflight().await });
    }
    let keys: Vec<&str> = a
        .inflight
        .value
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.prompt_cache_key.as_deref())
        .collect();
    state.titles.want(cx.service, keys);
    let titles = state.titles.map();
    let a = &mut state.active;
    let rows = a.rows(titles, &state.search);
    let columns = requests::active_columns(cx.tr);
    let selected = a
        .selected
        .and_then(|id| rows.iter().position(|r| r.id == id));
    let resp = LogTable::new("router-active", &columns, rows.len())
        .label(cx.tr.t("usage.tab.active"))
        .selected(selected)
        .show(ui, |row, col| {
            requests::active_cell(cx.tr, titles, rows[row], ActiveCol::ALL[col])
        });
    let picked = resp
        .clicked
        .or(resp.key_selected)
        .map(|i| rows.get(i).map(|r| r.id));
    let empty = rows.is_empty();
    if let Some(id) = picked {
        a.selected = id;
    }
    if empty {
        let body = resp.body_rect.shrink(16.0);
        layout::fixed(ui, body, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((body.height() / 2.0 - 20.0).max(0.0));
                let text = match &a.inflight.error {
                    Some(e) => error_text(cx.tr, e),
                    None if !routed => cx.tr.t("requests.active.router_off"),
                    None => cx.tr.t("requests.active.empty"),
                };
                ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
                // Only the router sees requests while they run.
                if !routed
                    && a.inflight.error.is_none()
                    && ui
                        .add(
                            crate::design::widgets::Button::ghost(
                                cx.tr.t("requests.active.router_off_action"),
                            )
                            .small(),
                        )
                        .clicked()
                {
                    out.push(Action::Go(crate::pages::Page::Router));
                }
            });
        });
    }
}
