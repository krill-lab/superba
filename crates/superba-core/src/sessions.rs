//! Session (thread) provider scan and migration types ([`crate::SessionMigrator`]).
//!
//! Facts: the app lists only threads whose `model_provider` equals the current
//! provider id. The provider is stored in `state_5.sqlite` `threads.model_provider`
//! and in each rollout's `session_meta.model_provider` (`sessions/**/rollout-*.jsonl`,
//! `archived_sessions/`, possibly `.jsonl.zst`). Migration requires every Codex
//! process of the home to be stopped (hence a full app restart).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::Notice;
use crate::ids::TargetId;
use crate::paths::CodexHome;
use crate::time::Timestamp;

/// Counts for one provider id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSessions {
    /// Rows in the sqlite `threads` table.
    pub threads: u32,
    /// Live rollout files (`sessions/`).
    pub rollouts: u32,
    /// Archived rollout files (`archived_sessions/`).
    pub archived: u32,
    /// Of the rollouts above, how many are `.zst` compressed.
    pub compressed: u32,
    /// Distinct sessions across all storage forms (union of thread ids from sqlite,
    /// rollouts and archived rollouts). This is the user-facing count.
    #[serde(default)]
    pub distinct: u32,
}

/// Provider distribution of a Codex home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReport {
    /// Codex home root.
    pub codex_home: PathBuf,
    /// When scanned.
    pub scanned_at: Timestamp,
    /// sqlite databases found.
    pub sqlite_paths: Vec<PathBuf>,
    /// Counts per provider id (`openai`, `krill`, ...).
    pub by_provider: BTreeMap<String, ProviderSessions>,
    /// Threads whose sqlite row and rollout disagree on the provider.
    pub inconsistent: u32,
    /// Problems (unreadable files, locked database ...).
    #[serde(default)]
    pub warnings: Vec<Notice>,
}

impl SessionReport {
    /// Thread count per provider (the baseline's `session_distribution`).
    pub fn distribution(&self) -> BTreeMap<String, u32> {
        self.by_provider
            .iter()
            .map(|(k, v)| (k.clone(), v.distinct))
            .collect()
    }

    /// Sessions not under `provider` (hidden in the app when `provider` is current).
    pub fn hidden_under(&self, provider: &str) -> u32 {
        self.distribution()
            .iter()
            .filter(|(k, _)| k.as_str() != provider)
            .map(|(_, v)| v)
            .sum()
    }
}

/// Which sessions to move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MigrationSelector {
    /// Every session whose provider is this id (takeover: `krill` -> `openai`).
    Provider {
        /// Source provider id.
        provider: String,
    },
    /// Every session stored under a provider other than `except`.
    /// Used by simple mode to gather all non-OpenAI providers in one migration.
    OtherProviders {
        /// Provider to leave unchanged.
        except: String,
    },
    /// Undo a previous migration using its ledger (reverse migration, plan 10.4).
    Ledger {
        /// Migration id.
        migration_id: String,
    },
    /// Explicit thread ids (user-adjusted selection in the preview).
    ThreadIds {
        /// Thread / session ids.
        ids: Vec<String>,
    },
    /// Sessions of `provider` created in `[from, to)` (generation-based reverse
    /// migration of sessions created while Superba was active).
    CreatedBetween {
        /// Source provider id.
        provider: String,
        /// Start (inclusive).
        from: Timestamp,
        /// End (exclusive); `None` = now.
        to: Option<Timestamp>,
    },
}

/// A migration request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRequest {
    /// Destination provider id.
    pub to_provider: String,
    /// Sessions to include (union).
    pub selectors: Vec<MigrationSelector>,
    /// Include `archived_sessions/`.
    pub include_archived: bool,
    /// Thread ids to leave out (unticked in the preview).
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One session in a migration preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationItem {
    /// Thread / session id.
    pub thread_id: String,
    /// Title, if known.
    #[serde(default)]
    pub title: Option<String>,
    /// Current provider.
    pub source_provider: String,
    /// Creation time, if known.
    #[serde(default)]
    pub created_at: Option<Timestamp>,
    /// Lives in `archived_sessions/`.
    #[serde(default)]
    pub archived: bool,
}

