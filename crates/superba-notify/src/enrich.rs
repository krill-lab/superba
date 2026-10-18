//! What a harness's own files add to an event: how long the turn ran, its
//! tokens and model (Codex rollouts, Claude Code transcripts), and the
//! machine's name.
//!
//! - **Codex** rollout `sessions/YYYY/MM/DD/rollout-<ts>-<thread id>.jsonl`:
//!   `task_started` / `task_complete` (`turn_id`, `duration_ms` since 0.16x,
//!   else the line timestamps; `error` when the turn failed), `turn_context`
//!   (`model`), `token_count` (`info.total_token_usage`: the turn's tokens are
//!   the difference between the last count before the turn and the last one
//!   in it). Codex runs `notify` right when the turn ends, possibly before the
//!   rollout has its `task_complete`: the reader is called again after a
//!   short wait when the end is not there yet.
//! - **Claude Code** transcript (`transcript_path`, JSON lines): the turn
//!   starts at the last `user` line that is a prompt (not a tool result);
//!   assistant lines carry `message.model` and `message.usage`
//!   (`input_tokens` + `cache_read_input_tokens` +
//!   `cache_creation_input_tokens` = OpenAI-style input).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use superba_core::{Timestamp, TurnTokens};

/// Bytes read from the end of a rollout / transcript.
pub const TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// What a rollout says about one turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnFacts {
    /// The turn's start was found.
    pub started: bool,
    /// The turn's end was found.
    pub ended: bool,
    /// Duration.
    pub duration_ms: Option<u64>,
    /// Tokens.
    pub tokens: Option<TurnTokens>,
    /// Model.
    pub model: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// The error the turn ended with.
    pub error: Option<String>,
    /// Last assistant message.
    pub last_message: Option<String>,
    /// Turn id (the last turn when none was asked for).
    pub turn_id: Option<String>,
}

/// The machine's name (`None` when unknown).
pub fn host_name() -> Option<String> {
    sysinfo::System::host_name()
        .map(|h| h.trim().trim_end_matches(".local").to_owned())
        .filter(|h| !h.is_empty())
}

/// Read up to [`TAIL_BYTES`] from the end of `path`, starting at a line.
pub fn read_tail(path: &Path) -> std::io::Result<String> {
    read_from(path, None).map(|(s, _)| s)
}

/// Read `path` from `offset` (or the last [`TAIL_BYTES`]); returns the text
/// (whole lines only) and the offset after the last complete line.
pub fn read_from(path: &Path, offset: Option<u64>) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = match offset {
        Some(o) if o <= len => o,
        _ => len.saturating_sub(TAIL_BYTES),
    };
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.take(len - start).read_to_end(&mut buf)?;
    // Skip a partial first line when we started in the middle of the file.
    let mut begin = 0;
    if start > 0 && offset.is_none() {
        begin = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |p| p + 1);
    }
    // Keep complete lines only (a writer may be mid-line).
    let end = buf
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(begin, |p| p + 1)
        .max(begin);
    let text = String::from_utf8_lossy(&buf[begin..end]).into_owned();
    Ok((text, start + end as u64))
}

/// Find the rollout of `thread_id` under `sessions_dir` (newest day first,
/// at most `days` day directories).
pub fn find_rollout(sessions_dir: &Path, thread_id: &str, days: usize) -> Option<PathBuf> {
    if thread_id.is_empty() || !thread_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    let suffix = format!("-{thread_id}.jsonl");
    let mut day_dirs = Vec::new();
    for year in sorted_dirs(sessions_dir).into_iter().rev() {
        for month in sorted_dirs(&year).into_iter().rev() {
            for day in sorted_dirs(&month).into_iter().rev() {
                day_dirs.push(day);
                if day_dirs.len() >= days {
                    break;
                }
            }
            if day_dirs.len() >= days {
                break;
            }
        }
        if day_dirs.len() >= days {
            break;
        }
    }
    for dir in day_dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("rollout-") && name.ends_with(&suffix) {
                return Some(entry.path());
            }
        }
    }
    None
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn turn_id_of(p: &Value) -> Option<&str> {
    p.get("turn_id").and_then(Value::as_str).or_else(|| {
        p.get("turn_attribution")
            .and_then(|a| a.get("turn_id"))
            .and_then(Value::as_str)
    })
}

fn usage_of(info: &Value) -> Option<TurnTokens> {
    let t = info.get("total_token_usage")?;
    let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(TurnTokens {
        input: n("input_tokens"),
        cached: n("cached_input_tokens"),
        output: n("output_tokens"),
    })
}

