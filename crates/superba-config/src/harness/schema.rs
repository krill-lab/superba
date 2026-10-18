//! A small JSON Schema (2020-12) checker for the vendored harness schemas
//! (pi's `models.schema.json`): `type`, `const`, `enum`, `anyOf` / `oneOf` /
//! `allOf`, `properties`, `patternProperties` (the `^.*$` catch-all; other
//! patterns are not checked), `additionalProperties`, `required`, `items`,
//! `minItems` / `maxItems`, `minLength` / `maxLength`, `minimum` / `maximum`
//! / `exclusiveMinimum` / `exclusiveMaximum` and local `$ref`s. Annotations
//! (`description`, `default`, `deprecated`) are ignored.

use serde_json::{Map, Value};

/// The first problem of `value` against `schema`: `(path, message)`.
pub fn check(schema: &Value, value: &Value) -> Result<(), (String, String)> {
    walk(schema, schema, value, "")
}

fn resolve<'a>(root: &'a Value, r: &str) -> Option<&'a Value> {
    let path = r.strip_prefix("#/")?;
    let mut node = root;
    for part in path.split('/') {
        node = node.get(part.replace("~1", "/").replace("~0", "~"))?;
    }
    Some(node)
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    let actual = type_name(v);
    actual == t
        || (t == "number" && actual == "integer")
        || (t == "integer" && v.as_f64().is_some_and(|f| f.fract() == 0.0))
}

fn fail(path: &str, msg: impl Into<String>) -> Result<(), (String, String)> {
    Err((
        if path.is_empty() {
            "/".into()
        } else {
            path.to_owned()
        },
        msg.into(),
    ))
}

fn walk(root: &Value, schema: &Value, v: &Value, path: &str) -> Result<(), (String, String)> {
    let s = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Bool(false) => return fail(path, "no value is allowed here"),
        Value::Object(s) => s,
        _ => return Ok(()),
    };
    if let Some(Value::String(r)) = s.get("$ref") {
        let target =
            resolve(root, r).ok_or_else(|| (path.to_owned(), format!("unknown $ref {r}")))?;
        walk(root, target, v, path)?;
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)),
            _ => true,
        };
        if !ok {
            return fail(path, format!("expected {t}, found {}", type_name(v)));
        }
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        return fail(path, format!("must be {c}"));
    }
    if let Some(Value::Array(e)) = s.get("enum")
        && !e.contains(v)
    {
        return fail(path, format!("must be one of {}", Value::Array(e.clone())));
    }
    if let Some(Value::Array(all)) = s.get("allOf") {
        for sub in all {
            walk(root, sub, v, path)?;
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(any)) = s.get(key) {
            let passing = any
                .iter()
                .filter(|sub| walk(root, sub, v, path).is_ok())
                .count();
            let ok = if key == "anyOf" {
                passing > 0
            } else {
                passing == 1
            };
            if !ok {
                // Report the first alternative's problem (the usual shape).
                let first = any.iter().find_map(|sub| walk(root, sub, v, path).err());
                return match first {
                    Some(e) => Err(e),
                    None => fail(path, "matches more than one alternative"),
                };
            }
        }
    }
    match v {
        Value::Object(o) => object(root, s, o, path)?,
        Value::Array(a) => {
            if let Some(items) = s.get("items") {
                for (i, item) in a.iter().enumerate() {
                    walk(root, items, item, &format!("{path}/{i}"))?;
                }
            }
            if let Some(n) = s.get("minItems").and_then(Value::as_u64)
                && (a.len() as u64) < n
            {
                return fail(path, format!("needs at least {n} items"));
            }
            if let Some(n) = s.get("maxItems").and_then(Value::as_u64)
                && (a.len() as u64) > n
            {
                return fail(path, format!("takes at most {n} items"));
            }
        }
        Value::String(t) => {
            let len = t.chars().count() as u64;
            if let Some(n) = s.get("minLength").and_then(Value::as_u64)
                && len < n
            {
                return fail(path, format!("must be at least {n} characters"));
            }
            if let Some(n) = s.get("maxLength").and_then(Value::as_u64)
                && len > n
            {
                return fail(path, format!("must be at most {n} characters"));
            }
        }
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(0.0);
            let num = |k: &str| s.get(k).and_then(Value::as_f64);
            if let Some(m) = num("minimum")
                && x < m
            {
                return fail(path, format!("must be at least {m}"));
            }
            if let Some(m) = num("maximum")
                && x > m
            {
                return fail(path, format!("must be at most {m}"));
            }
            if let Some(m) = num("exclusiveMinimum")
                && x <= m
            {
                return fail(path, format!("must be more than {m}"));
            }
            if let Some(m) = num("exclusiveMaximum")
                && x >= m
            {
                return fail(path, format!("must be less than {m}"));
            }
        }
        _ => {}
    }
    Ok(())
}

fn object(
    root: &Value,
    s: &Map<String, Value>,
    o: &Map<String, Value>,
    path: &str,
) -> Result<(), (String, String)> {
    if let Some(Value::Array(req)) = s.get("required") {
        for r in req.iter().filter_map(Value::as_str) {
            if !o.contains_key(r) {
                return fail(path, format!("{r} is required"));
            }
        }
    }
    let props = s.get("properties").and_then(Value::as_object);
    let patterns = s.get("patternProperties").and_then(Value::as_object);
    for (k, v) in o {
        let p = format!("{path}/{k}");
        let mut known = false;
        if let Some(sub) = props.and_then(|ps| ps.get(k)) {
            known = true;
            walk(root, sub, v, &p)?;
        }
        if let Some(pats) = patterns {
            for (pat, sub) in pats {
                if pat == "^.*$" || pat == ".*" || (pat == "^.+$" && !k.is_empty()) {
                    known = true;
                    walk(root, sub, v, &p)?;
                }
            }
        }
        if !known {
            match s.get("additionalProperties") {
                Some(Value::Bool(false)) => return fail(&p, "is not allowed here"),
                Some(sub @ Value::Object(_)) => walk(root, sub, v, &p)?,
                _ => {}
            }
        }
    }
    Ok(())
}