/// A computed migration (no writes yet).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// Migration id (also names the backup dir and ledger).
    pub id: String,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// Request.
    pub request: MigrationRequest,
    /// sqlite rows to update.
    pub thread_count: u32,
    /// Rollout files to rewrite.
    pub rollout_count: u32,
    /// Source provider -> count.
    pub by_source_provider: BTreeMap<String, u32>,
    /// Backup directory that will be created.
    pub backup_dir: PathBuf,
    /// Bytes to back up (for a disk-space check).
    pub backup_bytes: u64,
    /// Sessions included (for the adjustable preview).
    #[serde(default)]
    pub items: Vec<MigrationItem>,
}

/// Ledger summary of a completed migration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReceipt {
    /// Migration id.
    pub id: String,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// When.
    pub migrated_at: Timestamp,
    /// Destination provider id.
    pub to_provider: String,
    /// Source provider -> count.
    pub by_source_provider: BTreeMap<String, u32>,
    /// sqlite rows updated.
    pub threads: u32,
    /// Rollout files rewritten.
    pub rollouts: u32,
    /// Backup directory.
    pub backup_dir: PathBuf,
    /// Ledger file (per-item before/after hashes).
    pub ledger_path: PathBuf,
    /// True once rolled back.
    #[serde(default)]
    pub rolled_back: bool,
}

/// Result of a rollback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReport {
    /// sqlite rows restored.
    pub restored_threads: u32,
    /// Rollout files restored.
    pub restored_rollouts: u32,
    /// Items skipped because they changed after the migration.
    pub skipped_changed: u32,
}

/// The caller's statement that it listed and stopped every Codex process using a
/// Codex home (desktop app, app-server daemon, TUI/exec, IDE app-servers), for
/// example `superba-switch` after `CloseConsumers` with `AppController::processes`.
///
/// Passed to [`crate::SessionMigrator::migrate_stopped`] and its siblings, it lets the
/// migrator skip its own process detection (which can be inconclusive, e.g. when
/// another process's environment is unreadable). It never disables the other
/// safety checks (Codex thread writer locks, the migrate lock, SQLite busy checks).
/// Implementations reject tokens for another home or older than a few minutes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumersStopped {
    /// Root of the Codex home the check covered.
    pub codex_home: PathBuf,
    /// When the processes were checked.
    pub checked_at: Timestamp,
}

impl ConsumersStopped {
    /// A token for `home`, checked at `checked_at`.
    pub fn new(home: &CodexHome, checked_at: Timestamp) -> Self {
        Self {
            codex_home: home.root().to_path_buf(),
            checked_at,
        }
    }
}

/// Where a session name came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TitleSource {
    /// `state_5.sqlite` `threads.title`.
    Title,
    /// The thread's first user message.
    FirstUserMessage,
    /// Not found: the short key.
    #[default]
    ShortKey,
}

/// A session name for a router log key ([`crate::SwitchService::session_titles`]).
/// Titles are looked up at display time and never stored in the router database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitle {
    /// Thread id the key resolved to (`<thread id>` or `<source>:<parent id>`
    /// keys).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Name to show.
    pub title: String,
    /// Where it came from.
    #[serde(default)]
    pub source: TitleSource,
}

/// Sessions of one host (Sessions page host switcher,
/// [`crate::SwitchService::host_sessions`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSessions {
    /// Host (`local` for this computer).
    pub target_id: TargetId,
    /// Provider distribution.
    pub report: SessionReport,
    /// Migration history (this computer only).
    #[serde(default)]
    pub history: Vec<MigrationReceipt>,
    /// Remote hosts are scanned read-only in 0.3.
    #[serde(default)]
    pub read_only: bool,
    /// Command the user can run on the host to migrate (`spb sessions migrate
    /// --from <provider>`), for remote hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrate_command: Option<String>,
}

/// The host id the desktop app uses for this computer.
pub const LOCAL_HOST_ID: &str = "local";

/// One thread of the session browser ([`crate::SwitchService::session_list`]):
/// a `state_*.sqlite` `threads` row of this computer and/or a row of the
/// desktop app's `local_thread_catalog` (which also lists remote hosts).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionEntry {
    /// The app's host id (`local`, `remote-ssh-discovered:<alias>`,
    /// `remote-wsl:<distro>`, ...).
    pub host_id: String,
    /// The app's host kind (`local`, `ssh`, `wsl`, `remote-control`), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_kind: Option<String>,
    /// Thread id.
    pub thread_id: String,
    /// Name, else title, else first user message (one line; may be empty).
    pub title: String,
    /// Working directory.
    pub cwd: String,
    /// `model_provider`.
    pub provider: String,
    /// Model, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Git branch, when recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
    /// Last activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
    /// Codex's `threads.tokens_used`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    /// Archived.
    pub archived: bool,
    /// The rollout file (this computer only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollout_path: Option<PathBuf>,
}

