//! Request-side strip: remove the router's own stats line from assistant
//! `output_text` parts of an outgoing Responses request (HTTP body or WebSocket
//! `response.create` frame).
//!
//! Rule (spike RS 3.3): in `input[]` items with `type: "message"` (or no type)
//! and `role: "assistant"`, an `output_text` part whose text ends with `"\n\n"`
//! plus one line containing the marker `⚡` + U+2060 loses that suffix. Nothing
//! else changes: user messages, tool outputs and reasoning are never touched,
//! and every item without the marker keeps its exact bytes.
//!
//! Cost: one `memmem` per request; only when the marker is present is the top
//! level split into raw values and are the items carrying it re-serialised.

use std::borrow::Cow;
use std::fmt;
use std::io::Read;

use memchr::memmem;
use serde::Deserializer;
use serde::de::{MapAccess, Visitor};
use serde_json::value::RawValue;
use superba_core::codex::STATS_LINE_MARKER;

/// The word joiner of the marker as it appears escaped in JSON text.
const ESCAPED_JOINER: &[u8] = b"\\u2060";

/// True when `bytes` may contain a stats line (raw or JSON-escaped marker).
pub fn may_contain_line(bytes: &[u8]) -> bool {
    memmem::find(bytes, "\u{2060}".as_bytes()).is_some()
        || memmem::find(bytes, ESCAPED_JOINER).is_some()
}

/// `text` without a trailing stats line, or `None` when it has none.
pub fn strip_text(text: &str) -> Option<&str> {
    let pos = text.rfind("\n\n")?;
    let tail = &text[pos + 2..];
    (!tail.contains('\n') && tail.contains(STATS_LINE_MARKER)).then(|| &text[..pos])
}

/// Strip the stats lines of a JSON request `body`: `Some((new body, lines
/// removed))`, or `None` when nothing was removed (the body stays as it is).
pub fn strip(body: &[u8]) -> Option<(Vec<u8>, u32)> {
    if !may_contain_line(body) {
        return None;
    }
    let top = split_object(body)?;
    let input_at = top.iter().position(|(k, _)| k == "input")?;
    let items: Vec<&RawValue> = serde_json::from_str(top[input_at].1.get()).ok()?;
    let mut removed = 0u32;
    let mut new_items: Vec<Cow<'_, str>> = Vec::with_capacity(items.len());
    for item in &items {
        let raw = item.get();
        if may_contain_line(raw.as_bytes())
            && let Some((text, n)) = strip_item(raw)
        {
            removed += n;
            new_items.push(Cow::Owned(text));
            continue;
        }
        new_items.push(Cow::Borrowed(raw));
    }
    if removed == 0 {
        return None;
    }
    Some((assemble(&top, input_at, &new_items)?, removed))
}

/// [`strip`] for a body with `Content-Encoding: zstd` (decoded up to `limit`
/// bytes, re-encoded after a change).
pub fn strip_zstd(body: &[u8], limit: usize) -> Option<(Vec<u8>, Vec<u8>, u32)> {
    let plain = decode_zstd(body, limit)?;
    let (stripped, n) = strip(&plain)?;
    let encoded = zstd::stream::encode_all(stripped.as_slice(), 3).ok()?;
    Some((encoded, stripped, n))
}

/// Decode a zstd body of at most `limit` bytes.
pub fn decode_zstd(body: &[u8], limit: usize) -> Option<Vec<u8>> {
    let decoder = zstd::stream::read::Decoder::new(body).ok()?;
    let mut out = Vec::new();
    let read = decoder
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut out)
        .ok()?;
    (read <= limit).then_some(out)
}

/// Strip one input item; `Some((new JSON, lines removed))` when it changed.
fn strip_item(raw: &str) -> Option<(String, u32)> {
    let mut item: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = item.as_object_mut()?;
    let is_message = obj
        .get("type")
        .map(|t| t.as_str() == Some("message"))
        .unwrap_or(true);
    if !is_message || obj.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    let mut removed = 0;
    for part in obj.get_mut("content")?.as_array_mut()? {
        let Some(p) = part.as_object_mut() else {
            continue;
        };
        if p.get("type").and_then(|t| t.as_str()) != Some("output_text") {
            continue;
        }
        let Some(text) = p.get("text").and_then(|t| t.as_str()) else {
            continue;
        };
        if let Some(kept) = strip_text(text) {
            let kept = kept.to_owned();
            p.insert("text".into(), serde_json::Value::String(kept));
            removed += 1;
        }
    }
    if removed == 0 {
        return None;
    }
    Some((serde_json::to_string(&item).ok()?, removed))
}

/// The members of a top-level JSON object in order, values as raw text.
pub(crate) type RawObject<'a> = Vec<(Cow<'a, str>, &'a RawValue)>;

/// `top` written out again with `items` as the value of member `input_at`
/// (an array); every other member keeps its exact bytes.
pub(crate) fn assemble(
    top: &RawObject<'_>,
    input_at: usize,
    items: &[Cow<'_, str>],
) -> Option<Vec<u8>> {
    let size = top
        .iter()
        .map(|(k, v)| k.len() + v.get().len() + 4)
        .sum::<usize>()
        + items.iter().map(|i| i.len() + 1).sum::<usize>();
    let mut out = Vec::with_capacity(size);
    out.push(b'{');
    for (i, (key, value)) in top.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        // A key that round-trips through serde_json is valid JSON text.
        out.extend_from_slice(serde_json::to_string(key.as_ref()).ok()?.as_bytes());
        out.push(b':');
        if i == input_at {
            out.push(b'[');
            for (j, item) in items.iter().enumerate() {
                if j > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(item.as_bytes());
            }
            out.push(b']');
        } else {
            out.extend_from_slice(value.get().as_bytes());
        }
    }
    out.push(b'}');
    Some(out)
}

/// The members of a top-level JSON object in order, values as raw text.
pub(crate) fn split_object(body: &[u8]) -> Option<RawObject<'_>> {
    let text = std::str::from_utf8(body).ok()?;
    let mut de = serde_json::Deserializer::from_str(text);
    let out = (&mut de).deserialize_map(ObjectVisitor).ok()?;
    de.end().ok()?;
    Some(out)
}

struct ObjectVisitor;

impl<'de> Visitor<'de> for ObjectVisitor {
    type Value = RawObject<'de>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            let value: &'de RawValue = map.next_value()?;
            out.push((key, value));
        }
        Ok(out)
    }
}
