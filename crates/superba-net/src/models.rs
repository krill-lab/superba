//! Provider model lists ([`superba_core::LineProber::model_list`]): parsing of both
//! answer shapes and the list cache.
//!
//! - **Codex-native** (`/backend-api/codex/models?client_version=<x.y.z>`, relays
//!   such as Krill): `{"models":[ModelInfo...]}`, every entry kept unchanged in
//!   [`ModelSpec::raw`] for catalog generation.
//! - **OpenAI list** (`<base>/models` of compatible sites, tried with and without
//!   `/v1`): `{"data":[{"id":..}]}` with whatever metadata the server adds: the
//!   context window from `context_window`, `context_length`, `max_context_length`,
//!   `max_input_tokens`, `max_model_len` or `top_provider.context_length`
//!   (OpenRouter, LiteLLM, vLLM style servers), a display name from
//!   `display_name` / `name`, input modalities from `architecture`.
//!
//! **Cache**: per (site, base URL, key fingerprint, client version) for
//! [`MODEL_LIST_TTL`]; after that (or on refresh) the request carries
//! `If-None-Match` and a `304` reuses the stored answer. Kept in memory and,
//! with [`crate::HttpLineProber::with_model_cache_dir`], on disk so the
//! short-lived `spb` process reuses it. Files hold the server's answer (keys
//! scrubbed by the transport) and never the key; the key only enters the hashed
//! file name as a truncated fingerprint.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use superba_core::catalog::is_krill_host;
use superba_core::{
    CodexPath, Error, ModelInfo, ModelList, ModelListRequest, ModelListSource, ModelSpec, Result,
    Secret, Site, SiteKind, Timestamp,
};
use url::Url;

use crate::HttpLineProber;
use crate::cache::key_fingerprint;
use crate::classify::{edge_detail, edge_intercepted, error_message};
use crate::http::{Request, display_url, ensure_key_target, redact};
use crate::keys::{join_base, models_url_for_base};

/// A provider list is reused without asking for this long.
pub const MODEL_LIST_TTL: Duration = Duration::from_secs(600);

/// A parsed `/models` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedList {
    /// At least one entry was a Codex-native `ModelInfo` (has a `slug`).
    pub native: bool,
    /// Models in answer order, deduplicated by slug.
    pub models: Vec<ModelSpec>,
}

/// Parse a model list answer (`models`, `data` or a bare array; Codex-native
/// entries, OpenAI-style items or plain id strings).
pub(crate) fn parse_model_list(body: &[u8]) -> std::result::Result<ParsedList, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| format!("models answer is not JSON: {e}"))?;
    let items = v
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| v.get("data").and_then(Value::as_array))
        .or_else(|| v.as_array())
        .ok_or_else(|| "models answer has neither `models` nor `data`".to_owned())?;
    let mut native = false;
    let mut models: Vec<ModelSpec> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let spec = match item {
            Value::String(s) if !s.trim().is_empty() => Some(ModelSpec::new(s.trim())),
            Value::Object(o) if o.contains_key("slug") => {
                let s = ModelSpec::from_codex_entry(item);
                native |= s.is_some();
                s
            }
            Value::Object(o) => openai_item(o),
            _ => None,
        };
        if let Some(spec) = spec
            && seen.insert(spec.slug.clone())
        {
            models.push(spec);
        }
    }
    Ok(ParsedList { native, models })
}

