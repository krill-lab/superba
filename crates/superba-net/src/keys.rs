//! Authenticated requests: key checks, auth API health, model catalogs and the
//! Responses probe (account data is in [`crate::fetch`]).
//!
//! Key scoping: a site's key is only sent to the origins of that site's lines
//! (`check_key`, `probe_with_key` and the account requests verify that the line belongs
//! to the site; `check_authapi` refuses foreign origins before sending anything),
//! only over https or loopback http, and never along a redirect.

use superba_core::catalog::is_krill_host;
use superba_core::{
    AccountIdentity, AuthApiHealth, AuthMode, CodexPath, Error, KeyAccess, KeyCheck, Line,
    LineCaps, ModelInfo, Origin, ResolvedEndpoints, ResponsesProbe, Result, Secret, Site, SiteKind,
};
use url::Url;

use crate::HttpLineProber;
use crate::classify::{
    Whoami, edge_detail, edge_intercepted, edge_intercepted_stream, error_message, key_access,
    parse_whoami, status_detail, transport_detail, wham_lists_accounts,
};
use crate::http::{
    ACCEPT_SSE, Answer, Request, Transport, TransportKind, display_url, ensure_key_target, redact,
};
use crate::models::parse_models;
use crate::probe::{
    PROBE_BODY_LIMIT, pat_edge_blocked, pat_unavailable, summarize, with_client_version,
};
use crate::responses::{
    compact_body, judge_compact, judge_responses, responses_body, stream_finished,
};

/// Body bytes kept for a streamed Responses answer.
const RESPONSES_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Outcome of an authenticated whoami.
#[derive(Debug, Clone)]
pub(crate) struct WhoamiOutcome {
    pub health: AuthApiHealth,
    pub info: Option<Whoami>,
    pub status: Option<u16>,
    pub edge: bool,
    pub transport: bool,
}

impl WhoamiOutcome {
    fn healthy(&self) -> bool {
        matches!(self.health, AuthApiHealth::Healthy)
    }
}

/// Classify an authenticated whoami answer.
pub(crate) fn whoami_outcome(res: &std::result::Result<Answer, Transport>) -> WhoamiOutcome {
    let unhealthy = |reason: String| AuthApiHealth::Unhealthy { reason };
    match res {
        Err(t) => WhoamiOutcome {
            health: unhealthy(transport_detail(t)),
            info: None,
            status: None,
            edge: false,
            transport: true,
        },
        Ok(a) if edge_intercepted(a) => WhoamiOutcome {
            health: unhealthy(edge_detail(a)),
            info: None,
            status: Some(a.status),
            edge: true,
            transport: false,
        },
        Ok(a) if a.is_success() => {
            let parsed = parse_whoami(&a.body);
            WhoamiOutcome {
                health: match &parsed {
                    Ok(_) => AuthApiHealth::Healthy,
                    Err(e) => unhealthy(e.clone()),
                },
                info: parsed.ok(),
                status: Some(a.status),
                edge: false,
                transport: false,
            }
        }
        Ok(a) => WhoamiOutcome {
            health: unhealthy(status_detail(a)),
            info: None,
            status: Some(a.status),
            edge: false,
            transport: false,
        },
    }
}

/// Outcome of an authenticated `wham/accounts/check`.
#[derive(Debug, Clone)]
pub(crate) struct WhamOutcome {
    pub ok: bool,
    pub edge: bool,
    pub transport: bool,
    pub status: Option<u16>,
    pub detail: String,
}

