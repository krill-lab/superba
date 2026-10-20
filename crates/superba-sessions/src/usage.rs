//! Model requests read from Codex's rollout files, for the Usage page and
//! `spb usage` when no local router recorded them (or alongside the router's
//! richer rows).
//!
//! Source: `sessions/**/rollout-*.jsonl[.zst]` and `archived_sessions/` of a
//! Codex home (Codex 0.162 / 0.163, `codex-rs/rollout/src/policy.rs` lists what is
//! persisted). Per rollout:
//!
//! - `session_meta`: thread id, `model_provider`, `originator`, `cli_version`,
//!   `thread_source` (the first record of the file names the thread);
//! - `turn_context`: the model and reasoning effort of the turn;
//! - `event_msg thread_settings_applied`: model, `model_provider_id` and
//!   `service_tier` from then on;
//! - `token_usage_record` (Codex 0.162+): one record per completed model
//!   response with `response_id`, `turn_id` and its usage - one row each;
//! - `event_msg token_count` (older Codex): the running total and the last
//!   request's usage; a row whenever the total grew by exactly the last usage
//!   (token counts are re-sent on rate-limit updates, aborts and resumes);
//! - `event_msg task_started` / `turn_started`, `task_complete` /
//!   `turn_complete` (with `error` for a failed turn), `turn_aborted`;
//! - `compacted` (`compaction_response_id` marks that response as compaction).
//!
//! A request's start is derived: the latest input-side line before its usage
//! record (turn start, a user/developer message, a tool output, a compaction),
//! its end is the record's own timestamp. First token, TPS, transport, the
//! upstream line and HTTP status are not in the rollout (`None`).
//!
//! Reading is incremental and bounded: files are visited only when modified in
//! the queried range, plain files are tailed from the last complete line (a
//! partial last line is left for the next read), lines longer than
//! [`MAX_LINE_BYTES`] are skipped without being buffered, and the index keeps at
//! most [`MAX_ROWS`] rows. Only ids, model names, token counts, timestamps and
//! error codes are kept: prompt and output text is never stored or logged.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::value::RawValue;
use superba_core::{CodexHome, RequestKind, RequestStatus, Timestamp};

use crate::rollout::{is_compressed, is_rollout_name};
use crate::util::parse_rfc3339_ms;

/// Longest rollout line that is parsed; longer lines (huge tool outputs,
/// compaction histories) are skipped without being buffered.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Most rows the index keeps (newest files first).
pub const MAX_ROWS: usize = 200_000;
/// Rollout row ids are `-2^61 ..= -1` (Claude Code transcript rows lie below,
/// imported rows below `-2^62`).
pub const ROLLOUT_ID_FLOOR: i64 = -(1 << 61);
const ROLLOUT_ID_MASK: u64 = 0x1fff_ffff_ffff_ffff;
/// Directory depth searched below `sessions/` (`YYYY/MM/DD/`).
const WALK_MAX_DEPTH: usize = 8;
/// A file whose modification time is this much older than the range start is
/// not read (its last line was written before the range began).
const MTIME_SLACK: Duration = Duration::from_secs(60);
/// Cache entries unused for this long are dropped.
const IDLE_EVICT: Duration = Duration::from_secs(15 * 60);
/// The directory walk is reused for this long between queries.
const WALK_EVERY: Duration = Duration::from_secs(2);

/// Token usage of one request (OpenAI semantics: `input` includes cached and
/// cache-write tokens, `output` includes reasoning).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RolloutUsage {
    /// `input_tokens`.
    pub input: u64,
    /// `cached_input_tokens`.
    pub cached: u64,
    /// `cache_write_input_tokens`.
    pub cache_write: u64,
    /// `output_tokens`.
    pub output: u64,
    /// `reasoning_output_tokens`.
    pub reasoning: u64,
}