/// A number or a numeric string.
fn count(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite() && *f >= 1.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// An OpenAI-style list item with opportunistic metadata.
fn openai_item(o: &Map<String, Value>) -> Option<ModelSpec> {
    let text = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let id = text("id").or_else(|| text("model"))?;
    let display_name = text("display_name").or_else(|| text("name").filter(|n| n != &id));
    let window = [
        "context_window",
        "context_length",
        "max_context_length",
        "max_input_tokens",
        "max_model_len",
    ]
    .iter()
    .find_map(|k| count(o.get(*k)))
    .or_else(|| count(o.get("top_provider").and_then(|t| t.get("context_length"))))
    .filter(|w| *w > 0);
    let input_modalities = o
        .get("architecture")
        .and_then(|a| a.get("input_modalities"))
        .and_then(Value::as_array)
        .map(|m| {
            m.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(ModelSpec {
        display_name,
        description: text("description"),
        context_window: window,
        max_context_window: window,
        input_modalities,
        ..ModelSpec::new(id)
    })
}

/// [`ModelInfo`] items of a model list answer (`list_models`).
pub(crate) fn parse_models(body: &[u8]) -> std::result::Result<Vec<ModelInfo>, String> {
    Ok(parse_model_list(body)?
        .models
        .into_iter()
        .map(|m| ModelInfo {
            id: m.slug,
            display_name: m.display_name,
            context_window: m.context_window,
            max_context_window: m.max_context_window,
            service_tiers: m.service_tiers,
            raw: m.raw,
        })
        .collect())
}

/// URLs tried for a compatible site's list: `<base>/models`, then the same
/// without (or with) `/v1`.
pub(crate) fn compatible_candidates(base: &Url) -> Vec<Url> {
    let first = join_base(base, "models");
    let path = base.path().trim_end_matches('/');
    let second = match path.strip_suffix("/v1") {
        Some(rest) => {
            let mut u = base.clone();
            u.set_path(&format!("{rest}/models"));
            u
        }
        None => join_base(base, "v1/models"),
    };
    if second == first {
        vec![first]
    } else {
        vec![first, second]
    }
}

/// True for a Codex backend base (`<line>/backend-api/codex`).
pub(crate) fn is_codex_backend(base: &Url) -> bool {
    base.path()
        .trim_end_matches('/')
        .ends_with(CodexPath::BackendApiCodex.suffix())
}

/// Path and query for the source (no origin, never a key: keys are refused in
/// URLs before sending).
pub(crate) fn source_path(url: &Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    }
}

/// One cached answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CachedList {
    pub(crate) format: u32,
    /// Wall-clock milliseconds of the last successful (or 304) answer.
    pub(crate) fetched_at: i64,
    pub(crate) etag: Option<String>,
    /// URL path and query that answered.
    pub(crate) path: String,
    /// The answer body (UTF-8 JSON).
    pub(crate) body: String,
}

pub(crate) const CACHE_FORMAT: u32 = 1;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Memory (and optional disk) cache of provider lists.
#[derive(Debug, Default)]
pub(crate) struct ModelListCache {
    memory: Mutex<HashMap<String, CachedList>>,
    dir: Option<PathBuf>,
}

impl ModelListCache {
    /// Cache under `dir` (created on first write); `None` = memory only.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            memory: Mutex::default(),
            dir,
        }
    }

    fn id(site: &Site, base: &Url, key: &Secret, client_version: &str) -> String {
        let id = format!(
            "models|{}|{}|{}|{}",
            site.id.as_str(),
            display_url(base),
            key_fingerprint(key),
            client_version
        );
        hex::encode(&Sha256::digest(id.as_bytes())[..16])
    }

    /// The cache id of one explicit list ([`superba_core::ModelListKind`]):
    /// its own entry and ETag, apart from the other list and from the
    /// legacy single-source entry.
    pub(crate) fn source_id(
        site: &Site,
        url: &Url,
        key: &Secret,
        client_version: &str,
        kind: superba_core::ModelListKind,
    ) -> String {
        let id = format!(
            "models|{}|{}|{}|{}|{}",
            kind.as_str(),
            site.id.as_str(),
            display_url(url),
            key_fingerprint(key),
            client_version
        );
        hex::encode(&Sha256::digest(id.as_bytes())[..16])
    }

    fn file(&self, id: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{id}.json")))
    }

    pub(crate) fn get(&self, id: &str) -> Option<CachedList> {
        if let Some(c) = self.memory.lock().ok()?.get(id).cloned() {
            return Some(c);
        }
        let bytes = std::fs::read(self.file(id)?).ok()?;
        let c: CachedList = serde_json::from_slice(&bytes).ok()?;
        (c.format == CACHE_FORMAT).then_some(c)
    }

    pub(crate) fn put(&self, id: &str, c: CachedList) {
        if let Some(path) = self.file(id)
            && let Err(e) = write_atomic(&path, &c)
        {
            tracing::debug!(error = %e, "cannot write the model list cache");
        }
        if let Ok(mut m) = self.memory.lock() {
            m.insert(id.to_owned(), c);
        }
    }
}

fn write_atomic(path: &Path, c: &CachedList) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(c).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The key may go only to the site's own line origins (Krill: also its hosts).
pub(crate) fn ensure_site_origin(site: &Site, base: &Url) -> Result<()> {
    let on_line = site
        .lines
        .iter()
        .any(|l| l.origin.url().origin() == base.origin());
    let krill = site.kind == SiteKind::Krill
        && base.scheme() == "https"
        && base.host_str().is_some_and(is_krill_host);
    if on_line || krill {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "refusing to send the key of site {} to {}: not one of its origins",
            site.id,
            display_url(base)
        )))
    }
}

