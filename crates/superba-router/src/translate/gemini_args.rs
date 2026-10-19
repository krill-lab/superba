//! Gemini functionCall partialArgs from gateways/Vertex. Each value names a
//! singular JSON path; strings may continue in subsequent response chunks.

use super::{Invalid, gemini::field};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Arguments {
    value: Value,
    continuing: BTreeSet<String>,
}

enum Segment {
    Key(String),
    Index(usize),
}

/// Accept singular named children and array indexes, never JSONPath queries.
fn path(s: &str) -> Result<Vec<Segment>, Invalid> {
    let bad = || Invalid::new("unsupported Gemini partial argument JSON path");
    let mut rest = s.strip_prefix('$').ok_or_else(bad)?;
    let mut out = Vec::new();
    while !rest.is_empty() {
        if out.len() >= 64 {
            return Err(bad());
        }
        if let Some(tail) = rest.strip_prefix('.') {
            let end = tail.find(['.', '[']).unwrap_or(tail.len());
            let key = &tail[..end];
            if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Err(bad());
            }
            out.push(Segment::Key(key.into()));
            rest = &tail[end..];
        } else if let Some(tail) = rest.strip_prefix('[') {
            if tail.starts_with('"') {
                let mut values = serde_json::Deserializer::from_str(tail).into_iter::<String>();
                let key = values.next().ok_or_else(bad)?.map_err(|_| bad())?;
                rest = tail[values.byte_offset()..]
                    .strip_prefix(']')
                    .ok_or_else(bad)?;
                out.push(Segment::Key(key));
            } else {
                let (number, tail) = tail.split_once(']').ok_or_else(bad)?;
                let index = number.parse::<usize>().map_err(|_| bad())?;
                if index > 4096 {
                    return Err(bad());
                }
                out.push(Segment::Index(index));
                rest = tail;
            }
        } else {
            return Err(bad());
        }
    }
    if out.is_empty() {
        return Err(bad());
    }
    Ok(out)
}

fn slot<'a>(mut value: &'a mut Value, path: &[Segment]) -> Result<&'a mut Value, Invalid> {
    for segment in path {
        value = match segment {
            Segment::Key(key) => {
                if value.is_null() {
                    *value = Value::Object(Default::default());
                }
                value
                    .as_object_mut()
                    .ok_or_else(|| Invalid::new("conflicting Gemini argument path"))?
                    .entry(key.clone())
                    .or_insert(Value::Null)
            }
            Segment::Index(index) => {
                if value.is_null() {
                    *value = Value::Array(Vec::new());
                }
                let array = value
                    .as_array_mut()
                    .ok_or_else(|| Invalid::new("conflicting Gemini argument path"))?;
                if array.len() <= *index {
                    array.resize(index + 1, Value::Null);
                }
                &mut array[*index]
            }
        };
    }
    Ok(value)
}

impl Arguments {
    pub(super) fn push(&mut self, arg: &Value) -> Result<(), Invalid> {
        let key = field(arg, "jsonPath", "json_path")
            .as_str()
            .ok_or_else(|| Invalid::new("Gemini partial argument needs a JSON path"))?;
        let segments = path(key)?;
        let mut values = [
            ("stringValue", "string_value"),
            ("numberValue", "number_value"),
            ("boolValue", "bool_value"),
            ("nullValue", "null_value"),
        ]
        .into_iter()
        .filter_map(|(camel, snake)| {
            arg.get(camel)
                .or_else(|| arg.get(snake))
                .map(|v| (camel, v))
        });
        let (kind, value) = values
            .next()
            .ok_or_else(|| Invalid::new("missing Gemini partial argument value"))?;
        if values.next().is_some() {
            return Err(Invalid::new("ambiguous Gemini partial argument value"));
        }
        let continues = field(arg, "willContinue", "will_continue").as_bool() == Some(true);
        let valid = match kind {
            "stringValue" => value.is_string(),
            "numberValue" => value.is_number(),
            "boolValue" => value.is_boolean(),
            _ => value == "NULL_VALUE",
        };
        if !valid || (continues && !value.is_string()) {
            return Err(Invalid::new("invalid Gemini partial argument value"));
        }
        let target = slot(&mut self.value, &segments)?;
        if self.continuing.remove(key) {
            let Value::String(current) = target else {
                return Err(Invalid::new("Gemini continuation is not a string"));
            };
            current.push_str(
                value
                    .as_str()
                    .ok_or_else(|| Invalid::new("Gemini continuation is not a string"))?,
            );
        } else {
            *target = if kind == "nullValue" {
                Value::Null
            } else {
                value.clone()
            };
        }
        if continues {
            self.continuing.insert(key.to_owned());
        }
        if self.value.to_string().len() > 16 * 1024 * 1024 {
            return Err(Invalid::new(
                "Gemini partial arguments exceed the translation limit",
            ));
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<String, Invalid> {
        if !self.continuing.is_empty() {
            return Err(Invalid::new("unfinished Gemini partial argument"));
        }
        Ok(if self.value.is_null() {
            "{}".into()
        } else {
            self.value.to_string()
        })
    }
}
