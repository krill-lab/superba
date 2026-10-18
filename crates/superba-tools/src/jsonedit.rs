//! Changing one setting in a tool's JSON file while keeping the rest:
//!
//! - [`set_path`] / [`remove_path`]: a strict JSON object (Claude Code's
//!   `settings.json`), printed again with two-space indentation and the keys
//!   in their order;
//! - [`jsonc_set`] / [`jsonc_remove`]: a top-level key of a JSONC file
//!   (opencode's `opencode.json[c]`) edited in the text itself, so comments,
//!   trailing commas and formatting stay as they are.

use serde_json::{Map, Value};
use superba_core::{Error, Result};

/// Parse `bytes` (absent or empty: `{}`) as a JSON object.
pub fn parse_object(bytes: Option<&[u8]>, what: &str) -> Result<Map<String, Value>> {
    let text = bytes.map(String::from_utf8_lossy).unwrap_or_default();
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(Error::parse(what, "not a JSON object")),
        Err(e) => Err(Error::parse(what, e.to_string())),
    }
}

/// The value at the dotted `path` (`env.DISABLE_AUTOUPDATER`).
pub fn get_path<'a>(obj: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut cur = obj.get(parts.next()?)?;
    for p in parts {
        cur = cur.as_object()?.get(p)?;
    }
    Some(cur)
}

/// Set the dotted `path` to `value`, creating objects on the way; refused
/// when a part on the way is not an object.
pub fn set_path(obj: &mut Map<String, Value>, path: &str, value: Value) -> Result<()> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, head) = parts
        .split_last()
        .ok_or_else(|| Error::invalid("empty key"))?;
    let mut cur = obj;
    for p in head {
        let next = cur
            .entry((*p).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        cur = next.as_object_mut().ok_or_else(|| Error::Invalid {
            message: format!("`{p}` is not an object; the file was left alone"),
        })?;
    }
    cur.insert((*last).to_owned(), value);
    Ok(())
}

/// Remove the dotted `path`; an object on the way that becomes empty is
/// removed too when `prune` is set.
pub fn remove_path(obj: &mut Map<String, Value>, path: &str, prune: bool) -> Option<Value> {
    let parts: Vec<&str> = path.split('.').collect();
    remove_rec(obj, &parts, prune)
}

fn remove_rec(obj: &mut Map<String, Value>, parts: &[&str], prune: bool) -> Option<Value> {
    match parts {
        [] => None,
        [last] => obj.shift_remove(*last),
        [head, rest @ ..] => {
            let child = obj.get_mut(*head)?.as_object_mut()?;
            let removed = remove_rec(child, rest, prune);
            if prune && removed.is_some() && child.is_empty() {
                obj.shift_remove(*head);
            }
            removed
        }
    }
}

/// `obj` printed with two-space indentation and a final newline.
pub fn print_object(obj: &Map<String, Value>) -> Vec<u8> {
    let mut out = serde_json::to_vec_pretty(&Value::Object(obj.clone())).unwrap_or_default();
    out.push(b'\n');
    out
}

/// One top-level member of a JSONC object: byte ranges of the whole member
/// (key to value end) and of the value.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Member {
    key: String,
    start: usize,
    value: std::ops::Range<usize>,
}

/// The top-level object of a JSONC text: where `{` and `}` are and its
/// members. `None` when the text is not one object.
fn scan(text: &str) -> Option<(usize, usize, Vec<Member>)> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut depth = 0_usize;
    let mut open = None;
    let mut close = None;
    let mut members = Vec::new();
    // State inside the top-level object.
    let mut pending_key: Option<(String, usize)> = None;
    let mut value_start: Option<usize> = None;
    let mut current: Option<(String, usize, usize)> = None;
    let mut last_value_end = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let end = (i + 1).min(b.len());
                if depth == 1 && value_start.is_none() && pending_key.is_none() {
                    let key: String = serde_json::from_str(&text[start..end]).ok()?;
                    pending_key = Some((key, start));
                } else if depth == 1 && value_start == Some(usize::MAX) {
                    value_start = Some(start);
                    last_value_end = end;
                } else if depth == 1 {
                    last_value_end = end;
                }
                i = end;
                continue;
            }
            b':' if depth == 1 => {
                let (k, s) = pending_key.take()?;
                current = Some((k, s, 0));
                value_start = Some(usize::MAX);
            }
            b'{' | b'[' => {
                if depth == 0 {
                    if c != b'{' || open.is_some() {
                        return None;
                    }
                    open = Some(i);
                } else if depth == 1 && value_start == Some(usize::MAX) {
                    value_start = Some(i);
                }
                depth += 1;
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 1 {
                    last_value_end = i + 1;
                }
                if depth == 0 {
                    if let (Some((k, s, _)), Some(vs)) = (current.take(), value_start.take())
                        && vs != usize::MAX
                    {
                        members.push(Member {
                            key: k,
                            start: s,
                            value: vs..last_value_end,
                        });
                    }
                    close = Some(i);
                    break;
                }
            }
            b',' if depth == 1 => {
                if let (Some((k, s, _)), Some(vs)) = (current.take(), value_start.take())
                    && vs != usize::MAX
                {
                    members.push(Member {
                        key: k,
                        start: s,
                        value: vs..last_value_end,
                    });
                }
            }
            c if c.is_ascii_whitespace() => {}
            _ => {
                if depth == 0 {
                    return None;
                }
                if depth == 1 {
                    if value_start == Some(usize::MAX) {
                        value_start = Some(i);
                    }
                    last_value_end = i + 1;
                }
            }
        }
        i += 1;
    }
    Some((open?, close?, members))
}

