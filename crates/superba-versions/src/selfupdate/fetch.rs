//! One conditional GET of the release API: short timeout, ETag, no credentials.

use std::time::Duration;

use crate::http::{self, MAX_TEXT_BYTES};

/// What a request came back with.
#[derive(Debug)]
pub(crate) enum Fetched {
    /// 200 with a body.
    Fresh { body: Vec<u8>, etag: Option<String> },
    /// 304: the cached answer still holds.
    NotModified,
    /// 404 / 401 / 403: a private repository or no such page.
    Unavailable,
    /// GitHub's rate limit (403 with `x-ratelimit-remaining: 0`, or 429).
    Limited {
        /// `x-ratelimit-reset`: when the limit resets (Unix seconds).
        reset_at: Option<u64>,
        /// `retry-after`: seconds to wait (secondary rate limits).
        retry_after: Option<u64>,
    },
    /// Anything else (network, timeout, 5xx).
    Failed(String),
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

fn limited(headers: &reqwest::header::HeaderMap) -> Fetched {
    Fetched::Limited {
        reset_at: header_u64(headers, "x-ratelimit-reset"),
        retry_after: header_u64(headers, "retry-after"),
    }
}

fn short(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "could not connect".into()
    } else {
        "network error".into()
    }
}

/// GET `url`. `etag` makes it conditional.
pub(crate) async fn get(url: &str, etag: Option<&str>, timeout: Duration) -> Fetched {
    let client = match http::client(true) {
        Ok(c) => c,
        Err(e) => return Fetched::Failed(e.to_string()),
    };
    let mut req = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(tag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let work = async {
        let mut resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return Fetched::Failed(short(&e)),
        };
        let status = resp.status();
        match status.as_u16() {
            304 => return Fetched::NotModified,
            404 | 401 => return Fetched::Unavailable,
            403 => {
                let headers = resp.headers();
                let exhausted = header_u64(headers, "x-ratelimit-remaining") == Some(0);
                return if exhausted || headers.contains_key("retry-after") {
                    limited(headers)
                } else {
                    Fetched::Unavailable
                };
            }
            200 => {}
            429 => return limited(resp.headers()),
            n => return Fetched::Failed(format!("HTTP {n}")),
        }
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_TEXT_BYTES {
                        return Fetched::Failed("response too large".into());
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return Fetched::Failed(short(&e)),
            }
        }
        Fetched::Fresh { body, etag }
    };
    tokio::time::timeout(timeout, work)
        .await
        .unwrap_or_else(|_| Fetched::Failed("timed out".into()))
}
