//! Edit individual `config.toml` values without rewriting unrelated source text.
//!
//! Parser spans delimit edits, preserving comments, quoting, key order, whitespace,
//! multiline values, the UTF-8 BOM, and the absence of a final newline. New lines
//! use CRLF if the input contains CRLF. Existing line endings, including mixed
//! endings, remain untouched so an unmodified document always round-trips exactly.

use std::ops::Range;

use superba_core::{Error, Result};
use toml_edit::{
    Decor, Document, DocumentMut, InlineTable, Item, Key, RawString, TableLike, Value,
};

/// A parsed `config.toml` that remembers its BOM and line-ending style.
#[derive(Clone, Debug)]
pub struct ConfigDoc {
    doc: DocumentMut,
    bom: bool,
    crlf: bool,
    // DocumentMut's serializer regroups dotted keys, removes CR, and adds a final
    // newline. Keep the source as well, and apply edits using a spanned parse.
    source: String,
    generated: Vec<Generated>,
}

// Remember separators we introduced so removing a new entry can also undo its
// separator. In particular, a new header must not acquire preexisting EOF comments.
#[derive(Clone, Debug)]
struct Generated {
    range: Range<usize>,
    header: Option<String>,
}

struct Edit {
    range: Range<usize>,
    text: String,
}

impl ConfigDoc {
    /// Create an empty document for a new file, without a BOM and using LF.
    pub fn empty() -> Self {
        Self {
            doc: DocumentMut::new(),
            bom: false,
            crlf: false,
            source: String::new(),
            generated: Vec::new(),
        }
    }

