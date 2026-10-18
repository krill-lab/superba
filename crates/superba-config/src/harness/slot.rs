//! Addresses of the entries Superba owns in a harness file ([`SlotPath`]) and
//! their meaning on a parsed document ([`get`], [`set`]): the reference every
//! format editor's text edit is checked against.
//!
//! A path is a list of steps from the document root:
//!
//! - [`Seg::Key`]: an object member (`providers`, `superba`);
//! - [`Seg::Find`]: the element of an array that is an object whose fields
//!   have these string values (`[id=llm-pi-ai]`,
//!   `[providerId=superba,modelId=gpt-5.5]`); a new one is added last;
//! - [`Seg::Item`] / [`Seg::FirstItem`]: the array element equal to a string
//!   (`[="superba"]`), added last / first.
//!
//! Missing containers on the way are created (an object for a following
//! key, an array for a following find or item, an object with the find's
//! fields for a find); [`missing_containers`] names them so a restore can
//! take away what Superba created once it is empty again ([`is_skeleton`]).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One step of a [`SlotPath`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seg {
    /// An object member.
    Key(String),
    /// The array element that is an object with these string fields.
    Find(Vec<(String, String)>),
    /// The array element equal to this string, added last.
    Item(String),
    /// The array element equal to this string, added first.
    FirstItem(String),
}

/// Where an owned entry is (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SlotPath(pub Vec<Seg>);

fn quote_key(k: &str) -> String {
    let plain = !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '$'));
    if plain {
        k.to_owned()
    } else {
        serde_json::to_string(k).unwrap_or_else(|_| k.to_owned())
    }
}

impl SlotPath {
    /// The path of one top-level key.
    pub fn key(k: impl Into<String>) -> Self {
        SlotPath(vec![Seg::Key(k.into())])
    }

    /// A path from keys.
    pub fn keys(keys: &[&str]) -> Self {
        SlotPath(keys.iter().map(|k| Seg::Key((*k).to_owned())).collect())
    }

    /// This path, then a key.
    pub fn then_key(mut self, k: impl Into<String>) -> Self {
        self.0.push(Seg::Key(k.into()));
        self
    }

    /// This path, then a find.
    pub fn then_find(mut self, fields: &[(&str, &str)]) -> Self {
        self.0.push(Seg::Find(
            fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ));
        self
    }

    /// This path, then an item added last.
    pub fn then_item(mut self, v: impl Into<String>) -> Self {
        self.0.push(Seg::Item(v.into()));
        self
    }

    /// This path, then an item added first.
    pub fn then_first_item(mut self, v: impl Into<String>) -> Self {
        self.0.push(Seg::FirstItem(v.into()));
        self
    }

    /// A find at the root (an entry of a top-level array).
    pub fn find(fields: &[(&str, &str)]) -> Self {
        SlotPath(Vec::new()).then_find(fields)
    }

    /// The last step.
    pub fn last(&self) -> Option<&Seg> {
        self.0.last()
    }

    /// Every proper prefix, shortest first (not the empty path).
    pub fn prefixes(&self) -> Vec<SlotPath> {
        (1..self.0.len())
            .map(|n| SlotPath(self.0[..n].to_vec()))
            .collect()
    }

    /// One line for people and the state's keys:
    /// `[id=llm-pi-ai].config.providers.superba`, `models[id=gpt-5.5]`,
    /// `config.providerOrder[^="superba"]`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        for seg in &self.0 {
            match seg {
                Seg::Key(k) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(&quote_key(k));
                }
                Seg::Find(fields) => {
                    out.push('[');
                    let parts: Vec<String> = fields
                        .iter()
                        .map(|(k, v)| format!("{}={}", quote_key(k), quote_key(v)))
                        .collect();
                    out.push_str(&parts.join(","));
                    out.push(']');
                }
                Seg::Item(v) => {
                    out.push_str("[=");
                    out.push_str(&serde_json::to_string(v).unwrap_or_default());
                    out.push(']');
                }
                Seg::FirstItem(v) => {
                    out.push_str("[^=");
                    out.push_str(&serde_json::to_string(v).unwrap_or_default());
                    out.push(']');
                }
            }
        }
        out
    }
}

impl std::fmt::Display for SlotPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

/// `value` matches a find's fields.
pub fn matches_find(value: &Value, fields: &[(String, String)]) -> bool {
    value.as_object().is_some_and(|o| {
        fields
            .iter()
            .all(|(k, v)| o.get(k).and_then(Value::as_str) == Some(v.as_str()))
    })
}

fn step<'a>(node: &'a Value, seg: &Seg) -> Option<&'a Value> {
    match seg {
        Seg::Key(k) => node.as_object()?.get(k),
        Seg::Find(fields) => node.as_array()?.iter().find(|e| matches_find(e, fields)),
        Seg::Item(s) | Seg::FirstItem(s) => node
            .as_array()?
            .iter()
            .find(|e| e.as_str() == Some(s.as_str())),
    }
}

