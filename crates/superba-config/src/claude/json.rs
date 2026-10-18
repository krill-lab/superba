//! Pure JSON work on Claude Code's `settings.json`: parse, read and change
//! dotted paths (`model`, `env.ANTHROPIC_BASE_URL`) while every other key keeps
//! its value and place, and print the file.

use serde_json::{Map, Value};
use superba_core::claude::{KEY_ENV, get_path};
use superba_core::{ClaudeFileStatus, Error, Result};

/// Parse file bytes (`None` = absent): the object and the file's state. An
/// absent or empty file is an empty object.
pub fn parse(bytes: Option<&[u8]>) -> (Option<Value>, ClaudeFileStatus) {
    let Some(bytes) = bytes else {
        return (Some(Value::Object(Map::new())), ClaudeFileStatus::Absent);
    };
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (
            None,
            ClaudeFileStatus::Unreadable {
                message: "not UTF-8 text".into(),
            },
        );
    };
    if text.trim().is_empty() {
        return (Some(Value::Object(Map::new())), ClaudeFileStatus::Valid);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v @ Value::Object(_)) => (Some(v), ClaudeFileStatus::Valid),
        Ok(_) => (None, ClaudeFileStatus::NotObject),
        Err(e) => (
            None,
            ClaudeFileStatus::Invalid {
                line: e.line(),
                column: e.column(),
                message: e.to_string(),
            },
        ),
    }
}

/// Set (`Some`) or remove (`None`) a dotted path. Existing keys keep their
/// place; new keys go last; an `env` block is created when needed. Returns
/// whether `env` was created.
pub fn set_path(root: &mut Value, path: &str, value: Option<Value>) -> Result<bool> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| Error::invalid("settings.json is not a JSON object"))?;
    match path.split_once('.') {
        Some((KEY_ENV, name)) => {
            let mut created = false;
            if !obj.contains_key(KEY_ENV) {
                if value.is_none() {
                    return Ok(false);
                }
                obj.insert(KEY_ENV.into(), Value::Object(Map::new()));
                created = true;
            }
            let env = obj
                .get_mut(KEY_ENV)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    Error::invalid("settings.json: \"env\" is not an object; fix it first")
                })?;
            match value {
                Some(v) => {
                    env.insert(name.to_owned(), v);
                }
                None => {
                    env.shift_remove(name);
                }
            }
            Ok(created)
        }
        _ => {
            match value {
                Some(v) => {
                    obj.insert(path.to_owned(), v);
                }
                None => {
                    obj.shift_remove(path);
                }
            }
            Ok(false)
        }
    }
}

/// The value at a dotted path, cloned.
pub fn value_at(root: &Value, path: &str) -> Option<Value> {
    get_path(root, path).cloned()
}

/// Remove `env` when it is an empty object.
pub fn drop_empty_env(root: &mut Value) {
    if let Some(obj) = root.as_object_mut()
        && obj
            .get(KEY_ENV)
            .and_then(Value::as_object)
            .is_some_and(Map::is_empty)
    {
        obj.shift_remove(KEY_ENV);
    }
}

/// The file text: two-space indentation (as Claude Code writes it) and a
/// final newline.
pub fn print(root: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec_pretty(root).unwrap_or_else(|_| b"{}".to_vec());
    out.push(b'\n');
    out
}
