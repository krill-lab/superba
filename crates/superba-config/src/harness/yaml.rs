//! A YAML editor that changes only the entries it is asked to and keeps
//! every comment, blank line, `!!js` expression and the layout of
//! everything else (dsh's `cordis.patch.yml` files).
//!
//! The document is parsed into nodes with their positions (`saphyr-parser`
//! events); an edit replaces, inserts or removes whole lines of one entry
//! (rendered in block style with the surrounding indentation), or turns an
//! empty `[]` / `{}` / empty value into a block. Every edit is then checked:
//! the new text is parsed again and must mean exactly the old document with
//! the change applied ([`super::slot::set`]); anything else (flow-style
//! collections in the way, anchors and aliases, several documents, keys
//! that are not first on their line) is refused rather than guessed, and
//! nothing is written.
//!
//! Meaning: YAML 1.2 core schema for plain scalars (null, booleans,
//! numbers), quoted and block scalars are strings, a tagged scalar is
//! `{"$yaml_tag": "!!js", "value": "<text>"}` (the form Superba compares and
//! writes back).

use saphyr_parser::{Event, Parser, ScalarStyle, Span};
use serde_json::{Map, Number, Value};

use super::slot::{self, Seg, SlotPath};

/// The member name of a tagged scalar's tag in the JSON meaning.
pub const TAG_KEY: &str = "$yaml_tag";
/// The member name of a tagged scalar's text.
pub const TAG_VALUE: &str = "value";

/// A parse problem: 1-based line and column, and the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlError {
    /// 1-based line (0 = unknown).
    pub line: usize,
    /// 1-based column (0 = unknown).
    pub column: usize,
    /// What is wrong.
    pub message: String,
}

