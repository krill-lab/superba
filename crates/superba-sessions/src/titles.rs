//! Session names for the router's request log: `prompt_cache_key` (`<thread id>`
//! or `<source>:<parent thread id>`) or a thread id -> `state_5.sqlite`
//! `threads.name` (a name the user gave the thread), else `threads.title`, else
//! the thread's first user message, else the short key. Read-only (the database
//! is opened read-only, never locked for writing); titles are never copied into
//! the router database.
//!
//! Key forms (Codex 0.162/0.163 `core/src/client.rs` `prompt_cache_key`): the
//! thread's session id, or `<source>:<parent thread id>` for internal sessions
//! (`guardian:<id>` for the guardian reviewer, other internal sources likewise).
//! The `name` column exists since state migration 0041; older databases have
//! only `title` and `first_user_message`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::{Connection, params_from_iter};
use superba_core::{CodexHome, Result, SessionTitle, TitleSource};

use crate::sqlite;

/// Longest name returned (characters); longer titles end with `…`.
pub const MAX_TITLE_CHARS: usize = 80;
/// Characters of a key shown when no thread is known.
pub const SHORT_KEY_CHARS: usize = 8;
/// SQLite parameters per query.
const CHUNK: usize = 400;

/// Names for `keys` (keys without a thread map to the short key).
pub fn lookup(home: &CodexHome, keys: &[String]) -> Result<BTreeMap<String, SessionTitle>> {
    lookup_in(home, None, keys)
}

/// [`lookup`] with the user's home directory, so a `sqlite_home = "~/..."` in
/// `config.toml` resolves.
pub fn lookup_in(
    home: &CodexHome,
    home_dir: Option<&Path>,
    keys: &[String],
) -> Result<BTreeMap<String, SessionTitle>> {
    let parsed: Vec<(&String, ParsedKey<'_>)> = keys.iter().map(|k| (k, parse_key(k))).collect();
    let ids: BTreeSet<&str> = parsed.iter().filter_map(|(_, p)| p.thread_id()).collect();
    let rows = if ids.is_empty() {
        BTreeMap::new()
    } else {
        read_threads(home, home_dir, &ids)
    };
    let mut out = BTreeMap::new();
    for (key, p) in parsed {
        let found = p.thread_id().and_then(|id| rows.get(id));
        let title = match found {
            Some(row) => row.title(p.thread_id().map(str::to_owned)),
            None => SessionTitle {
                thread_id: None,
                title: p.short(),
                source: TitleSource::ShortKey,
            },
        };
        out.insert(key.clone(), title);
    }
    Ok(out)
}

/// A request log key.
#[derive(Debug, PartialEq, Eq)]
enum ParsedKey<'a> {
    /// A thread id (or anything else without a source prefix).
    Thread(&'a str),
    /// `<source>:<parent thread id>`.
    Internal { source: &'a str, parent: &'a str },
    /// Empty.
    Empty,
}

impl<'a> ParsedKey<'a> {
    fn thread_id(&self) -> Option<&'a str> {
        match self {
            ParsedKey::Thread(id) => Some(id),
            ParsedKey::Internal { parent, .. } => Some(parent),
            ParsedKey::Empty => None,
        }
    }

    fn short(&self) -> String {
        let short = |s: &str| s.chars().take(SHORT_KEY_CHARS).collect::<String>();
        match self {
            ParsedKey::Thread(id) => short(id),
            ParsedKey::Internal { source, parent } => format!("{source}:{}", short(parent)),
            ParsedKey::Empty => String::new(),
        }
    }
}

fn parse_key(key: &str) -> ParsedKey<'_> {
    let key = key.trim();
    if key.is_empty() {
        return ParsedKey::Empty;
    }
    match key.split_once(':') {
        Some((source, parent))
            if !source.is_empty()
                && !parent.is_empty()
                && source
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
        {
            ParsedKey::Internal { source, parent }
        }
        _ => ParsedKey::Thread(key),
    }
}

/// The name columns of one thread.
#[derive(Debug, Default)]
struct Row {
    name: Option<String>,
    title: Option<String>,
    first_user_message: Option<String>,
}

