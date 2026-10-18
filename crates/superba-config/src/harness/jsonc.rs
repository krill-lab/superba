//! A JSON / JSONC editor that changes only the entries it is asked to and
//! keeps comments, formatting and every other member in place (opencode's
//! `opencode.json[c]`, pi's `models.json` / `settings.json`, ZCode's
//! `provider_config.json`, WorkBuddy's `models.json`).
//!
//! Edits go through `jsonc-parser`'s concrete syntax tree (the editor
//! opencode itself and Deno use); new members take the file's indentation.
//! Every edit is checked like the YAML editor's: the new text must parse
//! (with the file's own rules: comments only where the harness reads them)
//! and mean exactly the old document with the change applied.

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstNode, CstRootNode};
use serde_json::Value;

use super::slot::{self, Seg, SlotPath};

/// The syntax a harness reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonFlavor {
    /// Plain JSON (ZCode, WorkBuddy, pi's `settings.json`).
    Strict,
    /// JSON with `//` and `/* */` comments, no trailing commas (pi's
    /// `models.json`: comments are stripped, then `JSON.parse`).
    Comments,
    /// JSONC: comments and trailing commas (opencode).
    Jsonc,
}

impl JsonFlavor {
    fn options(self) -> ParseOptions {
        let loose = self == JsonFlavor::Jsonc;
        ParseOptions {
            allow_comments: self != JsonFlavor::Strict,
            allow_loose_object_property_names: false,
            allow_trailing_commas: loose,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
            ..ParseOptions::default()
        }
    }
}

/// A parse problem: 1-based line and column, and the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// 1-based line.
    pub line: usize,
    /// 1-based column.
    pub column: usize,
    /// What is wrong.
    pub message: String,
}

/// The meaning of a document (`null` for an empty one).
pub fn to_value(text: &str, flavor: JsonFlavor) -> Result<Value, JsonError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    jsonc_parser::parse_to_serde_value::<Value>(text, &flavor.options()).map_err(|e| JsonError {
        line: e.line_display(),
        column: e.column_display(),
        message: e.kind().to_string(),
    })
}

fn input(v: &Value) -> CstInputValue {
    match v {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(s.clone()),
        Value::Array(a) => CstInputValue::Array(a.iter().map(input).collect()),
        Value::Object(o) => {
            CstInputValue::Object(o.iter().map(|(k, v)| (k.clone(), input(v))).collect())
        }
    }
}

fn is_null(node: &CstNode) -> bool {
    node.to_serde_value().is_some_and(|v| v.is_null())
}

/// Make the edit in the tree; `Ok(false)` = nothing to change.
fn edit(root: &CstRootNode, path: &SlotPath, value: Option<&Value>) -> Result<bool, String> {
    let segs = &path.0;
    let Some(mut cur) = root.value() else {
        let Some(v) = value else { return Ok(false) };
        let mut whole = Value::Null;
        slot::set(&mut whole, path, Some(v.clone()))?;
        root.set_value(input(&whole));
        return Ok(true);
    };
    if matches!(segs.first(), Some(Seg::Key(_)))
        && value.is_some()
        && cur.as_array().is_some_and(|a| a.elements().is_empty())
    {
        // A bare `[]` (WorkBuddy's fresh models.json) becomes an object.
        root.set_value(CstInputValue::Object(Vec::new()));
        cur = root.value().ok_or("the document lost its root")?;
    }
    for (i, seg) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        match seg {
            Seg::Key(k) => {
                let obj = cur
                    .as_object()
                    .ok_or_else(|| format!("expected an object before {k}"))?;
                match obj.get(k) {
                    Some(prop) => {
                        if last {
                            match value {
                                Some(v) => prop.set_value(input(v)),
                                None => prop.remove(),
                            }
                            return Ok(true);
                        }
                        let child = prop.value().ok_or_else(|| format!("{k} has no value"))?;
                        if is_null(&child) {
                            let Some(v) = value else { return Ok(false) };
                            let built = slot::build(&segs[i..], v.clone())?;
                            prop.set_value(input(&built));
                            return Ok(true);
                        }
                        cur = child;
                    }
                    None => {
                        let Some(v) = value else { return Ok(false) };
                        let built = slot::build(&segs[i..], v.clone())?;
                        obj.append(k, input(&built));
                        return Ok(true);
                    }
                }
            }
            Seg::Find(fields) => {
                let arr = cur.as_array().ok_or("expected a list")?;
                let hits: Vec<CstNode> = arr
                    .elements()
                    .into_iter()
                    .filter(|e| {
                        e.to_serde_value()
                            .is_some_and(|v| slot::matches_find(&v, fields))
                    })
                    .collect();
                if hits.len() > 1 {
                    return Err("several entries match; remove the duplicates first".into());
                }
                match hits.into_iter().next() {
                    Some(e) => {
                        if last {
                            let index = e.element_index().ok_or("not a list element")?;
                            e.remove();
                            if let Some(v) = value {
                                let built = slot::build(std::slice::from_ref(seg), v.clone())?;
                                arr.insert(index, input(&built));
                            }
                            return Ok(true);
                        }
                        cur = e;
                    }
                    None => {
                        let Some(v) = value else { return Ok(false) };
                        let built = slot::build(&segs[i..], v.clone())?;
                        arr.append(input(&built));
                        return Ok(true);
                    }
                }
            }
            Seg::Item(s) | Seg::FirstItem(s) => {
                if !last {
                    return Err("an item cannot hold anything".into());
                }
                let arr = cur.as_array().ok_or("expected a list")?;
                let hits: Vec<CstNode> = arr
                    .elements()
                    .into_iter()
                    .filter(|e| {
                        e.to_serde_value()
                            .is_some_and(|v| v.as_str() == Some(s.as_str()))
                    })
                    .collect();
                match value {
                    Some(_) if !hits.is_empty() => return Ok(false),
                    Some(_) => {
                        if matches!(seg, Seg::FirstItem(_)) {
                            arr.insert(0, CstInputValue::String(s.clone()));
                        } else {
                            arr.append(CstInputValue::String(s.clone()));
                        }
                    }
                    None => {
                        if hits.is_empty() {
                            return Ok(false);
                        }
                        for h in hits {
                            h.remove();
                        }
                    }
                }
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// A JSON / JSONC document edited in place (see the module docs).
#[derive(Clone, Debug)]
pub struct JsonDoc {
    text: String,
    value: Value,
    flavor: JsonFlavor,
}

impl JsonDoc {
    /// Parse a document.
    pub fn parse(text: &str, flavor: JsonFlavor) -> Result<Self, JsonError> {
        Ok(Self {
            value: to_value(text, flavor)?,
            text: text.to_owned(),
            flavor,
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
        let bom = self.text.starts_with('\u{feff}');
        let body = self.text.strip_prefix('\u{feff}').unwrap_or(&self.text);
        let root = CstRootNode::parse(body, &self.flavor.options())
            .map_err(|e| format!("the file is not valid JSON: {}", e.kind()))?;
        if !edit(&root, path, value)? {
            return Err(format!(
                "cannot change {} in this file safely; edit it by hand",
                path.display()
            ));
        }
        let mut next = root.to_string();
        if !next.ends_with('\n') && (body.ends_with('\n') || body.trim().is_empty()) {
            next.push('\n');
        }
        if bom {
            next.insert(0, '\u{feff}');
        }
        let now = to_value(&next, self.flavor).map_err(|e| {
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