impl YamlError {
    fn at(message: impl Into<String>) -> Self {
        Self {
            line: 0,
            column: 0,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
enum Kind {
    Scalar {
        text: String,
        style: ScalarStyle,
        tag: Option<String>,
    },
    Seq(Vec<Node>),
    Map(Vec<(Node, Node)>),
}

/// A node with its byte range in the text.
#[derive(Clone, Debug)]
struct Node {
    kind: Kind,
    start: usize,
    end: usize,
    flow: bool,
}

impl Node {
    fn is_null_scalar(&self) -> bool {
        matches!(&self.kind, Kind::Scalar { text, style: ScalarStyle::Plain, tag: None }
            if matches!(text.as_str(), "" | "~" | "null" | "Null" | "NULL"))
    }

    fn is_empty_flow(&self) -> bool {
        self.flow
            && match &self.kind {
                Kind::Seq(items) => items.is_empty(),
                Kind::Map(entries) => entries.is_empty(),
                Kind::Scalar { .. } => false,
            }
    }

    fn scalar_text(&self) -> Option<&str> {
        match &self.kind {
            Kind::Scalar {
                text, tag: None, ..
            } => Some(text),
            _ => None,
        }
    }
}

fn tag_text(handle: &str, suffix: &str) -> String {
    if handle == "tag:yaml.org,2002:" {
        format!("!!{suffix}")
    } else {
        format!("{handle}{suffix}")
    }
}

/// Character offset -> byte offset.
struct Offsets(Vec<usize>);

impl Offsets {
    fn new(text: &str) -> Self {
        let mut v: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
        v.push(text.len());
        Offsets(v)
    }

    fn byte(&self, chars: usize) -> usize {
        self.0
            .get(chars)
            .copied()
            .unwrap_or(*self.0.last().unwrap_or(&0))
    }
}

struct Events<'a> {
    parser: Parser<'a, saphyr_parser::StrInput<'a>>,
    offsets: Offsets,
}

impl<'a> Events<'a> {
    fn next(&mut self) -> Result<(Event<'a>, usize, usize), YamlError> {
        match self.parser.next_event() {
            None => Err(YamlError::at("unexpected end of the document")),
            Some(Err(e)) => Err(YamlError {
                line: e.marker().line(),
                column: e.marker().col() + 1,
                message: e.info().to_owned(),
            }),
            Some(Ok((ev, span))) => {
                let Span { start, end } = span;
                Ok((
                    ev,
                    self.offsets.byte(start.index()),
                    self.offsets.byte(end.index()),
                ))
            }
        }
    }
}

fn flow_at(text: &str, at: usize) -> bool {
    matches!(text.as_bytes().get(at), Some(b'[' | b'{'))
}

fn node(
    ev: Event<'_>,
    start: usize,
    end: usize,
    text: &str,
    it: &mut Events<'_>,
) -> Result<Node, YamlError> {
    let anchored = |a: usize| {
        if a != 0 {
            Err(YamlError::at(
                "the file uses YAML anchors; Superba does not edit it (edit it by hand)",
            ))
        } else {
            Ok(())
        }
    };
    match ev {
        Event::Scalar(value, style, anchor, tag) => {
            anchored(anchor)?;
            Ok(Node {
                kind: Kind::Scalar {
                    text: value.into_owned(),
                    style,
                    tag: tag.map(|t| tag_text(&t.handle, &t.suffix)),
                },
                start,
                end,
                flow: false,
            })
        }
        Event::SequenceStart(anchor, tag) => {
            anchored(anchor)?;
            if tag.is_some() {
                return Err(YamlError::at(
                    "the file has a tagged sequence; Superba does not edit it",
                ));
            }
            let flow = flow_at(text, start);
            let mut items = Vec::new();
            loop {
                let (ev, s, e) = it.next()?;
                if let Event::SequenceEnd = ev {
                    let end = if flow { e } else { last_end(&items, start) };
                    return Ok(Node {
                        kind: Kind::Seq(items),
                        start,
                        end,
                        flow,
                    });
                }
                items.push(node(ev, s, e, text, it)?);
            }
        }
        Event::MappingStart(anchor, tag) => {
            anchored(anchor)?;
            if tag.is_some() {
                return Err(YamlError::at(
                    "the file has a tagged mapping; Superba does not edit it",
                ));
            }
            let flow = flow_at(text, start);
            let mut entries: Vec<(Node, Node)> = Vec::new();
            loop {
                let (ev, s, e) = it.next()?;
                if let Event::MappingEnd = ev {
                    let end = if flow {
                        e
                    } else {
                        entries.last().map_or(start, |(k, v)| entry_end(text, k, v))
                    };
                    return Ok(Node {
                        kind: Kind::Map(entries),
                        start,
                        end,
                        flow,
                    });
                }
                let key = node(ev, s, e, text, it)?;
                let (ev, s, e) = it.next()?;
                let value = node(ev, s, e, text, it)?;
                entries.push((key, value));
            }
        }
        Event::Alias(_) => Err(YamlError::at(
            "the file uses YAML aliases; Superba does not edit it (edit it by hand)",
        )),
        other => Err(YamlError::at(format!("unexpected YAML event {other:?}"))),
    }
}

/// Where a mapping entry ends: after its value, or after the key's `:` when
/// the value is empty (the parser puts an empty value at the `:`).
fn entry_end(text: &str, k: &Node, v: &Node) -> usize {
    if v.start == v.end && v.is_null_scalar() {
        let from = k.end;
        let line_end = text[from..].find('\n').map_or(text.len(), |i| from + i);
        match text[from..line_end].find(':') {
            Some(i) => from + i + 1,
            None => from,
        }
    } else {
        v.end.max(k.end)
    }
}

fn last_end(items: &[Node], start: usize) -> usize {
    items.last().map_or(start, |n| n.end.max(n.start))
}

/// Parse `text` into its root node (`None` = no content: empty or comments).
fn parse(text: &str) -> Result<Option<Node>, YamlError> {
    let mut it = Events {
        parser: Parser::new_from_str(text),
        offsets: Offsets::new(text),
    };
    let mut root = None;
    let mut documents = 0;
    loop {
        let (ev, s, e) = it.next()?;
        match ev {
            Event::StreamStart | Event::DocumentEnd | Event::Nothing => {}
            Event::StreamEnd => break,
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return Err(YamlError::at(
                        "the file holds several YAML documents; Superba does not edit it",
                    ));
                }
            }
            ev => {
                root = Some(node(ev, s, e, text, &mut it)?);
            }
        }
    }
    Ok(root)
}

