//! The models a Claude Code member serves, when Superba did not write them
//! into the configuration ([`super::config::ClaudeMember::models`]): the
//! member's own Anthropic list, `GET <anthropic base>/v1/models?limit=1000`
//! with its key (`x-api-key` or bearer) and `anthropic-version`, pages
//! followed with `after_id`. Answers in the OpenAI shape (`data[].id`, as
//! Krill's `/v1/models` answers) are read too.
//!
//! Lists are kept for ten minutes (a failure for one) across reloads of the
//! same configuration, fetched once at a time per member, and never logged
//! with their key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http::HeaderMap;
use serde_json::Value;
use url::Url;

use crate::config::join_path;
use crate::state::{Generation, lock};

/// A list is used this long.
const FRESH: Duration = Duration::from_secs(600);
/// A failed fetch is not repeated for this long.
const RETRY: Duration = Duration::from_secs(60);
/// One list request may take this long.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// Pages of a list read at most.
const MAX_PAGES: usize = 10;
/// `anthropic-version` sent with list and count requests.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A model of a member's list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listed {
    /// Model id.
    pub id: String,
    /// Display name.
    pub display_name: Option<String>,
}

/// The models of a list answer (`data[]` in the Anthropic or OpenAI shape,
/// `models[]` with `slug`, or a bare array) and the next page's `after_id`
/// (Anthropic `has_more` + `last_id`).
pub fn parse_list(body: &[u8]) -> Option<(Vec<Listed>, Option<String>)> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let items = v
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| v.get("models").and_then(Value::as_array))
        .or_else(|| v.as_array())?;
    let text = |o: &Value, k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let models = items
        .iter()
        .filter_map(|item| match item {
            Value::String(s) if !s.trim().is_empty() => Some(Listed {
                id: s.trim().to_owned(),
                display_name: None,
            }),
            Value::Object(_) => {
                let id = text(item, "id")
                    .or_else(|| text(item, "slug"))
                    .or_else(|| text(item, "model"))?;
                let display_name = text(item, "display_name")
                    .or_else(|| text(item, "name"))
                    .filter(|n| n != &id);
                Some(Listed { id, display_name })
            }
            _ => None,
        })
        .collect();
    let more = (v.get("has_more").and_then(Value::as_bool) == Some(true))
        .then(|| text(&v, "last_id"))
        .flatten();
    Some((models, more))
}

#[derive(Clone, Debug)]
struct Entry {
    at: Instant,
    generation: u64,
    models: Option<Arc<Vec<Listed>>>,
}

/// Fetched lists by member id.
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Entry>>>>>,
}

impl Catalog {
    /// The list of member `index` (`None`: unknown, the fetch failed).
    pub async fn lists(
        &self,
        generation: &Arc<Generation>,
        index: usize,
    ) -> Option<Arc<Vec<Listed>>> {
        self.lists_via(generation, index, "Claude Code", || {
            fetch(generation, index)
        })
        .await
    }

    /// The list of member `index` read by `fetch` when none is fresh
    /// (`what` names the ingress in the log).
    pub async fn lists_via<F, Fut>(
        &self,
        generation: &Arc<Generation>,
        index: usize,
        what: &str,
        fetch: F,
    ) -> Option<Arc<Vec<Listed>>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<Listed>, String>>,
    {
        let id = generation.members[index].cfg.id.clone();
        let slot = lock(&self.slots).entry(id.clone()).or_default().clone();
        let mut entry = slot.lock().await;
        if let Some(e) = entry.as_ref() {
            let ttl = if e.models.is_some() { FRESH } else { RETRY };
            if e.generation == generation.number && e.at.elapsed() < ttl {
                return e.models.clone();
            }
        }
        let models = match fetch().await {
            Ok(list) => Some(Arc::new(list)),
            Err(reason) => {
                tracing::info!(member = %id, %reason, ingress = what, "model list unavailable");
                None
            }
        };
        *entry = Some(Entry {
            at: Instant::now(),
            generation: generation.number,
            models: models.clone(),
        });
        models
    }
}

/// `GET url` with `headers`, following pages; the models or why not.
pub(crate) async fn get_list(
    generation: &Generation,
    url: &Url,
    headers: &HeaderMap,
) -> Result<Vec<Listed>, String> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut u = url.clone();
        let mut q = String::from("limit=1000");
        if let Some(a) = &after {
            q.push_str("&after_id=");
            q.extend(url::form_urlencoded::byte_serialize(a.as_bytes()));
        }
        u.set_query(Some(&q));
        let resp = generation
            .http
            .get(u)
            .headers(headers.clone())
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .map_err(|e| crate::forward::describe_error(&e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| crate::forward::describe_error(&e))?;
        let (models, more) = parse_list(&body).ok_or("not a model list")?;
        out.extend(models);
        match more {
            Some(last) => after = Some(last),
            None => return Ok(out),
        }
    }
    Ok(out)
}

/// Read the list of member `index` (the first line that answers).
async fn fetch(generation: &Generation, index: usize) -> Result<Vec<Listed>, String> {
    let c = generation
        .claude
        .as_ref()
        .and_then(|c| c.member(index))
        .ok_or("not a Claude Code member")?;
    let mut headers = HeaderMap::new();
    headers.insert(c.auth.0.clone(), c.auth.1.clone());
    headers.insert(
        "anthropic-version",
        http::HeaderValue::from_static(ANTHROPIC_VERSION),
    );
    let mut last_error = String::from("no line");
    for base in &c.base {
        let url = join_path(base, "/v1/models", None);
        match get_list(generation, &url, &headers).await {
            Ok(list) => return Ok(list),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}
