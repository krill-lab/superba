//! Flatten `codex-rs/core/config.schema.json` (schemars, draft-07) into dotted keys.
//!
//! `$ref` and single-element `allOf` wrappers are resolved while keeping the field
//! documentation (outer node) apart from the type documentation (the definition).
//! Unions collapse to `enum` (string-enum branches, plus a table form when an object
//! branch exists), `bool_or_table` (structured features) or `union`; tagged unions
//! get a discriminator and `variant_of` labels. Maps become `<name>` segments.

use anyhow::{Result, bail};
use serde_json::{Map, Value};

pub use superba_catalog::format::EnumValue;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigKey {
    /// Dotted path; map keys are `<name>` (e.g. `mcp_servers.<name>.command`).
    pub path: String,
    /// `boolean|string|integer|number|array|table|map|enum|union|any|recursive`.
    pub kind: String,
    /// JSON types accepted (from `type` and from union branches).
    pub types: Vec<String>,
    pub enum_values: Option<Vec<EnumValue>>,
    /// True when the enum also accepts a table form (e.g. `approval_policy = { granular = .. }`).
    pub enum_has_table_form: bool,
    pub items: Option<String>,
    pub format: Option<String>,
    pub minimum: Option<serde_json::Number>,
    pub maximum: Option<serde_json::Number>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    /// Schema default when not `null` (schemars emits `null` for `Option` fields, which
    /// means "computed by Codex at runtime", not "the default is null").
    pub default: Option<Value>,
    /// The raw schema `default` of this property (or of its definition), unpruned.
    pub raw_default: Option<Value>,
    /// `schema` (on the property) or `parent` (taken from the parent table's default).
    pub default_source: Option<&'static str>,
    /// Field doc (the property's own description, or a single-`allOf` wrapper's).
    pub description: Option<String>,
    /// Type doc of the referenced definition (shared by every field of that type;
    /// sometimes developer-facing, e.g. "Raw MCP config shape used for ...").
    pub type_description: Option<String>,
    /// Definition the value comes from (`#/definitions/X`).
    pub definition: Option<String>,
    /// Heuristic: description says deprecated / legacy / no-op / ignored.
    pub deprecated_hint: bool,
    /// Path is inside a map (`<name>` segment), e.g. per-profile or per-server keys.
    pub under_map: bool,
    /// Tagged unions (several object branches): the discriminator of this key —
    /// `kind` for internally tagged (`{kind = "header", ..}`), `<external>` for
    /// externally tagged (`{otlp-http = {..}}`), `<branch>` otherwise.
    pub discriminator: Option<String>,
    /// Keys that only exist in some branches of a tagged union:
    /// `<union path>:<label>` (e.g. `otel.exporter:otlp-http`,
    /// `model_providers.<name>.gateway_oauth.delivery:kind=header`). Mutually exclusive
    /// with keys of other labels of the same union.
    pub variant_of: Vec<String>,
}

struct Ctx<'a> {
    defs: &'a Map<String, Value>,
    out: Vec<ConfigKey>,
    /// Maps never expanded (their value type duplicates top-level keys).
    no_expand: &'a [&'a str],
}

/// A schema node after `$ref` / single-`allOf` resolution, with the outer
/// description/default taking precedence over the definition's.
struct Resolved<'a> {
    node: &'a Map<String, Value>,
    description: Option<String>,
    type_description: Option<String>,
    default: Option<Value>,
    definition: Option<String>,
}

fn resolve<'a>(
    defs: &'a Map<String, Value>,
    node: &'a Value,
    stack: &[String],
) -> Result<Option<Resolved<'a>>> {
    // `true` / `false` schemas carry no key information.
    let Some(mut cur) = node.as_object() else {
        return Ok(None);
    };
    let description = cur
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut type_description: Option<String> = None;
    let mut default = cur.get("default").cloned();
    let mut definition = None;
    loop {
        let next: Option<&'a Value> = if let Some(r) = cur.get("$ref").and_then(Value::as_str) {
            let name = r
                .strip_prefix("#/definitions/")
                .ok_or_else(|| anyhow::anyhow!("unsupported $ref `{r}`"))?;
            if stack.iter().any(|s| s == name) {
                return Ok(Some(Resolved {
                    node: cur,
                    description,
                    type_description,
                    default,
                    definition: Some(format!("recursive:{name}")),
                }));
            }
            definition = Some(name.to_string());
            Some(
                defs.get(name)
                    .ok_or_else(|| anyhow::anyhow!("missing definition `{name}`"))?,
            )
        } else if let Some(Value::Array(all)) = cur.get("allOf")
            && all.len() == 1
            && !cur.contains_key("properties")
            && !cur.contains_key("type")
        {
            Some(&all[0])
        } else {
            None
        };
        let Some(next) = next else { break };
        let Some(n) = next.as_object() else { break };
        cur = n;
        if type_description.is_none() {
            type_description = cur
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if default.is_none() {
            default = cur.get("default").cloned();
        }
    }
    Ok(Some(Resolved {
        node: cur,
        description,
        type_description,
        default,
        definition,
    }))
}