fn is_int(t: &str) -> Option<Number> {
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if let Some(hex) = t.strip_prefix("0x") {
        return u64::from_str_radix(hex, 16).ok().map(Number::from);
    }
    if let Some(oct) = t.strip_prefix("0o") {
        return u64::from_str_radix(oct, 8).ok().map(Number::from);
    }
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if neg {
        t.parse::<i64>().ok().map(Number::from)
    } else {
        digits.parse::<u64>().ok().map(Number::from)
    }
}

fn is_float(t: &str) -> Option<Number> {
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let ok_mantissa = match mantissa.split_once('.') {
        Some((a, b)) => {
            a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
                && !(a.is_empty() && b.is_empty())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|c| c.is_ascii_digit()),
    };
    let ok_exp = exp.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit())
    });
    if !ok_mantissa || !ok_exp {
        return None;
    }
    t.parse::<f64>().ok().and_then(Number::from_f64)
}

/// The core-schema meaning of a plain scalar.
fn plain_value(t: &str) -> Value {
    match t {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => {
            if let Some(n) = is_int(t) {
                Value::Number(n)
            } else if let Some(n) = is_float(t) {
                Value::Number(n)
            } else {
                Value::String(t.to_owned())
            }
        }
    }
}

fn meaning(n: &Node) -> Result<Value, YamlError> {
    match &n.kind {
        Kind::Scalar { text, style, tag } => {
            let v = match style {
                ScalarStyle::Plain => plain_value(text),
                _ => Value::String(text.clone()),
            };
            Ok(match tag {
                None => v,
                Some(t) => {
                    let mut o = Map::new();
                    o.insert(TAG_KEY.into(), Value::String(t.clone()));
                    o.insert(TAG_VALUE.into(), Value::String(text.clone()));
                    Value::Object(o)
                }
            })
        }
        Kind::Seq(items) => Ok(Value::Array(
            items.iter().map(meaning).collect::<Result<_, _>>()?,
        )),
        Kind::Map(entries) => {
            let mut o = Map::new();
            for (k, v) in entries {
                let key = match &k.kind {
                    Kind::Scalar {
                        text, tag: None, ..
                    } => text.clone(),
                    _ => {
                        return Err(YamlError::at(
                            "a mapping key is not plain text; Superba does not edit the file",
                        ));
                    }
                };
                if o.contains_key(&key) {
                    return Err(YamlError::at(format!(
                        "the key {key:?} appears twice in one mapping; fix the file first"
                    )));
                }
                o.insert(key, meaning(v)?);
            }
            Ok(Value::Object(o))
        }
    }
}

/// The JSON meaning of a YAML document (`null` for no content).
pub fn to_value(text: &str) -> Result<Value, YamlError> {
    match parse(text)? {
        None => Ok(Value::Null),
        Some(n) => meaning(&n),
    }
}

// ---------------------------------------------------------------------------
// rendering

fn needs_quotes(s: &str) -> bool {
    if s.is_empty() || s.trim() != s || s.contains('\n') || s.contains('\t') {
        return true;
    }
    let first = s.chars().next().unwrap_or(' ');
    if !(first.is_alphanumeric() || matches!(first, '/' | '_' | '(' | '$' | '.')) {
        return true;
    }
    if s.contains(": ")
        || s.contains(" #")
        || s.ends_with(':')
        || s.contains([
            '{', '}', '[', ']', ',', '`', '"', '\'', '&', '*', '!', '|', '>', '%', '@', '\\',
        ])
    {
        return true;
    }
    if s.chars().any(char::is_control) {
        return true;
    }
    // Strings that YAML 1.1 or 1.2 would read as something else.
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "y" | "n"
            | "yes"
            | "no"
            | "on"
            | "off"
            | "true"
            | "false"
            | "null"
            | "~"
            | ".inf"
            | "-.inf"
            | "+.inf"
            | ".nan"
    ) {
        return true;
    }
    if s.bytes().all(|b| {
        b.is_ascii_digit()
            || matches!(
                b,
                b'.' | b'_' | b':' | b'+' | b'-' | b'e' | b'E' | b'x' | b'o'
            )
    }) && s.bytes().any(|b| b.is_ascii_digit())
    {
        return true;
    }
    !matches!(plain_value(s), Value::String(_))
}