    /// Parse UTF-8 file bytes, remembering a leading BOM and whether CRLF occurs.
    ///
    /// Empty or whitespace-only input has no keys. Invalid UTF-8 or TOML returns
    /// [`Error::Parse`] for `config.toml`, including a one-based line number when
    /// the parser provides a source span.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let bom = bytes.starts_with(b"\xef\xbb\xbf");
        let bytes = if bom { &bytes[3..] } else { bytes };
        let source = std::str::from_utf8(bytes).map_err(|error| {
            let line = 1 + bytes[..error.valid_up_to()]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            Error::parse("config.toml", format!("line {line}: {error}"))
        })?;
        let doc = parse_document(source)?.into_mut();
        Ok(Self {
            doc,
            bom,
            crlf: source.contains("\r\n"),
            source: source.to_owned(),
            generated: Vec::new(),
        })
    }

    /// Serialize to UTF-8 bytes, restoring the BOM if present.
    ///
    /// Unmodified input round-trips byte-for-byte. Inserted newlines use the
    /// remembered style; original mixed line endings are preserved as well.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.source.len() + usize::from(self.bom) * 3);
        if self.bom {
            bytes.extend_from_slice(b"\xef\xbb\xbf");
        }
        bytes.extend_from_slice(self.source.as_bytes());
        bytes
    }

    /// Get a value's source representation without surrounding decor.
    ///
    /// Paths contain dot-separated bare keys and traverse standard, implicit,
    /// dotted, and inline tables. A leaf that is a table written another way (a
    /// `[section]`, an implicit table or dotted keys) is returned as one inline
    /// table of its entries' source representations, in document order, so it
    /// compares equal ([`reprs_equal`]) to the inline table of the same value.
    /// Missing paths, scalar intermediate nodes, and arrays of tables (also
    /// inside a table leaf) return `None`.
    pub fn get_repr(&self, path: &str) -> Option<String> {
        let path = path_parts(path)?;
        let parsed = parse_document(&self.source).ok()?;
        item_repr(lookup(parsed.as_item(), &path)?, &self.source)
    }

    /// Get the semantic TOML value at a dotted path, or `None` if it is not a value.
    pub fn get_value(&self, path: &str) -> Option<toml::Value> {
        parse_repr(&self.get_repr(path)?)
    }

    /// Get the decoded string at a dotted path, or `None` for a non-string leaf.
    pub fn get_str(&self, path: &str) -> Option<String> {
        let path = path_parts(path)?;
        lookup(self.doc.as_item(), &path)?
            .as_str()
            .map(str::to_owned)
    }

    /// Whether a standard, implicit, dotted, or inline table exists at `path`.
    pub fn has_table(&self, path: &str) -> bool {
        path_parts(path)
            .and_then(|path| lookup(self.doc.as_item(), &path))
            .is_some_and(|item| item.as_table_like().is_some())
    }

    /// Whether any node exists at `path`, including tables and arrays of tables
    /// that have no value representation. Rollback must not mistake them for an
    /// absent owned key.
    pub(crate) fn has_path(&self, path: &str) -> bool {
        path_parts(path)
            .and_then(|path| lookup(self.doc.as_item(), &path))
            .is_some()
    }

    /// Set a value from a TOML value representation, preserving existing decor.
    ///
    /// Existing values change only within their source span. New entries retain
    /// their parent's dotted or inline style; missing standard tables are appended
    /// after existing content, with a blank line before the header. Missing parents
    /// inside an inline table are necessarily inline too. Root entries precede
    /// table headers. An inline-table repr for a table written another way (a
    /// `[section]`, an implicit table or dotted keys) updates that table in place:
    /// its entries are set, added and removed one by one with the rules above, so
    /// comments and the rest of the file stay ([`Self::remove`] prunes a table
    /// left empty; an empty repr then writes `{}`). Invalid paths or reprs return
    /// [`Error::Invalid`]; traversing a scalar or array of tables, replacing a
    /// table with a value that is not an inline table, or a table entry whose
    /// key is not a bare key returns [`Error::Conflict`]. Errors leave the
    /// document unchanged.
    pub fn set_repr(&mut self, path: &str, repr: &str) -> Result<()> {
        let parts =
            path_parts(path).ok_or_else(|| Error::invalid("expected a dotted bare-key path"))?;
        // Value's parser rejects extra assignments, headers, and trailing comments.
        let wanted: Value = repr
            .parse()
            .map_err(|error| Error::invalid(format!("invalid TOML value: {error}")))?;
        let repr = repr.trim();
        let parsed = parse_document(&self.source)?;
        let root = parsed.as_item();
        let mut parent = root;
        let mut depth = 0;
        let mut physical = root;
        let mut physical_depth = 0;
        let parent_depth = parts.len() - 1;

        while depth < parent_depth {
            let table = parent.as_table_like().ok_or_else(|| conflict(path))?;
            let Some(child) = table.get(parts[depth]) else {
                break;
            };
            let child_table = child.as_table_like().ok_or_else(|| conflict(path))?;
            depth += 1;
            parent = child;
            if !child_table.is_dotted() {
                physical = child;
                physical_depth = depth;
            }
        }

        if depth == parent_depth {
            let table = parent.as_table_like().ok_or_else(|| conflict(path))?;
            if let Some(old) = table.get(parts[parent_depth]) {
                let table_leaf =
                    old.is_table() || old.as_inline_table().is_some_and(InlineTable::is_dotted);
                if table_leaf {
                    return match wanted {
                        Value::InlineTable(wanted) => self.update_table(path, &wanted),
                        _ => Err(conflict(path)),
                    };
                }
                let span = old
                    .as_value()
                    .filter(|value| !value.as_inline_table().is_some_and(InlineTable::is_dotted))
                    .and_then(Value::span)
                    .ok_or_else(|| conflict(path))?;
                if self.source[span.clone()] == *repr {
                    return Ok(());
                }
                return self.apply(
                    vec![Edit {
                        range: span,
                        text: self.newlines(repr),
                    }],
                    None,
                );
            }
        }

        if let Some(inline) = physical.as_inline_table() {
            let mut value = repr.to_owned();
            // Inline tables cannot be extended by a [header] elsewhere in TOML.
            for segment in parts[depth + 1..].iter().rev() {
                value = format!("{{ {segment} = {value} }}");
            }
            let key = parts[physical_depth..=depth].join(".");
            let (at, text) = inline_insertion(inline, &key, &value, &self.source)?;
            return self.apply(
                vec![Edit {
                    range: at..at,
                    text: self.newlines(&text),
                }],
                None,
            );
        }

        let needs_header = depth < parent_depth
            || (depth > 0
                && parent
                    .as_table()
                    .is_some_and(|table| table.is_implicit() && !table.is_dotted()));
        if needs_header {
            let at = self.source.len();
            let header = parts[..parent_depth].join(".");
            let prefix = if self.source.is_empty() || ends_with_blank_line(&self.source) {
                ""
            } else if self.source.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            let heading = self.newlines(&format!("{prefix}[{header}]\n"));
            let text = format!(
                "{heading}{}",
                self.newlines(&format!("{} = {repr}\n", parts[parent_depth]))
            );
            let generated = Generated {
                range: at..at + heading.len(),
                header: Some(header),
            };
            return self.apply(
                vec![Edit {
                    range: at..at,
                    text,
                }],
                Some(generated),
            );
        }

        let table = physical.as_table().ok_or_else(|| conflict(path))?;
        let at = table
            .get_values()
            .iter()
            .filter_map(|(_, value)| value.span())
            .map(|span| line_end(&self.source, span.end))
            .max()
            .unwrap_or_else(|| {
                if physical_depth == 0 {
                    0
                } else {
                    table
                        .span()
                        .map_or(0, |span| line_end(&self.source, span.end))
                }
            });
        let key = parts[physical_depth..].join(".");
        let bridge = at > 0 && !self.source[..at].ends_with('\n');
        let prefix = if bridge {
            self.newlines("\n")
        } else {
            String::new()
        };
        let generated = bridge.then(|| Generated {
            range: at..at + prefix.len(),
            header: None,
        });
        let text = format!("{prefix}{}", self.newlines(&format!("{key} = {repr}\n")));
        self.apply(
            vec![Edit {
                range: at..at,
                text,
            }],
            generated,
        )
    }

    /// Remove a key, returning whether anything was removed.
    ///
    /// Leading comment decor moves to the next value in the same table, if any.
    /// Ancestors emptied by this removal are pruned unless their decor contains a
    /// comment. Unrelated empty tables and the root are never pruned. Missing or
    /// untraversable paths leave the document unchanged.
    pub fn remove(&mut self, path: &str) -> bool {
        let Some(mut parts) = path_parts(path) else {
            return false;
        };
        let Ok(parsed) = parse_document(&self.source) else {
            return false;
        };
        let root = parsed.as_item();
        if lookup(root, &parts).is_none() {
            return false;
        }

        // Only ancestors of the removed node can become newly empty. Their sole
        // child must be the node being removed, so preexisting empty tables survive.
        while parts.len() > 1 {
            let parent_path = &parts[..parts.len() - 1];
            let Some(parent) = lookup(root, parent_path) else {
                return false;
            };
            let Some(table) = parent.as_table_like() else {
                return false;
            };
            if table.len() != 1 || self.table_has_comment(parent, &parent_path.join(".")) {
                break;
            }
            parts.pop();
        }

        let mut edits = Vec::new();
        if !self.removal_edits(root, &parts, true, &mut edits) {
            return false;
        }
        merge_deletions(&mut edits);
        // Undo an EOF separator only when no following entry still needs it.
        for edit in &mut edits {
            if edit.text.is_empty() && edit.range.end == self.source.len() {
                for generated in &self.generated {
                    if generated.header.is_none() && generated.range.end == edit.range.start {
                        edit.range.start = generated.range.start;
                    }
                }
            }
        }
        self.apply(edits, None).is_ok()
    }

    /// Serialize a copy after removing each path with [`Self::remove`].
    pub fn stripped(&self, paths: &[&str]) -> String {
        let mut copy = self.clone();
        for path in paths {
            copy.remove(path);
        }
        let mut text = String::new();
        if copy.bom {
            text.push('\u{feff}');
        }
        text.push_str(&copy.source);
        text
    }

    /// Make the table at `path` (a `[section]`, an implicit table or dotted keys)
    /// hold exactly `wanted`'s entries: each one set in place or added with
    /// [`Self::set_repr`] (nested tables recurse), then the others removed.
    /// Errors restore the document.
    fn update_table(&mut self, path: &str, wanted: &InlineTable) -> Result<()> {
        let before = self.clone();
        let result = self.update_entries(path, wanted);
        if result.is_err() {
            *self = before;
        }
        result
    }

    fn update_entries(&mut self, path: &str, wanted: &InlineTable) -> Result<()> {
        let present: Vec<String> = {
            let parts = path_parts(path).ok_or_else(|| conflict(path))?;
            let parsed = parse_document(&self.source)?;
            lookup(parsed.as_item(), &parts)
                .and_then(Item::as_table_like)
                .ok_or_else(|| conflict(path))?
                .iter()
                .map(|(key, _)| key.to_owned())
                .collect()
        };
        // Entries are addressed by dotted paths, so every key must be bare.
        if !wanted
            .iter()
            .map(|(key, _)| key)
            .chain(present.iter().map(String::as_str))
            .all(is_bare_key)
        {
            return Err(conflict(path));
        }
        for (key, value) in wanted.iter() {
            let mut value = value.clone();
            value.decor_mut().clear();
            self.set_repr(&format!("{path}.{key}"), &value.to_string())?;
        }
        for key in present.iter().filter(|key| !wanted.contains_key(key)) {
            self.remove(&format!("{path}.{key}"));
        }
        if !self.has_path(path) {
            self.set_repr(path, "{}")?;
        }
        Ok(())
    }

    fn newlines(&self, text: &str) -> String {
        if !self.crlf {
            return text.to_owned();
        }
        let mut result = String::with_capacity(text.len());
        let mut previous = '\0';
        for ch in text.chars() {
            if ch == '\n' && previous != '\r' {
                result.push('\r');
            }
            result.push(ch);
            previous = ch;
        }
        result
    }

    fn table_has_comment(&self, item: &Item, path: &str) -> bool {
        let decor = match item {
            Item::Table(table) => table.decor(),
            Item::Value(Value::InlineTable(table)) => table.decor(),
            _ => return false,
        };
        let prefix = if let Some(generated) = self
            .generated
            .iter()
            .find(|g| g.header.as_deref() == Some(path))
        {
            let end = item.span().map_or(generated.range.end, |span| span.start);
            &self.source[generated.range.start..end]
        } else {
            raw(decor.prefix(), &self.source)
        };
        prefix.contains('#') || raw(decor.suffix(), &self.source).contains('#')
    }

    fn removal_edits(
        &self,
        root: &Item,
        path: &[&str],
        move_comments: bool,
        edits: &mut Vec<Edit>,
    ) -> bool {
        let Some(item) = lookup(root, path) else {
            return false;
        };
        match item {
            Item::Value(value) if !value.as_inline_table().is_some_and(InlineTable::is_dotted) => {
                self.value_removal(root, path, value, move_comments, edits)
            }
            Item::Table(table) => {
                if !table.is_implicit() && !table.is_dotted() {
                    let Some(span) = table.span() else {
                        return false;
                    };
                    let name = path.join(".");
                    let start = self
                        .generated
                        .iter()
                        .find(|g| g.header.as_deref() == Some(&name))
                        .map_or_else(
                            || decor_start(table.decor(), line_start(&self.source, span.start)),
                            |g| g.range.start,
                        );
                    edits.push(Edit {
                        range: start..line_end(&self.source, span.end),
                        text: String::new(),
                    });
                }
                for (key, _) in table.iter() {
                    let mut child = path.to_vec();
                    child.push(key);
                    if !self.removal_edits(root, &child, false, edits) {
                        return false;
                    }
                }
                true
            }
            Item::Value(Value::InlineTable(table)) => {
                for (key, _) in table.iter() {
                    let mut child = path.to_vec();
                    child.push(key);
                    if !self.removal_edits(root, &child, false, edits) {
                        return false;
                    }
                }
                true
            }
            // An array of tables is a leaf, never an intermediate path segment.
            Item::ArrayOfTables(array) => {
                for table in array.iter() {
                    collect_table_ranges(table, &self.source, edits);
                }
                true
            }
            _ => false,
        }
    }

    fn value_removal(
        &self,
        root: &Item,
        path: &[&str],
        value: &Value,
        move_comments: bool,
        edits: &mut Vec<Edit>,
    ) -> bool {
        let Some(span) = value.span() else {
            return false;
        };
        let Some((parent, physical)) = parents(root, path) else {
            return false;
        };
        let Some(table) = physical.as_table_like() else {
            return false;
        };
        let mut values = table.get_values();
        values.sort_by_key(|(_, value)| value.span().map(|span| span.start));
        let Some(index) = values
            .iter()
            .position(|(_, v)| v.span() == Some(span.clone()))
        else {
            return false;
        };
        let Some(key) = values[index].0.last() else {
            return false;
        };
        let prefix = raw(key.leaf_decor().prefix(), &self.source);
        let start = decor_start(key.leaf_decor(), line_start(&self.source, span.start));
        let mut range = start..line_end(&self.source, span.end);

        if let Some(inline) = physical.as_inline_table() {
            if let Some((next_keys, next)) = values.get(index + 1) {
                range.end = entry_start(next_keys, next, &self.source).unwrap_or(span.end);
            } else if index > 0 {
                let previous = values[index - 1].1;
                range.start = previous
                    .decor()
                    .suffix()
                    .and_then(RawString::span)
                    .map(|span| span.end)
                    .or_else(|| previous.span().map(|span| span.end))
                    .unwrap_or(start);
                range.end = span.end;
            } else {
                range.end = if inline.trailing_comma() {
                    inline.trailing().span().map_or(span.end, |span| span.start)
                } else {
                    span.end
                };
            }
        }

        if move_comments && prefix.contains('#') {
            let next = parent
                .as_table_like()
                .into_iter()
                .flat_map(TableLike::get_values)
                .filter_map(|(keys, value)| {
                    Some((
                        value.span()?.start,
                        entry_start(&keys, value, &self.source)?,
                    ))
                })
                .filter(|(at, _)| *at > span.start)
                .min_by_key(|(at, _)| *at);
            if let Some((_, at)) = next {
                edits.push(Edit {
                    range: at..at,
                    text: prefix.to_owned(),
                });
            }
        }
        edits.push(Edit {
            range,
            text: String::new(),
        });
        true
    }

    fn apply(&mut self, mut edits: Vec<Edit>, generated: Option<Generated>) -> Result<()> {
        edits.sort_by(|a, b| {
            b.range
                .start
                .cmp(&a.range.start)
                .then(b.range.end.cmp(&a.range.end))
        });
        let mut source = self.source.clone();
        let mut markers = self.generated.clone();
        for edit in edits {
            if source.get(edit.range.clone()).is_none() {
                return Err(Error::invalid("invalid TOML edit span"));
            }
            markers.retain_mut(|marker| {
                if edit.range.end <= marker.range.start {
                    let start = marker.range.start - edit.range.len() + edit.text.len();
                    marker.range = start..start + marker.range.len();
                    true
                } else {
                    edit.range.start >= marker.range.end
                }
            });
            source.replace_range(edit.range, &edit.text);
        }
        // Validate the whole candidate before publishing either text or syntax.
        let doc = parse_document(&source)?.into_mut();
        if let Some(generated) = generated {
            markers.push(generated);
        }
        self.doc = doc;
        self.source = source;
        self.generated = markers;
        Ok(())
    }
}