/// Strip comments and trailing commas so `serde_json` reads a JSONC text.
pub fn jsonc_to_json(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.push_str(&text[start..i]);
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b',' => {
                let rest = &text[i + 1..];
                let next = jsonc_to_json_peek(rest);
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(',');
                }
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The next character that is not whitespace or inside a comment.
fn jsonc_to_json_peek(text: &str) -> Option<char> {
    let stripped = jsonc_to_json_nocomma(text);
    stripped.chars().find(|c| !c.is_whitespace())
}

fn jsonc_to_json_nocomma(text: &str) -> String {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => return text[i..].to_owned(),
        }
    }
    String::new()
}

/// Parse a JSONC text as an object.
pub fn jsonc_object(text: &str, what: &str) -> Result<Map<String, Value>> {
    let json = jsonc_to_json(text.trim_start_matches('\u{feff}'));
    parse_object(Some(json.as_bytes()), what)
}

/// The JSONC `text` with the top-level `key` set to `value` (JSON text):
/// an existing member's value is replaced in place, a new member goes first
/// in the object. Refused when the text is not one object.
pub fn jsonc_set(text: &str, key: &str, value: &str, what: &str) -> Result<String> {
    if text.trim().is_empty() {
        return Ok(format!(
            "{{\n  {}: {value}\n}}\n",
            serde_json::to_string(key).unwrap_or_default()
        ));
    }
    jsonc_object(text, what)?;
    let (open, _close, members) =
        scan(text).ok_or_else(|| Error::parse(what, "not a JSON object"))?;
    if let Some(m) = members.iter().find(|m| m.key == key) {
        let mut out = text.to_owned();
        out.replace_range(m.value.clone(), value);
        return Ok(out);
    }
    let indent = members
        .first()
        .map(|m| {
            let line_start = text[..m.start].rfind('\n').map_or(0, |i| i + 1);
            text[line_start..m.start].to_owned()
        })
        .filter(|s| s.chars().all(|c| c == ' ' || c == '\t'))
        .unwrap_or_else(|| "  ".into());
    let entry = format!(
        "{}: {value}",
        serde_json::to_string(key).unwrap_or_default()
    );
    let mut out = text.to_owned();
    if members.is_empty() {
        let close = scan(text).map(|s| s.1).unwrap_or(open + 1);
        out.replace_range(open + 1..close, &format!("\n{indent}{entry}\n"));
    } else {
        out.insert_str(open + 1, &format!("\n{indent}{entry},"));
    }
    jsonc_object(&out, what)?;
    Ok(out)
}

/// The JSONC `text` without the top-level `key` (unchanged when absent).
pub fn jsonc_remove(text: &str, key: &str, what: &str) -> Result<String> {
    if text.trim().is_empty() {
        return Ok(text.to_owned());
    }
    jsonc_object(text, what)?;
    let (_open, close, members) =
        scan(text).ok_or_else(|| Error::parse(what, "not a JSON object"))?;
    let Some(pos) = members.iter().position(|m| m.key == key) else {
        return Ok(text.to_owned());
    };
    let m = &members[pos];
    let b = text.as_bytes();
    // From the member's line start (when only whitespace precedes it) to
    // after its comma (or, for the last member, from the previous comma).
    let line_start = text[..m.start].rfind('\n').map_or(0, |i| i + 1);
    let mut start = if text[line_start..m.start].trim().is_empty() {
        line_start
    } else {
        m.start
    };
    let mut end = m.value.end;
    let mut j = end;
    while j < close && b[j].is_ascii_whitespace() && b[j] != b'\n' {
        j += 1;
    }
    if j < close && b[j] == b',' {
        end = j + 1;
        if b.get(end) == Some(&b'\n') {
            end += 1;
        } else if start == line_start {
            while end < close && b[end] == b' ' {
                end += 1;
            }
        }
    } else if pos > 0 {
        let prev_end = members[pos - 1].value.end;
        if let Some(rel) = text[prev_end..m.start].find(',') {
            start = prev_end + rel;
        }
    } else if start == line_start && b.get(end) == Some(&b'\n') {
        end += 1;
    }
    let mut out = text.to_owned();
    out.replace_range(start..end, "");
    jsonc_object(&out, what)?;
    Ok(out)
}

/// The top-level `key` of a JSONC text as JSON text, when present.
pub fn jsonc_get(text: &str, key: &str) -> Option<String> {
    let (_, _, members) = scan(text)?;
    members
        .iter()
        .find(|m| m.key == key)
        .map(|m| text[m.value.clone()].to_owned())
}