impl SessionEntry {
    /// On this computer.
    pub fn is_local(&self) -> bool {
        self.host_id == LOCAL_HOST_ID
    }
}

/// The session browser's list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionList {
    /// Newest activity first.
    pub entries: Vec<SessionEntry>,
    /// A source hit its row cap (older threads are not listed).
    pub truncated: bool,
    /// When it was read.
    pub scanned_at: Timestamp,
    /// Sources that could not be read.
    pub warnings: Vec<Notice>,
}

/// What a thread is exported as ([`crate::SwitchService::session_export`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionExportFormat {
    /// Markdown transcript (returned as text, for the clipboard).
    Markdown,
    /// A new Claude Code session file (`~/.claude/projects/<slug>/<uuid>.jsonl`).
    ClaudeCode,
    /// A new pi session file (`~/.pi/agent/sessions/--<cwd>--/<ts>_<id>.jsonl`).
    Pi,
}

/// The result of an export.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionExport {
    /// Markdown: the transcript. Session formats: the command that resumes it.
    pub text: String,
    /// The new file (session formats).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// The new session id (session formats).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Messages written.
    pub messages: usize,
}

/// The desktop app's link to a thread, as its own "Copy link" makes it:
/// `codex://threads/<id>` on this computer, else `?hostId=` with the host id
/// encoded like JavaScript `URLSearchParams` (`remote-ssh-discovered:bj` →
/// `remote-ssh-discovered%3Abj`).
pub fn thread_link(thread_id: &str, host_id: &str) -> String {
    let base = format!("codex://threads/{thread_id}");
    if host_id.is_empty() || host_id == LOCAL_HOST_ID {
        return base;
    }
    format!("{base}?hostId={}", form_urlencode(host_id))
}

/// `application/x-www-form-urlencoded` byte serialization (WHATWG URL, what
/// `URLSearchParams.toString()` produces): `*-._` and ASCII alphanumerics stay,
/// space becomes `+`, every other UTF-8 byte is `%XX`.
pub fn form_urlencode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'*' | b'-' | b'.' | b'_' => out.push(char::from(byte)),
            b if b.is_ascii_alphanumeric() => out.push(char::from(b)),
            b' ' => out.push('+'),
            b => {
                out.push('%');
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 15)]));
            }
        }
    }
    out
}

/// Fuzzy score of `query` against `text` (higher is better), `None` when the
/// query's characters do not all occur in order. Case-insensitive; runs of
/// consecutive characters and matches at word starts score more, gaps cost.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    // A contiguous hit is always best: score it directly.
    let joined: String = lower.iter().collect();
    let needle: String = query.iter().collect();
    if let Some(pos) = joined.find(&needle) {
        let at = joined[..pos].chars().count();
        let start = at == 0 || !chars[at - 1].is_alphanumeric();
        return Some(
            1000 + 10 * query.len() as i32 + if start { 50 } else { 0 } - at.min(50) as i32,
        );
    }
    let mut score = 0i32;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (i, c) in lower.iter().enumerate() {
        if qi == query.len() {
            break;
        }
        if *c != query[qi] {
            continue;
        }
        score += 10;
        if i == 0 || !chars[i - 1].is_alphanumeric() {
            score += 8;
        }
        match last {
            Some(l) if l + 1 == i => score += 12,
            Some(l) => score -= ((i - l - 1) as i32).min(8),
            None => {}
        }
        last = Some(i);
        qi += 1;
    }
    (qi == query.len()).then_some(score)
}

/// Fuzzy score of a whitespace-separated `query` over several fields with
/// weights: every term must match some field; a term's best weighted field
/// score counts. `None` when a term matches nowhere.
pub fn fuzzy_fields(query: &str, fields: &[(&str, i32)]) -> Option<i32> {
    let mut total = 0;
    for term in query.split_whitespace() {
        let best = fields
            .iter()
            .filter_map(|(text, weight)| fuzzy_score(term, text).map(|s| s * weight))
            .max()?;
        total += best;
    }
    Some(total)
}