/// Compare TOML value reprs semantically; invalid reprs are equal only byte-for-byte.
pub fn reprs_equal(a: &str, b: &str) -> bool {
    a == b || matches!((parse_repr(a), parse_repr(b)), (Some(a), Some(b)) if a == b)
}

/// Encode `s` as a canonical TOML basic string using double quotes and TOML escapes.
pub fn string_repr(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 2);
    result.push('"');
    for ch in s.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\u{8}' => result.push_str("\\b"),
            '\t' => result.push_str("\\t"),
            '\n' => result.push_str("\\n"),
            '\u{c}' => result.push_str("\\f"),
            '\r' => result.push_str("\\r"),
            ch if ch <= '\u{1f}' || ch == '\u{7f}' => {
                result.push_str(&format!("\\u{:04X}", u32::from(ch)))
            }
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

/// Encode a static Codex literal as a basic string or `true`/`false`.
pub fn literal_repr(lit: superba_core::codex::TomlLiteral) -> String {
    match lit {
        superba_core::codex::TomlLiteral::Str(value) => string_repr(value),
        superba_core::codex::TomlLiteral::Bool(value) => value.to_string(),
    }
}

/// Parse a single TOML value representation, returning `None` for invalid input.
pub fn parse_repr(repr: &str) -> Option<toml::Value> {
    repr.parse().ok()
}

