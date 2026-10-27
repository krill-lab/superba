//! Unauthenticated line probing: latency and endpoint health, assembled into
//! [`LineCaps`].

use superba_core::{
    AuthMode, CodexPath, EndpointCheck, Line, LineCaps, LocalizedText, ProbeEndpoint, Site,
};
use url::Url;

use crate::HttpLineProber;
use crate::classify::unauth_check;
use crate::http::{Answer, Request, Transport, TransportKind};

/// Body bytes kept for probe answers (errors, whoami, a catalog prefix).
pub(crate) const PROBE_BODY_LIMIT: usize = 64 * 1024;

/// Result of the latency probe of one line.
#[derive(Debug, Clone)]
pub(crate) struct Latency {
    /// The check entry for `models_unauth` (with the median latency).
    pub check: EndpointCheck,
    /// Any HTTP answer arrived.
    pub reachable: bool,
}

/// Median of the samples (mean of the two middle values for an even count).
pub(crate) fn median(samples: &[u32]) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let mid = s.len() / 2;
    Some(if s.len() % 2 == 1 {
        s[mid]
    } else {
        let sum = u64::from(s[mid - 1]) + u64::from(s[mid]);
        u32::try_from(sum / 2).unwrap_or(u32::MAX)
    })
}

/// The models URL Codex uses on the backend-api path carries the client version.
pub(crate) fn with_client_version(mut url: Url, version: &str) -> Url {
    if !version.is_empty() {
        url.query_pairs_mut().append_pair("client_version", version);
    }
    url
}

impl HttpLineProber {
    pub(crate) async fn single_latency(&self, site: &Site, line: &Line) -> Option<u32> {
        let answer = self
            .get_unauth(
                &line.origin.join(&site.endpoints.models_unauth),
                self.config.latency_timeout,
            )
            .await;
        let check = unauth_check(
            ProbeEndpoint::ModelsUnauth,
            &answer,
            &self.config.unauth_expect,
            site.capabilities.latency_probe,
        );
        answer
            .ok()
            .filter(|_| !check.edge_blocked)
            .map(|a| a.elapsed_ms())
    }

    async fn get_unauth(
        &self,
        url: &Url,
        timeout: std::time::Duration,
    ) -> Result<Answer, Transport> {
        self.http
            .send(Request::get(url.clone(), timeout, PROBE_BODY_LIMIT))
            .await
    }

    /// Warm-up request (DNS, TCP, TLS), then `latency_samples` timed requests on
    /// the same pooled connection; the median is the line latency.
    pub(crate) async fn latency(&self, site: &Site, line: &Line) -> Latency {
        let cfg = &self.config;
        let url = line.origin.join(&site.endpoints.models_unauth);
        let expect_relay = site.capabilities.latency_probe;
        let mut warm = self.get_unauth(&url, cfg.request_timeout).await;
        if let Err(t) = &warm
            && t.kind != TransportKind::Timeout
        {
            // One retry for transient connect failures (not after a full timeout).
            warm = self.get_unauth(&url, cfg.request_timeout).await;
        }
        let Ok(first) = &warm else {
            return Latency {
                check: unauth_check(
                    ProbeEndpoint::ModelsUnauth,
                    &warm,
                    &cfg.unauth_expect,
                    expect_relay,
                ),
                reachable: false,
            };
        };
        let mut samples = Vec::new();
        let mut last: Result<Answer, Transport> = Ok(first.clone());
        for _ in 0..cfg.latency_samples {
            let r = self.get_unauth(&url, cfg.latency_timeout).await;
            if let Ok(a) = &r {
                samples.push(a.elapsed_ms());
                last = r;
            }
        }
        let latency_ms = if cfg.latency_samples == 0 {
            Some(first.elapsed_ms())
        } else {
            median(&samples)
        };
        let mut check = unauth_check(
            ProbeEndpoint::ModelsUnauth,
            &last,
            &cfg.unauth_expect,
            expect_relay,
        );
        check.latency_ms = latency_ms;
        if latency_ms.is_none() && check.detail.is_none() {
            check.detail = Some("every timed sample failed".to_owned());
        }
        tracing::debug!(
            site = %site.id, line = %line.id, latency_ms = ?latency_ms, status = ?check.status,
            "latency probe"
        );
        Latency {
            check,
            reachable: true,
        }
    }

