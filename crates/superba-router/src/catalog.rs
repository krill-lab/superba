//! Codex catalogs retain the upstream JSON (including service_tiers). A
//! generation owns its cache, so changing members or credentials invalidates it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_LENGTH, ETAG, IF_NONE_MATCH, RETRY_AFTER,
};
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;

use crate::forward::{ROUTE_HEADER, full, router_error};
use crate::limits::Admission;
use crate::proxy::{self, Ingress};
use crate::state::{RespBody, Shared, lock};

const TTL: Duration = Duration::from_secs(300);
const RETRY: Duration = Duration::from_secs(30);
const MAX_STALE: Duration = Duration::from_secs(3600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_SLOTS: usize = 32;

/// An ETag belongs to the exact member and line that supplied the body.
/// The proxy removes it before failing over to another origin.
#[derive(Clone)]
pub(crate) struct ValidatorRoute(pub HeaderValue);

#[derive(Clone)]
struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Answer {
    fn response(&self) -> Response<RespBody> {
        let mut r = Response::new(full(self.body.clone()));
        *r.status_mut() = self.status;
        *r.headers_mut() = self.headers.clone();
        r.headers_mut().remove(CONTENT_LENGTH);
        r
    }
}

#[derive(Default)]
struct Entry {
    good: Option<(Instant, Answer)>,
    answer: Option<Answer>,
    fetched: Option<Instant>,
    next: Option<Instant>,
}

/// Bounded, single-flight cache, shared by all local Codex clients.
#[derive(Default)]
pub(crate) struct Catalog {
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Entry>>>>,
}

impl Catalog {
    fn slot(&self, key: String) -> Option<Arc<tokio::sync::Mutex<Entry>>> {
        let mut slots = lock(&self.slots);
        if let Some(slot) = slots.get(&key) {
            return Some(slot.clone());
        }
        if slots.len() >= MAX_SLOTS {
            // Never evict a fetch in progress: callers of the same key must
            // still join it. An idle slot may be replaced by a newer version.
            let idle = slots
                .iter()
                .find_map(|(key, slot)| (Arc::strong_count(slot) == 1).then(|| key.clone()))?;
            slots.remove(&idle);
        }
        let slot = Arc::new(tokio::sync::Mutex::new(Entry::default()));
        slots.insert(key, slot.clone());
        Some(slot)
    }
}

/// A catalog request, after the router token and admission were checked.
pub(crate) async fn handle(
    shared: Arc<Shared>,
    mut req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let arrived = Instant::now();
    let generation = shared.current();
    let query = req.uri().query().unwrap_or("").to_owned();
    if generation.config.catalog.is_some() {
        crate::server::drain(req.into_body(), generation.config.tuning.max_body_bytes).await;
        return crate::merged_catalog::response(&generation, &query).await;
    }
    // Do not retain arbitrary, unbounded URLs in a process-wide cache.
    let slot = (query.len() <= 1024)
        .then(|| generation.catalog.slot(query.to_owned()))
        .flatten();
    let Some(slot) = slot else {
        return proxy::proxy_http(shared, req, rest, admission, Ingress::Codex).await;
    };
    let mut entry = slot.lock_owned().await;
    let now = Instant::now();
    if let Some(answer) = entry.answer.clone()
        && (entry.next.is_some_and(|until| now < until)
            || entry.fetched.is_some_and(|at| at >= arrived))
    {
        crate::server::drain(req.into_body(), generation.config.tuning.max_body_bytes).await;
        return answer.response();
    }
    // The cache, rather than a particular client's cache or conversation,
    // chooses the validation token and member order.
    for name in [
        IF_NONE_MATCH.as_str(),
        "session-id",
        "thread-id",
        "x-client-request-id",
    ] {
        req.headers_mut().remove(name);
    }
    req.headers_mut()
        .insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if let Some((_, good)) = &entry.good
        && let (Some(etag), Some(route)) = (good.headers.get(ETAG), good.headers.get(ROUTE_HEADER))
    {
        req.headers_mut().insert(IF_NONE_MATCH, etag.clone());
        req.extensions_mut().insert(ValidatorRoute(route.clone()));
    }
    let fetch = async move {
        let response = proxy::proxy_http(shared, req, rest, admission, Ingress::Codex).await;
        let (parts, mut body) = response.into_parts();
        let mut bytes = bytes::BytesMut::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.ok()?;
            if let Ok(data) = frame.into_data() {
                if bytes.len().saturating_add(data.len()) > MAX_BYTES {
                    return None;
                }
                bytes.extend_from_slice(&data);
            }
        }
        let body = bytes.freeze();
        Some(Answer {
            status: parts.status,
            headers: parts.headers,
            body,
        })
    };
    let answer = tokio::time::timeout(FETCH_TIMEOUT, fetch)
        .await
        .ok()
        .flatten();
    let now = Instant::now();
    let mut answer = match answer {
        Some(a) => a,
        None => {
            let r = router_error(
                StatusCode::BAD_GATEWAY,
                "catalog_unavailable",
                "model catalog fetch failed or timed out",
            );
            let (parts, body) = r.into_parts();
            Answer {
                status: parts.status,
                headers: parts.headers,
                body: body.collect().await.expect("infallible body").to_bytes(),
            }
        }
    };
    let mut next = now;
    if answer.status == StatusCode::NOT_MODIFIED {
        if let Some((_, good)) = &entry.good
            && answer.headers.get(ROUTE_HEADER) == good.headers.get(ROUTE_HEADER)
        {
            let mut validated = good.clone();
            for name in [ETAG, CACHE_CONTROL] {
                if let Some(value) = answer.headers.get(&name) {
                    validated.headers.insert(name, value.clone());
                }
            }
            answer = validated;
        } else {
            // Never invent a catalog from a 304 without a stored body.
            return router_error(
                StatusCode::BAD_GATEWAY,
                "catalog_unavailable",
                "upstream returned 304 without a cached catalog",
            );
        }
    }
    if answer.status == StatusCode::OK && is_catalog(&answer.body) {
        next = now + freshness(&answer.headers);
        if directive(&answer.headers, "no-store") {
            entry.good = None;
            entry.answer = None;
            entry.next = None;
            return answer.response();
        }
        entry.good = Some((now, answer.clone()));
    } else if answer.status == StatusCode::TOO_MANY_REQUESTS || answer.status.is_server_error() {
        let delay = answer
            .headers
            .get(RETRY_AFTER)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(RETRY)
            .clamp(Duration::from_secs(1), Duration::from_secs(300));
        next = now + delay;
        let stale = entry.good.as_ref().filter(|(at, good)| {
            now.duration_since(*at) < MAX_STALE
                && !directive(&good.headers, "must-revalidate")
                && !directive(&good.headers, "no-cache")
        });
        tracing::info!(
            status = answer.status.as_u16(),
            stale = stale.is_some(),
            retry_secs = delay.as_secs(),
            "upstream Codex catalog unavailable"
        );
        if let Some((at, good)) = stale {
            next = next.min(*at + MAX_STALE);
            answer = good.clone();
            answer
                .headers
                .insert("x-superba-catalog", HeaderValue::from_static("stale"));
        }
    } else {
        // An account rejection must not keep serving the old account's list.
        entry.good = None;
    }
    entry.next = Some(next);
    entry.fetched = Some(now);
    entry.answer = Some(answer.clone());
    answer.response()
}

fn is_catalog(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|v| v.get("models").is_some_and(serde_json::Value::is_array))
}

fn freshness(headers: &HeaderMap) -> Duration {
    let control = headers
        .get(CACHE_CONTROL)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if control
        .split(',')
        .any(|p| matches!(p.trim(), "no-cache" | "no-store"))
    {
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

fn directive(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim().eq_ignore_ascii_case(name))
}