fn parse_document(source: &str) -> Result<Document<&str>> {
    Document::parse(source).map_err(|error| {
        let message = if let Some(span) = error.span() {
            let line = 1 + source.as_bytes()[..span.start.min(source.len())]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            format!("line {line}: {error}")
        } else {
            error.to_string()
        };
        Error::parse("config.toml", message)
    })
}

fn path_parts(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<_> = path.split('.').collect();
    parts.iter().all(|part| is_bare_key(part)).then_some(parts)
}

fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// The source repr of `item`; a table that is not an inline value (a section,
/// an implicit table, dotted keys) as one inline table of its entries' reprs.
/// `None` for arrays of tables.
fn item_repr(item: &Item, source: &str) -> Option<String> {
    match item {
        Item::Value(value) => value_repr(value, source),
        Item::Table(table) => entries_repr(
            table
                .iter()
                .map(|(key, item)| Some((key, item_repr(item, source)?))),
        ),
        _ => None,
    }
}

fn value_repr(value: &Value, source: &str) -> Option<String> {
    match value {
        Value::InlineTable(table) if table.is_dotted() => entries_repr(
            table
                .iter()
                .map(|(key, value)| Some((key, value_repr(value, source)?))),
        ),
        value => source.get(value.span()?).map(str::to_owned),
    }
}