impl Row {
    fn title(&self, thread_id: Option<String>) -> SessionTitle {
        let pick = |v: &Option<String>| v.as_deref().map(clean).filter(|s| !s.is_empty());
        let (title, source) = match (pick(&self.name), pick(&self.title)) {
            (Some(t), _) | (None, Some(t)) => (t, TitleSource::Title),
            (None, None) => match pick(&self.first_user_message) {
                Some(t) => (t, TitleSource::FirstUserMessage),
                None => (
                    thread_id
                        .as_deref()
                        .map(|id| id.chars().take(SHORT_KEY_CHARS).collect())
                        .unwrap_or_default(),
                    TitleSource::ShortKey,
                ),
            },
        };
        SessionTitle {
            thread_id,
            title,
            source,
        }
    }
}

/// One line, whitespace collapsed, at most [`MAX_TITLE_CHARS`] characters.
fn clean(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(MAX_TITLE_CHARS - 1).collect();
    out.push('…');
    out
}

/// Rows for `ids` from every `state_*.sqlite` of the home (first hit wins). A
/// database that cannot be read is skipped: names are a convenience.
fn read_threads(
    home: &CodexHome,
    home_dir: Option<&Path>,
    ids: &BTreeSet<&str>,
) -> BTreeMap<String, Row> {
    let mut warnings = Vec::new();
    let dirs = crate::inventory::sqlite_dirs(home, home_dir, &mut warnings);
    let mut out: BTreeMap<String, Row> = BTreeMap::new();
    for path in sqlite::candidate_files(&dirs) {
        let is_state = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("state_"));
        if !is_state || crate::util::is_unlockable_share(&path) {
            continue;
        }
        let missing: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| !out.contains_key(*id))
            .collect();
        if missing.is_empty() {
            break;
        }
        match read_db(&path, &missing) {
            Ok(rows) => {
                for (id, row) in rows {
                    out.entry(id).or_insert(row);
                }
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "session titles unavailable")
            }
        }
    }
    out
}

fn read_db(path: &Path, ids: &[&str]) -> Result<BTreeMap<String, Row>> {
    let conn = sqlite::open_ro(path)?;
    let columns = thread_columns(&conn);
    if !columns.contains("id") {
        return Ok(BTreeMap::new());
    }
    let col = |name: &str| {
        if columns.contains(name) {
            name.to_owned()
        } else {
            "NULL".to_owned()
        }
    };
    let select = format!(
        "SELECT id, {}, {}, {} FROM threads WHERE id IN",
        col("name"),
        col("title"),
        col("first_user_message")
    );
    let mut out = BTreeMap::new();
    for chunk in ids.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("{select} ({marks})");
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| superba_core::Error::other(e.to_string()))?;
        let rows = stmt
            .query_map(params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Row {
                        name: r.get::<_, Option<String>>(1).ok().flatten(),
                        title: r.get::<_, Option<String>>(2).ok().flatten(),
                        first_user_message: r.get::<_, Option<String>>(3).ok().flatten(),
                    },
                ))
            })
            .map_err(|e| superba_core::Error::other(e.to_string()))?;
        for row in rows.flatten() {
            out.insert(row.0, row.1);
        }
    }
    Ok(out)
}

fn thread_columns(conn: &Connection) -> BTreeSet<String> {
    let Ok(mut stmt) = conn.prepare("PRAGMA table_info(threads)") else {
        return BTreeSet::new();
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) else {
        return BTreeSet::new();
    };
    rows.flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_into_thread_and_internal_forms() {
        assert_eq!(
            parse_key("019a1b2c-0000-7000-8000-000000000001"),
            ParsedKey::Thread("019a1b2c-0000-7000-8000-000000000001")
        );
        assert_eq!(
            parse_key("guardian:019a1b2c-0000"),
            ParsedKey::Internal {
                source: "guardian",
                parent: "019a1b2c-0000"
            }
        );
        assert_eq!(parse_key("  "), ParsedKey::Empty);
        assert_eq!(parse_key(":x"), ParsedKey::Thread(":x"));
        assert_eq!(
            ParsedKey::Internal {
                source: "guardian",
                parent: "0123456789abcdef"
            }
            .short(),
            "guardian:01234567"
        );
    }

    #[test]
    fn titles_are_one_line_and_bounded() {
        assert_eq!(clean("  a\n\tb  "), "a b");
        let long = "字".repeat(200);
        let c = clean(&long);
        assert_eq!(c.chars().count(), MAX_TITLE_CHARS);
        assert!(c.ends_with('…'));
    }
}
