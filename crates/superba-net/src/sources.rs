//! A provider's model lists read by explicit source (user bug 79, design
//! §2a): the **Codex list** (`/backend-api/codex/models?client_version=`, or
//! `/codex/v1/models` on an API-key base: Codex-native entries with
//! reasoning levels, context windows, ...) and the **standard list**
//! (`/v1/models`: OpenAI `{data:[{id}]}` or Anthropic
//! `{data:[{id,display_name}],has_more,last_id}`, paged with `after_id`),
//! read alone or both at once and merged by id.
//!
//! Each list keeps its own cache entry and ETag. An id in both lists keeps
//! the Codex metadata; every model says which lists named it
//! ([`ModelSpec::lists`]); a list that fails is reported with its error
//! ([`ModelList::sources`]) while the other is still used.
//!
//! The standard list of Claude Code is asked at the provider's Anthropic
//! endpoint ([`ModelListRequest::standard_base`]) with `x-api-key` and
//! `anthropic-version` as well; the caller vouches for that origin.

use serde_json::Value;
use superba_core::catalog::is_krill_host;
use superba_core::{
    CodexPath, Error, ModelList, ModelListKind, ModelListRequest, ModelListSource, ModelSource,
    ModelSourceStatus, ModelSpec, Result, SiteKind, Timestamp,
};
use url::Url;

use crate::HttpLineProber;
use crate::classify::{edge_detail, edge_intercepted, error_message};
use crate::http::{Request, display_url, ensure_key_target, redact};
use crate::keys::{join_base, models_url_for_base};
use crate::models::{
    CACHE_FORMAT, CachedList, MODEL_LIST_TTL, ModelListCache, compatible_candidates,
    ensure_site_origin, is_codex_backend, parse_model_list, source_path, whole_version,
};

/// Pages of an Anthropic list read at most.
const MAX_PAGES: usize = 20;

/// Where one list is asked.
#[derive(Clone, Debug)]
struct Target {
    kind: ModelListKind,
    /// URLs tried in order (a 404 tries the next).
    urls: Vec<Url>,
    /// Anthropic headers and paging.
    anthropic: bool,
}

/// The origin of `base` (scheme, host, port; no path).
fn origin_of(base: &Url) -> Url {
    let mut o = base.clone();
    o.set_path("");
    o.set_query(None);
    o.set_fragment(None);
    o
}

/// The Codex list of a base URL: the backend list on a Codex backend base,
/// `<base>/models` on a `/codex/v1` base, the backend list at the origin of
/// a Krill site; `None` for other sites.
pub(crate) fn codex_list_url(req: &ModelListRequest<'_>, client_version: &str) -> Option<Url> {
    let base = req.base_url;
    if is_codex_backend(base) {
        return Some(models_url_for_base(base, client_version));
    }
    let path = base.path().trim_end_matches('/');
    if path.ends_with(CodexPath::CodexV1.suffix()) {
        return Some(join_base(base, "models"));
    }
    let krill = req.site.kind == SiteKind::Krill || base.host_str().is_some_and(is_krill_host);
    krill.then(|| {
        models_url_for_base(
            &join_base(
                &origin_of(base),
                CodexPath::BackendApiCodex.suffix().trim_start_matches('/'),
            ),
            client_version,
        )
    })
}

/// The standard list's URLs: `<standard_base>/v1/models` when given, else
/// `<origin>/v1/models` for a Codex base, else `<base>/models` and the same
/// with / without `/v1`.
fn standard_urls(req: &ModelListRequest<'_>) -> Vec<Url> {
    if let Some(b) = req.standard_base {
        return vec![join_base(b, "v1/models")];
    }
    let base = req.base_url;
    let path = base.path().trim_end_matches('/');
    if is_codex_backend(base) || path.ends_with(CodexPath::CodexV1.suffix()) {
        return vec![join_base(&origin_of(base), "v1/models")];
    }
    compatible_candidates(base)
}