/// The value at `path` (`None` = absent).
pub fn get<'a>(root: &'a Value, path: &SlotPath) -> Option<&'a Value> {
    let mut node = root;
    for seg in &path.0 {
        node = step(node, seg)?;
    }
    Some(node)
}

/// Refuse documents where a find or item matches several elements: Superba
/// would not know which one is its own.
pub fn check_unique(root: &Value, path: &SlotPath) -> Result<(), String> {
    let mut node = root;
    for seg in &path.0 {
        let count = match (seg, node) {
            (Seg::Find(f), Value::Array(a)) => a.iter().filter(|e| matches_find(e, f)).count(),
            (Seg::Item(s) | Seg::FirstItem(s), Value::Array(a)) => {
                a.iter().filter(|e| e.as_str() == Some(s.as_str())).count()
            }
            _ => 1,
        };
        if count > 1 {
            return Err(format!(
                "{} matches {count} entries; Superba cannot tell which one is its own (remove the duplicates first)",
                path.display()
            ));
        }
        match step(node, seg) {
            Some(n) => node = n,
            None => return Ok(()),
        }
    }
    Ok(())
}

/// The empty container the step after `seg` needs.
fn container_for(next: &Seg) -> Value {
    match next {
        Seg::Key(_) => Value::Object(Map::new()),
        Seg::Find(_) | Seg::Item(_) | Seg::FirstItem(_) => Value::Array(Vec::new()),
    }
}

/// The new value of a missing step (a find gets its fields first).
fn fresh(seg: &Seg, inner: Value) -> Result<Value, String> {
    match seg {
        Seg::Find(fields) => {
            let mut o = Map::new();
            for (k, v) in fields {
                o.insert(k.clone(), Value::String(v.clone()));
            }
            match inner {
                Value::Object(m) => {
                    for (k, v) in m {
                        if let Some(Value::String(s)) = o.get(&k)
                            && v.as_str() != Some(s.as_str())
                        {
                            return Err(format!("the value of [{k}] does not match its address"));
                        }
                        o.insert(k, v);
                    }
                }
                Value::Null => {}
                _ => return Err("an entry found by its fields must be an object".into()),
            }
            Ok(Value::Object(o))
        }
        _ => Ok(inner),
    }
}

/// The value the remaining steps `segs` build around `value` when nothing of
/// them exists yet (an entry of the container before `segs[0]`).
pub fn build(segs: &[Seg], value: Value) -> Result<Value, String> {
    let Some((first, rest)) = segs.split_first() else {
        return Ok(value);
    };
    if rest.is_empty() {
        return match first {
            Seg::Item(s) | Seg::FirstItem(s) => {
                if value.as_str() != Some(s.as_str()) {
                    return Err(format!("the item [{s}] must hold its own text"));
                }
                Ok(value)
            }
            _ => fresh(first, value),
        };
    }
    let inner = build(rest, value)?;
    let mut container = container_for(&rest[0]);
    match (&mut container, &rest[0]) {
        (Value::Object(o), Seg::Key(k)) => {
            o.insert(k.clone(), inner);
        }
        (Value::Array(a), _) => a.push(inner),
        _ => unreachable!("container_for matches the step"),
    }
    fresh(first, container)
}

/// Put `value` at `path` in `root` (`None` = remove it), creating missing
/// containers. An empty array at the root becomes an object when the path
/// starts with a key (WorkBuddy writes `[]` into a fresh `models.json`).
pub fn set(root: &mut Value, path: &SlotPath, value: Option<Value>) -> Result<(), String> {
    let segs = &path.0;
    if segs.is_empty() {
        return Err("empty path".into());
    }
    let empty_array = root.as_array().is_some_and(Vec::is_empty);
    match (root.is_null(), empty_array, &segs[0], value.is_some()) {
        (true, _, _, false) => return Ok(()),
        (true, _, Seg::Key(_), true) => *root = Value::Object(Map::new()),
        (true, _, _, true) => *root = Value::Array(Vec::new()),
        (_, true, Seg::Key(_), true) => *root = Value::Object(Map::new()),
        (_, true, Seg::Key(_), false) => return Ok(()),
        _ => {}
    }
    set_in(root, segs, value)
}