/// Drop `null` members (schemars serializes unset `Option` fields of a `Default`
/// impl as `null`); an object left empty means "no meaningful default".
fn prune_default(v: &Value) -> Option<Value> {
    match v {
        Value::Null => None,
        Value::Object(m) => {
            let pruned: Map<String, Value> = m
                .iter()
                .filter_map(|(k, v)| prune_default(v).map(|v| (k.clone(), v)))
                .collect();
            (!pruned.is_empty()).then_some(Value::Object(pruned))
        }
        other => Some(other.clone()),
    }
}

fn types_of(node: &Map<String, Value>) -> Vec<String> {
    match node.get("type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

fn deprecated_hint(desc: &Option<String>) -> bool {
    let Some(d) = desc else { return false };
    let l = d.to_lowercase();
    l.starts_with("deprecated")
        || l.starts_with("legacy no-op")
        || l.starts_with("legacy setting")
        || l.starts_with("legacy fallback")
        || l.contains("deprecated and ignored")
        || l.contains("compatibility-only")
        || l.contains("its value is ignored")
}

fn walk(
    ctx: &mut Ctx<'_>,
    path: &str,
    node: &Value,
    parent_default: Option<Value>,
    under_map: bool,
    variant: Option<String>,
    stack: &mut Vec<String>,
) -> Result<()> {
    let defs = ctx.defs;
    let Some(r) = resolve(defs, node, stack)? else {
        return Ok(());
    };
    let n = r.node;
    let mut key = ConfigKey {
        path: path.to_string(),
        description: r.description.clone().map(|d| d.trim().to_string()),
        type_description: r.type_description.clone().map(|d| d.trim().to_string()),
        definition: r.definition.clone(),
        under_map,
        variant_of: variant.iter().cloned().collect(),
        ..Default::default()
    };
    key.deprecated_hint = deprecated_hint(&key.description)
        || (key.description.is_none() && deprecated_hint(&key.type_description));
    match (
        r.default.as_ref().and_then(prune_default),
        parent_default.as_ref().and_then(prune_default),
    ) {
        (Some(d), _) => {
            key.default = Some(d);
            key.default_source = Some("schema");
        }
        (None, Some(p)) => {
            key.default = Some(p);
            key.default_source = Some("parent");
        }
        _ => {}
    }
    key.format = n.get("format").and_then(Value::as_str).map(str::to_string);
    key.raw_default = r.default.clone();
    key.minimum = n.get("minimum").and_then(Value::as_number).cloned();
    key.maximum = n.get("maximum").and_then(Value::as_number).cloned();
    key.min_length = n.get("minLength").and_then(Value::as_u64);
    key.max_length = n.get("maxLength").and_then(Value::as_u64);
    key.types = types_of(n);

    if let Some(def) = &r.definition
        && def.starts_with("recursive:")
    {
        key.kind = "recursive".into();
        ctx.out.push(key);
        return Ok(());
    }
    let pushed_def = r.definition.clone();
    if let Some(d) = &pushed_def {
        stack.push(d.clone());
    }

    // (name, schema, variant label inherited by the child subtree)
    let mut children: Vec<(String, Value, Option<String>)> = Vec::new();
    let mut map_value: Option<Value> = None;

    if let Some(Value::Array(vals)) = n.get("enum") {
        key.kind = "enum".into();
        key.enum_values = Some(
            vals.iter()
                .map(|v| EnumValue {
                    value: v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string()),
                    description: None,
                })
                .collect(),
        );
    } else if let Some(Value::Array(branches)) = n.get("oneOf").or_else(|| n.get("anyOf")) {
        // Union: string-enum branches, scalar branches, object branches.
        let mut values = Vec::new();
        let mut types = Vec::new();
        let mut has_object = false;
        let objects = object_branches(defs, branches, stack)?;
        let labels = branch_labels(defs, &objects, stack)?;
        if objects.len() >= 2 {
            key.discriminator = Some(labels.0.clone());
        }
        let mut obj_idx = 0usize;
        for b in branches {
            let Some(rb) = resolve(defs, b, stack)? else {
                continue;
            };
            let bn = rb.node;
            if let Some(Value::Array(vals)) = bn.get("enum") {
                for v in vals {
                    values.push(EnumValue {
                        value: v
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string()),
                        description: rb.description.clone().or(rb.type_description.clone()),
                    });
                }
                for t in types_of(bn) {
                    if !types.contains(&t) {
                        types.push(t);
                    }
                }
            } else if bn.contains_key("properties")
                || bn.get("type").and_then(Value::as_str) == Some("object")
            {
                has_object = true;
                if !types.contains(&"object".to_string()) {
                    types.push("object".into());
                }
                let label = (objects.len() >= 2).then(|| format!("{path}:{}", labels.1[obj_idx]));
                obj_idx += 1;
                if let Some(Value::Object(props)) = bn.get("properties") {
                    for (k, v) in props {
                        children.push((
                            k.clone(),
                            v.clone(),
                            label.clone().or_else(|| variant.clone()),
                        ));
                    }
                }
                if let Some(ap @ Value::Object(_)) = bn.get("additionalProperties") {
                    map_value = Some(ap.clone());
                }
            } else {
                for t in types_of(bn) {
                    if !types.contains(&t) {
                        types.push(t);
                    }
                }
            }
        }
        key.types = types;
        if !values.is_empty() {
            key.kind = "enum".into();
            key.enum_values = Some(values);
            key.enum_has_table_form = has_object;
        } else if has_object {
            key.kind = if key.types.iter().any(|t| t == "boolean") {
                "bool_or_table".into()
            } else if objects.len() == 1 && key.types == ["object"] {
                // One-variant tagged enum (`ThreadStoreToml`: `{type = "local"}`).
                "table".into()
            } else {
                "union".into()
            };
        } else {
            key.kind = "union".into();
        }
    } else if let Some(Value::Object(props)) = n.get("properties") {
        key.kind = "table".into();
        for (k, v) in props {
            children.push((k.clone(), v.clone(), variant.clone()));
        }
        // Fixed keys plus free-form entries (e.g. `plugins._default` + `plugins.<name>`).
        if let Some(ap @ Value::Object(_)) = n.get("additionalProperties") {
            map_value = Some(ap.clone());
        }
    } else if let Some(ap @ Value::Object(_)) = n.get("additionalProperties") {
        key.kind = "map".into();
        map_value = Some(ap.clone());
    } else if let Some(t) = key.types.first() {
        key.kind = match t.as_str() {
            "object" => "table".into(),
            other => other.to_string(),
        };
        if t == "array"
            && let Some(items) = n.get("items")
            && let Some(ri) = resolve(defs, items, stack)?
        {
            let it = types_of(ri.node);
            key.items = Some(match (ri.definition, it.first()) {
                (Some(d), _) => d,
                (None, Some(t)) => t.clone(),
                _ => "any".into(),
            });
        }
    } else {
        key.kind = "any".into();
    }

    let own_default = key.default.clone();
    if !path.is_empty() {
        ctx.out.push(key);
    }
    let top = path.split('.').next().unwrap_or("");
    let expand =
        !(ctx.no_expand.contains(&top) && path.contains('.')) && !ctx.no_expand.contains(&path);
    for (k, v, var) in children {
        let child_default = own_default.as_ref().and_then(|d| d.get(&k)).cloned();
        let p = if path.is_empty() {
            k.clone()
        } else {
            format!("{path}.{k}")
        };
        walk(ctx, &p, &v, child_default, under_map, var, stack)?;
    }
    if let Some(mv) = map_value
        && expand
    {
        walk(
            ctx,
            &format!("{path}.<name>"),
            &mv,
            None,
            true,
            variant.clone(),
            stack,
        )?;
    }
    if pushed_def.is_some() {
        stack.pop();
    }
    Ok(())
}