/// One model request (or, where the rollout cannot separate requests, one failed
/// or cancelled turn) read from a rollout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutRequest {
    /// Stable id: negative, derived from the thread and the response (or turn)
    /// id, so it never collides with a router row id and survives re-reads.
    pub id: i64,
    /// Thread id (`session_meta.id`).
    pub thread_id: String,
    /// Turn id.
    pub turn_id: Option<String>,
    /// Responses API id (`token_usage_record` rows).
    pub response_id: Option<String>,
    /// Derived start of the request.
    pub started_at: Timestamp,
    /// When the usage record (or the failure) was written.
    pub completed_at: Timestamp,
    /// Model of the turn.
    pub model: Option<String>,
    /// Reasoning effort of the turn.
    pub effort: Option<String>,
    /// `service_tier` of the thread settings in effect.
    pub service_tier: Option<String>,
    /// Codex provider id of the thread (`openai`, a custom provider, ...).
    pub model_provider: Option<String>,
    /// `originator` of the thread (`Codex Desktop`, `codex_cli_rs`, `codex_exec`).
    pub originator: Option<String>,
    /// Codex version that wrote the thread.
    pub cli_version: Option<String>,
    /// `thread_source` (`user`, `subagent`, ...).
    pub thread_source: Option<String>,
    /// Turn or compaction.
    pub kind: RequestKind,
    /// Ok, failed turn or cancelled turn.
    pub status: RequestStatus,
    /// Usage (`None` for failed and cancelled rows).
    pub usage: Option<RolloutUsage>,
    /// The row stands for a whole turn (failed / cancelled turns, older Codex
    /// without per-response records), not one identified response.
    pub per_turn: bool,
}

impl RolloutRequest {
    /// `completed_at - started_at` in ms.
    pub fn duration_ms(&self) -> u64 {
        u64::try_from(self.completed_at.as_millis() - self.started_at.as_millis()).unwrap_or(0)
    }
}

/// Result of [`UsageIndex::requests`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RolloutPage {
    /// Rows in the range, oldest first.
    pub rows: Vec<RolloutRequest>,
    /// The row cap was reached: older files of the range were not read.
    pub truncated: bool,
    /// Rollout files read for the range.
    pub files: usize,
}

// ---------------------------------------------------------------------------
// Line reading
// ---------------------------------------------------------------------------

/// What [`read_lines`] stopped at.
#[derive(Debug, Default)]
pub(crate) struct ReadOutcome {
    /// Bytes consumed (up to and including the last complete line's `\n`).
    pub(crate) consumed: u64,
}