/// Classify an authenticated `wham/accounts/check` answer.
pub(crate) fn wham_outcome(res: &std::result::Result<Answer, Transport>) -> WhamOutcome {
    match res {
        Err(t) => WhamOutcome {
            ok: false,
            edge: false,
            transport: true,
            status: None,
            detail: transport_detail(t),
        },
        Ok(a) if edge_intercepted(a) => WhamOutcome {
            ok: false,
            edge: true,
            transport: false,
            status: Some(a.status),
            detail: edge_detail(a),
        },
        Ok(a) if a.is_success() => {
            let ok = wham_lists_accounts(&a.body);
            WhamOutcome {
                ok,
                edge: false,
                transport: false,
                status: Some(a.status),
                detail: if ok {
                    String::new()
                } else {
                    "the answer lists no account".to_owned()
                },
            }
        }
        Ok(a) => WhamOutcome {
            ok: false,
            edge: false,
            transport: false,
            status: Some(a.status),
            detail: status_detail(a),
        },
    }
}

/// Access of a key in PAT mode: the models result first (key, plan, quota,
/// channel), then the PAT endpoints (edge interception, whoami contract).
pub(crate) fn combine_pat(models: KeyAccess, who: &WhoamiOutcome, wham: &WhamOutcome) -> KeyAccess {
    if who.status == Some(401) || wham.status == Some(401) {
        return KeyAccess::InvalidKey;
    }
    if !models.is_ok() {
        return models;
    }
    if who.edge || wham.edge {
        return KeyAccess::EdgeBlocked;
    }
    if who.transport || wham.transport {
        return KeyAccess::Unreachable;
    }
    if let AuthApiHealth::Unhealthy { reason } = &who.health {
        return KeyAccess::Unknown {
            status: who.status,
            message: format!("whoami: {reason}"),
        };
    }
    if !wham.ok {
        return KeyAccess::Unknown {
            status: wham.status,
            message: format!("accounts/check: {}", wham.detail),
        };
    }
    KeyAccess::Ok
}

/// Result of the authenticated models request(s) of a key check.
#[derive(Debug, Clone)]
pub(crate) struct ModelsOutcome {
    pub access: KeyAccess,
    pub path: Option<CodexPath>,
    pub edge: bool,
}

impl HttpLineProber {
    /// The line must be one of the site's lines (same id and origin), so a key is
    /// never sent to an origin the site does not own.
    pub(crate) fn ensure_site_line(site: &Site, line: &Line) -> Result<()> {
        if site
            .lines
            .iter()
            .any(|l| l.id == line.id && l.origin == line.origin)
        {
            Ok(())
        } else {
            Err(Error::invalid(format!(
                "line {} ({}) is not a line of site {}",
                line.id, line.origin, site.id
            )))
        }
    }

    pub(crate) fn ensure_key(key: &Secret) -> Result<()> {
        if key.is_empty() {
            Err(Error::invalid("the key is empty"))
        } else {
            Ok(())
        }
    }

    pub(crate) async fn get_with_key(
        &self,
        url: Url,
        key: &Secret,
        limit: usize,
    ) -> std::result::Result<Answer, Transport> {
        self.http
            .send(Request::get(url, self.config.request_timeout, limit).with_key(key))
            .await
    }

    /// Authenticated whoami at `origin` + `path`.
    pub(crate) async fn whoami(&self, origin: &Origin, path: &str, key: &Secret) -> WhoamiOutcome {
        let res = self
            .get_with_key(origin.join(path), key, PROBE_BODY_LIMIT)
            .await;
        let mut out = whoami_outcome(&res);
        if let AuthApiHealth::Unhealthy { reason } = &mut out.health {
            *reason = redact(reason, Some(key));
        }
        tracing::debug!(origin = %origin, status = ?out.status, healthy = out.healthy(), "whoami");
        out
    }

    /// Authenticated accounts check, with diagnostics redacted before use.
    async fn wham(&self, origin: &Origin, path: &str, key: &Secret) -> WhamOutcome {
        let res = self
            .get_with_key(origin.join(path), key, PROBE_BODY_LIMIT)
            .await;
        let mut out = wham_outcome(&res);
        out.detail = redact(&out.detail, Some(key));
        out
    }

