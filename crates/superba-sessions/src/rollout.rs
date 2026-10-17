//! Rollout files: `sessions/**/rollout-*.jsonl[.zst]` and `archived_sessions/`.
//!
//! A rollout starts with a `session_meta` record whose `payload.model_provider`
//! assigns the session to a provider. Migration changes only the bytes of that
//! string token (byte-exact surgery found by [`crate::jsonscan`]); everything else
//! in the file stays identical, the modification time is restored and compressed
//! files are re-compressed. Because Codex's SQLite history projection
//! (`thread_history_*.sqlite`) stores byte offsets into the uncompressed rollout,
//! every rewrite reports its [`Edit`]s so those offsets can be shifted.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use superba_core::codex::PROVIDER_OPENAI;
use superba_core::{Error, Notice, Result};

use crate::jsonscan::{inspect_session_meta, json_string_token};
use crate::text;
use crate::util::{Fingerprint, atomic_replace, io_err, parse_rfc3339_ms, sha256_hex};

/// Suffix of compressed rollouts.
const ZST_SUFFIX: &str = ".jsonl.zst";
/// Suffix of plain rollouts.
const PLAIN_SUFFIX: &str = ".jsonl";
/// zstd level used when Superba re-compresses a rollout it rewrote.
const ZSTD_LEVEL: i32 = 3;
/// How many lines of a rollout are searched for the first `session_meta` record.
const HEAD_MAX_LINES: usize = 64;
/// How many bytes of a rollout are searched for the first `session_meta` record.
const HEAD_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Directory depth searched below `sessions/` (`YYYY/MM/DD/`) and
/// `archived_sessions/`.
const WALK_MAX_DEPTH: usize = 8;

/// Provider Codex assumes for a `session_meta` without `model_provider`.
pub(crate) const DEFAULT_PROVIDER: &str = PROVIDER_OPENAI;

/// First `session_meta` record of a rollout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RolloutHead {
    /// `payload.id` (the thread id).
    pub thread_id: Option<String>,
    /// `payload.model_provider`; `None` = absent (Codex treats it as `openai`).
    pub provider: Option<String>,
    /// `payload.timestamp` as ms since the epoch.
    pub created_ms: Option<i64>,
}

impl RolloutHead {
    /// Effective provider id.
    pub(crate) fn effective_provider(&self) -> &str {
        self.provider.as_deref().unwrap_or(DEFAULT_PROVIDER)
    }
}

/// One rollout file found under the Codex home.
#[derive(Clone, Debug)]
pub(crate) struct RolloutFile {
    /// Absolute path.
    pub path: PathBuf,
    /// Lives under `archived_sessions/`.
    pub archived: bool,
    /// `.jsonl.zst`.
    pub compressed: bool,
    /// Size on disk.
    pub len: u64,
    /// First `session_meta`, if any.
    pub head: Option<RolloutHead>,
}

/// True for `rollout-*.jsonl` / `rollout-*.jsonl.zst` file names.
pub(crate) fn is_rollout_name(name: &str) -> bool {
    name.starts_with("rollout-") && (name.ends_with(PLAIN_SUFFIX) || name.ends_with(ZST_SUFFIX))
}

/// True for the compressed form.
pub(crate) fn is_compressed(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(ZST_SUFFIX))
}

/// Find every rollout under `sessions/` and `archived_sessions/` and read its head.
/// Unreadable files and symbolic links are reported in `warnings` and skipped.
pub(crate) fn discover(
    sessions_dir: &Path,
    archived_dir: &Path,
    warnings: &mut Vec<Notice>,
) -> Vec<RolloutFile> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (root, archived) in [(sessions_dir, false), (archived_dir, true)] {
        if !root.is_dir() {
            continue;
        }
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .max_depth(WALK_MAX_DEPTH)
            .sort_by_file_name();
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    let path = err.path().unwrap_or(root).to_path_buf();
                    warnings.push(text::unreadable(&path, &err.to_string()));
                    continue;
                }
            };
            let Some(name) = entry.file_name().to_str() else {
                continue;
            };
            if !is_rollout_name(name) {
                continue;
            }
            let mut path = entry.path().to_path_buf();
            if entry.file_type().is_symlink() {
                // Rewrite the link target (keeping the link) when it is a regular
                // file; anything else is skipped with a warning.
                match fs::canonicalize(&path) {
                    Ok(target) if target.is_file() => path = target,
                    _ => {
                        warnings.push(text::symlink_skipped(&path));
                        continue;
                    }
                }
            } else if !entry.file_type().is_file() {
                continue;
            }
            if !seen.insert(crate::util::normalize_dir(&path)) {
                continue;
            }
            let compressed = is_compressed(&path);
            let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            match read_head(&path, compressed) {
                Ok(head) => out.push(RolloutFile {
                    path,
                    archived,
                    compressed,
                    len,
                    head,
                }),
                Err(err) => warnings.push(text::unreadable(&path, &err.to_string())),
            }
        }
    }
    out
}