fn entries_repr<'a>(entries: impl Iterator<Item = Option<(&'a str, String)>>) -> Option<String> {
    let entries = entries
        .map(|entry| {
            entry.map(|(key, repr)| {
                let key = if is_bare_key(key) {
                    key.to_owned()
                } else {
                    string_repr(key)
                };
                format!("{key} = {repr}")
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(if entries.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", entries.join(", "))
    })
}

fn lookup<'a>(mut item: &'a Item, path: &[&str]) -> Option<&'a Item> {
    for key in path {
        item = item.as_table_like()?.get(key)?;
    }
    Some(item)
}

// The logical parent owns the key; the physical parent owns the line or braces.
fn parents<'a>(root: &'a Item, path: &[&str]) -> Option<(&'a Item, &'a Item)> {
    let mut parent = root;
    let mut physical = root;
    for key in &path[..path.len() - 1] {
        parent = parent.as_table_like()?.get(key)?;
        if !parent.as_table_like()?.is_dotted() {
            physical = parent;
        }
    }
    Some((parent, physical))
}

fn conflict(path: &str) -> Error {
    Error::Conflict {
        message: format!(
            "cannot set {path}: the path crosses or replaces a non-value table/array or a scalar parent"
        ),
    }
}

fn raw<'a>(raw: Option<&'a RawString>, source: &'a str) -> &'a str {
    raw.and_then(|raw| raw.as_str().or_else(|| source.get(raw.span()?)))
        .unwrap_or("")
}