    /// Unauthenticated probe of one line (see [`superba_core::LineProber::probe`]).
    pub(crate) async fn probe_line(&self, site: &Site, line: &Line) -> LineCaps {
        let cfg = &self.config;
        let lat = self.latency(site, line).await;
        let mut caps = LineCaps {
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            checked_at: self.clock.now(),
            reachable: lat.reachable,
            latency_ms: lat.check.latency_ms,
            pat_ok: false,
            apikey_ok: false,
            codex_path: None,
            blocked_by_edge: lat.check.edge_blocked,
            key_access: None,
            checks: vec![lat.check],
            detail: None,
        };
        if !caps.reachable {
            caps.detail = Some(summarize(site, &caps));
            return caps;
        }

        let paths = &site.capabilities.codex_paths;
        if site.capabilities.key_check {
            // Relay-style site: every endpoint must answer the relay's 401.
            let e = &site.endpoints;
            let o = &line.origin;
            let pat = site.supports(AuthMode::Pat);
            let want = |p: CodexPath| paths.contains(&p);
            let backend_url =
                with_client_version(o.join(&e.backend_codex_models), &cfg.client_version);
            let (who, wham, backend, v1) = tokio::join!(
                self.maybe_get(pat, o.join(&e.whoami)),
                self.maybe_get(pat, o.join(&e.accounts_check)),
                self.maybe_get(want(CodexPath::BackendApiCodex), backend_url),
                self.maybe_get(want(CodexPath::CodexV1), o.join(&e.codex_v1_models)),
            );
            let check = |endpoint, r: &Option<Result<Answer, Transport>>| {
                r.as_ref()
                    .map(|r| unauth_check(endpoint, r, &cfg.unauth_expect, true))
            };
            let who = check(ProbeEndpoint::Whoami, &who);
            let wham = check(ProbeEndpoint::WhamAccountsCheck, &wham);
            let backend = check(ProbeEndpoint::BackendCodexModels, &backend);
            let v1 = check(ProbeEndpoint::CodexV1Models, &v1);
            let ok = |c: &Option<EndpointCheck>| c.as_ref().is_some_and(|c| c.ok);
            caps.pat_ok = pat && ok(&who) && ok(&wham);
            caps.codex_path = paths.iter().copied().find(|p| match p {
                CodexPath::BackendApiCodex => ok(&backend),
                CodexPath::CodexV1 => ok(&v1),
                CodexPath::AsIs => true,
            });
            caps.checks
                .extend([who, wham, backend, v1].into_iter().flatten());
        } else {
            // Generic site: any sane answer means its base URL is reachable.
            let healthy = caps.checks[0].ok;
            caps.codex_path = paths.first().copied().filter(|_| healthy);
        }
        caps.apikey_ok = site.supports(AuthMode::ApiKey) && caps.codex_path.is_some();
        caps.blocked_by_edge = caps.checks.iter().any(|c| c.edge_blocked);
        caps.detail = Some(summarize(site, &caps));
        if cfg.pat_requires_https {
            require_https_for_pat(site, line, &mut caps);
        }
        caps
    }

    async fn maybe_get(&self, cond: bool, url: Url) -> Option<Result<Answer, Transport>> {
        if cond {
            Some(self.get_unauth(&url, self.config.request_timeout).await)
        } else {
            None
        }
    }
}

