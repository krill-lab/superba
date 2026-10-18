//! "Format" in the file editor.
//!
//! `config.toml` is tidied with `toml_edit` without touching what it means or
//! any comment: keys start their line, ` = ` between key and value, one space
//! before a trailing comment, no trailing spaces, at most one blank line in a
//! row, one blank line before each `[table]` header, `[a.b]` without spaces,
//! and one line break at the end. Values, arrays, inline tables and strings
//! are left exactly as written. The result is parsed again and must hold the
//! same data, or formatting is refused. Line endings (all CRLF or LF) and the
//! byte order mark are the caller's business: the text comes in with LF.
//!
//! `auth.json` is pretty-printed by `serde_json` (two spaces, key order kept),
//! as Codex writes it.

use superba_core::{CodexFileKind, FileProblem};
use toml_edit::{DocumentMut, Item, Table};

use super::validate;

/// Format `text` as the given file kind; a syntax problem when it does not
/// parse.
pub fn format(kind: CodexFileKind, text: &str) -> Result<String, Box<FileProblem>> {
    match kind {
        CodexFileKind::ConfigToml => format_toml(text),
        CodexFileKind::AuthJson => format_json(text),
    }
}

/// Format `config.toml` text, keeping comments (see the module docs).
pub fn format_toml(text: &str) -> Result<String, Box<FileProblem>> {
    let crlf = uses_crlf(text);
    let lf = text.replace("\r\n", "\n");
    let mut doc: DocumentMut = match lf.parse() {
        Ok(d) => d,
        Err(e) => return Err(Box::new(validate::toml_syntax_problem(&lf, &e))),
    };
    let root = doc.as_table_mut();
    // The first thing in the file gets no blank line before it.
    let root_has_values = root.iter().any(|(_, item)| item.is_value());
    let first_table = if root_has_values {
        None
    } else {
        first_header_position(root)
    };
    let mut first = FirstItem {
        pending: root_has_values,
        table_position: first_table,
    };
    format_table(root, &mut first, true);
    let trailing = doc
        .trailing()
        .as_str()
        .map(normalize_trailing)
        .unwrap_or_default();
    doc.set_trailing(trailing);
    let mut out = doc.to_string();
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    if !out.is_empty() {
        out.push('\n');
    }
    // Same data, or refuse.
    if !same_data(&lf, &out) {
        return Err(Box::new(validate::internal_problem(
            "codex_files.format_changed",
            "Formatting would change what the file means; it was left as it is.",
        )));
    }
    if crlf {
        out = out.replace('\n', "\r\n");
    }
    Ok(out)
}

/// Format `auth.json` text (two spaces, key order kept, final line break).
pub fn format_json(text: &str) -> Result<String, Box<FileProblem>> {
    let crlf = uses_crlf(text);
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Err(Box::new(validate::json_syntax_problem(text, &e))),
    };
    let mut out = serde_json::to_string_pretty(&value).map_err(|_| {
        Box::new(validate::internal_problem(
            "codex_files.format_failed",
            "The file could not be formatted.",
        ))
    })?;
    out.push('\n');
    if crlf {
        out = out.replace('\n', "\r\n");
    }
    Ok(out)
}

/// Every line break of `text` is CRLF (and there is at least one).
pub fn uses_crlf(text: &str) -> bool {
    let lf = text.matches('\n').count();
    lf > 0 && text.matches("\r\n").count() == lf
}