fn set_in(node: &mut Value, segs: &[Seg], value: Option<Value>) -> Result<(), String> {
    let (seg, rest) = segs.split_first().expect("non-empty");
    let kind = |n: &Value| match n {
        Value::Object(_) => "an object",
        Value::Array(_) => "an array",
        Value::Null => "null",
        _ => "a scalar",
    };
    match seg {
        Seg::Key(k) => {
            let Value::Object(obj) = node else {
                return Err(format!("expected an object at {k}, found {}", kind(node)));
            };
            if rest.is_empty() {
                match value {
                    Some(v) => {
                        obj.insert(k.clone(), v);
                    }
                    None => {
                        obj.shift_remove(k);
                    }
                }
                return Ok(());
            }
            match obj.get_mut(k) {
                Some(child) if child.is_null() && value.is_some() => {
                    *child = build_container(rest, value)?;
                    Ok(())
                }
                Some(child) => set_in(child, rest, value),
                None => match value {
                    None => Ok(()),
                    Some(v) => {
                        let built = build(rest, v)?;
                        let mut c = container_for(&rest[0]);
                        put_built(&mut c, &rest[0], built);
                        obj.insert(k.clone(), c);
                        Ok(())
                    }
                },
            }
        }
        Seg::Find(fields) => {
            let Value::Array(arr) = node else {
                return Err(format!("expected an array, found {}", kind(node)));
            };
            let at = arr.iter().position(|e| matches_find(e, fields));
            if rest.is_empty() {
                match (at, value) {
                    (Some(i), Some(v)) => arr[i] = fresh(seg, v)?,
                    (None, Some(v)) => arr.push(fresh(seg, v)?),
                    (Some(i), None) => {
                        arr.remove(i);
                    }
                    (None, None) => {}
                }
                return Ok(());
            }
            match at {
                Some(i) => set_in(&mut arr[i], rest, value),
                None => match value {
                    None => Ok(()),
                    Some(v) => {
                        let built = build(segs, v)?;
                        arr.push(built);
                        Ok(())
                    }
                },
            }
        }
        Seg::Item(s) | Seg::FirstItem(s) => {
            if !rest.is_empty() {
                return Err("an item cannot hold anything".into());
            }
            let Value::Array(arr) = node else {
                return Err(format!("expected an array, found {}", kind(node)));
            };
            match value {
                Some(v) => {
                    if v.as_str() != Some(s.as_str()) {
                        return Err(format!("the item [{s}] must hold its own text"));
                    }
                    if !arr.iter().any(|e| e.as_str() == Some(s.as_str())) {
                        if matches!(seg, Seg::FirstItem(_)) {
                            arr.insert(0, v);
                        } else {
                            arr.push(v);
                        }
                    }
                }
                None => arr.retain(|e| e.as_str() != Some(s.as_str())),
            }
            Ok(())
        }
    }
}

/// The container for `rest` holding the built remainder (a `null` member
/// becomes this container).
fn build_container(rest: &[Seg], value: Option<Value>) -> Result<Value, String> {
    let v = value.expect("checked by the caller");
    let built = build(rest, v)?;
    let mut c = container_for(&rest[0]);
    put_built(&mut c, &rest[0], built);
    Ok(c)
}

fn put_built(container: &mut Value, first: &Seg, built: Value) {
    match (container, first) {
        (Value::Object(o), Seg::Key(k)) => {
            o.insert(k.clone(), built);
        }
        (Value::Array(a), _) => a.push(built),
        _ => {}
    }
}

/// The containers on the way to `path` that do not exist in `root` (they are
/// created by setting it), shortest first.
pub fn missing_containers(root: &Value, path: &SlotPath) -> Vec<SlotPath> {
    path.prefixes()
        .into_iter()
        .filter(|p| get(root, p).is_none_or(Value::is_null))
        .collect()
}

/// A container Superba created holds nothing of anyone's any more: an empty
/// object or array, or an entry found by its fields with nothing but them.
pub fn is_skeleton(value: &Value, last: Option<&Seg>) -> bool {
    match value {
        Value::Object(o) if o.is_empty() => true,
        Value::Array(a) if a.is_empty() => true,
        Value::Object(o) => match last {
            Some(Seg::Find(fields)) => o.keys().all(|k| fields.iter().any(|(f, _)| f == k)),
            _ => false,
        },
        _ => false,
    }
}

/// A document that holds nothing: absent, `null`, `{}` or `[]`.
pub fn is_empty_doc(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Member names whose string values are keys (masked for display).
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "apikey" | "api_key" | "authtoken" | "auth_token" | "token" | "secret" | "password"
    ) || n.ends_with("_api_key")
}

/// `value` with every secret member masked (`••••abcd`).
pub fn masked(value: &Value) -> (Value, bool) {
    match value {
        Value::Object(o) => {
            let mut any = false;
            let mut out = Map::new();
            for (k, v) in o {
                if is_secret_name(k)
                    && let Value::String(s) = v
                {
                    any = true;
                    out.insert(
                        k.clone(),
                        Value::String(superba_core::Secret::new(s.clone()).masked()),
                    );
                } else {
                    let (m, a) = masked(v);
                    any |= a;
                    out.insert(k.clone(), m);
                }
            }
            (Value::Object(out), any)
        }
        Value::Array(a) => {
            let mut any = false;
            let out = a
                .iter()
                .map(|v| {
                    let (m, x) = masked(v);
                    any |= x;
                    m
                })
                .collect();
            (Value::Array(out), any)
        }
        other => (other.clone(), false),
    }
}

/// A value on one line for people: strings without quotes, objects as
/// compact JSON, secrets masked. `secret_slot` masks a plain string value.
pub fn display_value(value: &Value, secret_slot: bool) -> (String, bool) {
    match value {
        Value::String(s) if secret_slot => (superba_core::Secret::new(s.clone()).masked(), true),
        Value::String(s) => (s.clone(), false),
        other => {
            let (m, any) = masked(other);
            (serde_json::to_string(&m).unwrap_or_default(), any)
        }
    }
}