fn scalar_text(v: &Value) -> Result<String, String> {
    match v {
        Value::Null => Ok("null".into()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(if needs_quotes(s) {
            serde_json::to_string(s).map_err(|e| e.to_string())?
        } else {
            s.clone()
        }),
        Value::Object(o) if o.is_empty() => Ok("{}".into()),
        Value::Array(a) if a.is_empty() => Ok("[]".into()),
        Value::Object(o) => match (o.get(TAG_KEY), o.get(TAG_VALUE), o.len()) {
            (Some(Value::String(tag)), Some(Value::String(text)), 2) => Ok(format!(
                "{tag} {}",
                serde_json::to_string(text).map_err(|e| e.to_string())?
            )),
            _ => Err("not a scalar".into()),
        },
        Value::Array(_) => Err("not a scalar".into()),
    }
}

fn inline(v: &Value) -> Option<String> {
    scalar_text(v).ok()
}

fn key_text(k: &str) -> Result<String, String> {
    scalar_text(&Value::String(k.to_owned()))
}

/// Lines of a block collection at `col` (the first line indented too).
fn block(v: &Value, col: usize, out: &mut Vec<String>) -> Result<(), String> {
    let pad = " ".repeat(col);
    match v {
        Value::Object(o) => {
            for (k, val) in o {
                let k = key_text(k)?;
                match inline(val) {
                    Some(s) => out.push(format!("{pad}{k}: {s}")),
                    None => {
                        out.push(format!("{pad}{k}:"));
                        block(val, col + 2, out)?;
                    }
                }
            }
            Ok(())
        }
        Value::Array(a) => {
            for item in a {
                match inline(item) {
                    Some(s) => out.push(format!("{pad}- {s}")),
                    None => {
                        let mut inner = Vec::new();
                        block(item, col + 2, &mut inner)?;
                        if item.is_object() {
                            // `- first: …` then the other members under it.
                            let first = inner.remove(0);
                            out.push(format!("{pad}- {}", first.trim_start()));
                            out.extend(inner);
                        } else {
                            out.push(format!("{pad}-"));
                            out.extend(inner);
                        }
                    }
                }
            }
            Ok(())
        }
        _ => Err("not a collection".into()),
    }
}

/// `key: value` as lines at `col`, the first line without its indentation.
fn render_entry(key: &str, v: &Value, col: usize) -> Result<String, String> {
    let mut lines = Vec::new();
    let k = key_text(key)?;
    match inline(v) {
        Some(s) => lines.push(format!("{k}: {s}")),
        None => {
            lines.push(format!("{k}:"));
            block(v, col + 2, &mut lines)?;
        }
    }
    Ok(lines.join("\n"))
}

/// `- value` as lines at `col`, the first line without its indentation.
fn render_item(v: &Value, col: usize) -> Result<String, String> {
    let mut lines = Vec::new();
    block(&Value::Array(vec![v.clone()]), col, &mut lines)?;
    let first = lines.remove(0);
    let mut out = vec![first.trim_start().to_owned()];
    out.extend(lines);
    Ok(out.join("\n"))
}

// ---------------------------------------------------------------------------
// editing

struct Text<'a> {
    s: &'a str,
    crlf: bool,
}

