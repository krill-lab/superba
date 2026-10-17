//! Usage view data from the OpenAI organization Usage and Costs APIs (Krill).
//!
//! - `GET /v1/organization/usage/completions?start_time&end_time&bucket_width=1d
//!   &group_by=model&limit` answers `{"object":"page","data":[{"object":"bucket",
//!   "start_time","end_time","results":[{"input_tokens","output_tokens",
//!   "input_cached_tokens","num_model_requests","model","cost_usd"}]}],
//!   "has_more","next_page"}`; `cost_usd` is Krill's extension.
//! - `GET /v1/organization/costs?start_time&end_time&bucket_width=1d&limit`
//!   answers the same page shape with results
//!   `{"amount":{"value":number,"currency":"usd"},"line_item","project_id"}`.
//!
//! Krill allows 10 usage requests per hour per user, so the client fetches one
//! page of daily buckets per endpoint (at most [`superba_core::UsageQuery::MAX_DAYS`]
//! days) and follows `next_page` only a few times. Daily cost comes from the costs
//! API when it answers, else from the summed `cost_usd`; per-model rows come from
//! the usage API grouped by model.

use std::collections::BTreeMap;

use serde_json::Value;
use superba_core::{CostSource, DailyUsage, ModelUsage, SiteId, Timestamp, Usd};

use crate::account::{count, dollars, number};

/// Label of usage rows whose model the site does not name.
pub(crate) const UNKNOWN_MODEL: &str = "(unknown)";

/// One parsed result row of a usage or costs bucket.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Row {
    pub model: Option<String>,
    pub cost: Option<Usd>,
    pub requests: Option<u64>,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// One bucket (`start_time` in Unix seconds).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Bucket {
    pub start_time: i64,
    pub rows: Vec<Row>,
}

/// One page of buckets.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Page {
    pub buckets: Vec<Bucket>,
    pub next_page: Option<String>,
}

/// Which API a page came from (selects the result fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Api {
    /// Usage API, completions.
    Usage,
    /// Costs API.
    Costs,
}

/// Parse one page. The answer must be a JSON object with a `data` array of
/// buckets (`object: "page"` is not required); unknown fields are ignored.
pub(crate) fn parse_page(api: Api, body: &[u8]) -> Result<Page, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "the answer is not JSON".to_owned())?;
    let obj = v.as_object().ok_or("the answer is not a JSON object")?;
    let data = obj
        .get("data")
        .and_then(Value::as_array)
        .ok_or("the answer has no data array")?;
    let mut page = Page::default();
    for bucket in data {
        let Some(b) = bucket.as_object() else {
            continue;
        };
        let Some(start_time) = b.get("start_time").and_then(number).map(|f| f as i64) else {
            continue;
        };
        let rows = b
            .get("results")
            .or_else(|| b.get("result"))
            .and_then(Value::as_array)
            .map(|rs| rs.iter().filter_map(|r| parse_row(api, r)).collect())
            .unwrap_or_default();
        page.buckets.push(Bucket { start_time, rows });
    }
    let has_more = obj
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    page.next_page = obj
        .get("next_page")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| has_more && !s.is_empty())
        .map(str::to_owned);
    Ok(page)
}

fn parse_row(api: Api, v: &Value) -> Option<Row> {
    let r = v.as_object()?;
    let model = r
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some(match api {
        Api::Usage => Row {
            model,
            cost: r.get("cost_usd").and_then(dollars),
            requests: r.get("num_model_requests").and_then(count),
            input_tokens: r.get("input_tokens").and_then(count),
            cached_input_tokens: r.get("input_cached_tokens").and_then(count),
            output_tokens: r.get("output_tokens").and_then(count),
        },
        Api::Costs => {
            let amount = r.get("amount")?;
            let cost = match amount {
                Value::Object(a) => {
                    let currency = a.get("currency").and_then(Value::as_str);
                    if currency.is_some_and(|c| !c.eq_ignore_ascii_case("usd")) {
                        // Only US dollars are summed; other currencies are skipped.
                        return None;
                    }
                    a.get("value").and_then(dollars)
                }
                other => dollars(other),
            };
            Row {
                model: None,
                cost: Some(cost?),
                ..Row::default()
            }
        }
    })
}

