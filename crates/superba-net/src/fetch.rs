//! Account card and usage view requests: whoami, the legacy billing endpoints and
//! the organization usage / costs APIs. Formats are in [`crate::account`] and
//! [`crate::org`], caching in [`crate::cache`].
//!
//! Every endpoint is optional: 404 / 405 / 501, a non-JSON success (a web app's
//! catch-all page) or an answer in another format mean "this site does not serve
//! it" and only hide that piece of data. A 401 means the key is invalid and is an
//! error. Keys go only to the line's origin (the line must belong to the site),
//! never along a redirect, and never into a log or an error text.

use superba_core::{
    AccountSummary, Error, Line, Result, Secret, Site, Timestamp, UsageNotice, UsageQuery,
    UsageReport,
};
use url::Url;

use crate::HttpLineProber;
use crate::account::{Subscription, month_to_date, parse_billing_usage, parse_subscription};
use crate::classify::{edge_detail, error_message, is_json, status_detail};
use crate::http::{Answer, Request, display_url, redact};
use crate::keys::whoami_outcome;
use crate::org::{self, Api, Bucket, ReportParts};
use crate::probe::PROBE_BODY_LIMIT;

/// Largest usage / costs page accepted.
const PAGE_BODY_LIMIT: usize = 2 * 1024 * 1024;
/// Pages followed per endpoint and refresh (each costs a rate-limited request).
const MAX_PAGES: usize = 3;

/// Outcome of one optional endpoint.
#[derive(Debug, Clone)]
pub(crate) enum Fetch<T> {
    /// It answered in the expected format.
    Ok(T),
    /// The site does not serve it.
    Missing,
    /// 401: the key is invalid.
    Unauthorized(Error),
    /// 429.
    RateLimited {
        retry_after: Option<u64>,
        error: Error,
    },
    /// Anything else (transport, 5xx, 403, edge interception).
    Failed(Error),
}

impl<T> Fetch<T> {
    fn map<U>(self, f: impl FnOnce(T) -> U) -> Fetch<U> {
        match self {
            Fetch::Ok(v) => Fetch::Ok(f(v)),
            Fetch::Missing => Fetch::Missing,
            Fetch::Unauthorized(e) => Fetch::Unauthorized(e),
            Fetch::RateLimited { retry_after, error } => Fetch::RateLimited { retry_after, error },
            Fetch::Failed(e) => Fetch::Failed(e),
        }
    }
}

/// `Retry-After` in seconds (only the delta-seconds form).
fn retry_after_secs(a: &Answer) -> Option<u64> {
    a.retry_after.as_deref()?.trim().parse::<u64>().ok()
}

fn http_error(url: &Url, a: &Answer, key: &Secret) -> Error {
    let message = error_message(&a.body).unwrap_or_else(|| status_detail(a));
    Error::Http {
        url: display_url(url),
        status: a.status,
        message: redact(&message, Some(key)),
    }
}

/// Classify one answer of an optional JSON endpoint and parse it.
fn classify<T>(
    url: &Url,
    res: std::result::Result<Answer, crate::http::Transport>,
    key: &Secret,
    parse: impl FnOnce(&[u8]) -> std::result::Result<T, String>,
) -> Fetch<T> {
    let a = match res {
        Ok(a) => a,
        Err(t) => return Fetch::Failed(t.into_error(url)),
    };
    if a.edge_header.is_some() {
        return Fetch::Failed(Error::Http {
            url: display_url(url),
            status: a.status,
            message: edge_detail(&a),
        });
    }
    match a.status {
        401 => Fetch::Unauthorized(http_error(url, &a, key)),
        404 | 405 | 501 => Fetch::Missing,
        429 => Fetch::RateLimited {
            retry_after: retry_after_secs(&a),
            error: http_error(url, &a, key),
        },
        s if (200..300).contains(&s) => {
            if a.truncated || !is_json(&a.body) {
                if a.truncated {
                    return Fetch::Failed(Error::Http {
                        url: display_url(url),
                        status: a.status,
                        message: "the answer is too large".into(),
                    });
                }
                return Fetch::Missing;
            }
            match parse(&a.body) {
                Ok(v) => Fetch::Ok(v),
                Err(e) => {
                    tracing::debug!(url = %display_url(url), error = %e, "unexpected format");
                    Fetch::Missing
                }
            }
        }
        _ => Fetch::Failed(http_error(url, &a, key)),
    }
}

/// `path`, then `path` without its leading `/v1` (one-api style relays serve the
/// billing endpoints at both).
fn variants(path: &str) -> Vec<String> {
    let mut out = vec![path.to_owned()];
    if let Some(rest) = path.strip_prefix("/v1/") {
        out.push(format!("/{rest}"));
    }
    out
}

