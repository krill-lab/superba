//! The check of an edited Codex file (GitHub issue 9).
//!
//! - Syntax: TOML (`toml_edit`) or JSON (`serde_json`), with the line and
//!   column of the error. A file with a syntax error is never saved: Codex
//!   could not start with it, and Superba refuses to touch it.
//! - `config.toml` against the settings catalog of every Codex that reads the
//!   home (the settings engine's own rules, `key_support` and
//!   `validate_value`): a value of the wrong type is an error (saving it needs
//!   an explicit "save anyway", the catalog may lag a newer Codex); a key no
//!   Codex reading the home knows, a deprecated key, a key without effect and
//!   an old name are warnings; keys Superba manages are marked.
//! - `auth.json`: the fields Codex reads and their types, a file without any
//!   credential, and placeholders of hidden secrets the user changed only
//!   partly.
//!
//! Messages never quote a value: `auth.json` holds keys, `config.toml` may.

use std::collections::BTreeMap;

use superba_core::{
    CatalogKey, CatalogSnapshot, CodexFileKind, FileProblem, FileSchema, KeySupport, LegacyKind,
    LocalizedText, OwnedKey, ProblemKind, ProblemSeverity, SettingKind, SettingValue,
    SettingsConsumer,
};

use super::json;
use super::mask::{self, Masker};
use crate::settings::{key_support, validate_value};

/// What the check knows besides the text.
#[derive(Clone, Copy, Debug, Default)]
pub struct CheckContext<'a> {
    /// The Codex versions reading the home and their catalog (`config.toml`).
    pub schema: Option<&'a FileSchema>,
    /// Keys Superba manages in the file (marked as information).
    pub owned: &'a [OwnedKey],
    /// The provider those keys are managed for (display name).
    pub owner: Option<&'a str>,
    /// The placeholders of hidden secrets in the text (`auth.json` shown
    /// masked): they are not partly changed secrets.
    pub masker: Option<&'a Masker>,
}

