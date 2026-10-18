//! A `.env` editor (dsh's `$DSH_HOME/.env`): one `NAME=value` line per owned
//! variable, changed or removed in place; every other line (comments, other
//! variables, blank lines) stays as it is.

use serde_json::{Map, Value};

use super::slot::{Seg, SlotPath};

fn name_of(line: &str) -> Option<(&str, bool)> {
    let l = line.trim_start();
    if l.starts_with('#') {
        return None;
    }
    let (l, export) = match l.strip_prefix("export ") {
        Some(rest) => (rest.trim_start(), true),
        None => (l, false),
    };
    let (name, _) = l.split_once('=')?;
    let name = name.trim();
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then_some((name, export))
}

fn value_of(line: &str) -> String {
    let rest = line.split_once('=').map_or("", |(_, v)| v).trim();
    for q in ['\'', '"', '`'] {
        if let Some(inner) = rest.strip_prefix(q)
            && let Some(end) = inner.find(q)
        {
            return inner[..end].to_owned();
        }
    }
    rest.split(" #").next().unwrap_or("").trim().to_owned()
}

/// The variables of a `.env` text (the last of a repeated name wins).
pub fn to_value(text: &str) -> Value {
    let mut out = Map::new();
    for line in text.lines() {
        if let Some((name, _)) = name_of(line) {
            out.insert(name.to_owned(), Value::String(value_of(line)));
        }
    }
    Value::Object(out)
}

/// How a value is written: bare when safe, else in single quotes; values
/// that cannot be written so that every `.env` reader gets them back are
/// refused.
pub fn quote(value: &str) -> Result<String, String> {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:/+=~@%,".contains(c))
        && !value.is_empty()
    {
        return Ok(value.to_owned());
    }
    if value.contains(['\'', '\n', '\r']) {
        return Err("the value cannot be written to a .env file".into());
    }
    Ok(format!("'{value}'"))
}

/// A `.env` document edited in place.
#[derive(Clone, Debug)]
pub struct DotenvDoc {
    text: String,
    value: Value,
}

impl DotenvDoc {
    /// Read a document.
    pub fn parse(text: &str) -> Self {
        Self {
            value: to_value(text),
            text: text.to_owned(),
        }
    }

    /// The text now.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The variables now.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Set (or remove) one variable. Its path is one key.
    pub fn set(&mut self, path: &SlotPath, value: Option<&Value>) -> Result<(), String> {
        let [Seg::Key(name)] = path.0.as_slice() else {
            return Err("a .env entry is one variable".into());
        };
        let new_line = match value {
            None => None,
            Some(Value::String(s)) => Some(format!("{name}={}", quote(s)?)),
            Some(_) => return Err("a .env value is text".into()),
        };
        let crlf = self.text.contains("\r\n");
        let nl = if crlf { "\r\n" } else { "\n" };
        let mut lines: Vec<String> = self.text.lines().map(str::to_owned).collect();
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| name_of(l).is_some_and(|(n, _)| n == name))
            .map(|(i, _)| i)
            .collect();
        if hits.len() > 1 {
            return Err(format!(
                "{name} is set {} times in the .env file; remove the extra lines first",
                hits.len()
            ));
        }
        match (hits.first(), new_line) {
            (Some(&i), Some(l)) => {
                let export = name_of(&lines[i]).is_some_and(|(_, e)| e);
                lines[i] = if export { format!("export {l}") } else { l };
            }
            (Some(&i), None) => {
                lines.remove(i);
            }
            (None, Some(l)) => lines.push(l),
            (None, None) => return Ok(()),
        }
        let mut next = lines.join(nl);
        if !next.is_empty()
            && (self.text.ends_with('\n') || self.text.is_empty() || value.is_some())
        {
            next.push_str(nl);
        }
        let now = to_value(&next);
        let mut expected = self.value.clone();
        if let Value::Object(o) = &mut expected {
            match value {
                Some(v) => {
                    o.insert(name.clone(), v.clone());
                }
                None => {
                    o.shift_remove(name);
                }
            }
        }
        if now != expected {
            return Err(
                "changing the .env file would change other variables; nothing was written".into(),
            );
        }
        self.text = next;
        self.value = now;
        Ok(())
    }
}