impl Text<'_> {
    fn line_start(&self, pos: usize) -> usize {
        self.s[..pos].rfind('\n').map_or(0, |i| i + 1)
    }

    /// The end of the line holding `pos` (before `\r\n` / `\n`).
    fn line_end(&self, pos: usize) -> usize {
        let e = self.s[pos..].find('\n').map_or(self.s.len(), |i| pos + i);
        if e > 0 && self.s.as_bytes().get(e - 1) == Some(&b'\r') {
            e - 1
        } else {
            e
        }
    }

    /// After the newline that ends the line holding `pos` (or the end).
    fn next_line(&self, pos: usize) -> usize {
        self.s[pos..]
            .find('\n')
            .map_or(self.s.len(), |i| pos + i + 1)
    }

    /// The end of the last line of a node ending at `end` (block scalars end
    /// after their newline).
    fn last_line_end(&self, start: usize, end: usize) -> usize {
        if end > start && self.s.as_bytes().get(end - 1) == Some(&b'\n') {
            let e = end - 1;
            if e > 0 && self.s.as_bytes().get(e - 1) == Some(&b'\r') {
                e - 1
            } else {
                e
            }
        } else {
            self.line_end(end)
        }
    }

    fn col(&self, pos: usize) -> usize {
        self.s[self.line_start(pos)..pos].chars().count()
    }

    fn first_on_line(&self, pos: usize) -> bool {
        self.s[self.line_start(pos)..pos].trim().is_empty()
    }

    /// Nothing but blanks or a comment after `pos` on its line.
    fn rest_blank(&self, pos: usize) -> bool {
        let rest = self.s[pos..self.line_end(pos)].trim_start();
        rest.is_empty() || rest.starts_with('#')
    }

    fn nl(&self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    fn lines(&self, rendered: &str) -> String {
        if self.crlf {
            rendered.replace('\n', "\r\n")
        } else {
            rendered.to_owned()
        }
    }

    /// The `-` before a sequence item starting at `item`.
    fn dash_before(&self, item: usize) -> Option<usize> {
        let before = self.s[..item].trim_end();
        before.ends_with('-').then(|| before.len() - 1)
    }
}

/// A text replacement.
struct Splice {
    from: usize,
    to: usize,
    with: String,
}

fn refuse(msg: impl Into<String>) -> String {
    msg.into()
}

/// Where the step `seg` leads from `container`: the index of the entry.
fn find_child(container: &Node, seg: &Seg) -> Result<Option<usize>, String> {
    match (&container.kind, seg) {
        (Kind::Map(entries), Seg::Key(k)) => Ok(entries
            .iter()
            .position(|(key, _)| key.scalar_text() == Some(k.as_str()))),
        (Kind::Seq(items), Seg::Find(fields)) => {
            let hits: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, n)| meaning(n).is_ok_and(|v| slot::matches_find(&v, fields)))
                .map(|(i, _)| i)
                .collect();
            if hits.len() > 1 {
                return Err(refuse("several entries match; remove the duplicates first"));
            }
            Ok(hits.first().copied())
        }
        (Kind::Seq(items), Seg::Item(s) | Seg::FirstItem(s)) => Ok(items
            .iter()
            .position(|n| n.scalar_text() == Some(s.as_str()))),
        (Kind::Scalar { .. }, _) if container.is_null_scalar() => Ok(None),
        (Kind::Map(_), _) => Err(refuse("expected a list here, the file has a mapping")),
        (Kind::Seq(_), _) => Err(refuse("expected a mapping here, the file has a list")),
        (Kind::Scalar { .. }, _) => Err(refuse(
            "expected a mapping or list here, the file has a value",
        )),
    }
}

fn child(container: &Node, i: usize) -> &Node {
    match &container.kind {
        Kind::Map(entries) => &entries[i].1,
        Kind::Seq(items) => &items[i],
        Kind::Scalar { .. } => unreachable!("scalars have no children"),
    }
}

/// The value of the entry a new subtree for `segs` adds to a container, and
/// the key it goes under (`None` for a sequence item).
fn new_entry(segs: &[Seg], value: &Value) -> Result<(Option<String>, Value), String> {
    let built = slot::build(segs, value.clone())?;
    Ok(match &segs[0] {
        Seg::Key(k) => (Some(k.clone()), built),
        _ => (None, built),
    })
}

/// How a container is placed: the key whose value it is (its column and
/// end), or the root.
#[derive(Clone, Copy)]
enum Place {
    Root,
    Value { key_col: usize, key_end: usize },
    Item,
}