/// Read lines until the first `session_meta` record (bounded).
pub(crate) fn read_head(path: &Path, compressed: bool) -> std::io::Result<Option<RolloutHead>> {
    let file = File::open(path)?;
    let reader: Box<dyn Read> = if compressed {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::with_capacity(256 * 1024, reader.take(HEAD_MAX_BYTES));
    let mut line = Vec::new();
    for _ in 0..HEAD_MAX_LINES {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let body = line.strip_suffix(b"\n").unwrap_or(&line);
        if !may_be_session_meta(body) {
            continue;
        }
        if let Some(meta) = inspect_session_meta(body) {
            return Ok(Some(RolloutHead {
                thread_id: meta.id,
                provider: meta.provider,
                created_ms: meta.timestamp.as_deref().and_then(parse_rfc3339_ms),
            }));
        }
    }
    Ok(None)
}

/// Naive substring search (the needle is short and lines are scanned once).
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Cheap prefilter before the full scan: the literal `session_meta`, or any
/// `\u` escape (a valid line may spell the type as `session\u005fmeta`).
fn may_be_session_meta(line: &[u8]) -> bool {
    contains(line, b"session_meta") || contains(line, b"\\u")
}

/// One byte-exact replacement in the uncompressed rollout content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Edit {
    /// Byte offset in the content before the migration.
    pub at: u64,
    /// Bytes replaced (UTF-8 JSON text; empty for an insertion).
    pub old: String,
    /// Replacement.
    pub new: String,
}

impl Edit {
    fn delta(&self) -> i64 {
        self.new.len() as i64 - self.old.len() as i64
    }
}

/// Map a byte offset of the original content to the edited content. Offsets of
/// line starts after an edit move by the edit's length change. `None` on
/// arithmetic overflow (a corrupt offset).
pub(crate) fn shift_offset(edits: &[Edit], offset: i64) -> Option<i64> {
    let mut shifted = offset;
    for edit in edits {
        if i64::try_from(edit.at).ok()? < offset {
            shifted = shifted.checked_add(edit.delta())?;
        }
    }
    Some(shifted)
}

/// Apply sorted, non-overlapping `edits` to `content`, checking each `old`.
pub(crate) fn apply_edits(content: &[u8], edits: &[Edit]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(content.len() + 64);
    let mut cursor = 0usize;
    for edit in edits {
        let at = usize::try_from(edit.at).map_err(|_| conflict("edit offset overflow"))?;
        let end = at
            .checked_add(edit.old.len())
            .filter(|end| *end <= content.len() && at >= cursor)
            .ok_or_else(|| conflict("edit outside the rollout content"))?;
        if &content[at..end] != edit.old.as_bytes() {
            return Err(conflict("rollout content differs from the recorded edit"));
        }
        out.extend_from_slice(&content[cursor..at]);
        out.extend_from_slice(edit.new.as_bytes());
        cursor = end;
    }
    out.extend_from_slice(&content[cursor..]);
    Ok(out)
}

/// The inverse of `edits`, expressed in the coordinates of the edited content.
pub(crate) fn inverse_edits(edits: &[Edit]) -> Vec<Edit> {
    let mut shift = 0i64;
    edits
        .iter()
        .map(|e| {
            let at = (e.at as i64 + shift) as u64;
            shift += e.delta();
            Edit {
                at,
                old: e.new.clone(),
                new: e.old.clone(),
            }
        })
        .collect()
}

fn conflict(message: &str) -> Error {
    Error::Conflict {
        message: message.to_owned(),
    }
}

/// Raw bytes of a rollout and its uncompressed content.
pub(crate) struct Loaded {
    /// Bytes on disk.
    pub raw: Vec<u8>,
    /// Uncompressed JSONL.
    pub plain: Vec<u8>,
    /// Fingerprint taken before reading.
    pub fingerprint: Fingerprint,
    /// Metadata (permissions) of the file.
    pub meta: fs::Metadata,
}