    /// Models URL of `path` on `line` (the generic models path for `AsIs`).
    fn models_url(&self, site: &Site, line: &Line, path: CodexPath) -> Url {
        let e = &site.endpoints;
        match path {
            CodexPath::BackendApiCodex => with_client_version(
                line.origin.join(&e.backend_codex_models),
                &self.config.client_version,
            ),
            CodexPath::CodexV1 => line.origin.join(&e.codex_v1_models),
            CodexPath::AsIs => line.origin.join(&e.models_unauth),
        }
    }

    /// Authenticated models on the given paths in order; falls through to the next
    /// path only when one is intercepted by the edge or does not exist.
    async fn models_access(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
        paths: &[CodexPath],
    ) -> ModelsOutcome {
        let mut edge = false;
        let mut out = ModelsOutcome {
            access: KeyAccess::Unknown {
                status: None,
                message: "no Codex path to check".to_owned(),
            },
            path: None,
            edge: false,
        };
        for (i, path) in paths.iter().copied().enumerate() {
            let res = self
                .get_with_key(self.models_url(site, line, path), key, PROBE_BODY_LIMIT)
                .await;
            let mut access = key_access(site.kind, &res);
            if let KeyAccess::Unknown { message, .. } = &mut access {
                *message = redact(message, Some(key));
            }
            edge |= matches!(access, KeyAccess::EdgeBlocked);
            let missing = matches!(
                access,
                KeyAccess::Unknown {
                    status: Some(404 | 405),
                    ..
                }
            );
            tracing::debug!(site = %site.id, line = %line.id, ?path, ?access, "key models check");
            out = ModelsOutcome {
                access,
                path: Some(path),
                edge,
            };
            let last = i + 1 == paths.len();
            if last || !(matches!(out.access, KeyAccess::EdgeBlocked) || missing) {
                break;
            }
        }
        out
    }

    /// Codex paths to try for `mode` on `site` (PAT only works on backend-api).
    fn paths_for(site: &Site, mode: AuthMode) -> Vec<CodexPath> {
        site.capabilities
            .codex_paths
            .iter()
            .copied()
            .filter(|p| !mode.writes_chatgpt_base_url() || *p == CodexPath::BackendApiCodex)
            .collect()
    }

    /// See [`superba_core::LineProber::check_key`].
    pub(crate) async fn check_key_impl(
        &self,
        site: &Site,
        line: &Line,
        mode: AuthMode,
        key: &Secret,
    ) -> Result<KeyCheck> {
        if !mode.uses_key() {
            return Err(Error::invalid(format!("{mode:?} does not use a key")));
        }
        if !site.supports(mode) {
            return Err(Error::invalid(format!(
                "site {} does not offer {mode:?}",
                site.id
            )));
        }
        Self::ensure_site_line(site, line)?;
        Self::ensure_key(key)?;
        let pat = mode.writes_chatgpt_base_url() || mode.needs_authapi_env();
        let relay = site.capabilities.key_check;
        let paths = Self::paths_for(site, mode);
        let e = &site.endpoints;
        let (who, wham, models) = tokio::join!(
            async {
                if relay {
                    Some(self.whoami(&line.origin, &e.whoami, key).await)
                } else {
                    None
                }
            },
            async {
                if pat && relay {
                    Some(self.wham(&line.origin, &e.accounts_check, key).await)
                } else {
                    None
                }
            },
            self.models_access(site, line, key, &paths),
        );
        let access = match (&who, &wham) {
            (Some(w), Some(h)) if pat => combine_pat(models.access.clone(), w, h),
            _ => models.access.clone(),
        };
        Ok(KeyCheck {
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            checked_at: self.clock.now(),
            key_last4: key.last4(),
            whoami: who
                .as_ref()
                .filter(|_| mode.needs_authapi_env())
                .map(|w| w.health.clone()),
            identity: who.as_ref().and_then(|w| w.info.as_ref()).map(identity),
            access,
            codex_path: models.path,
        })
    }