pub fn flatten(schema: &Value, no_expand: &[&str]) -> Result<Vec<ConfigKey>> {
    let Some(defs) = schema.get("definitions").and_then(Value::as_object) else {
        bail!("schema has no definitions");
    };
    let mut ctx = Ctx {
        defs,
        out: Vec::new(),
        no_expand,
    };
    let mut stack = Vec::new();
    walk(&mut ctx, "", schema, None, false, None, &mut stack)?;
    Ok(merge_duplicates(ctx.out))
}

/// Object branches of a union (after `$ref` resolution), in branch order.
fn object_branches<'a>(
    defs: &'a Map<String, Value>,
    branches: &'a [Value],
    stack: &[String],
) -> Result<Vec<&'a Map<String, Value>>> {
    let mut out = Vec::new();
    for b in branches {
        if let Some(rb) = resolve(defs, b, stack)?
            && rb.node.get("enum").is_none()
            && (rb.node.contains_key("properties")
                || rb.node.get("type").and_then(Value::as_str) == Some("object"))
        {
            out.push(rb.node);
        }
    }
    Ok(out)
}

/// Single string value of a property schema (`enum: [x]` or `const: x`).
fn single_value(defs: &Map<String, Value>, v: &Value, stack: &[String]) -> Result<Option<String>> {
    let Some(r) = resolve(defs, v, stack)? else {
        return Ok(None);
    };
    if let Some(Value::String(c)) = r.node.get("const") {
        return Ok(Some(c.clone()));
    }
    Ok(match r.node.get("enum") {
        Some(Value::Array(a)) if a.len() == 1 => a[0].as_str().map(str::to_string),
        _ => None,
    })
}