/// Feed every complete line of `reader` to `f` (without the `\n`, with a
/// trailing `\r` removed). A last line without `\n` is not consumed; lines
/// longer than [`MAX_LINE_BYTES`] are skipped without buffering them.
pub(crate) fn read_lines(
    reader: impl Read,
    mut f: impl FnMut(&[u8]),
) -> std::io::Result<ReadOutcome> {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    let mut consumed = 0u64;
    let mut pending = 0u64;
    loop {
        let buf = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            break;
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        let complete = done > chunk.len();
        if !skipping {
            if line.len() + chunk.len() > MAX_LINE_BYTES {
                skipping = true;
                line.clear();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        pending += done as u64;
        reader.consume(done);
        if complete {
            if !skipping {
                let l = line.strip_suffix(b"\r").unwrap_or(&line);
                if !l.is_empty() {
                    f(l);
                }
            }
            line.clear();
            skipping = false;
            consumed += pending;
            pending = 0;
        }
    }
    Ok(ReadOutcome { consumed })
}

// ---------------------------------------------------------------------------
// Wire shapes (only the fields used; everything else is skipped)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(borrow, default)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    kind: Cow<'a, str>,
    #[serde(borrow, default)]
    payload: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct TypeHead<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    role: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    cached_input_tokens: Option<i64>,
    #[serde(default)]
    cache_write_input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    reasoning_output_tokens: Option<i64>,
}

impl WireUsage {
    fn usage(&self) -> RolloutUsage {
        let n = |v: Option<i64>| v.and_then(|v| u64::try_from(v).ok()).unwrap_or(0);
        RolloutUsage {
            input: n(self.input_tokens),
            cached: n(self.cached_input_tokens),
            cache_write: n(self.cache_write_input_tokens),
            output: n(self.output_tokens),
            reasoning: n(self.reasoning_output_tokens),
        }
    }
}

#[derive(Deserialize)]
struct WireMeta {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model_provider: Option<String>,
    #[serde(default)]
    originator: Option<String>,
    #[serde(default)]
    cli_version: Option<String>,
    #[serde(default)]
    thread_source: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct WireCollabSettings {
    #[serde(default)]
    reasoning_effort: Option<String>,
}

#[derive(Deserialize)]
struct WireCollab {
    #[serde(default)]
    settings: Option<WireCollabSettings>,
}

#[derive(Deserialize)]
struct WireTurnContext {
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    collaboration_mode: Option<WireCollab>,
}

#[derive(Deserialize)]
struct WireRecord {
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireCompacted {
    #[serde(default)]
    compaction_response_id: Option<String>,
}

#[derive(Deserialize)]
struct WireTurnEvent {
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    codex_error_info: Option<serde_json::Value>,
    /// Only read for the refusal of a ciphertext another provider made
    /// ([`ENCRYPTED_CONTENT_CODE`]); never kept.
    #[serde(default)]
    message: Option<String>,
}

/// The code of a failed turn whose provider could not read content another
/// provider encrypted in the conversation (GitHub issue 11). Codex records it
/// as `other` with the upstream's answer as the message (`"The encrypted
/// content for item amsg_… could not be verified"`, `"Encrypted function output
/// content could not be decrypted or decoded."`).
pub const ENCRYPTED_CONTENT_CODE: &str = "invalid_encrypted_content";

/// The short code of a failed turn: [`ENCRYPTED_CONTENT_CODE`] when its
/// message says so, else the `codex_error_info` code.
fn failure_code(err: &WireError) -> Option<String> {
    let encrypted = err.message.as_deref().is_some_and(|m| {
        m.contains(ENCRYPTED_CONTENT_CODE)
            || (m.contains("The encrypted content ") && m.contains("could not be verified"))
            || m.contains("Encrypted function output content could not be decrypted")
    });
    if encrypted {
        return Some(ENCRYPTED_CONTENT_CODE.to_owned());
    }
    error_code(err.codex_error_info.as_ref())
}

#[derive(Deserialize)]
struct WireTurnComplete {
    #[serde(default)]
    turn_id: Option<String>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct WireTurnAborted {
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Deserialize)]
struct WireTokenInfo {
    #[serde(default)]
    total_token_usage: Option<WireUsage>,
    #[serde(default)]
    last_token_usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireTokenCount {
    #[serde(default)]
    info: Option<WireTokenInfo>,
}

#[derive(Deserialize)]
struct WireThreadSettings {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_provider_id: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
}

#[derive(Deserialize)]
struct WireSettingsApplied {
    #[serde(default)]
    thread_settings: Option<WireThreadSettings>,
}

/// The short code of a `codex_error_info` (`"usage_limit_exceeded"` or
/// `{"response_stream_disconnected": {...}}`).
fn error_code(info: Option<&serde_json::Value>) -> Option<String> {
    match info? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => m.keys().next().cloned(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// FNV-1a over the parts, folded into a negative id.
fn row_id(parts: &[&str]) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    -((h & ROLLOUT_ID_MASK) as i64) - 1
}

/// Current turn.
#[derive(Clone, Debug, Default)]
struct Turn {
    id: Option<String>,
    started_ms: i64,
    /// Rows written for this turn.
    rows: u32,
}

/// Parser state of one rollout (kept between incremental reads).
#[derive(Clone, Debug, Default)]
struct Parser {
    thread_id: Option<String>,
    model_provider: Option<String>,
    originator: Option<String>,
    cli_version: Option<String>,
    thread_source: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<String>,
    turn: Option<Turn>,
    /// Latest input-side line (the next request starts after it).
    boundary_ms: Option<i64>,
    /// First input-side line since the last output or row (a request is in
    /// flight when set).
    pending_ms: Option<i64>,
    /// End of the last row of the current turn.
    last_end_ms: Option<i64>,
    /// The file carries per-response `token_usage_record`s (token counts are
    /// then never rows).
    has_records: bool,
    /// Running total of the last `token_count` (fallback).
    last_total: Option<RolloutUsage>,
    /// Line timestamp of the previous line (lines without one inherit it).
    last_ts: i64,
}

fn add(a: RolloutUsage, b: RolloutUsage) -> RolloutUsage {
    RolloutUsage {
        input: a.input.saturating_add(b.input),
        cached: a.cached.saturating_add(b.cached),
        cache_write: a.cache_write.saturating_add(b.cache_write),
        output: a.output.saturating_add(b.output),
        reasoning: a.reasoning.saturating_add(b.reasoning),
    }
}

impl Parser {
    fn mark_input(&mut self, ts: i64) {
        self.boundary_ms = Some(ts);
        self.pending_ms.get_or_insert(ts);
    }

    fn start_of_request(&self) -> i64 {
        let turn = self.turn.as_ref().map(|t| t.started_ms);
        [turn, self.boundary_ms, self.last_end_ms]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(self.last_ts)
    }

    fn row(
        &mut self,
        ts: i64,
        started: i64,
        id_parts: &[&str],
        status: RequestStatus,
        usage: Option<RolloutUsage>,
        per_turn: bool,
    ) -> Option<RolloutRequest> {
        let thread_id = self.thread_id.clone()?;
        let mut parts = vec![thread_id.as_str()];
        parts.extend_from_slice(id_parts);
        let id = row_id(&parts);
        let turn_id = self.turn.as_ref().and_then(|t| t.id.clone());
        if let Some(t) = &mut self.turn {
            t.rows += 1;
        }
        self.last_end_ms = Some(ts);
        self.pending_ms = None;
        Some(RolloutRequest {
            id,
            thread_id,
            turn_id,
            response_id: None,
            started_at: Timestamp::from_millis(started.min(ts)),
            completed_at: Timestamp::from_millis(ts),
            model: self.model.clone(),
            effort: self.effort.clone(),
            service_tier: self.service_tier.clone(),
            model_provider: self.model_provider.clone(),
            originator: self.originator.clone(),
            cli_version: self.cli_version.clone(),
            thread_source: self.thread_source.clone(),
            kind: RequestKind::Turn,
            status,
            usage,
            per_turn,
        })
    }

    /// Parse one line; push the rows it completes.
    fn line(&mut self, raw: &[u8], out: &mut Vec<RolloutRequest>) {
        let Ok(line) = serde_json::from_slice::<Line<'_>>(raw) else {
            return;
        };
        let ts = line
            .timestamp
            .as_deref()
            .and_then(parse_rfc3339_ms)
            .unwrap_or(self.last_ts);
        self.last_ts = ts;
        let Some(payload) = line.payload else {
            return;
        };
        let p = payload.get().as_bytes();
        match line.kind.as_ref() {
            "session_meta" => {
                if self.thread_id.is_some() {
                    return;
                }
                if let Ok(m) = serde_json::from_slice::<WireMeta>(p) {
                    self.thread_id = m.id;
                    self.model_provider = m.model_provider;
                    self.originator = m.originator;
                    self.cli_version = m.cli_version;
                    self.thread_source = m.thread_source.and_then(|v| match v {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Object(m) => m.keys().next().cloned(),
                        _ => None,
                    });
                }
            }
            "turn_context" => {
                if let Ok(c) = serde_json::from_slice::<WireTurnContext>(p) {
                    if let Some(m) = c.model.filter(|m| !m.is_empty()) {
                        self.model = Some(m);
                    }
                    let effort = c.effort.or_else(|| {
                        c.collaboration_mode
                            .and_then(|m| m.settings)
                            .and_then(|s| s.reasoning_effort)
                    });
                    if effort.is_some() {
                        self.effort = effort;
                    }
                    if self.turn.is_none() {
                        self.turn = Some(Turn {
                            id: c.turn_id,
                            started_ms: ts,
                            rows: 0,
                        });
                    }
                }
                self.mark_input(ts);
            }
            "token_usage_record" => self.record(p, ts, out),
            "compacted" => {
                if let Ok(c) = serde_json::from_slice::<WireCompacted>(p)
                    && let Some(rid) = c.compaction_response_id
                    && let Some(row) = out
                        .iter_mut()
                        .rev()
                        .find(|r| r.response_id.as_deref() == Some(rid.as_str()))
                {
                    row.kind = RequestKind::Compaction;
                }
                self.mark_input(ts);
            }
            "response_item" => {
                let Ok(h) = serde_json::from_slice::<TypeHead<'_>>(p) else {
                    return;
                };
                match (h.kind.as_deref(), h.role.as_deref()) {
                    (Some("message"), Some("assistant")) => self.pending_ms = None,
                    // Context and notices Codex adds (also when a turn is
                    // stopped or fails) are not what a request waits for.
                    (Some("message"), Some("developer")) => {}
                    (Some("message"), _)
                    | (
                        Some(
                            "function_call_output"
                            | "custom_tool_call_output"
                            | "tool_search_output"
                            | "local_shell_call_output"
                            | "compaction"
                            | "context_compaction",
                        ),
                        _,
                    ) => self.mark_input(ts),
                    // Model output (reasoning, tool calls, web search, ...).
                    _ => self.pending_ms = None,
                }
            }
            "event_msg" => self.event(p, ts, out),
            _ => {}
        }
    }

    fn record(&mut self, p: &[u8], ts: i64, out: &mut Vec<RolloutRequest>) {
        let Ok(r) = serde_json::from_slice::<WireRecord>(p) else {
            return;
        };
        // The file comes from a Codex that writes records (also when this one
        // was copied from a parent thread, with its token counts).
        self.has_records = true;
        // Records copied from a parent thread keep their owner's id.
        if r.thread_id.is_some() && r.thread_id != self.thread_id {
            return;
        }
        let Some(usage) = r.usage.as_ref().map(WireUsage::usage) else {
            return;
        };
        if let Some(rid) = &r.response_id
            && out
                .iter()
                .any(|x| x.response_id.as_deref() == Some(rid.as_str()))
        {
            return;
        }
        if let Some(tid) = &r.turn_id {
            match &mut self.turn {
                // A turn start without an id (older Codex) learns it here.
                Some(t) if t.id.is_none() => t.id = Some(tid.clone()),
                Some(t) if t.id.as_ref() == Some(tid) => {}
                // A record of a turn whose start was not seen.
                _ => {
                    self.turn = Some(Turn {
                        id: Some(tid.clone()),
                        started_ms: self.boundary_ms.unwrap_or(ts),
                        rows: 0,
                    });
                    self.last_end_ms = None;
                }
            }
        }
        let started = self.start_of_request();
        let key = r
            .response_id
            .clone()
            .unwrap_or_else(|| format!("{}#{}", r.turn_id.as_deref().unwrap_or(""), ts));
        if let Some(mut row) = self.row(
            ts,
            started,
            &["resp", &key],
            RequestStatus::Ok,
            Some(usage),
            false,
        ) {
            row.response_id = r.response_id;
            out.push(row);
        }
    }

    fn event(&mut self, p: &[u8], ts: i64, out: &mut Vec<RolloutRequest>) {
        let Ok(h) = serde_json::from_slice::<TypeHead<'_>>(p) else {
            return;
        };
        match h.kind.as_deref().unwrap_or("") {
            "task_started" | "turn_started" => {
                let id = serde_json::from_slice::<WireTurnEvent>(p)
                    .ok()
                    .and_then(|e| e.turn_id);
                self.turn = Some(Turn {
                    id,
                    started_ms: ts,
                    rows: 0,
                });
                self.boundary_ms = Some(ts);
                self.pending_ms = Some(ts);
                self.last_end_ms = None;
            }
            "task_complete" | "turn_complete" => {
                let e = serde_json::from_slice::<WireTurnComplete>(p).ok();
                if let Some(err) = e.as_ref().and_then(|e| e.error.as_ref()) {
                    let code = failure_code(err);
                    let started = self.start_of_request();
                    let turn = self.turn_key(e.as_ref().and_then(|e| e.turn_id.as_deref()), ts);
                    if let Some(row) = self.row(
                        ts,
                        started,
                        &["failed", &turn],
                        RequestStatus::Failed { code },
                        None,
                        true,
                    ) {
                        out.push(row);
                    }
                }
                self.end_turn();
            }
            "turn_aborted" => {
                let e = serde_json::from_slice::<WireTurnAborted>(p).ok();
                if let Some(pending) = self.pending_ms {
                    let started = self.boundary_ms.map_or(pending, |b| b.max(pending));
                    let turn = self.turn_key(e.as_ref().and_then(|e| e.turn_id.as_deref()), ts);
                    if let Some(row) = self.row(
                        ts,
                        started,
                        &["cancelled", &turn],
                        RequestStatus::Cancelled,
                        None,
                        true,
                    ) {
                        out.push(row);
                    }
                }
                self.end_turn();
            }
            "token_count" => self.token_count(p, ts, out),
            "thread_settings_applied" => {
                if let Ok(s) = serde_json::from_slice::<WireSettingsApplied>(p)
                    && let Some(s) = s.thread_settings
                {
                    if let Some(m) = s.model.filter(|m| !m.is_empty()) {
                        self.model = Some(m);
                    }
                    if s.model_provider_id.is_some() {
                        self.model_provider = s.model_provider_id;
                    }
                    if s.service_tier.is_some() {
                        self.service_tier = s.service_tier;
                    }
                }
            }
            "context_compacted" | "user_message" => self.mark_input(ts),
            "agent_message" | "agent_reasoning" => self.pending_ms = None,
            _ => {}
        }
    }

    fn turn_key(&self, turn_id: Option<&str>, ts: i64) -> String {
        turn_id
            .map(str::to_owned)
            .or_else(|| self.turn.as_ref().and_then(|t| t.id.clone()))
            .unwrap_or_else(|| ts.to_string())
    }

    fn end_turn(&mut self) {
        self.turn = None;
        self.pending_ms = None;
        self.last_end_ms = None;
    }

    fn token_count(&mut self, p: &[u8], ts: i64, out: &mut Vec<RolloutRequest>) {
        let Ok(t) = serde_json::from_slice::<WireTokenCount>(p) else {
            return;
        };
        let Some(info) = t.info else {
            return;
        };
        let total = info.total_token_usage.as_ref().map(WireUsage::usage);
        let last = info.last_token_usage.as_ref().map(WireUsage::usage);
        let prev = self.last_total;
        if total.is_some() {
            self.last_total = total;
        }
        if self.has_records {
            return;
        }
        let (Some(total), Some(last)) = (total, last) else {
            return;
        };
        if last.input == 0 && last.output == 0 {
            return;
        }
        let grew = match prev {
            // The total must have grown by exactly this request.
            Some(prev) => prev != total && add(prev, last) == total,
            None => true,
        };
        if !grew {
            return;
        }
        let started = self.start_of_request();
        let turn = self.turn_key(None, ts);
        let seq = self.turn.as_ref().map_or(0, |t| t.rows).to_string();
        if let Some(row) = self.row(
            ts,
            started,
            &["count", &turn, &seq],
            RequestStatus::Ok,
            Some(last),
            true,
        ) {
            out.push(row);
        }
    }
}

/// Parse a whole rollout from bytes (tests, fixtures).
pub fn parse_rollout(bytes: &[u8]) -> Vec<RolloutRequest> {
    let mut parser = Parser::default();
    let mut out = Vec::new();
    let _ = read_lines(bytes, |l| parser.line(l, &mut out));
    out
}

// ---------------------------------------------------------------------------
// Index
// ---------------------------------------------------------------------------

/// What is cached for one rollout file.
#[derive(Debug)]
struct FileState {
    len: u64,
    mtime: Option<SystemTime>,
    /// Bytes parsed (plain files; at a line boundary).
    offset: u64,
    compressed: bool,
    parser: Parser,
    rows: Vec<RolloutRequest>,
    used: Instant,
}

/// A rollout file found by the walk.
#[derive(Clone, Debug)]
struct Found {
    path: PathBuf,
    len: u64,
    mtime: Option<SystemTime>,
}

/// Incremental reader of every rollout of one Codex home. Not thread-safe;
/// callers keep it behind a mutex and call it from a blocking thread.
#[derive(Debug)]
pub struct UsageIndex {
    home: CodexHome,
    files: HashMap<PathBuf, FileState>,
    walked: Option<(Instant, Vec<Found>)>,
}

impl UsageIndex {
    /// An empty index of `home`.
    pub fn new(home: CodexHome) -> Self {
        Self {
            home,
            files: HashMap::new(),
            walked: None,
        }
    }

    /// The Codex home.
    pub fn home(&self) -> &CodexHome {
        &self.home
    }

    fn walk(&mut self) -> Vec<Found> {
        if let Some((at, found)) = &self.walked
            && at.elapsed() < WALK_EVERY
        {
            return found.clone();
        }
        let mut found = Vec::new();
        for root in [self.home.sessions_dir(), self.home.archived_sessions_dir()] {
            if !root.is_dir() {
                continue;
            }
            for entry in walkdir::WalkDir::new(&root)
                .follow_links(false)
                .max_depth(WALK_MAX_DEPTH)
                .into_iter()
                .filter_map(std::result::Result::ok)
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str() else {
                    continue;
                };
                if !is_rollout_name(name) {
                    continue;
                }
                // On Windows the walk's metadata comes from the directory listing,
                // whose size/mtime lag behind while Codex still has the rollout
                // open for appending; stat the file itself there.
                let meta = if cfg!(windows) {
                    std::fs::metadata(entry.path()).ok()
                } else {
                    entry.metadata().ok()
                };
                let Some(meta) = meta else {
                    continue;
                };
                found.push(Found {
                    path: entry.path().to_path_buf(),
                    len: meta.len(),
                    mtime: meta.modified().ok(),
                });
            }
        }
        // Newest first: the row cap keeps the most recent files.
        found.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
        self.walked = Some((Instant::now(), found.clone()));
        found
    }

    /// Bring one file's cache entry up to date.
    fn refresh_file(&mut self, f: &Found) {
        let compressed = is_compressed(&f.path);
        let fresh = match self.files.get(&f.path) {
            Some(s) => {
                s.compressed != compressed
                    || f.len < s.len
                    || (compressed && (f.len != s.len || f.mtime != s.mtime))
            }
            None => true,
        };
        if fresh {
            self.files.insert(
                f.path.clone(),
                FileState {
                    len: 0,
                    mtime: None,
                    offset: 0,
                    compressed,
                    parser: Parser::default(),
                    rows: Vec::new(),
                    used: Instant::now(),
                },
            );
        }
        let Some(state) = self.files.get_mut(&f.path) else {
            return;
        };
        state.used = Instant::now();
        if !fresh && state.len == f.len && state.mtime == f.mtime {
            return;
        }
        let result = if compressed {
            read_compressed(&f.path, state)
        } else {
            read_plain(&f.path, state)
        };
        if result.is_err() {
            // Unreadable now (rotated, locked): try again from scratch next time.
            self.files.remove(&f.path);
            return;
        }
        state.len = f.len;
        state.mtime = f.mtime;
    }

    /// Rows started in `[from, to)` (both optional), refreshing the files
    /// modified in that range first. Rows are deduplicated by id (a rollout that
    /// was compressed in between appears once) and sorted oldest first.
    pub fn requests(&mut self, from: Option<Timestamp>, to: Option<Timestamp>) -> RolloutPage {
        let found = self.walk();
        let floor = from.map(|f| {
            f.to_system_time()
                .checked_sub(MTIME_SLACK)
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
        let mut page = RolloutPage::default();
        let mut rows: BTreeMap<i64, RolloutRequest> = BTreeMap::new();
        let mut total = 0usize;
        for f in &found {
            if let (Some(floor), Some(m)) = (floor, f.mtime)
                && m < floor
            {
                continue;
            }
            if total >= MAX_ROWS {
                page.truncated = true;
                break;
            }
            self.refresh_file(f);
            let Some(state) = self.files.get(&f.path) else {
                continue;
            };
            page.files += 1;
            total += state.rows.len();
            for r in &state.rows {
                let in_range =
                    from.is_none_or(|t| r.started_at >= t) && to.is_none_or(|t| r.started_at < t);
                if in_range {
                    rows.entry(r.id).or_insert_with(|| r.clone());
                }
            }
        }
        // Forget files that are gone or idle (bounded memory).
        let live: std::collections::HashSet<&PathBuf> = found.iter().map(|f| &f.path).collect();
        self.files
            .retain(|p, s| live.contains(p) && s.used.elapsed() < IDLE_EVICT);
        let mut rows: Vec<RolloutRequest> = rows.into_values().collect();
        rows.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        page.rows = rows;
        page
    }

    /// The row with `id` among the cached files, if any.
    pub fn row(&self, id: i64) -> Option<RolloutRequest> {
        self.files
            .values()
            .flat_map(|s| s.rows.iter())
            .find(|r| r.id == id)
            .cloned()
    }
}

fn read_plain(path: &Path, state: &mut FileState) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(state.offset))?;
    let parser = &mut state.parser;
    let rows = &mut state.rows;
    let outcome = read_lines(file, |l| parser.line(l, rows))?;
    state.offset += outcome.consumed;
    Ok(())
}

fn read_compressed(path: &Path, state: &mut FileState) -> std::io::Result<()> {
    let file = File::open(path)?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    state.parser = Parser::default();
    state.rows.clear();
    let parser = &mut state.parser;
    let rows = &mut state.rows;
    read_lines(decoder, |l| parser.line(l, rows))?;
    state.offset = 0;
    Ok(())
}