/// Inputs of [`build_report`].
pub(crate) struct ReportParts<'a> {
    pub site_id: &'a SiteId,
    pub start: Timestamp,
    pub days: u32,
    pub fetched_at: Timestamp,
    /// Costs API buckets, when it answered.
    pub costs: Option<&'a [Bucket]>,
    /// Usage API buckets (grouped by model), when it answered.
    pub usage: Option<&'a [Bucket]>,
}

/// Assemble the report: one row per day (oldest first), per-model totals sorted
/// by cost, then requests, then name.
pub(crate) fn build_report(parts: ReportParts<'_>) -> superba_core::UsageReport {
    let start_secs = parts.start.as_unix_secs();
    let days = parts.days as usize;
    let day_of = |bucket_start: i64| -> Option<usize> {
        let offset = bucket_start.checked_sub(start_secs)?;
        let index = usize::try_from(offset.div_euclid(86_400)).ok()?;
        (index < days).then_some(index)
    };
    let mut daily: Vec<DailyUsage> = (0..days)
        .map(|i| DailyUsage {
            start: Timestamp::from_unix_secs(start_secs + i as i64 * 86_400),
            ..DailyUsage::default()
        })
        .collect();

    let usage_has_cost = parts
        .usage
        .is_some_and(|b| b.iter().flat_map(|b| &b.rows).any(|r| r.cost.is_some()));
    let cost_source = if parts.costs.is_some() {
        Some(CostSource::Costs)
    } else if usage_has_cost {
        Some(CostSource::UsageApi)
    } else {
        None
    };
    if cost_source.is_some() {
        for d in &mut daily {
            d.cost = Some(Usd::ZERO);
        }
    }
    if let Some(costs) = parts.costs {
        for bucket in costs {
            if let Some(i) = day_of(bucket.start_time) {
                let sum: Usd = bucket.rows.iter().filter_map(|r| r.cost).sum();
                daily[i].cost = Some(daily[i].cost.unwrap_or_default().saturating_add(sum));
            }
        }
    }

    let mut models: BTreeMap<String, ModelUsage> = BTreeMap::new();
    if let Some(usage) = parts.usage {
        for d in &mut daily {
            d.requests = Some(0);
            d.input_tokens = Some(0);
            d.output_tokens = Some(0);
        }
        for bucket in usage {
            let index = day_of(bucket.start_time);
            for row in &bucket.rows {
                if let Some(i) = index {
                    let d = &mut daily[i];
                    add(&mut d.requests, row.requests);
                    add(&mut d.input_tokens, row.input_tokens);
                    add(&mut d.output_tokens, row.output_tokens);
                    if cost_source == Some(CostSource::UsageApi)
                        && let Some(c) = row.cost
                    {
                        d.cost = Some(d.cost.unwrap_or_default().saturating_add(c));
                    }
                }
                if index.is_none() {
                    continue;
                }
                let name = row
                    .model
                    .clone()
                    .unwrap_or_else(|| UNKNOWN_MODEL.to_owned());
                let m = models.entry(name.clone()).or_insert_with(|| ModelUsage {
                    model: name,
                    ..ModelUsage::default()
                });
                if let Some(c) = row.cost {
                    m.cost = Some(m.cost.unwrap_or_default().saturating_add(c));
                }
                m.requests = m.requests.saturating_add(row.requests.unwrap_or(0));
                m.input_tokens = m.input_tokens.saturating_add(row.input_tokens.unwrap_or(0));
                m.cached_input_tokens = m
                    .cached_input_tokens
                    .saturating_add(row.cached_input_tokens.unwrap_or(0));
                m.output_tokens = m
                    .output_tokens
                    .saturating_add(row.output_tokens.unwrap_or(0));
            }
        }
    }
    let mut models: Vec<ModelUsage> = models.into_values().collect();
    models.sort_by(|a, b| {
        b.cost
            .unwrap_or_default()
            .cmp(&a.cost.unwrap_or_default())
            .then(b.requests.cmp(&a.requests))
            .then(a.model.cmp(&b.model))
    });
    let total_cost = cost_source.map(|_| daily.iter().filter_map(|d| d.cost).sum());
    superba_core::UsageReport {
        site_id: parts.site_id.clone(),
        start: parts.start,
        end: Timestamp::from_unix_secs(start_secs + days as i64 * 86_400),
        days: parts.days,
        daily,
        models,
        total_cost,
        cost_source,
        usage_available: parts.usage.is_some(),
        fetched_at: parts.fetched_at,
        from_cache: false,
        notice: None,
    }
}

