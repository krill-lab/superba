//! Per-member model name mapping in request bodies.
//!
//! Bodies are otherwise forwarded byte for byte, `Content-Encoding` included.
//! Only when a member maps the request's top-level `"model"` is the body parsed:
//! identity bodies directly, `zstd` bodies (Codex compresses them in PAT mode) by
//! a bounded decode and a re-encode. Other encodings are left untouched.

use std::collections::BTreeMap;
use std::io::Read;

use bytes::Bytes;
use http::HeaderMap;
use http::header::CONTENT_ENCODING;

/// The rewritten body, or `None` when it stays as it is.
pub fn map_model(
    body: &Bytes,
    headers: &HeaderMap,
    map: &BTreeMap<String, String>,
    limit: usize,
) -> Option<Bytes> {
    if map.is_empty() || body.is_empty() {
        return None;
    }
    let encoding = headers
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase());
    match encoding.as_deref() {
        None | Some("") | Some("identity") => rewrite_json(body, map).map(Bytes::from),
        Some("zstd") => {
            let plain = decode_zstd(body, limit)?;
            let rewritten = rewrite_json(&plain, map)?;
            zstd::stream::encode_all(rewritten.as_slice(), 3)
                .ok()
                .map(Bytes::from)
        }
        Some(other) => {
            tracing::debug!(
                encoding = other,
                "model mapping skipped: unsupported encoding"
            );
            None
        }
    }
}

fn decode_zstd(body: &[u8], limit: usize) -> Option<Vec<u8>> {
    let decoder = zstd::stream::read::Decoder::new(body).ok()?;
    let mut out = Vec::new();
    let read = decoder
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut out)
        .ok()?;
    (read <= limit).then_some(out)
}

fn rewrite_json(body: &[u8], map: &BTreeMap<String, String>) -> Option<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    let current = obj.get("model")?.as_str()?;
    let target = map.get(current)?;
    if target == current {
        return None;
    }
    obj.insert("model".into(), serde_json::Value::String(target.clone()));
    serde_json::to_vec(&value).ok()
}