/// Insert a new entry (built from `segs` and `value`) into `container`.
fn insert(
    t: &Text<'_>,
    container: &Node,
    place: Place,
    segs: &[Seg],
    value: &Value,
) -> Result<Splice, String> {
    let (key, entry) = new_entry(segs, value)?;
    let first = matches!(segs[0], Seg::FirstItem(_));
    let empty_or_null = container.is_empty_flow() || container.is_null_scalar();
    if empty_or_null {
        // Turn `[]`, `{}` or an empty value into a block holding the entry.
        let whole = match &key {
            Some(k) => {
                let mut o = Map::new();
                o.insert(k.clone(), entry);
                Value::Object(o)
            }
            None => Value::Array(vec![entry]),
        };
        return match place {
            Place::Root => {
                if !t.first_on_line(container.start) || t.col(container.start) != 0 {
                    return Err(refuse("the document's root is not at the start of a line"));
                }
                let mut lines = Vec::new();
                block(&whole, 0, &mut lines)?;
                let to = t.last_line_end(container.start, container.end.max(container.start));
                if !t.rest_blank(container.end.max(container.start)) {
                    return Err(refuse("something follows the empty list on its line"));
                }
                Ok(Splice {
                    from: container.start,
                    to,
                    with: t.lines(&lines.join("\n")),
                })
            }
            Place::Value { key_col, key_end } => {
                let colon = t.s[key_end..]
                    .find(':')
                    .map(|i| key_end + i)
                    .filter(|c| *c < t.line_end(key_end))
                    .ok_or_else(|| refuse("cannot find the ':' after a key"))?;
                let line_end = t.line_end(colon);
                let rest = t.s[colon + 1..line_end].trim_start();
                let value_text = rest.split(" #").next().unwrap_or("").trim();
                let value_text = if rest.starts_with('#') {
                    ""
                } else {
                    value_text
                };
                if !matches!(
                    value_text,
                    "" | "~" | "null" | "Null" | "NULL" | "[]" | "{}"
                ) {
                    return Err(refuse("unexpected text after a key"));
                }
                // A comment after the empty value stays on the key's line.
                let after_colon = &t.s[colon + 1..line_end];
                let comment = match after_colon.find('#') {
                    Some(i) => &after_colon[after_colon[..i].trim_end().len()..],
                    None => "",
                };
                let mut lines = Vec::new();
                block(&whole, key_col + 2, &mut lines)?;
                Ok(Splice {
                    from: colon + 1,
                    to: line_end,
                    with: t.lines(&format!("{comment}\n{}", lines.join("\n"))),
                })
            }
            Place::Item => Err(refuse("an empty list item cannot be extended")),
        };
    }
    if container.flow {
        return Err(refuse(
            "this part of the file is written in flow style ({…} or […]); Superba does not edit it",
        ));
    }
    match (&container.kind, &key) {
        (Kind::Map(entries), Some(k)) => {
            let (first_key, _) = entries.first().ok_or_else(|| refuse("empty mapping"))?;
            let col = t.col(first_key.start);
            let (last_key, last_val) = entries.last().expect("non-empty");
            let end = entry_end(t.s, last_key, last_val);
            if !t.rest_blank(end) {
                return Err(refuse("something follows the last entry on its line"));
            }
            let at = t.last_line_end(last_val.start.min(end), end);
            let text = render_entry(k, &entry, col)?;
            Ok(Splice {
                from: at,
                to: at,
                with: t.lines(&format!("\n{}{}", " ".repeat(col), text)),
            })
        }
        (Kind::Seq(items), None) => {
            let first_item = items.first().ok_or_else(|| refuse("empty list"))?;
            let dash = t
                .dash_before(first_item.start)
                .ok_or_else(|| refuse("cannot find the '-' of a list item"))?;
            let col = t.col(dash);
            let text = render_item(&entry, col)?;
            if first {
                let from = t.line_start(dash);
                if !t.first_on_line(dash) {
                    return Err(refuse("a list item is not first on its line"));
                }
                Ok(Splice {
                    from,
                    to: from,
                    with: t.lines(&format!("{}{}\n", " ".repeat(col), text)),
                })
            } else {
                let last = items.last().expect("non-empty");
                if !t.rest_blank(last.end.max(last.start)) {
                    return Err(refuse("something follows the last list item on its line"));
                }
                let at = t.last_line_end(last.start, last.end.max(last.start));
                Ok(Splice {
                    from: at,
                    to: at,
                    with: t.lines(&format!("\n{}{}", " ".repeat(col), text)),
                })
            }
        }
        _ => Err(refuse("the entry does not fit this part of the file")),
    }
}