fn decor_start(decor: &Decor, fallback: usize) -> usize {
    decor
        .prefix()
        .and_then(RawString::span)
        .map_or(fallback, |span| span.start)
}

fn entry_start(keys: &[&Key], value: &Value, source: &str) -> Option<usize> {
    let key = keys.last()?;
    key.leaf_decor()
        .prefix()
        .and_then(RawString::span)
        .map(|span| span.start)
        // A dotted path's first Key can refer to its first occurrence on an
        // earlier line. The value span always belongs to this actual entry.
        .or_else(|| value.span().map(|span| line_start(source, span.start)))
}

fn line_start(source: &str, at: usize) -> usize {
    source[..at].rfind('\n').map_or(0, |at| at + 1)
}

fn line_end(source: &str, at: usize) -> usize {
    source[at..]
        .find('\n')
        .map_or(source.len(), |end| at + end + 1)
}

fn ends_with_blank_line(source: &str) -> bool {
    source
        .strip_suffix('\n')
        .and_then(|text| text.rsplit_once('\n'))
        .is_some_and(|(_, last)| last.trim_matches([' ', '\t', '\r']).is_empty())
}

fn inline_insertion(
    table: &InlineTable,
    key: &str,
    value: &str,
    source: &str,
) -> Result<(usize, String)> {
    if let Some(at) = table
        .get_values()
        .iter()
        .filter_map(|(_, value)| value.span())
        .map(|span| span.end)
        .max()
    {
        return Ok((at, format!(", {key} = {value}")));
    }
    let span = table
        .span()
        .ok_or_else(|| Error::invalid("inline table has no source span"))?;
    let suffix = if source[span.clone()].len() == 2 {
        " "
    } else {
        ""
    };
    Ok((span.start + 1, format!(" {key} = {value}{suffix}")))
}