/// `(discriminator, per-branch labels)`: internally tagged (a property every branch
/// fixes to one string value) → `kind`, `kind=header`; externally tagged (each branch
/// has exactly one required property) → `<external>`, `otlp-http`; else `<branch>`, `#i`.
fn branch_labels(
    defs: &Map<String, Value>,
    objects: &[&Map<String, Value>],
    stack: &[String],
) -> Result<(String, Vec<String>)> {
    let props = |o: &Map<String, Value>| {
        o.get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    if let Some(first) = objects.first() {
        'cand: for name in props(first).keys() {
            let mut labels = Vec::new();
            for o in objects {
                let Some(v) = props(o).get(name).cloned() else {
                    continue 'cand;
                };
                let Some(val) = single_value(defs, &v, stack)? else {
                    continue 'cand;
                };
                labels.push(format!("{name}={val}"));
            }
            return Ok((name.clone(), labels));
        }
    }
    let external: Option<Vec<String>> = objects
        .iter()
        .map(|o| {
            let p = props(o);
            (p.len() == 1).then(|| p.keys().next().cloned()).flatten()
        })
        .collect();
    if let Some(labels) = external {
        return Ok(("<external>".into(), labels));
    }
    Ok((
        "<branch>".into(),
        (0..objects.len()).map(|i| format!("#{i}")).collect(),
    ))
}