/// Read a rollout (decompressing `.zst`).
pub(crate) fn load(path: &Path) -> Result<Loaded> {
    let fingerprint = Fingerprint::of(path)?;
    let meta = fs::symlink_metadata(path).map_err(|e| io_err(path, &e))?;
    if !meta.is_file() {
        return Err(Error::invalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let raw = fs::read(path).map_err(|e| io_err(path, &e))?;
    let plain = if is_compressed(path) {
        zstd::stream::decode_all(raw.as_slice()).map_err(|e| io_err(path, &e))?
    } else {
        raw.clone()
    };
    Ok(Loaded {
        raw,
        plain,
        fingerprint,
        meta,
    })
}

/// Encode content in the on-disk form of `path`.
pub(crate) fn encode(path: &Path, plain: &[u8]) -> Result<Vec<u8>> {
    if !is_compressed(path) {
        return Ok(plain.to_vec());
    }
    let encoded = zstd::stream::encode_all(plain, ZSTD_LEVEL).map_err(|e| io_err(path, &e))?;
    let check = zstd::stream::decode_all(encoded.as_slice()).map_err(|e| io_err(path, &e))?;
    if check != plain {
        return Err(Error::other(format!(
            "zstd round trip failed for {}",
            path.display()
        )));
    }
    Ok(encoded)
}

/// Every `session_meta` line of `plain`: `(line start, inspection)`.
fn meta_lines(plain: &[u8]) -> Vec<(usize, crate::jsonscan::MetaLine)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < plain.len() {
        let end = plain[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(plain.len(), |p| start + p);
        let line = &plain[start..end];
        if may_be_session_meta(line)
            && let Some(meta) = inspect_session_meta(line)
        {
            out.push((start, meta));
        }
        start = end + 1;
    }
    out
}

/// Compute the edits moving every `session_meta` of `plain` whose provider is in
/// `sources` to `to`. A missing (or `null`) provider counts as
/// [`DEFAULT_PROVIDER`]; a missing one is inserted as the payload's last member.
pub(crate) fn provider_edits(plain: &[u8], sources: &BTreeSet<String>, to: &str) -> Vec<Edit> {
    let default_selected = sources.contains(DEFAULT_PROVIDER);
    let token = json_string_token(to);
    let mut edits = Vec::new();
    for (start, meta) in meta_lines(plain) {
        let replace = |span: &std::ops::Range<usize>| Edit {
            at: (start + span.start) as u64,
            old: String::from_utf8_lossy(&plain[start + span.start..start + span.end]).into_owned(),
            new: token.clone(),
        };
        match (&meta.provider, &meta.provider_scalar_span) {
            // A string provider equal to `from`.
            (Some(provider), Some(span)) if sources.contains(provider) && provider != to => {
                edits.push(replace(span))
            }
            // `null` (or another scalar Codex reads as "no provider", i.e. the
            // default): replace the token, never add a duplicate member.
            (None, Some(span)) if default_selected => edits.push(replace(span)),
            // No member at all: append one as the payload's last member.
            (None, None) if !meta.provider_present && default_selected => {
                let sep = if meta.payload_nonempty { "," } else { "" };
                edits.push(Edit {
                    at: (start + meta.payload_end) as u64,
                    old: String::new(),
                    new: format!("{sep}\"model_provider\":{token}"),
                });
            }
            // Object/array values are left alone (Codex cannot read them either).
            _ => {}
        }
    }
    edits
}

/// Thread id of the first `session_meta` of `plain`.
pub(crate) fn first_thread_id(plain: &[u8]) -> Option<String> {
    meta_lines(plain).into_iter().next().and_then(|(_, m)| m.id)
}

/// A computed rewrite of one rollout (nothing written yet).
pub(crate) struct Rewrite {
    /// Bytes on disk now.
    pub raw_before: Vec<u8>,
    /// Bytes to write.
    pub raw_after: Vec<u8>,
    /// Edits in the uncompressed content.
    pub edits: Vec<Edit>,
    /// SHA-256 of the uncompressed content before.
    pub plain_before_sha: String,
    /// SHA-256 of the uncompressed content after.
    pub plain_after_sha: String,
    /// Fingerprint the file must still have when it is replaced.
    pub fingerprint: Fingerprint,
    /// Metadata to copy (permissions).
    pub meta: fs::Metadata,
}

/// Provider ids an edit replaces (`null`/missing count as the default).
pub(crate) fn replaced_providers(edits: &[Edit]) -> BTreeSet<String> {
    edits
        .iter()
        .map(|e| {
            serde_json::from_str::<String>(&e.old).unwrap_or_else(|_| DEFAULT_PROVIDER.to_owned())
        })
        .collect()
}

/// `x.jsonl` for both `x.jsonl` and `x.jsonl.zst`.
pub(crate) fn logical_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_suffix(".zst") {
        Some(plain) if plain.ends_with(PLAIN_SUFFIX) => PathBuf::from(plain),
        _ => path.to_path_buf(),
    }
}

/// Plan the provider rewrite of `path` (thread `thread_id`) from any of
/// `sources` to `to`. `Ok(None)` when no `session_meta` of the file needs it.
pub(crate) fn plan_rewrite(
    path: &Path,
    thread_id: &str,
    sources: &BTreeSet<String>,
    to: &str,
) -> Result<Option<Rewrite>> {
    let loaded = load(path)?;
    if first_thread_id(&loaded.plain).as_deref() != Some(thread_id) {
        return Err(conflict(&format!(
            "{} no longer belongs to thread {thread_id}",
            path.display()
        )));
    }
    let edits = provider_edits(&loaded.plain, sources, to);
    if edits.is_empty() {
        return Ok(None);
    }
    let plain_after = apply_edits(&loaded.plain, &edits)?;
    // Paranoia: no session_meta may still carry `from`, and every edited one must
    // now carry `to`.
    let leftover = provider_edits(&plain_after, sources, to);
    let now_to = meta_lines(&plain_after)
        .iter()
        .filter(|(_, m)| m.provider.as_deref() == Some(to))
        .count();
    if !leftover.is_empty() || now_to < edits.len() {
        return Err(Error::other(format!(
            "rewriting {} did not produce the expected provider",
            path.display()
        )));
    }
    let raw_after = encode(path, &plain_after)?;
    Ok(Some(Rewrite {
        plain_before_sha: sha256_hex(&loaded.plain),
        plain_after_sha: sha256_hex(&plain_after),
        raw_before: loaded.raw,
        raw_after,
        edits,
        fingerprint: loaded.fingerprint,
        meta: loaded.meta,
    }))
}

/// Replace `path` with `bytes` if it still has `fingerprint` and its content still
/// hashes to `expected_sha` (an edit that keeps size and mtime is caught too);
/// keep its permissions and set its modification time to `mtime`.
pub(crate) fn replace_guarded(
    path: &Path,
    bytes: &[u8],
    expected_sha: &str,
    fingerprint: &Fingerprint,
    meta: &fs::Metadata,
    mtime: Option<SystemTime>,
) -> Result<()> {
    let changed = || conflict(&format!("{} changed during the migration", path.display()));
    if Fingerprint::of(path)? != *fingerprint {
        return Err(changed());
    }
    let current = fs::read(path).map_err(|e| io_err(path, &e))?;
    if sha256_hex(&current) != expected_sha {
        return Err(changed());
    }
    atomic_replace(path, bytes, Some(meta), mtime)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn null_provider_is_replaced_and_logical_paths() {
        let line = r#"{"type":"session_meta","payload":{"id":"t1","model_provider":null}}"#;
        let edits = provider_edits(line.as_bytes(), &set(&["openai"]), "krill");
        assert_eq!(
            String::from_utf8(apply_edits(line.as_bytes(), &edits).unwrap()).unwrap(),
            r#"{"type":"session_meta","payload":{"id":"t1","model_provider":"krill"}}"#
        );
        assert_eq!(replaced_providers(&edits), set(&["openai"]));
        assert!(provider_edits(line.as_bytes(), &set(&["krill"]), "openai").is_empty());
        assert_eq!(
            logical_path(Path::new("/a/rollout-x.jsonl.zst")),
            PathBuf::from("/a/rollout-x.jsonl")
        );
        assert_eq!(
            logical_path(Path::new("/a/rollout-x.jsonl")),
            PathBuf::from("/a/rollout-x.jsonl")
        );
    }

    const META_KRILL: &str = r#"{"timestamp":"2026-10-16T07:35:26.660Z","ordinal":0,"type":"session_meta","payload":{"id":"t1","timestamp":"2026-10-16T07:35:26.266Z","model_provider":"krill","base_instructions":{"text":"say \"model_provider\":\"krill\""}}}"#;

    #[test]
    fn provider_edit_touches_only_the_value_token() {
        let content = format!("{META_KRILL}\n{{\"type\":\"event_msg\",\"payload\":{{}}}}\n");
        let edits = provider_edits(content.as_bytes(), &set(&["krill"]), "openai");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].old, "\"krill\"");
        assert_eq!(edits[0].new, "\"openai\"");
        let after = apply_edits(content.as_bytes(), &edits).unwrap();
        let expected = content.replacen(
            "\"model_provider\":\"krill\"",
            "\"model_provider\":\"openai\"",
            1,
        );
        assert_eq!(String::from_utf8(after.clone()).unwrap(), expected);
        let back = apply_edits(&after, &inverse_edits(&edits)).unwrap();
        assert_eq!(back, content.as_bytes());
    }

    #[test]
    fn missing_provider_is_default_and_inserted() {
        let line = r#"{"type":"session_meta","payload":{"id":"t1"}}"#;
        assert!(provider_edits(line.as_bytes(), &set(&["krill"]), "openai").is_empty());
        let edits = provider_edits(line.as_bytes(), &set(&["openai"]), "krill");
        let after = apply_edits(line.as_bytes(), &edits).unwrap();
        assert_eq!(
            String::from_utf8(after).unwrap(),
            r#"{"type":"session_meta","payload":{"id":"t1","model_provider":"krill"}}"#
        );
        let empty = r#"{"type":"session_meta","payload":{}}"#;
        let edits = provider_edits(empty.as_bytes(), &set(&["openai"]), "x");
        assert_eq!(
            String::from_utf8(apply_edits(empty.as_bytes(), &edits).unwrap()).unwrap(),
            r#"{"type":"session_meta","payload":{"model_provider":"x"}}"#
        );
    }

    #[test]
    fn offsets_after_an_edit_shift_by_its_delta() {
        let edits = vec![Edit {
            at: 10,
            old: "\"krill\"".into(),
            new: "\"openai\"".into(),
        }];
        assert_eq!(shift_offset(&edits, 0), Some(0));
        assert_eq!(shift_offset(&edits, 10), Some(10));
        assert_eq!(shift_offset(&edits, 11), Some(12));
        assert_eq!(shift_offset(&edits, 500), Some(501));
        assert_eq!(shift_offset(&edits, i64::MAX), None);
        let two = vec![
            Edit {
                at: 5,
                old: "ab".into(),
                new: "a".into(),
            },
            Edit {
                at: 50,
                old: "".into(),
                new: "xyz".into(),
            },
        ];
        assert_eq!(shift_offset(&two, 20), Some(19));
        assert_eq!(shift_offset(&two, 60), Some(62));
        let inv = inverse_edits(&two);
        assert_eq!(inv[0].at, 5);
        assert_eq!(inv[1].at, 49);
    }

    #[test]
    fn apply_edits_rejects_mismatch() {
        let edits = vec![Edit {
            at: 0,
            old: "zz".into(),
            new: "y".into(),
        }];
        assert!(apply_edits(b"ab", &edits).is_err());
        let out_of_range = vec![Edit {
            at: 5,
            old: "a".into(),
            new: "b".into(),
        }];
        assert!(apply_edits(b"ab", &out_of_range).is_err());
    }

    #[test]
    fn rollout_names() {
        assert!(is_rollout_name("rollout-2026-10-16T15-35-26-x.jsonl"));
        assert!(is_rollout_name("rollout-2026-10-16T15-35-26-x.jsonl.zst"));
        assert!(!is_rollout_name("rollout-x.zst"));
        assert!(!is_rollout_name("history.jsonl"));
        assert!(is_compressed(Path::new("/a/rollout-x.jsonl.zst")));
        assert!(!is_compressed(Path::new("/a/rollout-x.jsonl")));
    }

    #[test]
    fn replace_guarded_refuses_same_size_same_mtime_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-x.jsonl");
        fs::write(&path, b"abc\n").unwrap();
        let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let set_mtime = |p: &Path| {
            File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(mtime))
                .unwrap();
        };
        set_mtime(&path);
        let fp = Fingerprint::of(&path).unwrap();
        let meta = fs::metadata(&path).unwrap();
        let sha = sha256_hex(b"abc\n");
        // Same size, same mtime, different content.
        fs::write(&path, b"xyz\n").unwrap();
        set_mtime(&path);
        assert_eq!(Fingerprint::of(&path).unwrap(), fp);
        let err = replace_guarded(&path, b"new\n", &sha, &fp, &meta, Some(mtime)).unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert_eq!(fs::read(&path).unwrap(), b"xyz\n");
        // Unchanged content is replaced.
        fs::write(&path, b"abc\n").unwrap();
        set_mtime(&path);
        replace_guarded(&path, b"new\n", &sha, &fp, &meta, Some(mtime)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new\n");
    }
}