/// Check `text` as the given file. Problems are sorted by position.
pub fn validate(kind: CodexFileKind, text: &str, cx: &CheckContext<'_>) -> Vec<FileProblem> {
    let mut out = match kind {
        CodexFileKind::ConfigToml => validate_toml(text, cx),
        CodexFileKind::AuthJson => validate_json(text, cx),
    };
    out.sort_by(|a, b| {
        (a.line, a.column, a.severity)
            .cmp(&(b.line, b.column, b.severity))
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// No problem keeps the text from being saved and none needs "save anyway".
pub fn clean(problems: &[FileProblem]) -> bool {
    !problems.iter().any(|p| p.blocks_save() || p.needs_force())
}

/// 1-based line and column (in characters) of byte `at` in `text`.
pub fn line_col(text: &str, at: usize) -> (usize, usize) {
    let at = floor_char_boundary(text, at.min(text.len()));
    let before = &text[..at];
    let line = before.matches('\n').count() + 1;
    let col = before
        .rsplit('\n')
        .next()
        .map_or(0, |l| l.trim_end_matches('\r').chars().count())
        + 1;
    (line, col)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

fn problem(
    src: &str,
    span: std::ops::Range<usize>,
    severity: ProblemSeverity,
    kind: ProblemKind,
    path: Option<String>,
    message: LocalizedText,
) -> FileProblem {
    let (line, column) = line_col(src, span.start);
    FileProblem {
        severity,
        kind,
        line,
        column,
        start: span.start,
        end: span.end.max(span.start),
        path,
        message,
    }
}

pub(crate) fn internal_problem(key: &str, fallback: &str) -> FileProblem {
    FileProblem {
        severity: ProblemSeverity::Error,
        kind: ProblemKind::Syntax,
        line: 1,
        column: 1,
        start: 0,
        end: 0,
        path: None,
        message: LocalizedText::new(key, fallback),
    }
}

/// The syntax error of a TOML text (the parser's description, no excerpt of
/// the file).
pub(crate) fn toml_syntax_problem(src: &str, e: &toml_edit::TomlError) -> FileProblem {
    let span = e.span().unwrap_or(0..0);
    let message = e.message().lines().next().unwrap_or("").trim().to_owned();
    problem(
        src,
        span,
        ProblemSeverity::Error,
        ProblemKind::Syntax,
        None,
        text("codex_files.toml_syntax", "TOML syntax error: {message}").arg("message", message),
    )
}

/// The syntax error of a JSON text.
pub(crate) fn json_syntax_problem(src: &str, e: &serde_json::Error) -> FileProblem {
    let (line, column) = (e.line().max(1), e.column().max(1));
    // serde_json's column counts bytes; find the byte offset for underlining.
    let start = src
        .split_inclusive('\n')
        .take(line - 1)
        .map(str::len)
        .sum::<usize>()
        + column.saturating_sub(1);
    let start = floor_char_boundary(src, start.min(src.len()));
    let (line, column) = line_col(src, start);
    let what = match e.classify() {
        serde_json::error::Category::Eof => text("codex_files.json_eof", "the file ends too early"),
        _ => {
            // "expected `,` or `}` at line 3 column 5" -> without the position.
            let msg = e.to_string();
            let msg = msg
                .rsplit_once(" at line ")
                .map_or(msg.as_str(), |(m, _)| m)
                .to_owned();
            text("codex_files.json_detail", "{detail}").arg("detail", msg)
        }
    };
    FileProblem {
        severity: ProblemSeverity::Error,
        kind: ProblemKind::Syntax,
        line,
        column,
        start,
        end: (start + 1).min(src.len()),
        path: None,
        message: text("codex_files.json_syntax", "JSON syntax error: {detail}")
            .arg_text("detail", what),
    }
}

// ------------------------------------------------------------- config.toml

/// A table, a leaf value, or an array of tables at a dotted path.
struct Found {
    path: String,
    key_span: std::ops::Range<usize>,
    value: Option<SettingValue>,
    is_table: bool,
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Every key of the document with the span of its name, in document order of
/// the parser (parents before children).
fn collect(table: &toml_edit::Table, values: &toml::Table, prefix: &str, out: &mut Vec<Found>) {
    for (key, item) in table.iter() {
        let path = join(prefix, key);
        let span = table
            .key(key)
            .and_then(toml_edit::Key::span)
            .or_else(|| item.span())
            .unwrap_or(0..0);
        let v = values.get(key);
        let value = v.and_then(SettingValue::from_toml);
        match item {
            toml_edit::Item::Table(sub) => {
                out.push(Found {
                    path: path.clone(),
                    key_span: span,
                    value,
                    is_table: true,
                });
                if let Some(toml::Value::Table(vt)) = v {
                    collect(sub, vt, &path, out);
                }
            }
            toml_edit::Item::Value(toml_edit::Value::InlineTable(it)) => {
                out.push(Found {
                    path: path.clone(),
                    key_span: span,
                    value,
                    is_table: true,
                });
                if let Some(toml::Value::Table(vt)) = v {
                    collect_inline(it, vt, &path, out);
                }
            }
            _ => out.push(Found {
                path,
                key_span: span,
                value,
                is_table: false,
            }),
        }
    }
}

/// The keys of an inline table.
fn collect_inline(
    it: &toml_edit::InlineTable,
    values: &toml::Table,
    prefix: &str,
    out: &mut Vec<Found>,
) {
    for (key, value) in it.iter() {
        let path = join(prefix, key);
        let span = it
            .key(key)
            .and_then(toml_edit::Key::span)
            .or_else(|| value.span())
            .unwrap_or(0..0);
        let v = values.get(key);
        let is_table = matches!(value, toml_edit::Value::InlineTable(_));
        out.push(Found {
            path: path.clone(),
            key_span: span,
            value: v.and_then(SettingValue::from_toml),
            is_table,
        });
        if let (toml_edit::Value::InlineTable(inner), Some(toml::Value::Table(vt))) = (value, v) {
            collect_inline(inner, vt, &path, out);
        }
    }
}

/// A consumer with its catalog snapshot.
struct Seen {
    consumer: SettingsConsumer,
    snapshot: std::sync::Arc<CatalogSnapshot>,
}

fn consumer_label(c: &SettingsConsumer) -> String {
    match &c.version {
        Some(v) => format!("{} {v}", c.label),
        None => c.label.clone(),
    }
}

fn support_label(s: &superba_core::ConsumerSupport) -> String {
    match &s.version {
        Some(v) => format!("{} {v}", s.label),
        None => s.label.clone(),
    }
}

/// A catalog key starts with `path` (pattern segments `<name>` match any
/// segment): the table is known, only its keys need a look.
fn known_prefix(s: &CatalogSnapshot, path: &str) -> bool {
    if s.key(path).is_some() || s.open_ancestor(path).is_some() {
        return true;
    }
    let segs: Vec<&str> = path.split('.').collect();
    s.keys.keys().any(|p| {
        let pat: Vec<&str> = p.split('.').collect();
        pat.len() > segs.len()
            && pat
                .iter()
                .zip(&segs)
                .all(|(a, b)| a == b || (a.starts_with('<') && a.ends_with('>')))
    })
}

/// A key that takes any content: nothing below it is checked.
fn free_form(s: &CatalogSnapshot, path: &str) -> bool {
    s.open_ancestor(path).is_some()
        || s.key(path)
            .is_some_and(|k| matches!(k.kind, SettingKind::Any | SettingKind::Recursive))
}

fn validate_toml(src: &str, cx: &CheckContext<'_>) -> Vec<FileProblem> {
    let doc = match toml_edit::Document::parse(src) {
        Ok(d) => d,
        Err(e) => return vec![toml_syntax_problem(src, &e)],
    };
    let values: toml::Table = toml::from_str(src).unwrap_or_default();
    let mut found = Vec::new();
    collect(doc.as_table(), &values, "", &mut found);
    let mut out = Vec::new();
    for f in &found {
        if let Some(o) = cx.owned.iter().find(|o| o.path == f.path) {
            out.push(problem(
                src,
                f.key_span.clone(),
                ProblemSeverity::Info,
                ProblemKind::Owned,
                Some(o.path.clone()),
                superba_core::codex_files::texts::owned_key(cx.owner),
            ));
        }
    }
    let Some(schema) = cx.schema else {
        return out;
    };
    let Some(catalog) = schema.catalog.as_deref() else {
        return out;
    };
    let seen: Vec<Seen> = schema
        .consumers
        .iter()
        .filter_map(|c| {
            Some(Seen {
                consumer: c.clone(),
                snapshot: catalog.snapshot(c.version.as_ref())?,
            })
        })
        .collect();
    if seen.is_empty() {
        return out;
    }
    // Paths below which nothing more is checked (unknown or free-form tables).
    let mut skip: Vec<String> = Vec::new();
    for f in &found {
        if skip.iter().any(|s| {
            f.path
                .strip_prefix(s.as_str())
                .is_some_and(|r| r.starts_with('.'))
        }) {
            continue;
        }
        let supports = key_support(&f.path, &schema.consumers, catalog);
        let all_missing =
            !supports.is_empty() && supports.iter().all(|s| s.support == KeySupport::Missing);
        if f.is_table {
            if seen.iter().any(|s| free_form(&s.snapshot, &f.path)) {
                skip.push(f.path.clone());
                check_type(src, f, &seen, &mut out);
                continue;
            }
            if !seen.iter().any(|s| known_prefix(&s.snapshot, &f.path)) {
                if all_missing {
                    out.push(unknown_key(src, f, &seen));
                }
                skip.push(f.path.clone());
                continue;
            }
            // A known table: its own type, then its keys one by one.
            if seen.iter().any(|s| s.snapshot.key(&f.path).is_some()) {
                check_type(src, f, &seen, &mut out);
            }
            continue;
        }
        if all_missing {
            out.push(unknown_key(src, f, &seen));
            continue;
        }
        check_support(src, f, &supports, &seen, &mut out);
        check_type(src, f, &seen, &mut out);
    }
    out
}

fn unknown_key(src: &str, f: &Found, seen: &[Seen]) -> FileProblem {
    let who: Vec<String> = seen.iter().map(|s| consumer_label(&s.consumer)).collect();
    problem(
        src,
        f.key_span.clone(),
        ProblemSeverity::Warning,
        ProblemKind::Schema,
        Some(f.path.clone()),
        text(
            "codex_files.unknown_key",
            "{path} is not a setting of {codex}: Codex ignores it (check the spelling).",
        )
        .arg("path", &f.path)
        .arg("codex", who.join(", ")),
    )
}

/// Missing in some Codex, deprecated, without effect, an old name.
fn check_support(
    src: &str,
    f: &Found,
    supports: &[superba_core::ConsumerSupport],
    seen: &[Seen],
    out: &mut Vec<FileProblem>,
) {
    let mut legacy_done = false;
    for s in supports {
        let codex = support_label(s);
        let msg = match s.support {
            KeySupport::Missing => Some(
                text(
                    "codex_files.not_in_version",
                    "{codex} does not know {path}: it ignores it.",
                )
                .arg("path", &f.path)
                .arg("codex", &codex),
            ),
            KeySupport::Deprecated => Some(
                text("codex_files.deprecated", "{path} is deprecated in {codex}.")
                    .arg("path", &f.path)
                    .arg("codex", &codex),
            ),
            KeySupport::Removed => Some(
                text(
                    "codex_files.removed",
                    "{path} has no effect any more in {codex}.",
                )
                .arg("path", &f.path)
                .arg("codex", &codex),
            ),
            KeySupport::Legacy if !legacy_done => {
                legacy_done = true;
                let canonical = seen.iter().find_map(|s| {
                    s.snapshot.legacy_rule(&f.path).and_then(|r| match &r.kind {
                        LegacyKind::Alias { canonical } | LegacyKind::Replace { canonical, .. } => {
                            Some(canonical.clone())
                        }
                        _ => None,
                    })
                });
                Some(match canonical {
                    Some(c) => text(
                        "codex_files.legacy_named",
                        "{path} is an old name: use {canonical}.",
                    )
                    .arg("path", &f.path)
                    .arg("canonical", c),
                    None => text(
                        "codex_files.legacy",
                        "{path} is an old name Codex still reads.",
                    )
                    .arg("path", &f.path),
                })
            }
            _ => None,
        };
        if let Some(m) = msg {
            out.push(problem(
                src,
                f.key_span.clone(),
                ProblemSeverity::Warning,
                ProblemKind::Schema,
                Some(f.path.clone()),
                m,
            ));
        }
    }
}

/// The value against the key of every Codex that knows it, newest first (one
/// message per key).
fn check_type(src: &str, f: &Found, seen: &[Seen], out: &mut Vec<FileProblem>) {
    let Some(value) = &f.value else {
        return;
    };
    let mut known: Vec<&Seen> = seen.iter().collect();
    known.sort_by(|a, b| b.snapshot.codex_version.cmp(&a.snapshot.codex_version));
    for (i, s) in known.iter().enumerate() {
        let snap = &s.snapshot;
        let Some(key) = snap.key(&f.path).cloned().or_else(|| {
            snap.feature_for_path(&f.path)
                .filter(|feat| feat.config_path == f.path)
                .map(|_| CatalogKey::new(f.path.clone(), SettingKind::BoolOrTable))
        }) else {
            continue;
        };
        if let Err(expected) = validate_value(&key, value) {
            let m = if i == 0 {
                text("codex_files.wrong_type", "{path}: expected {expected}.")
                    .arg("path", &f.path)
                    .arg("expected", &expected)
            } else {
                text(
                    "codex_files.wrong_type_for",
                    "{path}: {codex} expects {expected}.",
                )
                .arg("path", &f.path)
                .arg("codex", consumer_label(&s.consumer))
                .arg("expected", &expected)
            };
            out.push(problem(
                src,
                f.key_span.clone(),
                ProblemSeverity::Error,
                ProblemKind::Schema,
                Some(f.path.clone()),
                m,
            ));
            return;
        }
    }
}

// --------------------------------------------------------------- auth.json

/// Fields Codex reads in `auth.json` and the JSON kinds each accepts.
const AUTH_FIELDS: &[(&str, &[json::Kind])] = &[
    ("OPENAI_API_KEY", &[json::Kind::String, json::Kind::Null]),
    (
        "personal_access_token",
        &[json::Kind::String, json::Kind::Null],
    ),
    ("auth_mode", &[json::Kind::String, json::Kind::Null]),
    ("tokens", &[json::Kind::Object, json::Kind::Null]),
    ("last_refresh", &[json::Kind::String, json::Kind::Null]),
    ("tokens.id_token", &[json::Kind::String]),
    ("tokens.access_token", &[json::Kind::String]),
    ("tokens.refresh_token", &[json::Kind::String]),
    ("tokens.account_id", &[json::Kind::String, json::Kind::Null]),
];

fn kinds_text(kinds: &[json::Kind]) -> String {
    kinds
        .iter()
        .map(|k| k.name())
        .collect::<Vec<_>>()
        .join(" or ")
}

fn validate_json(src: &str, cx: &CheckContext<'_>) -> Vec<FileProblem> {
    if src.trim().is_empty() {
        return vec![problem(
            src,
            0..0,
            ProblemSeverity::Error,
            ProblemKind::Syntax,
            None,
            text(
                "codex_files.auth_empty",
                "auth.json is empty: Codex would ask you to sign in again. Write a JSON object, or restore a backup.",
            ),
        )];
    }
    let value: serde_json::Value = match serde_json::from_str(src) {
        Ok(v) => v,
        Err(e) => return vec![json_syntax_problem(src, &e)],
    };
    if !value.is_object() {
        return vec![problem(
            src,
            0..src.len().min(1),
            ProblemSeverity::Error,
            ProblemKind::Syntax,
            None,
            text(
                "codex_files.auth_not_object",
                "auth.json must be a JSON object.",
            ),
        )];
    }
    let members = json::members(src);
    let mut out = Vec::new();
    let mut by_path: BTreeMap<String, &json::Member> = BTreeMap::new();
    for m in &members {
        by_path.entry(m.dotted()).or_insert(m);
    }
    for m in &members {
        let path = m.dotted();
        // Only the top level and the login's tokens are Codex's.
        if m.path.len() > 2 || (m.path.len() == 2 && m.path[0] != "tokens") {
            continue;
        }
        match AUTH_FIELDS.iter().find(|(p, _)| *p == path) {
            Some((_, kinds)) => {
                if !kinds.contains(&m.kind) {
                    out.push(problem(
                        src,
                        m.key.clone(),
                        ProblemSeverity::Error,
                        ProblemKind::Auth,
                        Some(path.clone()),
                        text(
                            "codex_files.auth_wrong_type",
                            "{field}: expected {expected}, found {found}.",
                        )
                        .arg("field", &path)
                        .arg("expected", kinds_text(kinds))
                        .arg("found", m.kind.name()),
                    ));
                }
            }
            None => out.push(problem(
                src,
                m.key.clone(),
                ProblemSeverity::Warning,
                ProblemKind::Auth,
                Some(path.clone()),
                text(
                    "codex_files.auth_unknown_field",
                    "Codex does not read {field}.",
                )
                .arg("field", &path),
            )),
        }
        if m.kind == json::Kind::String && mask::is_secret_name(m.name()) {
            let raw = &src[m.value.start + 1..m.value.end.saturating_sub(1).max(m.value.start + 1)];
            if mask::has_mask(raw) && !cx.masker.is_some_and(|mk| mk.knows(raw)) {
                out.push(problem(
                    src,
                    m.value.clone(),
                    ProblemSeverity::Error,
                    ProblemKind::Secret,
                    Some(path.clone()),
                    text(
                        "codex_files.secret_partly_changed",
                        "The hidden value of {field} was changed only partly. Show the secrets to edit it, or undo the change.",
                    )
                    .arg("field", &path),
                ));
            }
        }
        if path == "last_refresh"
            && m.kind == json::Kind::String
            && let Some(s) = value.get("last_refresh").and_then(|v| v.as_str())
            && crate::authjson::parse_rfc3339(s).is_none()
        {
            out.push(problem(
                src,
                m.value.clone(),
                ProblemSeverity::Warning,
                ProblemKind::Auth,
                Some(path.clone()),
                text(
                    "codex_files.auth_bad_time",
                    "last_refresh is not a date and time like 2026-10-18T09:30:00Z.",
                ),
            ));
        }
    }
    // A file Codex finds no credential in.
    let non_empty = |k: &str| {
        value
            .get(k)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    };
    let has_tokens = value
        .get("tokens")
        .is_some_and(serde_json::Value::is_object);
    if !has_tokens && !non_empty("OPENAI_API_KEY") && !non_empty("personal_access_token") {
        out.push(problem(
            src,
            0..src.len().min(1),
            ProblemSeverity::Warning,
            ProblemKind::Auth,
            None,
            text(
                "codex_files.auth_no_credential",
                "auth.json holds no credential (no API key, PAT or ChatGPT login): Codex will ask you to sign in.",
            ),
        ));
    }
    if has_tokens {
        for field in ["id_token", "access_token", "refresh_token"] {
            if !by_path.contains_key(&format!("tokens.{field}")) {
                out.push(problem(
                    src,
                    by_path.get("tokens").map_or(0..0, |m| m.key.clone()),
                    ProblemSeverity::Warning,
                    ProblemKind::Auth,
                    Some("tokens".into()),
                    text(
                        "codex_files.auth_tokens_missing",
                        "tokens has no {field}: the ChatGPT login may not work.",
                    )
                    .arg("field", field),
                ));
            }
        }
    }
    out
}
