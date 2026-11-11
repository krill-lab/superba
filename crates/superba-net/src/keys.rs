//! Authenticated requests: key checks, model catalogs and the Responses probe
//! (account data is in [`crate::fetch`]).
//!
//! Key scoping: a site's key is only sent to the origins of that site's lines
//! (`check_key`, `probe_with_key` and the account requests verify that the line belongs
//! to the site), only over https or loopback http, and never along a redirect.

use superba_core::{
    AccountIdentity, AuthApiHealth, AuthMode, CodexPath, Error, KeyAccess, KeyCheck, Line,
    LineCaps, ModelInfo, Origin, ResolvedEndpoints, ResponsesProbe, Result, Secret, Site,
};
use url::Url;

use crate::HttpLineProber;
use crate::classify::{
    Whoami, edge_detail, edge_intercepted, edge_intercepted_stream, error_message, key_access,
    parse_whoami, status_detail, transport_detail,
};
use crate::http::{
    ACCEPT_SSE, Answer, Request, Transport, TransportKind, display_url, ensure_key_target, redact,
};
use crate::models::parse_models;
use crate::probe::{PROBE_BODY_LIMIT, summarize, with_client_version};
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
        },
        Ok(a) if edge_intercepted(a) => WhoamiOutcome {
            health: unhealthy(edge_detail(a)),
            info: None,
            status: Some(a.status),
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
            }
        }
        Ok(a) => WhoamiOutcome {
            health: unhealthy(status_detail(a)),
            info: None,
            status: Some(a.status),
        },
    }
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
        allow_plain_http: bool,
    ) -> std::result::Result<Answer, Transport> {
        self.http
            .send(
                Request::get(url, self.config.request_timeout, limit)
                    .with_key(key)
                    .allow_plain_http(allow_plain_http),
            )
            .await
    }

    /// Authenticated whoami at `origin` + `path`.
    pub(crate) async fn whoami(&self, origin: &Origin, path: &str, key: &Secret) -> WhoamiOutcome {
        let res = self
            .get_with_key(origin.join(path), key, PROBE_BODY_LIMIT, false)
            .await;
        let mut out = whoami_outcome(&res);
        if let AuthApiHealth::Unhealthy { reason } = &mut out.health {
            *reason = redact(reason, Some(key));
        }
        tracing::debug!(origin = %origin, status = ?out.status, healthy = out.healthy(), "whoami");
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
                .get_with_key(
                    self.models_url(site, line, path),
                    key,
                    PROBE_BODY_LIMIT,
                    site.allows_plain_http_for(line.origin.url()),
                )
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
        ensure_key_target(
            line.origin.url(),
            key,
            site.allows_plain_http_for(line.origin.url()),
        )?;
        Self::ensure_key(key)?;
        let relay = site.capabilities.key_check;
        let paths = site.capabilities.codex_paths.clone();
        let e = &site.endpoints;
        let (who, models) = tokio::join!(
            async {
                if relay {
                    Some(self.whoami(&line.origin, &e.whoami, key).await)
                } else {
                    None
                }
            },
            self.models_access(site, line, key, &paths),
        );
        Ok(KeyCheck {
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            checked_at: self.clock.now(),
            key_last4: key.last4(),
            whoami: None,
            identity: who.as_ref().and_then(|w| w.info.as_ref()).map(identity),
            access: models.access,
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
        ensure_key_target(
            line.origin.url(),
            key,
            site.allows_plain_http_for(line.origin.url()),
        )?;
        Self::ensure_key(key)?;
        let mut caps = self.probe_line(site, line).await;
        let key_modes = site.capabilities.auth_modes.iter().any(|m| m.uses_key());
        if !caps.reachable || !key_modes {
            return Ok(caps);
        }
        // The API-key view of the models request: all of the site's paths.
        let paths = site.capabilities.codex_paths.clone();
        let models = self.models_access(site, line, key, &paths).await;
        caps.key_access = Some(models.access.clone());
        caps.blocked_by_edge |= models.edge;
        if models.access.is_ok() && models.path.is_some() && models.path != caps.codex_path {
            // The keyed request is the stronger signal (e.g. the edge only blocks
            // authenticated traffic on one path).
            caps.codex_path = models.path;
            caps.apikey_ok = site.supports(AuthMode::ApiKey);
        }
        caps.detail = Some(summarize(site, &caps));
        Ok(caps)
    }

    /// See [`superba_core::LineProber::list_models`].
    pub(crate) async fn list_models_impl(
        &self,
        openai_base_url: &Url,
        key: &Secret,
        allow_plain_http: bool,
    ) -> Result<Vec<ModelInfo>> {
        Self::ensure_key(key)?;
        ensure_key_target(openai_base_url, key, allow_plain_http)?;
        let url = models_url_for_base(openai_base_url, &self.config.client_version);
        let res = self
            .get_with_key(
                url.clone(),
                key,
                self.config.max_catalog_bytes,
                allow_plain_http,
            )
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
        ensure_key_target(base, key, endpoints.allows_plain_http())?;
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
            .allow_plain_http(endpoints.allows_plain_http())
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
                .allow_plain_http(endpoints.allows_plain_http())
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
    use super::*;

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