/// `major.minor.patch` of a Codex version (Codex sends its whole version).
pub(crate) fn whole_version(v: &superba_core::CodexVersion) -> String {
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

fn list_from(cached: &CachedList) -> Result<ModelList> {
    let parsed =
        parse_model_list(cached.body.as_bytes()).map_err(|e| Error::parse("model list", e))?;
    Ok(ModelList {
        source: ModelListSource::Upstream {
            path: cached.path.clone(),
            fetched_at: Timestamp::from_millis(cached.fetched_at),
            etag: cached.etag.clone(),
        },
        models: parsed.models,
        fallback_reason: None,
        sources: Vec::new(),
    })
}

impl HttpLineProber {
    /// Keep provider model lists under `dir` (e.g. `<data dir>/cache/models`) as
    /// well as in memory, so `spb` reuses a list for [`MODEL_LIST_TTL`] across
    /// runs. Without it lists are cached in memory only.
    pub fn with_model_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.models = std::sync::Arc::new(ModelListCache::new(Some(dir.into())));
        self
    }

    /// See [`superba_core::LineProber::model_list`].
    pub(crate) async fn model_list_impl(&self, req: ModelListRequest<'_>) -> Result<ModelList> {
        if let Some(source) = req.source {
            return self.sourced_model_list(&req, source).await;
        }
        let (site, base, key) = (req.site, req.base_url, req.key);
        Self::ensure_key(key)?;
        ensure_key_target(base, key)?;
        ensure_site_origin(site, base)?;
        let client_version = req
            .client_version
            .map(whole_version)
            .unwrap_or_else(|| self.config.client_version.clone());
        let id = ModelListCache::id(site, base, key, &client_version);
        let now = self.clock.now();
        let cached = self.models.get(&id);
        if let Some(c) = &cached {
            let age = now.duration_since(Timestamp::from_millis(c.fetched_at));
            if !req.refresh && age.is_some_and(|a| a < MODEL_LIST_TTL) {
                return list_from(c);
            }
        }
        let native = is_codex_backend(base);
        let candidates = if native {
            vec![models_url_for_base(base, &client_version)]
        } else {
            compatible_candidates(base)
        };
        let mut missing = Vec::new();
        for url in candidates {
            let path = source_path(&url);
            let etag = cached
                .as_ref()
                .filter(|c| c.path == path)
                .and_then(|c| c.etag.clone());
            let request = Request::get(
                url.clone(),
                self.config.request_timeout,
                self.config.max_catalog_bytes,
            )
            .with_key(key)
            .if_none_match(etag);
            let a = self
                .http
                .send(request)
                .await
                .map_err(|t| t.into_error(&url))?;
            tracing::debug!(url = %display_url(&url), status = a.status, "model list");
            let http_error = |message: String| Error::Http {
                url: display_url(&url),
                status: a.status,
                message,
            };
            if a.status == 304
                && let Some(c) = cached.as_ref().filter(|c| c.path == path)
            {
                let mut c = c.clone();
                c.fetched_at = now.as_millis();
                self.models.put(&id, c.clone());
                return list_from(&c);
            }
            // An edge answering for the relay (Krill's `l-block`, or an empty /
            // non-JSON 2xx on a Codex backend path); compatible sites only by header.
            if edge_intercepted(&a) && !a.truncated && (native || a.edge_header.is_some()) {
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
                    let parsed = std::str::from_utf8(&a.body)
                        .ok()
                        .and_then(|b| parse_model_list(b.as_bytes()).ok().map(|p| (b, p)));
                    match parsed {
                        Some((body, _)) => {
                            let c = CachedList {
                                format: CACHE_FORMAT,
                                fetched_at: now.as_millis(),
                                etag: a.etag.clone(),
                                path,
                                body: body.to_owned(),
                            };
                            self.models.put(&id, c.clone());
                            return list_from(&c);
                        }
                        // A compatible site answering its web page for unknown
                        // paths: it does not list models there.
                        None if !native => missing.push(display_url(&url)),
                        None => {
                            return Err(Error::parse(
                                "model list",
                                format!("{} did not answer with a model list", display_url(&url)),
                            ));
                        }
                    }
                }
                401 => {
                    return Err(http_error(
                        "the key was rejected (invalid or expired key)".into(),
                    ));
                }
                404 | 405 => missing.push(display_url(&url)),
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
                "site {} does not list models ({})",
                site.id,
                missing.join(", ")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shapes_parse_and_keep_what_they_carry() {
        let backend = r#"{"models":[{"slug":"gpt-6.1-sol","display_name":"GPT-6.1-Sol","context_window":500000,"service_tiers":[{"id":"priority","name":"Fast","description":"x"}],"extra":true},{"slug":"gpt-6-astra"},{"slug":"gpt-6-astra"}]}"#;
        let p = parse_model_list(backend.as_bytes()).unwrap();
        assert!(p.native);
        assert_eq!(p.models.len(), 2);
        assert_eq!(p.models[0].context_window, Some(500_000));
        assert_eq!(p.models[0].service_tiers.len(), 1);
        assert_eq!(p.models[0].raw.as_ref().unwrap()["extra"], true);

        let openai = r#"{"object":"list","data":[
            {"id":"gpt-5","object":"model"},
            {"id":"or/model","name":"Nice Name","context_length":131072,"architecture":{"input_modalities":["text","image"]}},
            {"id":"litellm","max_input_tokens":"200000"},
            {"id":"vllm","max_model_len":32768},
            {"id":"router","top_provider":{"context_length":64000}},
            {"id":"same","name":"same"}
        ]}"#;
        let p = parse_model_list(openai.as_bytes()).unwrap();
        assert!(!p.native);
        let get = |id: &str| p.models.iter().find(|m| m.slug == id).unwrap().clone();
        assert_eq!(get("gpt-5").context_window, None);
        assert_eq!(get("or/model").display_name.as_deref(), Some("Nice Name"));
        assert_eq!(get("or/model").context_window, Some(131_072));
        assert_eq!(get("or/model").max_context_window, Some(131_072));
        assert_eq!(get("or/model").input_modalities, ["text", "image"]);
        assert_eq!(get("litellm").context_window, Some(200_000));
        assert_eq!(get("vllm").context_window, Some(32_768));
        assert_eq!(get("router").context_window, Some(64_000));
        assert_eq!(get("same").display_name, None);
        assert!(get("gpt-5").raw.is_none());

        let bare = r#"["a", {"id":"b"}, 3]"#;
        assert_eq!(parse_models(bare.as_bytes()).unwrap().len(), 2);
        assert!(parse_models(b"{}").is_err());
        assert!(parse_models(b"").is_err());
        assert!(parse_models(b"<html></html>").is_err());
    }

    #[test]
    fn compatible_sites_try_with_and_without_v1() {
        let u = |s: &str| Url::parse(s).unwrap();
        let paths = |b: &str| {
            compatible_candidates(&u(b))
                .iter()
                .map(|x| x.path().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths("https://h/v1"), ["/v1/models", "/models"]);
        assert_eq!(paths("https://h/v1/"), ["/v1/models", "/models"]);
        assert_eq!(paths("https://h/api"), ["/api/models", "/api/v1/models"]);
        assert_eq!(paths("https://h"), ["/models", "/v1/models"]);
        assert!(is_codex_backend(&u("https://h/backend-api/codex")));
        assert!(!is_codex_backend(&u("https://h/v1")));
    }

    #[test]
    fn cache_files_never_contain_the_key() {
        let dir = std::env::temp_dir().join(format!(
            "superba-net-models-cache-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let cache = ModelListCache::new(Some(dir.clone()));
        let key = Secret::new("sk-secret-value-models-1234");
        let site = superba_core::catalog::builtin_sites().remove(0);
        let base = Url::parse("https://example.com/v1").unwrap();
        let id = ModelListCache::id(&site, &base, &key, "0.163.0");
        assert!(!id.contains("1234"));
        cache.put(
            &id,
            CachedList {
                format: CACHE_FORMAT,
                fetched_at: 1,
                etag: Some("\"e1\"".into()),
                path: "/v1/models".into(),
                body: r#"{"data":[{"id":"m"}]}"#.into(),
            },
        );
        let fresh = ModelListCache::new(Some(dir.clone()));
        assert_eq!(fresh.get(&id).unwrap().etag.as_deref(), Some("\"e1\""));
        for e in std::fs::read_dir(&dir).unwrap() {
            let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
            assert!(!text.contains("sk-secret"));
        }
        assert_ne!(
            ModelListCache::id(&site, &base, &Secret::new("sk-other-key-000000"), "0.163.0"),
            id
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
