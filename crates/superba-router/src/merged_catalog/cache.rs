//! Independent L1/L2 caches, with origin-scoped validators and bounded backoff.

use std::sync::Arc;
use std::time::{Duration, Instant};

use http::header::{AUTHORIZATION, CACHE_CONTROL, ETAG, IF_NONE_MATCH, RETRY_AFTER};
use serde_json::Value;
use superba_core::{CatalogLayer, CatalogSourceStatus};
use url::Url;

use crate::config::{MemberKind, join_path};
use crate::state::{Generation, lock, unix_ms};

const TTL: Duration = Duration::from_secs(600);
const MAX_STALE: Duration = Duration::from_secs(3600);
pub(super) const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct Good {
    entries: Vec<Value>,
    endpoint: String,
    etag: Option<http::HeaderValue>,
    observed: u64,
    at: Instant,
    reusable: bool,
}

#[derive(Default)]
pub(super) struct Entry {
    good: Option<Good>,
    next: Option<Instant>,
    result: Option<Source>,
}

#[derive(Clone)]
pub(super) struct Source {
    pub entries: Vec<Value>,
    pub status: CatalogSourceStatus,
}

/// Standard models always come from the standard endpoint, including Krill's
/// universe endpoint (its Codex endpoint has a different authorization scope).
pub(super) fn standard_base(g: &Generation, member: usize, line: usize) -> Url {
    let m = &g.members[member];
    let mut base = m.lines[line].cfg.base_url.clone();
    if m.cfg.kind == MemberKind::Krill {
        base.set_path("/v1");
        base.set_query(None);
    } else if let Some(path) = base.path().strip_suffix("/backend-api/codex") {
        let path = format!("{path}/v1");
        base.set_path(&path);
    }
    base
}

pub(super) fn native_responses(g: &Generation, member: usize) -> bool {
    g.openai.members[member].responses.is_some()
}

pub(super) async fn fetch(
    g: &Arc<Generation>,
    member: usize,
    standard: bool,
    query: &str,
) -> Source {
    let query = url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "client_version")
        .map(|(_, v)| v.into_owned())
        .filter(|v| {
            v.len() <= 64
                && v.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
        })
        .unwrap_or_else(|| "0.163.0".into());
    let query = format!("client_version={query}");
    let key = format!("{member}:{standard}:{}", if standard { "" } else { &query });
    let slot = {
        let mut slots = lock(&g.merged_catalog.sources);
        if slots.len() >= 128
            && !slots.contains_key(&key)
            && let Some(idle) = slots
                .iter()
                .find(|(_, s)| Arc::strong_count(s) == 1)
                .map(|(k, _)| k.clone())
        {
            slots.remove(&idle);
        }
        slots.entry(key).or_default().clone()
    };
    let mut entry = slot.lock().await;
    if let Some(result) = &entry.result
        && entry.next.is_some_and(|at| Instant::now() < at)
    {
        return result.clone();
    }
    let m = &g.members[member];
    let layer = if standard {
        CatalogLayer::Standard
    } else {
        CatalogLayer::Codex
    };
    let mut status = CatalogSourceStatus {
        provider: m.cfg.id.clone(),
        layer,
        endpoint: String::new(),
        observed_at: None,
        stale: false,
        error: None,
    };
    let mut backoff = Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut rejected = false;
    for (line, l) in m.lines.iter().enumerate() {
        let base = if standard {
            standard_base(g, member, line)
        } else {
            l.cfg.base_url.clone()
        };
        let endpoint = join_path(&base, "/models", (!standard).then_some(&query));
        status.endpoint = endpoint.to_string();
        let mut request = g
            .http
            .get(endpoint.clone())
            .header(AUTHORIZATION, m.auth.clone())
            .header(http::header::ACCEPT_ENCODING, "identity");
        if let Some(good) = &entry.good
            && good.endpoint == endpoint.as_str()
            && let Some(etag) = &good.etag
        {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let fetched = tokio::time::timeout_at(deadline, async {
            let response = request
                .send()
                .await
                .map_err(|_| "model list connection failed".to_string())?;
            let code = response.status();
            let headers = response.headers().clone();
            let body = read_bounded(response).await?;
            Ok::<_, String>((code, headers, body))
        })
        .await;
        let (code, headers, body) = match fetched {
            Ok(Ok(r)) => r,
            Ok(Err(error)) => {
                status.error = Some(error);
                continue;
            }
            Err(_) => {
                status.error = Some("model list timed out".into());
                break;
            }
        };
        if code == http::StatusCode::NOT_MODIFIED {
            if let Some(good) = &mut entry.good
                && good.endpoint == endpoint.as_str()
            {
                good.at = Instant::now();
                good.observed = unix_ms();
                status.observed_at = Some(good.observed);
                status.error = None;
                let result = Source {
                    entries: good.entries.clone(),
                    status,
                };
                entry.next = Some(Instant::now() + freshness(&headers));
                entry.result = Some(result.clone());
                return result;
            }
            status.error = Some("model list returned 304 without a stored catalog".into());
            continue;
        }
        if code.is_success() {
            match parse(&body, standard) {
                Ok(entries) => {
                    status.observed_at = Some(unix_ms());
                    status.error = None;
                    let control = headers
                        .get(CACHE_CONTROL)
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or("");
                    let no_store = control.split(',').any(|p| p.trim() == "no-store");
                    entry.good = (!no_store).then(|| Good {
                        entries: entries.clone(),
                        endpoint: endpoint.to_string(),
                        etag: headers.get(ETAG).cloned(),
                        observed: status.observed_at.unwrap_or_default(),
                        at: Instant::now(),
                        reusable: !control.contains("must-revalidate")
                            && !control.contains("no-cache"),
                    });
                    let result = Source { entries, status };
                    entry.next = Some(Instant::now() + freshness(&headers));
                    entry.result = (!no_store).then(|| result.clone());
                    return result;
                }
                Err(error) => {
                    status.error = Some(error);
                    continue;
                }
            }
        }
        status.error = Some(format!("model list returned HTTP {}", code.as_u16()));
        if matches!(code.as_u16(), 401 | 403) {
            entry.good = None;
            rejected = true;
            break;
        }
        if code == http::StatusCode::TOO_MANY_REQUESTS {
            backoff = Duration::from_secs(
                headers
                    .get(RETRY_AFTER)
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30)
                    .clamp(1, 600),
            );
            break;
        }
        if !code.is_server_error() {
            break;
        }
    }
    let entries = match &entry.good {
        Some(good) if !rejected && good.reusable && good.at.elapsed() < MAX_STALE => {
            status.stale = true;
            status.observed_at = Some(good.observed);
            status.endpoint = good.endpoint.clone();
            good.entries.clone()
        }
        _ => Vec::new(),
    };
    let result = Source { entries, status };
    entry.next = Some(Instant::now() + backoff);
    entry.result = Some(result.clone());
    result
}