impl HttpLineProber {
    async fn get_optional<T>(
        &self,
        url: Url,
        key: &Secret,
        limit: usize,
        parse: impl FnOnce(&[u8]) -> std::result::Result<T, String>,
    ) -> Fetch<T> {
        let res = self
            .http
            .send(Request::get(url.clone(), self.config.request_timeout, limit).with_key(key))
            .await;
        let out = classify(&url, res, key, parse);
        tracing::debug!(
            url = %display_url(&url),
            outcome = match &out {
                Fetch::Ok(_) => "ok",
                Fetch::Missing => "missing",
                Fetch::Unauthorized(_) => "unauthorized",
                Fetch::RateLimited { .. } => "rate limited",
                Fetch::Failed(_) => "failed",
            },
            "account endpoint"
        );
        out
    }

    /// Billing subscription + month-to-date usage (each with the `/v1` fallback).
    pub(crate) async fn billing(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Fetch<superba_core::BillingSummary> {
        let e = &site.endpoints;
        let mut found: Option<(Subscription, usize)> = None;
        for (i, path) in variants(&e.billing_subscription).iter().enumerate() {
            match self
                .get_optional(
                    line.origin.join(path),
                    key,
                    PROBE_BODY_LIMIT,
                    parse_subscription,
                )
                .await
            {
                Fetch::Ok(s) => {
                    found = Some((s, i));
                    break;
                }
                Fetch::Missing => continue,
                Fetch::Unauthorized(e) => return Fetch::Unauthorized(e),
                Fetch::RateLimited { retry_after, error } => {
                    return Fetch::RateLimited { retry_after, error };
                }
                Fetch::Failed(e) => return Fetch::Failed(e),
            }
        }
        let Some((subscription, variant)) = found else {
            return Fetch::Missing;
        };
        let period = month_to_date(self.clock.now());
        let mut paths = variants(&e.billing_usage);
        if variant < paths.len() {
            // Try the family that answered the subscription first.
            paths.swap(0, variant);
        }
        let mut used = None;
        for path in paths {
            let mut url = line.origin.join(&path);
            url.query_pairs_mut()
                .append_pair("start_date", &period.0)
                .append_pair("end_date", &period.1);
            match self
                .get_optional(url, key, PROBE_BODY_LIMIT, parse_billing_usage)
                .await
            {
                Fetch::Ok(u) => {
                    used = Some(u);
                    break;
                }
                Fetch::Missing => continue,
                Fetch::Unauthorized(e) => return Fetch::Unauthorized(e),
                Fetch::RateLimited { error, .. } | Fetch::Failed(error) => {
                    tracing::debug!(error = %error, "billing usage unavailable");
                    break;
                }
            }
        }
        Fetch::Ok(subscription.into_summary(used, period))
    }

    /// whoami email (`Ok(None)` when whoami has none).
    async fn whoami_email(&self, site: &Site, line: &Line, key: &Secret) -> Result<Option<String>> {
        let url = line.origin.join(&site.endpoints.whoami);
        let res = self.get_with_key(url.clone(), key, PROBE_BODY_LIMIT).await;
        let a = res.map_err(|t| t.into_error(&url))?;
        let out = whoami_outcome(&Ok(a.clone()));
        match out.info {
            Some(w) => Ok(w.email),
            None => Err(Error::Http {
                url: display_url(&url),
                status: a.status,
                message: match out.health {
                    superba_core::AuthApiHealth::Unhealthy { reason } => redact(&reason, Some(key)),
                    superba_core::AuthApiHealth::Healthy => String::new(),
                },
            }),
        }
    }

    /// See [`superba_core::LineProber::account_summary`].
    pub(crate) async fn account_summary_impl(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Result<Option<AccountSummary>> {
        Self::ensure_site_line(site, line)?;
        Self::ensure_key(key)?;
        let caps = &site.capabilities;
        if !caps.account_summary && !caps.key_check {
            return Ok(None);
        }
        let origin = &line.origin.as_str();
        let now = self.clock.monotonic();
        if let Some(hit) = self.caches.summary.get(&site.id, origin, key, now) {
            return hit;
        }
        let result = self.fetch_summary(site, line, key).await;
        self.caches.summary.put(
            &site.id,
            origin,
            key,
            self.clock.monotonic(),
            result.clone(),
        );
        result
    }

    async fn fetch_summary(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Result<Option<AccountSummary>> {
        let caps = &site.capabilities;
        let origin = &line.origin.as_str();
        let ask_billing = caps.account_summary
            && !self
                .caches
                .summary
                .billing_missing(&site.id, origin, self.clock.monotonic());
        let who = async {
            if caps.key_check {
                Some(self.whoami_email(site, line, key).await)
            } else {
                None
            }
        };
        let bill = async {
            if ask_billing {
                Some(self.billing(site, line, key).await)
            } else {
                None
            }
        };
        let (who, bill) = futures_util::join!(who, bill);

        let mut summary = AccountSummary {
            key_last4: Some(key.last4()).filter(|s| !s.is_empty()),
            fetched_at: Some(self.clock.now()),
            ..AccountSummary::default()
        };
        let mut have = false;
        let mut first_error: Option<Error> = None;
        match who {
            Some(Ok(email)) => {
                summary.email = email;
                have = true;
            }
            Some(Err(e)) if matches!(e, Error::Http { status: 401, .. }) => return Err(e),
            Some(Err(e)) => first_error = Some(e),
            None => {}
        }
        match bill {
            Some(Fetch::Ok(b)) => {
                self.caches.summary.set_billing_missing(
                    &site.id,
                    origin,
                    self.clock.monotonic(),
                    false,
                );
                summary.plan_name = b.plan_title.clone();
                summary.expires_at = b.access_until;
                if let Some(r) = b.remaining {
                    summary.balance = Some(r.amount(2));
                    summary.currency = Some("USD".into());
                }
                summary.billing = Some(b);
                have = true;
            }
            Some(Fetch::Missing) => {
                self.caches.summary.set_billing_missing(
                    &site.id,
                    origin,
                    self.clock.monotonic(),
                    true,
                );
            }
            Some(Fetch::Unauthorized(e)) => return Err(e),
            Some(Fetch::RateLimited { error, .. } | Fetch::Failed(error)) => {
                first_error.get_or_insert(error);
            }
            None => {}
        }
        match (have, first_error) {
            (true, _) => Ok(Some(summary)),
            (false, Some(e)) => Err(e),
            (false, None) => Ok(None),
        }
    }

    /// One endpoint's pages (following `next_page` up to [`MAX_PAGES`]).
    async fn pages(
        &self,
        api: Api,
        line: &Line,
        path: &str,
        key: &Secret,
        query: &[(&str, String)],
    ) -> Fetch<Vec<Bucket>> {
        let mut buckets = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = line.origin.join(path);
            {
                let mut q = url.query_pairs_mut();
                for (k, v) in query {
                    q.append_pair(k, v);
                }
                if let Some(c) = &cursor {
                    q.append_pair("page", c);
                }
            }
            let page = match self
                .get_optional(url, key, PAGE_BODY_LIMIT, |b| org::parse_page(api, b))
                .await
            {
                Fetch::Ok(p) => p,
                // A later page that fails makes the whole answer incomplete: report
                // the failure (a 401 still means the key is invalid) instead of
                // returning, and caching, partial data.
                Fetch::Missing if !buckets.is_empty() => {
                    return Fetch::Failed(Error::Http {
                        url: display_url(&line.origin.join(path)),
                        status: 404,
                        message: "a later page of the answer is missing".into(),
                    });
                }
                other => return other.map(|_| Vec::new()),
            };
            buckets.extend(page.buckets);
            match page.next_page {
                Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                _ => break,
            }
        }
        Fetch::Ok(buckets)
    }

    /// See [`superba_core::LineProber::usage_report`].
    pub(crate) async fn usage_report_impl(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
        query: &UsageQuery,
    ) -> Result<UsageReport> {
        Self::ensure_site_line(site, line)?;
        Self::ensure_key(key)?;
        query.validate()?;
        if !site.capabilities.usage_api {
            return Err(Error::Unsupported {
                what: format!("usage data on site {}", site.id),
            });
        }
        let now = self.clock.now();
        let (start, end) = query.range(now);
        let cached = self.caches.usage.load(&site.id, key, start, query.days);
        if let Some(c) = &cached {
            let age = now.duration_since(c.fetched_at).unwrap_or_default();
            let window = if query.refresh {
                UsageQuery::REFRESH_DEBOUNCE_SECS
            } else {
                UsageQuery::CACHE_TTL_SECS
            };
            if age.as_secs() < window {
                let mut r = c.clone();
                r.from_cache = true;
                return Ok(r);
            }
        }

        let range = [
            ("start_time", start.as_unix_secs().to_string()),
            ("end_time", end.as_unix_secs().to_string()),
            ("bucket_width", "1d".to_owned()),
            ("limit", query.days.to_string()),
        ];
        let mut usage_query = range.to_vec();
        usage_query.push(("group_by", "model".to_owned()));
        let e = &site.endpoints;
        let (costs, usage) = futures_util::join!(
            self.pages(Api::Costs, line, &e.costs, key, &range),
            self.pages(Api::Usage, line, &e.usage_completions, key, &usage_query),
        );

        let mut notice: Option<UsageNotice> = None;
        let mut first_error: Option<Error> = None;
        let mut take = |f: Fetch<Vec<Bucket>>| -> Result<Option<Vec<Bucket>>> {
            match f {
                Fetch::Ok(b) => Ok(Some(b)),
                Fetch::Missing => Ok(None),
                Fetch::Unauthorized(e) => Err(e),
                Fetch::RateLimited { retry_after, error } => {
                    notice.get_or_insert(UsageNotice::RateLimited {
                        retry_after_secs: retry_after,
                    });
                    first_error.get_or_insert(error);
                    Ok(None)
                }
                Fetch::Failed(error) => {
                    notice.get_or_insert(UsageNotice::RefreshFailed {
                        reason: short_reason(&error),
                    });
                    first_error.get_or_insert(error);
                    Ok(None)
                }
            }
        };
        let costs = take(costs)?;
        let usage = take(usage)?;

        if let Some(notice) = notice {
            if let Some(mut c) = cached {
                c.from_cache = true;
                c.notice = Some(notice);
                return Ok(c);
            }
            if costs.is_none() && usage.is_none() {
                return Err(first_error.unwrap_or_else(|| Error::other("usage refresh failed")));
            }
            let mut partial = self.report(site, start, query.days, now, &costs, &usage);
            partial.notice = Some(notice);
            return Ok(partial);
        }
        if costs.is_none() && usage.is_none() {
            return Err(Error::Unsupported {
                what: format!(
                    "{} serves neither the organization costs nor the usage API",
                    line.origin
                ),
            });
        }
        let report = self.report(site, start, query.days, now, &costs, &usage);
        self.caches.usage.store(key, &report);
        Ok(report)
    }

    fn report(
        &self,
        site: &Site,
        start: Timestamp,
        days: u32,
        now: Timestamp,
        costs: &Option<Vec<Bucket>>,
        usage: &Option<Vec<Bucket>>,
    ) -> UsageReport {
        org::build_report(ReportParts {
            site_id: &site.id,
            start,
            days,
            fetched_at: now,
            costs: costs.as_deref(),
            usage: usage.as_deref(),
        })
    }
}

/// A short reason for a notice (status or error kind, no URL).
fn short_reason(e: &Error) -> String {
    match e {
        Error::Http { status, .. } => format!("HTTP {status}"),
        Error::Timeout { .. } => "timeout".into(),
        Error::Network { message, .. } => crate::classify::short(message),
        other => crate::classify::short(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn answer(status: u16, body: &str) -> Answer {
        Answer {
            status,
            edge_header: None,
            content_type: Some("application/json".into()),
            cors: false,
            location: None,
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::from_millis(1),
            retry_after: None,
            etag: None,
        }
    }

    fn outcome(status: u16, body: &str) -> &'static str {
        let url = Url::parse("https://relay.example/v1/x").unwrap();
        let key = Secret::new("sk-test-key-0000");
        match classify(&url, Ok(answer(status, body)), &key, parse_billing_usage) {
            Fetch::Ok(_) => "ok",
            Fetch::Missing => "missing",
            Fetch::Unauthorized(_) => "unauthorized",
            Fetch::RateLimited { .. } => "rate_limited",
            Fetch::Failed(_) => "failed",
        }
    }

    #[test]
    fn optional_endpoints_are_classified() {
        assert_eq!(outcome(200, r#"{"total_usage":1}"#), "ok");
        assert_eq!(outcome(200, "<html>app</html>"), "missing");
        assert_eq!(outcome(200, r#"{"other":1}"#), "missing");
        assert_eq!(outcome(404, ""), "missing");
        assert_eq!(outcome(405, ""), "missing");
        assert_eq!(
            outcome(401, r#"{"error":{"message":"bad key"}}"#),
            "unauthorized"
        );
        assert_eq!(outcome(429, ""), "rate_limited");
        assert_eq!(outcome(403, ""), "failed");
        assert_eq!(outcome(502, ""), "failed");
        let url = Url::parse("https://relay.example/v1/x").unwrap();
        let key = Secret::new("sk-test-key-0000");
        let mut a = answer(429, "");
        a.retry_after = Some(" 120 ".into());
        assert!(matches!(
            classify(&url, Ok(a), &key, parse_billing_usage),
            Fetch::RateLimited {
                retry_after: Some(120),
                ..
            }
        ));
        let mut edge = answer(200, "");
        edge.edge_header = Some("Filter".into());
        assert!(matches!(
            classify(&url, Ok(edge), &key, parse_billing_usage),
            Fetch::Failed(Error::Http { status: 200, .. })
        ));
    }

    #[test]
    fn billing_paths_fall_back_without_v1() {
        assert_eq!(
            variants("/v1/dashboard/billing/usage"),
            ["/v1/dashboard/billing/usage", "/dashboard/billing/usage"]
        );
        assert_eq!(variants("/api/usage"), ["/api/usage"]);
    }
}