/// Keys reached through several union branches (e.g. `delivery.kind` in both the
/// `header` and the `cookie` variant) collapse into one entry: enum values, types and
/// variant labels are unioned; a default survives only when all variants agree.
fn merge_duplicates(keys: Vec<ConfigKey>) -> Vec<ConfigKey> {
    let mut out: Vec<ConfigKey> = Vec::with_capacity(keys.len());
    let mut index = std::collections::HashMap::new();
    for k in keys {
        let Some(&i) = index.get(&k.path) else {
            index.insert(k.path.clone(), out.len());
            out.push(k);
            continue;
        };
        let m: &mut ConfigKey = &mut out[i];
        if m.kind != k.kind {
            m.kind = "union".into();
        }
        for t in k.types {
            if !m.types.contains(&t) {
                m.types.push(t);
            }
        }
        if let Some(vals) = k.enum_values {
            let mv = m.enum_values.get_or_insert_with(Vec::new);
            for v in vals {
                if !mv.iter().any(|x| x.value == v.value) {
                    mv.push(v);
                }
            }
        }
        for v in k.variant_of {
            if !m.variant_of.contains(&v) {
                m.variant_of.push(v);
            }
        }
        if m.default != k.default {
            m.default = None;
            m.default_source = None;
        }
        if m.raw_default != k.raw_default {
            m.raw_default = None;
        }
        if m.description.is_none() {
            m.description = k.description;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn find<'a>(keys: &'a [ConfigKey], p: &str) -> &'a ConfigKey {
        keys.iter()
            .find(|k| k.path == p)
            .unwrap_or_else(|| panic!("{p} missing"))
    }

    #[test]
    fn flatten_resolves_refs_unions_maps_and_defaults() {
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "approval_policy": {"allOf": [{"$ref": "#/definitions/AskForApproval"}], "description": "Field doc."},
                "history": {"allOf": [{"$ref": "#/definitions/History"}], "default": {"max_bytes": null, "persistence": "save-all"}},
                "servers": {"type": "object", "additionalProperties": {"$ref": "#/definitions/Server"}},
                "profiles": {"type": "object", "additionalProperties": {"$ref": "#/definitions/Server"}},
                "delivery": {"oneOf": [
                    {"type": "object", "properties": {"kind": {"type": "string", "enum": ["header"]}, "name": {"type": "string"}}},
                    {"type": "object", "properties": {"kind": {"type": "string", "enum": ["cookie"]}, "name": {"type": "string"}}}
                ]},
                "toggle": {"anyOf": [{"type": "boolean"}, {"$ref": "#/definitions/Toggle"}]}
            },
            "definitions": {
                "AskForApproval": {"description": "Type doc.", "oneOf": [
                    {"type": "string", "enum": ["never"], "description": "Never ask."},
                    {"type": "string", "enum": ["on-request"]},
                    {"type": "object", "properties": {"granular": {"type": "boolean"}}}
                ]},
                "History": {"type": "object", "properties": {
                    "max_bytes": {"type": ["integer", "null"], "minimum": 0, "format": "uint"},
                    "persistence": {"type": "string", "enum": ["save-all", "none"]}
                }},
                "Server": {"type": "object", "properties": {"command": {"type": "string", "description": "Cmd."}}},
                "Toggle": {"type": "object", "properties": {"enabled": {"type": "boolean"}}}
            }
        });
        let keys = flatten(&schema, &["profiles"]).unwrap();
        let ap = find(&keys, "approval_policy");
        assert_eq!(ap.kind, "enum");
        assert!(ap.enum_has_table_form);
        assert_eq!(ap.description.as_deref(), Some("Field doc."));
        assert_eq!(ap.type_description.as_deref(), Some("Type doc."));
        assert_eq!(ap.definition.as_deref(), Some("AskForApproval"));
        let vals = ap.enum_values.as_ref().unwrap();
        assert_eq!(vals[0].description.as_deref(), Some("Never ask."));
        assert_eq!(find(&keys, "approval_policy.granular").kind, "boolean");

        let h = find(&keys, "history");
        assert_eq!(h.default, Some(json!({"persistence": "save-all"})));
        assert_eq!(
            h.raw_default,
            Some(json!({"max_bytes": null, "persistence": "save-all"}))
        );
        let p = find(&keys, "history.persistence");
        assert_eq!(
            (p.default.clone(), p.default_source),
            (Some(json!("save-all")), Some("parent"))
        );
        assert_eq!(find(&keys, "history.max_bytes").default, None);

        assert_eq!(find(&keys, "servers").kind, "map");
        let cmd = find(&keys, "servers.<name>.command");
        assert!(cmd.under_map);
        assert_eq!(cmd.description.as_deref(), Some("Cmd."));
        assert!(keys.iter().all(|k| !k.path.starts_with("profiles.")));

        let d = find(&keys, "delivery");
        assert_eq!(d.discriminator.as_deref(), Some("kind"));
        let kind = find(&keys, "delivery.kind");
        let kinds: Vec<&str> = kind
            .enum_values
            .as_ref()
            .unwrap()
            .iter()
            .map(|v| v.value.as_str())
            .collect();
        assert_eq!(kinds, ["header", "cookie"]);
        assert_eq!(
            kind.variant_of,
            ["delivery:kind=header", "delivery:kind=cookie"]
        );
        assert_eq!(keys.iter().filter(|k| k.path == "delivery.name").count(), 1);

        assert_eq!(find(&keys, "toggle").kind, "bool_or_table");
        assert_eq!(find(&keys, "toggle.enabled").kind, "boolean");
    }

    #[test]
    fn unsupported_refs_fail() {
        let schema =
            json!({"definitions": {}, "properties": {"x": {"$ref": "http://elsewhere/X"}}});
        assert!(flatten(&schema, &[]).is_err());
        let schema =
            json!({"definitions": {}, "properties": {"x": {"$ref": "#/definitions/Missing"}}});
        assert!(flatten(&schema, &[]).is_err());
        assert!(flatten(&json!({"properties": {}}), &[]).is_err());
    }
}