pub(super) async fn read_bounded(
    mut response: reqwest::Response,
) -> std::result::Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "incomplete response".to_string())?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
            return Err("response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn parse(bytes: &[u8], standard: bool) -> std::result::Result<Vec<Value>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "invalid model list JSON".to_string())?;
    let rows = value
        .get(if standard { "data" } else { "models" })
        .and_then(Value::as_array)
        .ok_or_else(|| {
            if standard {
                "expected a standard data[] list"
            } else {
                "expected a Codex models[] catalog"
            }
            .to_string()
        })?;
    if rows.len() > 2048 {
        return Err("too many model entries".into());
    }
    let id_key = if standard { "id" } else { "slug" };
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        let id = row[id_key]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .ok_or_else(|| format!("model entry has no valid {id_key}"))?;
        if !ids.insert(id) {
            return Err("duplicate model id".into());
        }
        if !standard && !valid_codex(row) {
            return Err(format!("incomplete Codex entry: {id}"));
        }
    }
    Ok(rows.clone())
}

pub(super) fn valid_codex(v: &Value) -> bool {
    ["slug", "display_name", "shell_type", "visibility"]
        .iter()
        .all(|k| v[*k].is_string())
        && ["supported_reasoning_levels", "experimental_supported_tools"]
            .iter()
            .all(|k| v[*k].is_array())
        && ["supported_in_api", "support_verbosity"]
            .iter()
            .all(|k| v[*k].is_boolean())
        && v["priority"].as_i64().is_some()
        && v["truncation_policy"].is_object()
        && (v["base_instructions"].is_string()
            || v["model_messages"]["instructions_template"].is_string())
}

fn freshness(headers: &http::HeaderMap) -> Duration {
    let control = headers
        .get(CACHE_CONTROL)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if control.contains("no-store") || control.contains("no-cache") {
        return Duration::ZERO;
    }
    control
        .split(',')
        .find_map(|p| {
            p.trim()
                .strip_prefix("max-age=")?
                .trim_matches('"')
                .parse::<u64>()
                .ok()
        })
        .map(Duration::from_secs)
        .unwrap_or(TTL)
        .min(TTL)
}
