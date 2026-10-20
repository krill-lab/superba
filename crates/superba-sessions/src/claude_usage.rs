//! Model requests read from Claude Code's local transcripts, for the Usage
//! page and `spb usage` when Claude Code did not go through the local router.
//!
//! Source: `<Claude Code config dir>/projects/<project>/<session>.jsonl` and
//! the subagent transcripts `<project>/<session>/subagents/*.jsonl`. Each
//! model response is written as one `"type":"assistant"` line per content
//! block, every one repeating `message.id`, `message.model` and
//! `message.usage`; the index keeps one row per message id (the usage of its
//! last line, the first line's timestamp as start and the last one's as end).
//! Synthetic messages (`<synthetic>` model, written by Claude Code itself) are
//! skipped.
//!
//! Token semantics are converted to OpenAI's: `input` = `input_tokens` +
//! `cache_creation_input_tokens` + `cache_read_input_tokens`, `cached` =
//! cache reads, `cache_write` = cache creation.
//!
//! Reading is incremental and bounded like [`crate::usage::UsageIndex`]: only
//! files modified in the queried range are visited, files are tailed from the
//! last complete line, overlong lines are skipped without
//! being buffered (`crate::usage::MAX_LINE_BYTES`), and at most [`MAX_ROWS`] rows are kept. Only ids, model
//! names, token counts and timestamps are kept: message text is never stored.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use superba_core::Timestamp;

use crate::usage::{MAX_ROWS, ROLLOUT_ID_FLOOR, RolloutUsage, read_lines};
use crate::util::parse_rfc3339_ms;

/// Ids of transcript rows lie in `[ID_FLOOR, ID_CEIL)`: below the rollout
/// rows, above the imported ones.
pub const ID_FLOOR: i64 = -(1 << 62);
/// See [`ID_FLOOR`].
pub const ID_CEIL: i64 = ROLLOUT_ID_FLOOR;
/// The model Claude Code writes on messages it made up itself.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";
/// Depth below `projects/` (`<project>/<session>/subagents/<agent>.jsonl`).
const WALK_MAX_DEPTH: usize = 4;
const MTIME_SLACK: Duration = Duration::from_secs(60);
const IDLE_EVICT: Duration = Duration::from_secs(15 * 60);
const WALK_EVERY: Duration = Duration::from_secs(2);

/// One model response read from a transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeRequest {
    /// Stable negative id in [`ID_FLOOR`]`..`[`ID_CEIL`], derived from the
    /// message id.
    pub id: i64,
    /// `sessionId`.
    pub session_id: Option<String>,
    /// `message.id` (Anthropic's `msg_...`).
    pub message_id: String,
    /// `requestId` (Anthropic's `req_...`).
    pub request_id: Option<String>,
    /// `message.model`.
    pub model: String,
    /// Timestamp of the message's first line.
    pub started_at: Timestamp,
    /// Timestamp of its last line.
    pub completed_at: Timestamp,
    /// Usage in OpenAI semantics (`reasoning` is always 0).
    pub usage: RolloutUsage,
    /// A subagent's request (`isSidechain` or a `subagents/` transcript).
    pub sidechain: bool,
}

impl ClaudeRequest {
    /// `completed_at - started_at` in ms.
    pub fn duration_ms(&self) -> u64 {
        u64::try_from(self.completed_at.as_millis() - self.started_at.as_millis()).unwrap_or(0)
    }
}

/// Result of [`ClaudeUsageIndex::requests`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudePage {
    /// Rows started in the range, oldest first.
    pub rows: Vec<ClaudeRequest>,
    /// The row cap was reached: older files of the range were not read.
    pub truncated: bool,
    /// Transcript files read for the range.
    pub files: usize,
}