    /// See [`superba_core::LineProber::probe_with_key`].
    pub(crate) async fn probe_with_key_impl(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Result<LineCaps> {
        Self::ensure_site_line(site, line)?;
        Self::ensure_key(key)?;
        let mut caps = self.probe_line(site, line).await;
        let key_modes = site.capabilities.auth_modes.iter().any(|m| m.uses_key());
        if !caps.reachable || !key_modes {
            return Ok(caps);
        }
        let relay = site.capabilities.key_check;
        let pat = relay && site.supports(AuthMode::Pat);
        // The API-key view of the models request: all of the site's paths.
        let paths = site.capabilities.codex_paths.clone();
        let e = &site.endpoints;
        let (who, wham, models) = tokio::join!(
            async {
                if pat {
                    Some(self.whoami(&line.origin, &e.whoami, key).await)
                } else {
                    None
                }
            },
            async {
                if pat {
                    Some(self.wham(&line.origin, &e.accounts_check, key).await)
                } else {
                    None
                }
            },
            self.models_access(site, line, key, &paths),
        );
        caps.key_access = Some(models.access.clone());
        caps.blocked_by_edge |= models.edge;
        if models.access.is_ok() && models.path.is_some() && models.path != caps.codex_path {
            // The keyed request is the stronger signal (e.g. the edge only blocks
            // authenticated traffic on one path).
            caps.codex_path = models.path;
            caps.apikey_ok = site.supports(AuthMode::ApiKey);
        }
        // A PAT failure seen only with the key is not visible in the unauthenticated
        // checks `summarize` reads, so it gets its own message.
        let mut keyed_pat_detail = None;
        if let (Some(w), Some(h)) = (&who, &wham) {
            caps.blocked_by_edge |= w.edge || h.edge;
            // The keyed PAT requests are what Codex itself sends, so they decide:
            // they can also restore PAT after a transient unauthenticated failure.
            let keyed_pat = w.healthy() && h.ok;
            caps.pat_ok = keyed_pat;
            if !keyed_pat {
                keyed_pat_detail = Some(if w.edge {
                    pat_edge_blocked(&e.whoami)
                } else if h.edge {
                    pat_edge_blocked(&e.accounts_check)
                } else if !w.healthy() {
                    let reason = match &w.health {
                        AuthApiHealth::Unhealthy { reason } => reason.clone(),
                        AuthApiHealth::Healthy => String::new(),
                    };
                    pat_unavailable(&e.whoami, &reason)
                } else {
                    pat_unavailable(&e.accounts_check, &h.detail)
                });
            }
            caps.pat_ok &= caps.codex_path == Some(CodexPath::BackendApiCodex);
        }
        caps.detail = Some(keyed_pat_detail.unwrap_or_else(|| summarize(site, &caps)));
        if self.config.pat_requires_https {
            crate::probe::require_https_for_pat(site, line, &mut caps);
        }
        Ok(caps)
    }

    /// See [`superba_core::LineProber::check_authapi`].
    pub(crate) async fn check_authapi_impl(
        &self,
        site: &Site,
        origin: &Origin,
        key: &Secret,
    ) -> Result<AuthApiHealth> {
        let on_line = site.lines.iter().any(|l| &l.origin == origin);
        let krill = site.kind == SiteKind::Krill
            && origin.url().scheme() == "https"
            && is_krill_host(origin.host());
        if !(on_line || krill) {
            return Err(Error::invalid(format!(
                "refusing to send the key of site {} to {origin}: not one of its origins",
                site.id
            )));
        }
        Self::ensure_key(key)?;
        Ok(self
            .whoami(origin, &site.endpoints.whoami, key)
            .await
            .health)
    }

    /// See [`superba_core::LineProber::list_models`].
    pub(crate) async fn list_models_impl(
        &self,
        openai_base_url: &Url,
        key: &Secret,
    ) -> Result<Vec<ModelInfo>> {
        Self::ensure_key(key)?;
        ensure_key_target(openai_base_url, key)?;
        let url = models_url_for_base(openai_base_url, &self.config.client_version);
        let res = self
            .get_with_key(url.clone(), key, self.config.max_catalog_bytes)
            .await;
        let a = res.map_err(|t| t.into_error(&url))?;
        if edge_intercepted(&a) && !a.truncated {
            return Err(Error::Http {
                url: display_url(&url),
                status: a.status,
                message: edge_detail(&a),
            });
        }
        if !a.is_success() {
            return Err(Error::Http {
                url: display_url(&url),
                status: a.status,
                message: error_message(&a.body)
                    .map(|message| redact(&message, Some(key)))
                    .unwrap_or_else(|| format!("HTTP {}", a.status)),
            });
        }
        if a.truncated {
            return Err(Error::parse(
                "model catalog",
                format!("larger than {} bytes", self.config.max_catalog_bytes),
            ));
        }
        parse_models(&a.body).map_err(|e| Error::parse("model catalog", e))
    }

    /// See [`superba_core::LineProber::probe_responses`].
    pub(crate) async fn probe_responses_impl(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<ResponsesProbe> {
        let base = &endpoints.openai_base_url;
        Self::ensure_key(key)?;
        ensure_key_target(base, key)?;
        if base.origin() != endpoints.origin.url().origin() {
            return Err(Error::invalid(format!(
                "openai_base_url {} is not on the endpoints' origin {}",
                display_url(base),
                endpoints.origin
            )));
        }
        let model = model.trim();
        if model.is_empty() {
            return Err(Error::invalid(
                "a model is required for the Responses probe",
            ));
        }
        let url = join_base(base, "responses");
        let req = Request::get(url, self.config.responses_timeout, RESPONSES_BODY_LIMIT)
            .with_key(key)
            .with_json(responses_body(model), ACCEPT_SSE)
            .until(stream_finished);
        let res = self.http.send(req).await;
        let mut probe = ResponsesProbe {
            ok: false,
            status: None,
            latency_ms: None,
            compact_ok: None,
            detail: None,
        };
        match &res {
            Err(t) => {
                probe.detail = Some(match t.kind {
                    TransportKind::Timeout => "no complete answer before the timeout".to_owned(),
                    _ => transport_detail(t),
                });
            }
            Ok(a) => {
                probe.status = Some(a.status);
                probe.latency_ms = Some(a.elapsed_ms());
                if edge_intercepted_stream(a) {
                    probe.detail = Some(edge_detail(a));
                } else if a.is_success() {
                    match judge_responses(a) {
                        Ok(()) => probe.ok = true,
                        Err(e) => probe.detail = Some(e),
                    }
                } else {
                    probe.detail = Some(status_detail(a));
                }
            }
        }
        if probe.ok && self.config.probe_compact {
            let url = join_base(base, "responses/compact");
            let req = Request::get(url, self.config.responses_timeout, PROBE_BODY_LIMIT * 16)
                .with_key(key)
                .with_json(compact_body(model), crate::http::ACCEPT_JSON);
            probe.compact_ok = Some(match self.http.send(req).await {
                Ok(a) => judge_compact(&a),
                Err(_) => false,
            });
        }
        if let Some(d) = &probe.detail {
            probe.detail = Some(crate::http::redact(d, Some(key)));
        }
        Ok(probe)
    }
}

fn identity(w: &Whoami) -> AccountIdentity {
    AccountIdentity {
        email: w.email.clone(),
        user_id: Some(w.user_id.clone()),
        name: None,
    }
}

/// `<base>/<tail>` keeping the base path (`https://h/v1` + `responses` =
/// `https://h/v1/responses`).
pub(crate) fn join_base(base: &Url, tail: &str) -> Url {
    let mut u = base.clone();
    let path = format!("{}/{}", base.path().trim_end_matches('/'), tail);
    u.set_path(&path);
    u
}

/// `<base>/models`, with Codex's `client_version` on the backend-api path.
pub(crate) fn models_url_for_base(base: &Url, client_version: &str) -> Url {
    let u = join_base(base, "models");
    if base
        .path()
        .trim_end_matches('/')
        .ends_with(CodexPath::BackendApiCodex.suffix())
    {
        with_client_version(u, client_version)
    } else {
        u
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
            content_type: None,
            cors: false,
            location: None,
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::from_millis(1),
            retry_after: None,
            etag: None,
        }
    }

    const WHOAMI: &str = r#"{"chatgpt_user_id":"u","chatgpt_account_id":"a","chatgpt_plan_type":"pro","chatgpt_account_is_fedramp":false}"#;
    const WHAM: &str = r#"{"accounts":[{"id":"a"}]}"#;

    #[test]
    fn pat_access_prefers_models_reasons_then_pat_endpoints() {
        let who_ok = whoami_outcome(&Ok(answer(200, WHOAMI)));
        let wham_ok = wham_outcome(&Ok(answer(200, WHAM)));
        assert!(who_ok.healthy() && wham_ok.ok);
        assert_eq!(combine_pat(KeyAccess::Ok, &who_ok, &wham_ok), KeyAccess::Ok);
        assert_eq!(
            combine_pat(KeyAccess::QuotaExhausted, &who_ok, &wham_ok),
            KeyAccess::QuotaExhausted
        );

        let mut blocked = answer(200, "");
        blocked.edge_header = Some("x".into());
        let wham_edge = wham_outcome(&Ok(blocked));
        assert!(wham_edge.edge);
        assert_eq!(
            combine_pat(KeyAccess::Ok, &who_ok, &wham_edge),
            KeyAccess::EdgeBlocked
        );
        assert_eq!(
            combine_pat(KeyAccess::AccessPointDenied, &who_ok, &wham_edge),
            KeyAccess::AccessPointDenied
        );

        let who_401 = whoami_outcome(&Ok(answer(401, r#"{"error":{"message":"invalid token"}}"#)));
        assert_eq!(
            combine_pat(KeyAccess::Ok, &who_401, &wham_ok),
            KeyAccess::InvalidKey
        );

        let who_bad = whoami_outcome(&Ok(answer(200, r#"{"chatgpt_user_id":"u"}"#)));
        match combine_pat(KeyAccess::Ok, &who_bad, &wham_ok) {
            KeyAccess::Unknown { status, message } => {
                assert_eq!(status, Some(200));
                assert!(message.starts_with("whoami: "), "{message}");
            }
            other => panic!("{other:?}"),
        }
        let wham_empty = wham_outcome(&Ok(answer(200, r#"{"accounts":[]}"#)));
        assert!(matches!(
            combine_pat(KeyAccess::Ok, &who_ok, &wham_empty),
            KeyAccess::Unknown { .. }
        ));
        let down = wham_outcome(&Err(Transport {
            kind: TransportKind::Timeout,
            message: "timed out".into(),
        }));
        assert_eq!(
            combine_pat(KeyAccess::Ok, &who_ok, &down),
            KeyAccess::Unreachable
        );
    }

    #[test]
    fn base_urls_are_joined_without_losing_their_path() {
        let b = Url::parse("https://h.example/backend-api/codex").unwrap();
        assert_eq!(
            models_url_for_base(&b, "0.162.0").as_str(),
            "https://h.example/backend-api/codex/models?client_version=0.162.0"
        );
        let v1 = Url::parse("https://h.example/codex/v1/").unwrap();
        assert_eq!(
            models_url_for_base(&v1, "0.162.0").as_str(),
            "https://h.example/codex/v1/models"
        );
        let root = Url::parse("https://h.example").unwrap();
        assert_eq!(
            join_base(&root, "responses").as_str(),
            "https://h.example/responses"
        );
    }
}