fn add(slot: &mut Option<u64>, value: Option<u64>) {
    if let Some(v) = value {
        *slot = Some(slot.unwrap_or(0).saturating_add(v));
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    const DAY0: i64 = 1_793_491_200; // 2026-11-01T00:00Z

    fn usage_page() -> Value {
        json!({
            "object": "page",
            "data": [
                {"object": "bucket", "start_time": DAY0, "end_time": DAY0 + 86_400, "results": [
                    {"object": "organization.usage.completions.result", "input_tokens": 1000,
                     "input_cached_tokens": 400, "output_tokens": 50, "num_model_requests": 3,
                     "model": "gpt-a", "cost_usd": 0.25},
                    {"input_tokens": "10", "output_tokens": 5, "num_model_requests": 1,
                     "model": null, "cost_usd": "0.000001"}
                ]},
                {"object": "bucket", "start_time": DAY0 + 86_400, "end_time": DAY0 + 2 * 86_400, "results": [
                    {"input_tokens": 2000, "output_tokens": 100, "num_model_requests": 4,
                     "model": "gpt-b", "cost_usd": 1.5},
                    {"input_tokens": 1, "output_tokens": 1, "num_model_requests": 1,
                     "model": "gpt-a", "cost_usd": 0.25}
                ]},
                {"object": "bucket", "start_time": DAY0 + 9 * 86_400, "results": [
                    {"model": "outside", "num_model_requests": 99, "cost_usd": 9}
                ]}
            ],
            "has_more": false,
            "next_page": null
        })
    }

    fn parse(api: Api, v: &Value) -> Page {
        parse_page(api, v.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn usage_pages_parse_and_aggregate_by_day_and_model() {
        let page = parse(Api::Usage, &usage_page());
        assert_eq!(page.buckets.len(), 3);
        assert_eq!(page.next_page, None);
        let site = SiteId::new("krill");
        let report = build_report(ReportParts {
            site_id: &site,
            start: Timestamp::from_unix_secs(DAY0),
            days: 3,
            fetched_at: Timestamp::from_unix_secs(DAY0 + 3600),
            costs: None,
            usage: Some(&page.buckets),
        });
        assert_eq!(report.cost_source, Some(CostSource::UsageApi));
        assert_eq!(report.daily.len(), 3);
        assert_eq!(report.daily[0].cost, Some(Usd::from_micros(250_001)));
        assert_eq!(report.daily[0].requests, Some(4));
        assert_eq!(report.daily[1].cost, Some(Usd::from_micros(1_750_000)));
        assert_eq!(report.daily[2].cost, Some(Usd::ZERO), "empty days are zero");
        assert_eq!(report.daily[2].requests, Some(0));
        assert_eq!(report.total_cost, Some(Usd::from_micros(2_000_001)));
        assert_eq!(report.end.utc_date_string(), "2026-11-04");
        assert_eq!(
            report
                .models
                .iter()
                .map(|m| (m.model.as_str(), m.requests))
                .collect::<Vec<_>>(),
            [("gpt-b", 4), ("gpt-a", 4), (UNKNOWN_MODEL, 1)],
            "most expensive first; buckets outside the range are dropped"
        );
        let a = &report.models[1];
        assert_eq!(a.cost, Some(Usd::from_micros(500_000)));
        assert_eq!(a.input_tokens, 1001);
        assert_eq!(a.cached_input_tokens, 400);
        assert!(report.usage_available);
    }

    #[test]
    fn costs_take_precedence_and_skip_foreign_currencies() {
        let costs = json!({
            "object": "page",
            "data": [
                {"start_time": DAY0, "results": [
                    {"object": "organization.costs.result", "amount": {"value": 0.5, "currency": "usd"}, "line_item": null},
                    {"amount": {"value": "0.25", "currency": "USD"}},
                    {"amount": {"value": 100, "currency": "eur"}}
                ]},
                {"start_time": DAY0 + 86_400, "results": []}
            ],
            "has_more": true,
            "next_page": "page_abc"
        });
        let costs = parse(Api::Costs, &costs);
        assert_eq!(costs.next_page.as_deref(), Some("page_abc"));
        let usage = parse(Api::Usage, &usage_page());
        let site = SiteId::new("krill");
        let report = build_report(ReportParts {
            site_id: &site,
            start: Timestamp::from_unix_secs(DAY0),
            days: 2,
            fetched_at: Timestamp::from_unix_secs(DAY0),
            costs: Some(&costs.buckets),
            usage: Some(&usage.buckets),
        });
        assert_eq!(report.cost_source, Some(CostSource::Costs));
        assert_eq!(report.daily[0].cost, Some(Usd::from_micros(750_000)));
        assert_eq!(report.daily[1].cost, Some(Usd::ZERO));
        assert_eq!(report.total_cost, Some(Usd::from_micros(750_000)));
        assert_eq!(
            report.daily[1].requests,
            Some(5),
            "usage still feeds counts"
        );
        assert_eq!(report.models[0].model, "gpt-b");
    }

    #[test]
    fn reports_without_cost_data_say_so() {
        let usage = json!({"data": [{"start_time": DAY0, "results": [
            {"model": "m", "num_model_requests": 2, "input_tokens": 5, "output_tokens": 1}
        ]}]});
        let usage = parse(Api::Usage, &usage);
        let site = SiteId::new("x");
        let report = build_report(ReportParts {
            site_id: &site,
            start: Timestamp::from_unix_secs(DAY0),
            days: 1,
            fetched_at: Timestamp::from_unix_secs(DAY0),
            costs: None,
            usage: Some(&usage.buckets),
        });
        assert_eq!(report.cost_source, None);
        assert_eq!(report.total_cost, None);
        assert_eq!(report.daily[0].cost, None);
        assert_eq!(report.models[0].cost, None);
        assert_eq!(report.models[0].requests, 2);

        let empty = build_report(ReportParts {
            site_id: &site,
            start: Timestamp::from_unix_secs(DAY0),
            days: 2,
            fetched_at: Timestamp::from_unix_secs(DAY0),
            costs: None,
            usage: None,
        });
        assert!(!empty.usage_available);
        assert_eq!(empty.daily[0].requests, None);
        assert!(empty.models.is_empty());
    }

    #[test]
    fn malformed_pages_are_rejected_and_odd_rows_skipped() {
        assert!(parse_page(Api::Usage, b"<html>").is_err());
        assert!(parse_page(Api::Usage, br#"{"object":"page"}"#).is_err());
        assert!(parse_page(Api::Usage, b"[]").is_err());
        let page = parse(
            Api::Costs,
            &json!({"data": [
                {"results": []},
                {"start_time": DAY0, "results": [{"amount": {"currency": "usd"}}, 7, {"x": 1}]}
            ], "has_more": false, "next_page": "ignored"}),
        );
        assert_eq!(page.buckets.len(), 1);
        assert!(page.buckets[0].rows.is_empty());
        assert_eq!(page.next_page, None, "next_page counts only with has_more");
    }
}