/// The next page of an Anthropic list (`has_more` + `last_id`).
fn next_page(first: &Url, body: &Value) -> Option<Url> {
    if body.get("has_more").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let last = body
        .get("last_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let mut u = first.clone();
    u.query_pairs_mut()
        .append_pair("after_id", last)
        .append_pair("limit", "1000");
    Some(u)
}

/// Merge an Anthropic page's `data` into `all`.
fn append_data(all: &mut Vec<Value>, body: &Value) {
    if let Some(items) = body.get("data").and_then(Value::as_array) {
        all.extend(items.iter().cloned());
    }
}

/// Merge the lists: the Codex list first in its order, then ids only in the
/// standard list; an id in both keeps the Codex metadata (a missing display
/// name is taken from the standard list).
pub(crate) fn merge(
    codex: Option<Vec<ModelSpec>>,
    standard: Option<Vec<ModelSpec>>,
) -> Vec<ModelSpec> {
    let mut out: Vec<ModelSpec> = Vec::new();
    for mut m in codex.unwrap_or_default() {
        m.lists = vec![ModelListKind::Codex];
        out.push(m);
    }
    for mut m in standard.unwrap_or_default() {
        match out.iter_mut().find(|x| x.slug == m.slug) {
            Some(x) => {
                if !x.lists.contains(&ModelListKind::Standard) {
                    x.lists.push(ModelListKind::Standard);
                }
                if x.display_name.is_none() {
                    x.display_name = m.display_name.take();
                }
            }
            None => {
                m.lists = vec![ModelListKind::Standard];
                out.push(m);
            }
        }
    }
    out
}

impl HttpLineProber {
    /// [`superba_core::LineProber::model_list`] with an explicit source.
    pub(crate) async fn sourced_model_list(
        &self,
        req: &ModelListRequest<'_>,
        source: ModelSource,
    ) -> Result<ModelList> {
        let key = req.key;
        Self::ensure_key(key)?;
        let client_version = req
            .client_version
            .map(whole_version)
            .unwrap_or_else(|| self.config.client_version.clone());
        let mut targets = Vec::new();
        for kind in source.kinds() {
            match kind {
                ModelListKind::Codex => {
                    if let Some(url) = codex_list_url(req, &client_version) {
                        targets.push(Target {
                            kind: *kind,
                            urls: vec![url],
                            anthropic: false,
                        });
                    } else {
                        targets.push(Target {
                            kind: *kind,
                            urls: Vec::new(),
                            anthropic: false,
                        });
                    }
                }
                ModelListKind::Standard => targets.push(Target {
                    kind: *kind,
                    urls: standard_urls(req),
                    anthropic: req.standard_base.is_some(),
                }),
            }
        }
        // Key scoping: the site's own origins, or the endpoint the caller
        // vouched for.
        for t in &targets {
            for u in &t.urls {
                ensure_key_target(u, key)?;
                let vouched = req
                    .standard_base
                    .is_some_and(|b| t.kind == ModelListKind::Standard && b.origin() == u.origin());
                if !vouched {
                    ensure_site_origin(req.site, u)?;
                }
            }
        }
        // The same URL for both lists (a `/codex/v1` base whose Anthropic
        // endpoint is `/codex`): asked once.
        let same =
            targets.len() == 2 && !targets[0].urls.is_empty() && targets[0].urls == targets[1].urls;
        let fetches = targets
            .iter()
            .take(if same { 1 } else { targets.len() })
            .map(|t| self.fetch_list(req, t, &client_version));
        let mut results = futures_util::future::join_all(fetches).await;
        if same {
            let copy = match &results[0] {
                Ok(c) => Ok(c.clone()),
                Err(e) => Err(e.clone()),
            };
            results.push(copy);
        }

        let mut sources = Vec::new();
        let mut codex = None;
        let mut standard = None;
        let mut first_ok: Option<CachedList> = None;
        for (t, r) in targets.iter().zip(results) {
            match r {
                Ok(c) => {
                    let parsed = parse_model_list(c.body.as_bytes())
                        .map_err(|e| Error::parse("model list", e));
                    match parsed {
                        Ok(p) => {
                            sources.push(ModelSourceStatus {
                                kind: t.kind,
                                path: Some(c.path.clone()),
                                fetched_at: Some(Timestamp::from_millis(c.fetched_at)),
                                count: p.models.len(),
                                error: None,
                            });
                            match t.kind {
                                ModelListKind::Codex => codex = Some(p.models),
                                ModelListKind::Standard => standard = Some(p.models),
                            }
                            if first_ok.is_none() {
                                first_ok = Some(c);
                            }
                        }
                        Err(e) => sources.push(ModelSourceStatus {
                            kind: t.kind,
                            path: Some(c.path.clone()),
                            fetched_at: None,
                            count: 0,
                            error: Some(e),
                        }),
                    }
                }
                Err(e) => sources.push(ModelSourceStatus {
                    kind: t.kind,
                    path: t.urls.first().map(source_path),
                    fetched_at: None,
                    count: 0,
                    error: Some(e),
                }),
            }
        }
        let models = merge(codex, standard);
        let source = match &first_ok {
            Some(c) => ModelListSource::Upstream {
                path: c.path.clone(),
                fetched_at: Timestamp::from_millis(c.fetched_at),
                etag: c.etag.clone(),
            },
            None => ModelListSource::Upstream {
                path: String::new(),
                fetched_at: self.clock.now(),
                etag: None,
            },
        };
        Ok(ModelList {
            source,
            models,
            fallback_reason: None,
            sources,
        })
    }

    /// One list, through its cache (TTL, ETag); an Anthropic list is read
    /// page by page and cached as one body.
    async fn fetch_list(
        &self,
        req: &ModelListRequest<'_>,
        t: &Target,
        client_version: &str,
    ) -> Result<CachedList> {
        let key = req.key;
        if t.urls.is_empty() {
            return Err(Error::Unsupported {
                what: format!("{} has no Codex model list", req.site.id),
            });
        }
        let id = ModelListCache::source_id(req.site, &t.urls[0], key, client_version, t.kind);
        let now = self.clock.now();
        let cached = self.models.get(&id);
        if let Some(c) = &cached {
            let age = now.duration_since(Timestamp::from_millis(c.fetched_at));
            if !req.refresh && age.is_some_and(|a| a < MODEL_LIST_TTL) {
                return Ok(c.clone());
            }
        }
        let mut missing = Vec::new();
        for url in &t.urls {
            let path = source_path(url);
            let etag = cached
                .as_ref()
                .filter(|c| c.path == path)
                .and_then(|c| c.etag.clone());
            let mut request = Request::get(
                url.clone(),
                self.config.request_timeout,
                self.config.max_catalog_bytes,
            )
            .with_key(key)
            .if_none_match(etag);
            if t.anthropic {
                request = request.anthropic();
            }
            let a = self
                .http
                .send(request)
                .await
                .map_err(|e| e.into_error(url))?;
            tracing::debug!(url = %display_url(url), status = a.status, kind = t.kind.as_str(), "model list");
            let http_error = |message: String| Error::Http {
                url: display_url(url),
                status: a.status,
                message,
            };
            if a.status == 304
                && let Some(c) = cached.as_ref().filter(|c| c.path == path)
            {
                let mut c = c.clone();
                c.fetched_at = now.as_millis();
                self.models.put(&id, c.clone());
                return Ok(c);
            }
            if edge_intercepted(&a) && !a.truncated && a.edge_header.is_some() {
                return Err(http_error(edge_detail(&a)));
            }
            match a.status {
                200..=299 => {
                    if a.truncated {
                        return Err(Error::parse(
                            "model list",
                            format!("larger than {} bytes", self.config.max_catalog_bytes),
                        ));
                    }
                    let Some(first) = serde_json::from_slice::<Value>(&a.body)
                        .ok()
                        .filter(|_| parse_model_list(&a.body).is_ok())
                    else {
                        missing.push(display_url(url));
                        continue;
                    };
                    let (body, etag) = match next_page(url, &first) {
                        None => (
                            String::from_utf8_lossy(&a.body).into_owned(),
                            a.etag.clone(),
                        ),
                        Some(mut next) => {
                            let mut all = Vec::new();
                            append_data(&mut all, &first);
                            for _ in 1..MAX_PAGES {
                                let mut r = Request::get(
                                    next.clone(),
                                    self.config.request_timeout,
                                    self.config.max_catalog_bytes,
                                )
                                .with_key(key);
                                if t.anthropic {
                                    r = r.anthropic();
                                }
                                let p = self.http.send(r).await.map_err(|e| e.into_error(&next))?;
                                if !(200..300).contains(&p.status) {
                                    return Err(Error::Http {
                                        url: display_url(&next),
                                        status: p.status,
                                        message: error_message(&p.body)
                                            .map(|m| redact(&m, Some(key)))
                                            .unwrap_or_else(|| format!("HTTP {}", p.status)),
                                    });
                                }
                                let page: Value = serde_json::from_slice(&p.body)
                                    .map_err(|e| Error::parse("model list", e.to_string()))?;
                                append_data(&mut all, &page);
                                match next_page(url, &page) {
                                    Some(n) if n != next => next = n,
                                    _ => break,
                                }
                            }
                            // Paged lists are cached as one answer, without an ETag.
                            (serde_json::json!({ "data": all }).to_string(), None)
                        }
                    };
                    let c = CachedList {
                        format: CACHE_FORMAT,
                        fetched_at: now.as_millis(),
                        etag,
                        path,
                        body,
                    };
                    self.models.put(&id, c.clone());
                    return Ok(c);
                }
                401 => {
                    return Err(http_error(
                        "the key was rejected (invalid or expired key)".into(),
                    ));
                }
                404 | 405 => missing.push(display_url(url)),
                _ => {
                    return Err(http_error(
                        error_message(&a.body)
                            .map(|m| redact(&m, Some(key)))
                            .unwrap_or_else(|| format!("HTTP {}", a.status)),
                    ));
                }
            }
        }
        Err(Error::Unsupported {
            what: format!(
                "{} does not list models ({})",
                req.site.id,
                missing.join(", ")
            ),
        })
    }
}
