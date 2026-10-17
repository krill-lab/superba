//! The stats line: rendering from the template Superba wrote, and the rewrite
//! of the held events (spike RS 3.1).
//!
//! The line goes after the final answer's text as `"\n\n> ⚡ …"` (the marker is
//! `⚡` + U+2060 WORD JOINER, invisible in the app and never produced by a
//! model). The rewrite emits one synthetic `response.output_text.delta` with the
//! suffix and appends the same suffix to the text in `output_text.done`,
//! `content_part.done`, `output_item.done` and the matching item of
//! `response.completed`, so `concat(deltas) == done.text == part.text == item
//! text == completed output text`. Only these events are parsed and
//! re-serialised; everything else keeps its bytes.

use bytes::Bytes;
use serde_json::Value;
use superba_core::codex::STATS_LINE_MARKER;
use superba_core::i18n::interpolate;
use superba_core::{StatsLineStyle, Usd};

use super::sse::{self, EventKind};
use super::tap::{Candidate, group_digits};

/// The numbers a line shows (`None` = not measured, shown as `—`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineValues {
    /// Output tokens per second of the answering request (grouped digits).
    pub tps: Option<String>,
    /// First token of the answering request (`0.82s`).
    pub ttft: Option<String>,
    /// Turn input (incl. cached).
    pub input: u64,
    /// Turn output.
    pub output: u64,
    /// Turn cached.
    pub cached: u64,
    /// Turn cost estimate.
    pub cost: Option<Usd>,
    /// Requests in the turn.
    pub requests: u32,
}

/// `$0.019`, `$1.23`, `<$0.001`.
pub fn format_usd(usd: Usd) -> String {
    let micros = usd.micros().max(0);
    if micros == 0 {
        return "$0".into();
    }
    if micros < 1_000 {
        return "<$0.001".into();
    }
    let dollars = micros as f64 / 1e6;
    if dollars >= 1.0 {
        format!("${dollars:.2}")
    } else {
        format!("${dollars:.3}")
    }
}