fn collect_table_ranges(table: &toml_edit::Table, source: &str, edits: &mut Vec<Edit>) {
    if !table.is_implicit()
        && !table.is_dotted()
        && let Some(span) = table.span()
    {
        edits.push(Edit {
            range: decor_start(table.decor(), line_start(source, span.start))
                ..line_end(source, span.end),
            text: String::new(),
        });
    }
    for (keys, value) in table.get_values() {
        if let (Some(key), Some(span)) = (keys.last(), value.span()) {
            edits.push(Edit {
                range: decor_start(key.leaf_decor(), line_start(source, span.start))
                    ..line_end(source, span.end),
                text: String::new(),
            });
        }
    }
    for (_, item) in table.iter() {
        match item {
            Item::Table(child) => collect_table_ranges(child, source, edits),
            Item::ArrayOfTables(array) => {
                for child in array.iter() {
                    collect_table_ranges(child, source, edits);
                }
            }
            _ => {}
        }
    }
}

fn merge_deletions(edits: &mut Vec<Edit>) {
    edits.sort_by_key(|edit| edit.range.start);
    let mut merged: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits.drain(..) {
        if let Some(previous) = merged.last_mut()
            && previous.text.is_empty()
            && edit.text.is_empty()
            && edit.range.start <= previous.range.end
        {
            previous.range.end = previous.range.end.max(edit.range.end);
        } else {
            merged.push(edit);
        }
    }
    *edits = merged;
}