/// Facts about `turn_id` (or the last turn) in a Codex rollout's text.
pub fn codex_turn(text: &str, turn_id: Option<&str>) -> TurnFacts {
    let mut facts = TurnFacts::default();
    let mut in_turn = false;
    let mut start_at: Option<Timestamp> = None;
    let mut before: Option<TurnTokens> = None;
    let mut last_total: Option<TurnTokens> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        let Some(p) = v.get("payload") else {
            continue;
        };
        let line_type = v.get("type").and_then(Value::as_str).unwrap_or("");
        let event = p.get("type").and_then(Value::as_str).unwrap_or("");
        match (line_type, event) {
            ("event_msg", "task_started" | "turn_started") => {
                let id = turn_id_of(p);
                if turn_id.is_none() || id == turn_id {
                    facts = TurnFacts {
                        started: true,
                        turn_id: id.map(str::to_owned),
                        ..TurnFacts::default()
                    };
                    in_turn = true;
                    start_at = at;
                    before = last_total;
                } else {
                    in_turn = false;
                }
            }
            ("event_msg", "task_complete" | "turn_complete") if in_turn => {
                let id = turn_id_of(p);
                if id.is_some() && facts.turn_id.is_some() && id != facts.turn_id.as_deref() {
                    continue;
                }
                facts.ended = true;
                facts.duration_ms =
                    p.get("duration_ms")
                        .and_then(Value::as_u64)
                        .or_else(|| match (start_at, at) {
                            (Some(s), Some(e)) => u64::try_from(e.as_millis() - s.as_millis()).ok(),
                            _ => None,
                        });
                facts.error = p.get("error").and_then(|e| {
                    e.get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| e.as_str().map(str::to_owned))
                });
                if let Some(m) = p.get("last_agent_message").and_then(Value::as_str) {
                    facts.last_message = Some(m.to_owned());
                }
                in_turn = false;
            }
            ("event_msg", "turn_aborted") if in_turn => {
                facts.ended = true;
                in_turn = false;
            }
            ("event_msg", "token_count") => {
                if let Some(total) = p.get("info").and_then(usage_of) {
                    last_total = Some(total);
                    if in_turn || (facts.started && facts.ended && facts.tokens.is_none()) {
                        let b = before.unwrap_or_default();
                        facts.tokens = Some(TurnTokens {
                            input: total.input.saturating_sub(b.input),
                            cached: total.cached.saturating_sub(b.cached),
                            output: total.output.saturating_sub(b.output),
                        });
                    }
                }
            }
            ("turn_context", _) if in_turn || !facts.started => {
                let id = p.get("turn_id").and_then(Value::as_str);
                if facts.turn_id.is_none() || id.is_none() || id == facts.turn_id.as_deref() {
                    if let Some(m) = p.get("model").and_then(Value::as_str) {
                        facts.model = Some(m.to_owned());
                    }
                    if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                        facts.cwd = Some(c.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    facts
}

/// Facts about the last turn of a Claude Code transcript; `now` ends a turn
/// that has no end line (the Stop hook runs right at the end).
pub fn claude_turn(text: &str, now: Timestamp) -> TurnFacts {
    let mut facts = TurnFacts::default();
    let mut start_at: Option<Timestamp> = None;
    let mut last_at: Option<Timestamp> = None;
    let mut tokens = TurnTokens::default();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        match v.get("type").and_then(Value::as_str) {
            Some("user") if is_prompt(&v) => {
                facts = TurnFacts {
                    started: true,
                    turn_id: v.get("uuid").and_then(Value::as_str).map(str::to_owned),
                    cwd: v.get("cwd").and_then(Value::as_str).map(str::to_owned),
                    ..TurnFacts::default()
                };
                start_at = at;
                tokens = TurnTokens::default();
            }
            Some("assistant") if facts.started => {
                let msg = v.get("message");
                if let Some(m) = msg.and_then(|m| m.get("model")).and_then(Value::as_str)
                    && !m.starts_with('<')
                {
                    facts.model = Some(m.to_owned());
                }
                if let Some(u) = msg.and_then(|m| m.get("usage")) {
                    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let cached = n("cache_read_input_tokens");
                    tokens.input += n("input_tokens") + cached + n("cache_creation_input_tokens");
                    tokens.cached += cached;
                    tokens.output += n("output_tokens");
                }
                last_at = at.or(last_at);
            }
            _ => {}
        }
    }
    if facts.started {
        if tokens.input + tokens.output > 0 {
            facts.tokens = Some(tokens);
        }
        let end = if now.as_millis() > 0 {
            Some(now)
        } else {
            last_at
        };
        if let (Some(s), Some(e)) = (start_at, end) {
            facts.duration_ms = u64::try_from(e.as_millis() - s.as_millis()).ok();
        }
    }
    facts
}

/// A `user` line that is the user's prompt (not a tool result, not a
/// meta / command line).
fn is_prompt(v: &Value) -> bool {
    if v.get("isMeta").and_then(Value::as_bool) == Some(true)
        || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return false;
    }
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(_)) => true,
        Some(Value::Array(parts)) => parts
            .iter()
            .any(|p| p.get("type").and_then(Value::as_str) == Some("text")),
        _ => false,
    }
}

/// Parse `2026-10-18T15:00:41.974Z` (also with an offset) to a [`Timestamp`].
pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = &s[19..];
    let mut ms = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        rest = &frac[digits.len()..];
        let mut padded = digits.chars().take(3).collect::<String>();
        while padded.len() < 3 {
            padded.push('0');
        }
        ms = padded.parse().ok()?;
    }
    let offset_min = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (o.get(1..3)?.parse::<i64>().ok()? * 60 + o.get(4..6)?.parse::<i64>().ok()?)
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_min * 60;
    Some(Timestamp::from_millis(secs * 1000 + ms))
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
