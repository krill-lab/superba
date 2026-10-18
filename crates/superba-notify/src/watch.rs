//! Failed Codex turns, which no Codex hook reports: `notify` and the `Stop`
//! hook run only for turns that finished (checked with Codex 0.163: a turn
//! whose requests fail ends with `task_complete` carrying `error` in its
//! rollout and runs neither). While Superba runs, this scanner reads what was
//! appended to recent rollouts since the last scan and reports each such turn.
//!
//! Bookmarks (rollout path -> bytes read) live in the policy state, so a turn
//! is reported once across restarts and processes; the first scan of a file
//! Superba has never seen starts at its end unless the file is new (created
//! after the previous scan), and turns that ended more than
//! [`MAX_AGE_MS`] ago are not reported.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use superba_core::notify::harness;
use superba_core::{EventKind, NotifyEvent, Timestamp};

use crate::enrich::{self, parse_rfc3339};
use crate::policy::PolicyState;

/// Rollouts modified within this window are read.
pub const RECENT: Duration = Duration::from_secs(30 * 60);
/// Failures older than this are not reported.
pub const MAX_AGE_MS: i64 = 30 * 60 * 1000;

/// Scan `sessions_dir` and return the failed turns found since the last scan.
pub fn scan(sessions_dir: &Path, state: &mut PolicyState, now: Timestamp) -> Vec<NotifyEvent> {
    let first_scan = state.watch_scanned_ms.is_none();
    let last_scan_ms = state.watch_scanned_ms.unwrap_or(0);
    state.watch_scanned_ms = Some(now.as_millis());
    let mut events = Vec::new();
    for path in recent_rollouts(sessions_dir, now) {
        let key = path.to_string_lossy().into_owned();
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let bookmark = state.watch.get(&key).copied();
        let created_ms = std::fs::metadata(&path)
            .and_then(|m| m.created().or_else(|_| m.modified()))
            .map(|t| Timestamp::from_system_time(t).as_millis())
            .unwrap_or(0);
        let offset = match bookmark {
            Some(o) if o <= len => o,
            // Truncated / replaced: start over.
            Some(_) => 0,
            None if first_scan || created_ms < last_scan_ms => {
                state.watch.insert(key, len);
                continue;
            }
            None => 0,
        };
        if offset == len {
            continue;
        }
        let Ok((text, end)) = enrich::read_from(&path, Some(offset)) else {
            continue;
        };
        state.watch.insert(key, end);
        events.extend(failed_turns(&path, &text, now));
    }
    events
}

fn recent_rollouts(sessions_dir: &Path, now: Timestamp) -> Vec<std::path::PathBuf> {
    let cutoff = now
        .to_system_time()
        .checked_sub(RECENT)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut out = Vec::new();
    let mut stack = vec![(sessions_dir.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                // YYYY/MM/DD: only the newest directories can hold recent files,
                // but the modification time of a directory is reliable enough
                // only for the day level; walk all three levels.
                if depth < 3 && meta.modified().map(|m| m >= cutoff).unwrap_or(true) {
                    stack.push((path, depth + 1));
                }
            } else if meta.modified().map(|m| m >= cutoff).unwrap_or(false)
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Turns of one rollout chunk that ended with an error.
pub fn failed_turns(path: &Path, text: &str, now: Timestamp) -> Vec<NotifyEvent> {
    let thread_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(thread_id_from_stem);
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(p) = v.get("payload") else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("event_msg")
            || !matches!(
                p.get("type").and_then(Value::as_str),
                Some("task_complete" | "turn_complete")
            )
        {
            continue;
        }
        let Some(error) = p.get("error").filter(|e| !e.is_null()) else {
            continue;
        };
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339)
            .unwrap_or(now);
        if now.as_millis() - at.as_millis() > MAX_AGE_MS {
            continue;
        }
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| error.as_str().map(str::to_owned))
            .unwrap_or_else(|| error.to_string());
        let mut e = NotifyEvent::new(EventKind::TurnFailed, harness::CODEX, at);
        e.thread_id = thread_id.clone();
        e.turn_id = p.get("turn_id").and_then(Value::as_str).map(str::to_owned);
        e.detail = Some(message);
        e.duration_ms = p.get("duration_ms").and_then(Value::as_u64);
        out.push(e);
    }
    out
}

/// `rollout-2026-10-18T15-00-41-<uuid>` -> the uuid (the last five
/// dash-separated groups).
pub fn thread_id_from_stem(stem: &str) -> Option<String> {
    let parts: Vec<&str> = stem.rsplitn(6, '-').collect();
    if parts.len() < 6 {
        return None;
    }
    let mut id: Vec<&str> = parts[..5].to_vec();
    id.reverse();
    let id = id.join("-");
    (id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then_some(id)
}