/// The text after the marker: the template with its placeholders filled, on one
/// line, a leading `⚡` (and word joiner) removed.
pub fn render_text(template: &str, v: &LineValues) -> String {
    let t = template.trim();
    let t = t.strip_prefix('⚡').unwrap_or(t);
    let t = t.strip_prefix('\u{2060}').unwrap_or(t).trim_start();
    let dash = || "—".to_owned();
    let args = vec![
        ("tps".to_owned(), v.tps.clone().unwrap_or_else(dash)),
        ("ttft".to_owned(), v.ttft.clone().unwrap_or_else(dash)),
        ("input".to_owned(), group_digits(v.input)),
        ("output".to_owned(), group_digits(v.output)),
        ("cached".to_owned(), group_digits(v.cached)),
        (
            "cost".to_owned(),
            v.cost.map(format_usd).unwrap_or_else(dash),
        ),
        ("requests".to_owned(), v.requests.to_string()),
    ];
    let filled = interpolate(t, &args);
    filled
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The full suffix: `"\n\n> ⚡ <text>"` (or `> *…*` for the italic style).
pub fn render_suffix(template: &str, style: StatsLineStyle, v: &LineValues) -> String {
    let text = render_text(template, v);
    let line = if text.is_empty() {
        STATS_LINE_MARKER.to_owned()
    } else {
        format!("{STATS_LINE_MARKER} {text}")
    };
    match style {
        StatsLineStyle::QuoteItalic => format!("\n\n> *{line}*"),
        _ => format!("\n\n> {line}"),
    }
}

/// Rewritten events of one injection, as JSON texts.
#[derive(Debug)]
pub struct Rewritten {
    /// The synthetic delta.
    pub delta: String,
    /// Held events in order: `Some(new JSON)` when patched, `None` = unchanged.
    pub held: Vec<Option<String>>,
    /// `response.completed`.
    pub completed: String,
}

fn append(text: &mut Value, suffix: &str) -> bool {
    match text {
        Value::String(s) => {
            s.push_str(suffix);
            true
        }
        _ => false,
    }
}

fn index_matches(v: &Value, key: &str, want: u64) -> bool {
    v.get(key).and_then(Value::as_u64).is_none_or(|i| i == want)
}

fn patch_content(content: Option<&mut Value>, cand: &Candidate, suffix: &str) -> bool {
    let Some(parts) = content.and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(part) = usize::try_from(cand.content_index)
        .ok()
        .and_then(|i| parts.get_mut(i))
    else {
        return false;
    };
    if part.get("type").and_then(Value::as_str) != Some("output_text") {
        return false;
    }
    part.get_mut("text").is_some_and(|t| append(t, suffix))
}

/// Patch the held events (`(kind, JSON)`) and the completed event. `None` when
/// the candidate's `output_text.done` cannot be patched.
pub fn rewrite_events<'a>(
    held: impl Iterator<Item = (EventKind, &'a [u8])>,
    completed: &[u8],
    cand: &Candidate,
    suffix: &str,
) -> Option<Rewritten> {
    let mut out = Vec::new();
    let mut done_patched = false;
    for (kind, json) in held {
        let patched = match kind {
            EventKind::TextDone | EventKind::PartDone | EventKind::ItemDone => {
                let mut v: Value = serde_json::from_slice(json).ok()?;
                let ok = match kind {
                    EventKind::TextDone => {
                        v.get("item_id").and_then(Value::as_str) == Some(cand.item_id.as_str())
                            && index_matches(&v, "content_index", cand.content_index)
                            && v.get_mut("text").is_some_and(|t| append(t, suffix))
                    }
                    EventKind::PartDone => {
                        v.get("item_id").and_then(Value::as_str) == Some(cand.item_id.as_str())
                            && index_matches(&v, "content_index", cand.content_index)
                            && v.get_mut("part").is_some_and(|p| {
                                p.get("type").and_then(Value::as_str) == Some("output_text")
                                    && p.get_mut("text").is_some_and(|t| append(t, suffix))
                            })
                    }
                    _ => v.get_mut("item").is_some_and(|item| {
                        item.get("id").and_then(Value::as_str) == Some(cand.item_id.as_str())
                            && patch_content(item.get_mut("content"), cand, suffix)
                    }),
                };
                if ok && kind == EventKind::TextDone {
                    done_patched = true;
                }
                ok.then(|| serde_json::to_string(&v).ok()).flatten()
            }
            _ => None,
        };
        out.push(patched);
    }
    if !done_patched {
        return None;
    }
    let mut c: Value = serde_json::from_slice(completed).ok()?;
    if let Some(items) = c
        .get_mut("response")
        .and_then(|r| r.get_mut("output"))
        .and_then(Value::as_array_mut)
    {
        for item in items {
            if item.get("id").and_then(Value::as_str) == Some(cand.item_id.as_str()) {
                patch_content(item.get_mut("content"), cand, suffix);
            }
        }
    }
    let delta = serde_json::json!({
        "type": "response.output_text.delta",
        "item_id": cand.item_id,
        "output_index": cand.output_index,
        "content_index": cand.content_index,
        "delta": suffix,
        "logprobs": [],
    });
    Some(Rewritten {
        delta: serde_json::to_string(&delta).ok()?,
        held: out,
        completed: serde_json::to_string(&c).ok()?,
    })
}

fn sse_event(name: Option<&str>, json: &str) -> Bytes {
    let mut s = String::with_capacity(json.len() + 64);
    if let Some(n) = name {
        s.push_str("event: ");
        s.push_str(n);
        s.push('\n');
    }
    s.push_str("data: ");
    s.push_str(json);
    s.push_str("\n\n");
    Bytes::from(s)
}

/// The SSE bytes of an injection: the synthetic delta, the held events (patched
/// ones re-framed, others unchanged) and the patched `response.completed`.
pub fn rewrite_sse(
    held: &[(EventKind, Bytes)],
    completed_raw: &[u8],
    cand: &Candidate,
    suffix: &str,
) -> Option<Vec<Bytes>> {
    let parsed: Vec<sse::SseEvent<'_>> =
        held.iter().map(|(_, raw)| sse::parse_event(raw)).collect();
    let completed = sse::parse_event(completed_raw);
    let r = rewrite_events(
        held.iter()
            .zip(&parsed)
            .map(|((k, _), ev)| (*k, &ev.data[..])),
        &completed.data,
        cand,
        suffix,
    )?;
    let named = parsed.first().is_some_and(|ev| ev.name.is_some());
    let mut out = Vec::with_capacity(held.len() + 2);
    out.push(sse_event(
        named.then_some("response.output_text.delta"),
        &r.delta,
    ));
    for (((_, raw), ev), patched) in held.iter().zip(&parsed).zip(r.held) {
        match patched {
            Some(json) => out.push(sse_event(ev.name, &json)),
            None => out.push(raw.clone()),
        }
    }
    out.push(sse_event(completed.name, &r.completed));
    Some(out)
}

/// The WebSocket frames of an injection (JSON texts): the synthetic delta, the
/// held frames (patched or unchanged) and the patched `response.completed`.
pub fn rewrite_frames(
    held: &[(EventKind, Bytes)],
    completed: &[u8],
    cand: &Candidate,
    suffix: &str,
) -> Option<Vec<Bytes>> {
    let r = rewrite_events(
        held.iter().map(|(k, raw)| (*k, &raw[..])),
        completed,
        cand,
        suffix,
    )?;
    let mut out = Vec::with_capacity(held.len() + 2);
    out.push(Bytes::from(r.delta));
    for ((_, raw), patched) in held.iter().zip(r.held) {
        out.push(patched.map_or_else(|| raw.clone(), Bytes::from));
    }
    out.push(Bytes::from(r.completed));
    Some(out)
}