/// The text range of entry `i` of `container`: from its key (or `-`) to the
/// end of its value's last line, and the column it starts at.
fn entry_range(t: &Text<'_>, container: &Node, i: usize) -> Result<(usize, usize, usize), String> {
    match &container.kind {
        Kind::Map(entries) => {
            let (k, v) = &entries[i];
            let end = entry_end(t.s, k, v);
            if !t.rest_blank(end) {
                return Err(refuse("something follows an entry on its line"));
            }
            Ok((
                k.start,
                t.last_line_end(v.start.min(end), end),
                t.col(k.start),
            ))
        }
        Kind::Seq(items) => {
            let n = &items[i];
            let dash = t
                .dash_before(n.start)
                .ok_or_else(|| refuse("cannot find the '-' of a list item"))?;
            let end = n.end.max(n.start);
            if !t.rest_blank(end) {
                return Err(refuse("something follows a list item on its line"));
            }
            Ok((dash, t.last_line_end(n.start, end), t.col(dash)))
        }
        Kind::Scalar { .. } => Err(refuse("not a collection")),
    }
}

fn entry_count(container: &Node) -> usize {
    match &container.kind {
        Kind::Map(e) => e.len(),
        Kind::Seq(i) => i.len(),
        Kind::Scalar { .. } => 0,
    }
}

/// Replace entry `i` of `container` with `value`.
fn replace(
    t: &Text<'_>,
    container: &Node,
    i: usize,
    seg: &Seg,
    value: &Value,
) -> Result<Splice, String> {
    if container.flow {
        return Err(refuse(
            "this part of the file is written in flow style ({…} or […]); Superba does not edit it",
        ));
    }
    let (from, to, col) = entry_range(t, container, i)?;
    let text = match (&container.kind, seg) {
        (Kind::Map(_), Seg::Key(k)) => render_entry(k, value, col)?,
        (Kind::Seq(_), Seg::Find(_)) => {
            render_item(&slot::build(std::slice::from_ref(seg), value.clone())?, col)?
        }
        (Kind::Seq(_), _) => render_item(value, col)?,
        _ => return Err(refuse("the entry does not fit this part of the file")),
    };
    Ok(Splice {
        from,
        to,
        with: t.lines(&text),
    })
}

/// Remove entry `i` of `container` (an empty container becomes `[]` / `{}`).
fn remove(t: &Text<'_>, container: &Node, place: Place, i: usize) -> Result<Splice, String> {
    if container.flow {
        return Err(refuse(
            "this part of the file is written in flow style ({…} or […]); Superba does not edit it",
        ));
    }
    let (from, to, _) = entry_range(t, container, i)?;
    let empty = match &container.kind {
        Kind::Seq(_) => "[]",
        _ => "{}",
    };
    if entry_count(container) == 1 {
        return match place {
            Place::Root => {
                if !t.first_on_line(from) {
                    return Err(refuse("the only entry is not first on its line"));
                }
                Ok(Splice {
                    from: t.line_start(from),
                    to,
                    with: empty.to_owned(),
                })
            }
            Place::Value { key_end, .. } => {
                let colon = t.s[key_end..]
                    .find(':')
                    .map(|c| key_end + c)
                    .ok_or_else(|| refuse("cannot find the ':' after a key"))?;
                // Keep a comment on the key's line after the new `{}` / `[]`.
                let key_rest = &t.s[colon + 1..t.line_end(colon)];
                let comment = if key_rest.trim_start().starts_with('#') {
                    key_rest
                } else {
                    ""
                };
                Ok(Splice {
                    from: colon + 1,
                    to,
                    with: format!(" {empty}{comment}"),
                })
            }
            Place::Item => Err(refuse("cannot empty a list item")),
        };
    }
    if !t.first_on_line(from) {
        return Err(refuse(
            "the entry shares its line with a list item marker; Superba does not remove it",
        ));
    }
    let start = t.line_start(from);
    let after = t.next_line(to);
    // Keep the file's final newline state: removing the last line of a file
    // that does not end in a newline takes the newline before it instead.
    if after == t.s.len() && !t.s.ends_with('\n') && start > 0 {
        let prev_nl = if t.s[..start].ends_with("\r\n") {
            start - 2
        } else {
            start - 1
        };
        return Ok(Splice {
            from: prev_nl,
            to: t.s.len(),
            with: String::new(),
        });
    }
    Ok(Splice {
        from: start,
        to: after,
        with: String::new(),
    })
}