/// Path of a probe endpoint on `site` (for messages).
pub(crate) fn endpoint_path(site: &Site, e: ProbeEndpoint) -> String {
    let ep = &site.endpoints;
    match e {
        ProbeEndpoint::ModelsUnauth => ep.models_unauth.clone(),
        ProbeEndpoint::Whoami => ep.whoami.clone(),
        ProbeEndpoint::WhamAccountsCheck => ep.accounts_check.clone(),
        ProbeEndpoint::BackendCodexModels => ep.backend_codex_models.clone(),
        ProbeEndpoint::CodexV1Models => ep.codex_v1_models.clone(),
        _ => ep.billing_subscription.clone(),
    }
}

/// One-line summary of a probe (keys in the core `probe.*` catalog).
pub(crate) fn summarize(site: &Site, caps: &LineCaps) -> LocalizedText {
    let find = |e: ProbeEndpoint| caps.checks.iter().find(|c| c.endpoint == e);
    if !caps.reachable {
        let detail = find(ProbeEndpoint::ModelsUnauth)
            .and_then(|c| c.detail.clone())
            .unwrap_or_else(|| "no answer".to_owned());
        return LocalizedText::new("probe.unreachable", "Line unreachable: {detail}")
            .arg("detail", detail);
    }
    if caps.codex_path.is_none() {
        let detail = [
            ProbeEndpoint::BackendCodexModels,
            ProbeEndpoint::CodexV1Models,
            ProbeEndpoint::ModelsUnauth,
        ]
        .into_iter()
        .filter_map(find)
        .find(|c| !c.ok)
        .map(|c| {
            format!(
                "{}: {}",
                endpoint_path(site, c.endpoint),
                c.detail.clone().unwrap_or_else(|| "failed".into())
            )
        })
        .unwrap_or_else(|| "no usable path".to_owned());
        return LocalizedText::new(
            "probe.no_codex_path",
            "No Codex path works on this line: {detail}",
        )
        .arg("detail", detail);
    }
    if site.supports(AuthMode::Pat) && !caps.pat_ok {
        let pat_checks = [ProbeEndpoint::Whoami, ProbeEndpoint::WhamAccountsCheck];
        if let Some(c) = pat_checks
            .into_iter()
            .filter_map(find)
            .find(|c| c.edge_blocked)
        {
            return pat_edge_blocked(&endpoint_path(site, c.endpoint));
        }
        if let Some(c) = pat_checks.into_iter().filter_map(find).find(|c| !c.ok) {
            let status = c
                .status
                .map(|s| format!("HTTP {s}"))
                .or_else(|| c.detail.clone())
                .unwrap_or_else(|| "no answer".to_owned());
            return pat_unavailable(&endpoint_path(site, c.endpoint), &status);
        }
    }
    if caps.codex_path == Some(CodexPath::CodexV1)
        && site.capabilities.codex_paths.first() == Some(&CodexPath::BackendApiCodex)
    {
        return LocalizedText::new(
            "probe.codex_v1_fallback",
            "/backend-api/codex is blocked on this line; API key mode uses /codex/v1 instead.",
        );
    }
    LocalizedText::new("probe.ok", "All checks passed.")
}

/// `probe.pat_edge_blocked`.
pub(crate) fn pat_edge_blocked(endpoint: &str) -> LocalizedText {
    LocalizedText::new(
        "probe.pat_edge_blocked",
        "PAT is unavailable on this line: the line's edge intercepts {endpoint}. An API key still works.",
    )
    .arg("endpoint", endpoint)
}

/// `probe.pat_needs_https`.
pub(crate) fn pat_needs_https() -> LocalizedText {
    LocalizedText::new(
        "probe.pat_needs_https",
        "PAT needs an https line: Codex refuses a plain-http ChatGPT backend. An API key still works.",
    )
}

/// PAT writes `chatgpt_base_url = <line>/backend-api/`, which Codex refuses over
/// plain http (loopback test lines): clear `pat_ok` and say why when the PAT
/// endpoints themselves were fine.
pub(crate) fn require_https_for_pat(site: &Site, line: &Line, caps: &mut LineCaps) {
    if line.origin.is_https() || !site.supports(AuthMode::Pat) {
        return;
    }
    let pat_endpoints_ok = caps.pat_ok;
    caps.pat_ok = false;
    if pat_endpoints_ok && caps.reachable && caps.codex_path.is_some() {
        caps.detail = Some(pat_needs_https());
    }
}

