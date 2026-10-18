//! Turn activity from rollout files (plan 7.4 signals A and B).
//!
//! Signal A (tail): Codex appends `event_msg` lines to
//! `sessions/YYYY/MM/DD/rollout-<ts>-<thread id>.jsonl`; a turn starts with
//! `task_started` (alias `turn_started`) and ends with `task_complete`
//! (`turn_complete`) or `turn_aborted`. A started turn without an end is in
//! flight — unless no live Codex process started before it: an app-server killed
//! by a restart never writes the end event, and turns do not resume across
//! processes (verified, Codex 0.162).
//!
//! Signal B (mtime): written within the last 20 s, used only when the tail holds
//! no turn events at all (inconclusive), since post-turn bookkeeping
//! (`thread_settings_applied`, `token_count`) also touches the file.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use superba_core::{ActivitySignal, TargetId, ThreadActivity, Timestamp};

/// Event types that start a turn.
pub const TURN_START_EVENTS: &[&str] = &["task_started", "turn_started"];
/// Event types that end a turn.
pub const TURN_END_EVENTS: &[&str] = &["task_complete", "turn_complete", "turn_aborted"];

/// Tunables of the rollout scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanOpts {
    /// Only rollouts modified within this window are read (plan: "recent N minutes").
    pub tail_window: Duration,
    /// Signal B window.
    pub mtime_window: Duration,
    /// Bytes read from the end of each rollout.
    pub tail_bytes: u64,
    /// "Now".
    pub now: Timestamp,
}

impl ScanOpts {
    /// Defaults: 30 min tail window, 20 s mtime window, 512 KiB tails.
    pub fn at(now: Timestamp) -> Self {
        Self {
            tail_window: Duration::from_secs(30 * 60),
            mtime_window: Duration::from_secs(20),
            tail_bytes: 512 * 1024,
            now,
        }
    }
}

/// What the tail of one rollout says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TailAnalysis {
    /// Turn events were found (start or end).
    pub conclusive: bool,
    /// Start time of the earliest still-open turn.
    pub open_turn_started_at: Option<Timestamp>,
    /// Id of that turn.
    pub open_turn_id: Option<String>,
    /// Timestamp of the last parsable line.
    pub last_event_at: Option<Timestamp>,
}

/// One recently written rollout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutFinding {
    /// File.
    pub path: PathBuf,
    /// Thread id from the file name.
    pub thread_id: Option<String>,
    /// Modification time.
    pub mtime: Timestamp,
    /// Tail analysis.
    pub tail: TailAnalysis,
    /// Session header facts (which client runs the session, where).
    pub head: SessionHead,
}

/// Facts from a rollout's first line (`session_meta`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionHead {
    /// `originator` (`Codex Desktop`, `codex_cli_rs`, `codex_exec`,
    /// `codex_vscode`, ...).
    pub originator: Option<String>,
    /// `cwd`.
    pub cwd: Option<String>,
}

/// Bytes read from the start of a rollout for its header. The header line
/// carries the base instructions (tens of KiB) after `cwd` and `originator`,
/// so only its beginning is needed.
pub const HEAD_BYTES: usize = 16 * 1024;

/// The JSON string value of the first `"<field>":"…"` in `text` (escapes
/// decoded); `text` may end in the middle of the line.
fn json_string_field(text: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let start = text.find(&needle)? + needle.len();
    let rest = text[start..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return serde_json::from_str::<String>(&rest[..=i]).ok(),
            _ => i += 1,
        }
    }
    None
}

/// Parse the session header from the first bytes of a rollout.
pub fn parse_session_head(head: &str) -> SessionHead {
    let line = head.lines().next().unwrap_or_default();
    if !line.contains("\"session_meta\"") {
        return SessionHead::default();
    }
    SessionHead {
        originator: json_string_field(line, "originator"),
        cwd: json_string_field(line, "cwd"),
    }
}

fn read_head(path: &Path) -> SessionHead {
    let Ok(mut f) = std::fs::File::open(path) else {
        return SessionHead::default();
    };
    let mut buf = vec![0u8; HEAD_BYTES];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return SessionHead::default(),
        }
    }
    parse_session_head(&String::from_utf8_lossy(&buf[..n]))
}

/// Parse an RFC 3339 timestamp (`2026-10-16T07:39:40.633Z`, offsets allowed).
pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut d = date.splitn(3, '-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let (time, offset_secs) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0i64)
    } else if let Some(pos) = rest.rfind(['+', '-']) {
        let (t, off) = rest.split_at(pos);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        (
            t,
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60),
        )
    } else {
        (rest, 0)
    };
    let mut t = time.splitn(3, ':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec_str = t.next()?;
    let (sec, frac) = sec_str.split_once('.').unwrap_or((sec_str, ""));
    let sec: i64 = sec.parse().ok()?;
    let ms: i64 = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().take(3).collect();
        let n: i64 = digits.parse().ok()?;
        n * 10i64.pow(3 - u32::try_from(digits.len()).ok()?)
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset_secs;
    Some(Timestamp::from_millis(secs * 1000 + ms))
}