fn same_data(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<String> {
        let v: toml::Table = toml::from_str(s).ok()?;
        toml::to_string(&v).ok()
    };
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Where the first `[table]` header of a document without top-level values is.
fn first_header_position(root: &Table) -> Option<isize> {
    fn walk(t: &Table, best: &mut Option<isize>) {
        for (_, item) in t.iter() {
            match item {
                Item::Table(sub) => {
                    if !sub.is_implicit()
                        && !sub.is_dotted()
                        && let Some(p) = sub.position()
                    {
                        *best = Some(best.map_or(p, |b| b.min(p)));
                    }
                    walk(sub, best);
                }
                Item::ArrayOfTables(arr) => {
                    for sub in arr.iter() {
                        if let Some(p) = sub.position() {
                            *best = Some(best.map_or(p, |b| b.min(p)));
                        }
                        walk(sub, best);
                    }
                }
                _ => {}
            }
        }
    }
    let mut best = None;
    walk(root, &mut best);
    best
}

/// Which item is the first of the document.
struct FirstItem {
    /// The first top-level value has not been formatted yet.
    pending: bool,
    /// Position of the first header (no top-level values).
    table_position: Option<isize>,
}

fn format_table(t: &mut Table, first: &mut FirstItem, at_root: bool) {
    let mut first_in_table = true;
    for (mut key, item) in t.iter_mut() {
        match item {
            Item::Value(value) => {
                let lead = if at_root && first.pending {
                    first.pending = false;
                    Lead::None
                } else if first_in_table {
                    Lead::None
                } else {
                    Lead::KeepOne
                };
                first_in_table = false;
                let decor = key.leaf_decor_mut();
                let prefix = decor.prefix().and_then(|r| r.as_str()).unwrap_or("");
                let prefix = normalize_prefix(prefix, lead);
                decor.set_prefix(prefix);
                decor.set_suffix(" ");
                let vd = value.decor_mut();
                let suffix = vd.suffix().and_then(|r| r.as_str()).unwrap_or("");
                let suffix = normalize_suffix(suffix);
                vd.set_prefix(" ");
                vd.set_suffix(suffix);
            }
            Item::Table(sub) => {
                if sub.is_dotted() {
                    let d = key.dotted_decor_mut();
                    d.set_prefix("");
                    d.set_suffix("");
                    // Lines of a dotted table belong to the table they are in.
                    format_dotted(sub, first, at_root, &mut first_in_table);
                } else {
                    let d = key.leaf_decor_mut();
                    d.set_prefix("");
                    d.set_suffix("");
                    let d = key.dotted_decor_mut();
                    d.set_prefix("");
                    d.set_suffix("");
                    if !sub.is_implicit() {
                        format_header(sub, first);
                    }
                    format_table(sub, first, false);
                }
            }
            Item::ArrayOfTables(arr) => {
                let d = key.leaf_decor_mut();
                d.set_prefix("");
                d.set_suffix("");
                for sub in arr.iter_mut() {
                    format_header(sub, first);
                    format_table(sub, first, false);
                }
            }
            Item::None => {}
        }
    }
}

/// The key/value lines of a dotted table (`a.b = 1`), formatted like the
/// lines of the table holding them.
fn format_dotted(t: &mut Table, first: &mut FirstItem, at_root: bool, first_in_table: &mut bool) {
    for (mut key, item) in t.iter_mut() {
        match item {
            Item::Value(value) => {
                let lead = if at_root && first.pending {
                    first.pending = false;
                    Lead::None
                } else if *first_in_table {
                    Lead::None
                } else {
                    Lead::KeepOne
                };
                *first_in_table = false;
                let decor = key.leaf_decor_mut();
                let prefix = decor.prefix().and_then(|r| r.as_str()).unwrap_or("");
                let prefix = normalize_prefix(prefix, lead);
                decor.set_prefix(prefix);
                decor.set_suffix(" ");
                let vd = value.decor_mut();
                let suffix = vd.suffix().and_then(|r| r.as_str()).unwrap_or("");
                let suffix = normalize_suffix(suffix);
                vd.set_prefix(" ");
                vd.set_suffix(suffix);
            }
            Item::Table(sub) if sub.is_dotted() => {
                let d = key.dotted_decor_mut();
                d.set_prefix("");
                d.set_suffix("");
                format_dotted(sub, first, at_root, first_in_table);
            }
            // Anything else under a dotted key is left as it is.
            _ => {}
        }
    }
}

fn format_header(t: &mut Table, first: &FirstItem) {
    let is_first =
        !first.pending && first.table_position.is_some() && t.position() == first.table_position;
    let decor = t.decor_mut();
    let prefix = decor.prefix().and_then(|r| r.as_str()).unwrap_or("");
    let body = normalize_prefix(prefix, Lead::None);
    let prefix = if is_first { body } else { format!("\n{body}") };
    let suffix = decor.suffix().and_then(|r| r.as_str()).unwrap_or("");
    let suffix = normalize_suffix(suffix);
    decor.set_prefix(prefix);
    decor.set_suffix(suffix);
}

/// Blank lines allowed before an item's comments.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lead {
    /// None (first item of the file or of a table).
    None,
    /// At most one, as written.
    KeepOne,
}

/// The whitespace and comment lines before a key or a header: comments
/// unindented and without trailing spaces, at most one blank line in a row,
/// nothing on the key's own line before it.
fn normalize_prefix(raw: &str, lead: Lead) -> String {
    let mut lines: Vec<&str> = raw.split('\n').collect();
    // The last piece is the indentation on the key's own line.
    lines.pop();
    let mut out = String::new();
    let mut blank = false;
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            if !out.is_empty() || lead == Lead::KeepOne {
                blank = true;
            }
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(t);
        out.push('\n');
    }
    if blank {
        out.push('\n');
    }
    out
}

/// What follows a value or a header on its line: ` # comment` or nothing.
fn normalize_suffix(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('#') {
        format!(" {t}")
    } else {
        String::new()
    }
}

/// Comments after the last item, with a blank line before them when there
/// was one.
fn normalize_trailing(raw: &str) -> String {
    let mut body = normalize_prefix(&format!("{raw}\n"), Lead::KeepOne);
    while body.ends_with("\n\n") {
        body.pop();
    }
    if body.trim().is_empty() {
        return String::new();
    }
    body
}