/// The splice that sets `path` to `value` (`None` = removes it); `None`
/// when nothing changes.
fn plan_edit(
    t: &Text<'_>,
    root: Option<&Node>,
    path: &SlotPath,
    value: Option<&Value>,
) -> Result<Option<Splice>, String> {
    let segs = &path.0;
    let Some(root) = root else {
        // No content: the document becomes the new block.
        let Some(v) = value else { return Ok(None) };
        let (key, entry) = new_entry(segs, v)?;
        let whole = match key {
            Some(k) => {
                let mut o = Map::new();
                o.insert(k, entry);
                Value::Object(o)
            }
            None => Value::Array(vec![entry]),
        };
        let mut lines = Vec::new();
        block(&whole, 0, &mut lines)?;
        let at = t.s.len();
        let sep = if at == 0 || t.s.ends_with('\n') {
            ""
        } else {
            t.nl()
        };
        return Ok(Some(Splice {
            from: at,
            to: at,
            with: format!("{sep}{}{}", t.lines(&lines.join("\n")), t.nl()),
        }));
    };
    let mut container = root;
    let mut place = Place::Root;
    for (depth, seg) in segs.iter().enumerate() {
        let last = depth + 1 == segs.len();
        match find_child(container, seg)? {
            None => {
                return match value {
                    None => Ok(None),
                    Some(v) => insert(t, container, place, &segs[depth..], v).map(Some),
                };
            }
            Some(i) => {
                if last {
                    return match value {
                        None => remove(t, container, place, i).map(Some),
                        Some(v) => {
                            if meaning(child(container, i)).as_ref() == Ok(v) {
                                return Ok(None);
                            }
                            if matches!(seg, Seg::Item(_) | Seg::FirstItem(_)) {
                                return Ok(None);
                            }
                            replace(t, container, i, seg, v).map(Some)
                        }
                    };
                }
                place = match &container.kind {
                    Kind::Map(entries) => {
                        let k = &entries[i].0;
                        Place::Value {
                            key_col: t.col(k.start),
                            key_end: k.end,
                        }
                    }
                    _ => Place::Item,
                };
                container = child(container, i);
            }
        }
    }
    Ok(None)
}

/// A YAML document edited in place (see the module docs).
#[derive(Clone, Debug)]
pub struct YamlDoc {
    text: String,
    value: Value,
}

impl YamlDoc {
    /// Parse a document.
    pub fn parse(text: &str) -> Result<Self, YamlError> {
        Ok(Self {
            value: to_value(text)?,
            text: text.to_owned(),
        })
    }

    /// The text now.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What the document means now.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Set `path` to `value` (`None` = remove it), keeping everything else.
    /// Refused (and nothing changed) when the edit cannot be made safely.
    pub fn set(&mut self, path: &SlotPath, value: Option<&Value>) -> Result<(), String> {
        let mut expected = self.value.clone();
        slot::set(&mut expected, path, value.cloned())?;
        if expected == self.value {
            return Ok(());
        }
        let root = parse(&self.text).map_err(|e| e.message)?;
        let t = Text {
            s: &self.text,
            crlf: self.text.contains("\r\n"),
        };
        let Some(splice) = plan_edit(&t, root.as_ref(), path, value)? else {
            return Err(format!(
                "cannot change {} in this file safely; edit it by hand",
                path.display()
            ));
        };
        let mut next = String::with_capacity(self.text.len() + splice.with.len());
        next.push_str(&self.text[..splice.from]);
        next.push_str(&splice.with);
        next.push_str(&self.text[splice.to..]);
        let now = to_value(&next).map_err(|e| {
            format!(
                "changing {} would break the file ({}); nothing was written",
                path.display(),
                e.message
            )
        })?;
        if now != expected {
            return Err(format!(
                "changing {} would change other parts of the file; nothing was written (edit it by hand)",
                path.display()
            ));
        }
        self.text = next;
        self.value = now;
        Ok(())
    }
}