/// Analyse rollout lines (a tail may start with a partial line; it is skipped).
pub fn analyze_tail(text: &str) -> TailAnalysis {
    let mut out = TailAnalysis::default();
    // Open turns: (turn id, started at).
    let mut open: Vec<(Option<String>, Option<Timestamp>)> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        if at.is_some() {
            out.last_event_at = at;
        }
        if v.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let Some(payload) = v.get("payload") else {
            continue;
        };
        let Some(kind) = payload.get("type").and_then(Value::as_str) else {
            continue;
        };
        let turn_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if TURN_START_EVENTS.contains(&kind) {
            out.conclusive = true;
            open.push((turn_id, at));
        } else if TURN_END_EVENTS.contains(&kind) {
            out.conclusive = true;
            match &turn_id {
                Some(id) if open.iter().any(|(o, _)| o.as_deref() == Some(id)) => {
                    open.retain(|(o, _)| o.as_deref() != Some(id));
                }
                // An end without a matching start closes everything before it.
                _ => open.clear(),
            }
        }
    }
    if let Some((id, at)) = open.into_iter().next() {
        out.open_turn_id = id;
        out.open_turn_started_at = at.or(out.last_event_at);
    }
    out
}

/// Thread id from `rollout-<timestamp>-<uuid>.jsonl[.zst]`.
pub fn thread_id_from_file_name(name: &str) -> Option<String> {
    let stem = name.strip_prefix("rollout-")?;
    let stem = stem
        .strip_suffix(".jsonl.zst")
        .or_else(|| stem.strip_suffix(".jsonl"))?;
    if stem.len() < 36 {
        return None;
    }
    let id = &stem[stem.len() - 36..];
    let ok = id.chars().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    });
    ok.then(|| id.to_owned())
}

fn read_tail(path: &Path, bytes: u64) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(bytes);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity(usize::try_from(len - start).unwrap_or(0));
    f.read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        // Drop the partial first line.
        text = text
            .split_once('\n')
            .map(|(_, r)| r.to_owned())
            .unwrap_or_default();
    }
    Ok(text)
}

/// Scan `sessions_dir` for plain `rollout-*.jsonl` files written within the tail
/// window and analyse their tails. A missing directory yields no findings.
pub fn scan_rollouts(sessions_dir: &Path, opts: &ScanOpts) -> std::io::Result<Vec<RolloutFinding>> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }
    let oldest = opts.now.saturating_sub(opts.tail_window);
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(sessions_dir)
        .max_depth(5)
        .follow_links(false)
    {
        let entry = match entry {
            Ok(e) => e,
            Err(e) if e.depth() == 0 => return Err(std::io::Error::other(e.to_string())),
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let mtime = Timestamp::from_system_time(modified);
        if mtime < oldest {
            continue;
        }
        let tail = match read_tail(entry.path(), opts.tail_bytes) {
            Ok(t) => analyze_tail(&t),
            Err(_) => TailAnalysis::default(),
        };
        let head = read_head(entry.path());
        out.push(RolloutFinding {
            path: entry.path().to_path_buf(),
            thread_id: thread_id_from_file_name(&name),
            mtime,
            tail,
            head,
        });
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.mtime));
    Ok(out)
}

/// Busy threads from findings. `live_starts` are start times of live Codex
/// consumer processes of the home (`None` = unknown start time, treated as old);
/// with no live consumer nothing can be in flight.
pub fn busy_threads(
    findings: &[RolloutFinding],
    live_starts: &[Option<Timestamp>],
    opts: &ScanOpts,
) -> Vec<ThreadActivity> {
    if live_starts.is_empty() {
        return Vec::new();
    }
    let slack =
        Duration::from_millis(u64::try_from(crate::procs::START_TIME_SLACK_MS).unwrap_or(0));
    let mut out = Vec::new();
    for f in findings {
        let signal = if f.tail.conclusive {
            let Some(started) = f.tail.open_turn_started_at else {
                continue;
            };
            let owner_alive = live_starts
                .iter()
                .any(|s| s.is_none_or(|s| s <= started.saturating_add(slack)));
            if !owner_alive {
                continue;
            }
            ActivitySignal::RolloutTail
        } else {
            let recent = opts
                .now
                .duration_since(f.mtime)
                .is_none_or(|age| age <= opts.mtime_window);
            if !recent {
                continue;
            }
            ActivitySignal::RolloutMtime
        };
        out.push(ThreadActivity {
            target_id: TargetId::local(),
            thread_id: f.thread_id.clone(),
            title: None,
            rollout_path: Some(f.path.to_string_lossy().into_owned()),
            last_event_at: f.tail.last_event_at.or(Some(f.mtime)),
            signal,
            originator: f.head.originator.clone(),
            cwd: f.head.cwd.clone(),
        });
    }
    out
}

/// The busy entry for the desktop app's own first-start setup (see
/// [`ActivitySignal::AppSetup`]): no thread, no title; the UI names it.
pub fn app_setup_activity() -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::local(),
        thread_id: None,
        title: None,
        rollout_path: None,
        last_event_at: None,
        signal: ActivitySignal::AppSetup,
        originator: None,
        cwd: None,
    }
}