/// The row id of a message id.
pub fn row_id(message_id: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in message_id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ID_FLOOR + (h & 0x1fff_ffff_ffff_ffff) as i64
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<Cow<'a, str>>,
    #[serde(rename = "requestId", borrow)]
    request_id: Option<Cow<'a, str>>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(borrow)]
    message: Option<Message<'a>>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// Rows of one transcript, by message id.
#[derive(Debug, Default)]
struct Parser {
    by_message: HashMap<String, usize>,
}

impl Parser {
    fn line(&mut self, bytes: &[u8], sidechain_file: bool, rows: &mut Vec<ClaudeRequest>) {
        // Cheap filter before parsing: most lines are user / tool lines.
        if !contains(bytes, b"\"assistant\"") || !contains(bytes, b"\"usage\"") {
            return;
        }
        let Ok(l) = serde_json::from_slice::<Line<'_>>(bytes) else {
            return;
        };
        if l.kind.as_deref() != Some("assistant") {
            return;
        }
        let Some(m) = l.message else { return };
        let (Some(id), Some(model), Some(u)) = (m.id, m.model, m.usage) else {
            return;
        };
        if id.is_empty() || model.is_empty() || model == SYNTHETIC_MODEL {
            return;
        }
        let Some(at) = l
            .timestamp
            .as_deref()
            .and_then(parse_rfc3339_ms)
            .map(Timestamp::from_millis)
        else {
            return;
        };
        let read = u.cache_read_input_tokens.unwrap_or(0);
        let write = u.cache_creation_input_tokens.unwrap_or(0);
        let usage = RolloutUsage {
            input: u.input_tokens.unwrap_or(0) + read + write,
            cached: read,
            cache_write: write,
            output: u.output_tokens.unwrap_or(0),
            reasoning: 0,
        };
        let sidechain = sidechain_file || l.is_sidechain.unwrap_or(false);
        if let Some(&i) = self.by_message.get(id.as_ref()) {
            let r = &mut rows[i];
            r.usage = usage;
            r.completed_at = r.completed_at.max(at);
            r.started_at = r.started_at.min(at);
            r.sidechain |= sidechain;
            if r.request_id.is_none() {
                r.request_id = l.request_id.map(Cow::into_owned);
            }
            return;
        }
        self.by_message.insert(id.to_string(), rows.len());
        rows.push(ClaudeRequest {
            id: row_id(&id),
            session_id: l.session_id.map(Cow::into_owned),
            message_id: id.into_owned(),
            request_id: l.request_id.map(Cow::into_owned),
            model: model.into_owned(),
            started_at: at,
            completed_at: at,
            usage,
            sidechain,
        });
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Rows of a whole transcript (tests, one-off reads).
pub fn parse_transcript(bytes: &[u8], sidechain_file: bool) -> Vec<ClaudeRequest> {
    let mut parser = Parser::default();
    let mut rows = Vec::new();
    let _ = read_lines(bytes, |l| parser.line(l, sidechain_file, &mut rows));
    rows
}

#[derive(Debug)]
struct FileState {
    len: u64,
    mtime: Option<SystemTime>,
    offset: u64,
    parser: Parser,
    rows: Vec<ClaudeRequest>,
    used: Instant,
}

#[derive(Clone, Debug)]
struct Found {
    path: PathBuf,
    len: u64,
    mtime: Option<SystemTime>,
    sidechain: bool,
}

/// Incremental reader of every transcript under one Claude Code configuration
/// directory. Not thread-safe; callers keep it behind a mutex and call it from
/// a blocking thread.
#[derive(Debug)]
pub struct ClaudeUsageIndex {
    root: PathBuf,
    files: HashMap<PathBuf, FileState>,
    walked: Option<(Instant, Vec<Found>)>,
}

impl ClaudeUsageIndex {
    /// An empty index of the Claude Code configuration directory `root`
    /// (`ClaudeHome::root`).
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            files: HashMap::new(),
            walked: None,
        }
    }

    /// The configuration directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn walk(&mut self) -> Vec<Found> {
        if let Some((at, found)) = &self.walked
            && at.elapsed() < WALK_EVERY
        {
            return found.clone();
        }
        let mut found = Vec::new();
        let projects = self.root.join("projects");
        if projects.is_dir() {
            for entry in walkdir::WalkDir::new(&projects)
                .follow_links(false)
                .max_depth(WALK_MAX_DEPTH)
                .into_iter()
                .filter_map(std::result::Result::ok)
            {
                if !entry.file_type().is_file()
                    || entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl")
                {
                    continue;
                }
                let meta = if cfg!(windows) {
                    std::fs::metadata(entry.path()).ok()
                } else {
                    entry.metadata().ok()
                };
                let Some(meta) = meta else { continue };
                let sidechain = entry
                    .path()
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|n| n == "subagents");
                found.push(Found {
                    path: entry.path().to_path_buf(),
                    len: meta.len(),
                    mtime: meta.modified().ok(),
                    sidechain,
                });
            }
        }
        found.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
        self.walked = Some((Instant::now(), found.clone()));
        found
    }

    fn refresh_file(&mut self, f: &Found) {
        let fresh = self.files.get(&f.path).is_none_or(|s| f.len < s.len);
        if fresh {
            self.files.insert(
                f.path.clone(),
                FileState {
                    len: 0,
                    mtime: None,
                    offset: 0,
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
        let read = (|| -> std::io::Result<()> {
            let mut file = File::open(&f.path)?;
            file.seek(SeekFrom::Start(state.offset))?;
            let parser = &mut state.parser;
            let rows = &mut state.rows;
            let outcome = read_lines(file, |l| parser.line(l, f.sidechain, rows))?;
            state.offset += outcome.consumed;
            Ok(())
        })();
        if read.is_err() {
            self.files.remove(&f.path);
            return;
        }
        state.len = f.len;
        state.mtime = f.mtime;
    }

    /// Rows started in `[from, to)`, refreshing the transcripts modified in
    /// that range first; one row per message id, oldest first.
    pub fn requests(&mut self, from: Option<Timestamp>, to: Option<Timestamp>) -> ClaudePage {
        let found = self.walk();
        let floor = from.map(|f| {
            f.to_system_time()
                .checked_sub(MTIME_SLACK)
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
        let mut page = ClaudePage::default();
        let mut rows: BTreeMap<i64, ClaudeRequest> = BTreeMap::new();
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
                if from.is_none_or(|t| r.started_at >= t) && to.is_none_or(|t| r.started_at < t) {
                    rows.entry(r.id).or_insert_with(|| r.clone());
                }
            }
        }
        let live: std::collections::HashSet<&PathBuf> = found.iter().map(|f| &f.path).collect();
        self.files
            .retain(|p, s| live.contains(p) && s.used.elapsed() < IDLE_EVICT);
        let mut rows: Vec<ClaudeRequest> = rows.into_values().collect();
        rows.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        page.rows = rows;
        page
    }

    /// The row with `id` among the cached transcripts, if any.
    pub fn row(&self, id: i64) -> Option<ClaudeRequest> {
        self.files
            .values()
            .flat_map(|s| s.rows.iter())
            .find(|r| r.id == id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, model: &str, ts: &str, out: u64, side: bool) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"s1","requestId":"req_1","isSidechain":{side},"message":{{"id":"{id}","model":"{model}","content":[{{"type":"text","text":"secret"}}],"usage":{{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":{out}}}}}}}"#
        )
    }

    #[test]
    fn one_row_per_message_with_last_usage() {
        let text = [
            line("msg_a", "claude-x", "2026-10-20T10:00:00.000Z", 1, false),
            "not json".into(),
            r#"{"type":"user","message":{"content":"hi"}}"#.into(),
            line("msg_a", "claude-x", "2026-10-20T10:00:03.000Z", 7, false),
            line(
                "msg_s",
                SYNTHETIC_MODEL,
                "2026-10-20T10:00:04.000Z",
                1,
                false,
            ),
            line("msg_b", "claude-y", "2026-10-20T10:00:05.000Z", 2, true),
        ]
        .join("\n")
            + "\n";
        let rows = parse_transcript(text.as_bytes(), false);
        assert_eq!(rows.len(), 2);
        let a = &rows[0];
        assert_eq!(a.message_id, "msg_a");
        assert_eq!(a.usage.input, 60);
        assert_eq!(a.usage.cached, 30);
        assert_eq!(a.usage.cache_write, 20);
        assert_eq!(a.usage.output, 7);
        assert_eq!(a.duration_ms(), 3000);
        assert_eq!(a.request_id.as_deref(), Some("req_1"));
        assert!(!a.sidechain);
        assert!(rows[1].sidechain);
        assert!((ID_FLOOR..ID_CEIL).contains(&a.id));
    }

    #[test]
    fn subagent_file_marks_sidechain() {
        let text = line("msg_c", "m", "2026-10-20T10:00:00Z", 1, false) + "\n";
        assert!(parse_transcript(text.as_bytes(), true)[0].sidechain);
    }
}