/// `probe.pat_unavailable`.
pub(crate) fn pat_unavailable(endpoint: &str, status: &str) -> LocalizedText {
    LocalizedText::new(
        "probe.pat_unavailable",
        "PAT is unavailable on this line: {endpoint} answered {status}.",
    )
    .arg("endpoint", endpoint)
    .arg("status", status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[5]), Some(5));
        assert_eq!(median(&[9, 1, 5]), Some(5));
        assert_eq!(median(&[4, 1, 3, 2]), Some(2));
        assert_eq!(median(&[u32::MAX, u32::MAX]), Some(u32::MAX));
    }

    #[test]
    fn client_version_is_appended_as_a_query() {
        let u = Url::parse("https://h.example/backend-api/codex/models").unwrap();
        assert_eq!(
            with_client_version(u.clone(), "0.162.0").as_str(),
            "https://h.example/backend-api/codex/models?client_version=0.162.0"
        );
        assert_eq!(with_client_version(u.clone(), ""), u);
    }

    /// Every text this crate emits has a key in both core catalogs, and both
    /// translations use exactly the placeholders the code fills in.
    #[test]
    fn emitted_texts_are_translated_in_every_core_catalog() {
        use superba_core::i18n::{Locale, core_catalog_source, flatten_toml};
        use superba_core::{LatencyGrade, catalog::krill_site};

        let site = krill_site();
        let mut caps = LineCaps {
            site_id: site.id.clone(),
            line_id: site.lines[0].id.clone(),
            checked_at: superba_core::Timestamp::EPOCH,
            reachable: false,
            latency_ms: None,
            pat_ok: false,
            apikey_ok: false,
            codex_path: None,
            blocked_by_edge: false,
            key_access: None,
            checks: Vec::new(),
            detail: None,
        };
        let mut texts = vec![summarize(&site, &caps)];
        caps.reachable = true;
        texts.push(summarize(&site, &caps));
        caps.codex_path = Some(CodexPath::CodexV1);
        caps.pat_ok = true;
        texts.push(summarize(&site, &caps));
        caps.codex_path = Some(CodexPath::BackendApiCodex);
        texts.push(summarize(&site, &caps));
        texts.push(pat_edge_blocked("/x"));
        texts.push(pat_unavailable("/x", "HTTP 404"));
        for g in [
            LatencyGrade::Excellent,
            LatencyGrade::Good,
            LatencyGrade::Fair,
            LatencyGrade::Poor,
            LatencyGrade::Unknown,
        ] {
            texts.push(g.text());
        }
        let keys: Vec<&str> = texts.iter().map(|t| t.key.as_str()).collect();
        for k in [
            "probe.unreachable",
            "probe.no_codex_path",
            "probe.codex_v1_fallback",
            "probe.ok",
            "probe.pat_edge_blocked",
            "probe.pat_unavailable",
        ] {
            assert!(keys.contains(&k), "{k} not produced: {keys:?}");
        }
        let placeholders = |s: &str| {
            let mut v: Vec<String> = s
                .split('{')
                .skip(1)
                .filter_map(|p| p.split_once('}').map(|(n, _)| n.to_owned()))
                .collect();
            v.sort();
            v
        };
        for locale in Locale::ALL {
            let cat = flatten_toml(core_catalog_source(locale)).unwrap();
            for t in &texts {
                let tpl = cat
                    .get(&t.key)
                    .unwrap_or_else(|| panic!("{} missing in {locale}", t.key));
                let mut args: Vec<String> = t.args.iter().map(|(n, _)| n.clone()).collect();
                args.sort();
                assert_eq!(placeholders(tpl), args, "{} in {locale}", t.key);
                assert_eq!(placeholders(&t.fallback), args, "{} fallback", t.key);
            }
        }
    }
}
