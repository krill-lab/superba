//! Codex settings a profile manages: dotted `config.toml` paths from the settings
//! catalog mapped to a value to write ([`SettingEntry::Set`]) or to an explicit
//! removal ([`SettingEntry::Unset`], "reset to Codex's default").
//!
//! Superba owns exactly the paths listed in a profile's [`CodexSettings`]; every
//! other key in `config.toml` stays the user's. A path that leaves the list is
//! restored to the value it had before Superba first took it over.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::codex::{PAT_RECOMMENDED_KEYS, TomlLiteral, config_keys};
use crate::error::{Error, Result};

/// A TOML value a setting can hold. Floats are always finite; TOML date-times are
/// not supported (no Codex setting uses them).
#[derive(Clone, Debug)]
pub enum SettingValue {
    /// `true` / `false`.
    Bool(bool),
    /// Integer.
    Integer(i64),
    /// Finite float.
    Float(f64),
    /// String (also enum values and paths).
    String(String),
    /// Array.
    Array(Vec<SettingValue>),
    /// Table (written as an inline table).
    Table(BTreeMap<String, SettingValue>),
}

impl PartialEq for SettingValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Integer(a), Self::Integer(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a.total_cmp(b).is_eq(),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Table(a), Self::Table(b)) => a == b,
            _ => false,
        }
    }
}

// Floats are finite (constructors and deserialization reject NaN) and compared
// with `total_cmp`, so equality is reflexive.
impl Eq for SettingValue {}

impl SettingValue {
    /// A float value; `None` for NaN or infinity.
    pub fn float(x: f64) -> Option<Self> {
        x.is_finite().then_some(Self::Float(x))
    }

    /// The bool, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// The number as `f64` (integers and floats).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(i) => Some(*i as f64),
            Self::Float(x) => Some(*x),
            _ => None,
        }
    }

    /// Short type name for messages (`boolean`, `integer`, `number`, `string`,
    /// `array`, `table`).
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "boolean",
            Self::Integer(_) => "integer",
            Self::Float(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Table(_) => "table",
        }
    }

    /// TOML value representation as written after `key = ` (`"live"`, `true`,
    /// `1.5`, `["a", "b"]`, `{ enabled = true }`).
    pub fn to_toml_repr(&self) -> String {
        match self {
            Self::Bool(b) => b.to_string(),
            Self::Integer(i) => i.to_string(),
            Self::Float(x) => {
                // Debug is the shortest round-trip form and uses an exponent for
                // very large or small values (`1e300`), both valid TOML floats.
                let mut s = format!("{x:?}");
                if !s.contains(['.', 'e', 'E']) {
                    s.push_str(".0");
                }
                s
            }
            Self::String(s) => toml_string(s),
            Self::Array(items) => {
                let inner: Vec<String> = items.iter().map(Self::to_toml_repr).collect();
                format!("[{}]", inner.join(", "))
            }
            Self::Table(map) if map.is_empty() => "{}".to_owned(),
            Self::Table(map) => {
                let inner: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{} = {}", toml_key(k), v.to_toml_repr()))
                    .collect();
                format!("{{ {} }}", inner.join(", "))
            }
        }
    }

    /// Convert a parsed TOML value; `None` for date-times and non-finite floats.
    pub fn from_toml(value: &toml::Value) -> Option<Self> {
        Some(match value {
            toml::Value::Boolean(b) => Self::Bool(*b),
            toml::Value::Integer(i) => Self::Integer(*i),
            toml::Value::Float(x) => Self::float(*x)?,
            toml::Value::String(s) => Self::String(s.clone()),
            toml::Value::Array(items) => {
                Self::Array(items.iter().map(Self::from_toml).collect::<Option<_>>()?)
            }
            toml::Value::Table(t) => Self::Table(
                t.iter()
                    .map(|(k, v)| Some((k.clone(), Self::from_toml(v)?)))
                    .collect::<Option<_>>()?,
            ),
            toml::Value::Datetime(_) => return None,
        })
    }

    /// Parse a TOML value representation (`"live"`, `true`, `[1, 2]`); `None` when
    /// it is not a single valid value.
    pub fn from_toml_repr(repr: &str) -> Option<Self> {
        let doc: toml::Table = toml::from_str(&format!("v = {repr}")).ok()?;
        doc.get("v").and_then(Self::from_toml)
    }
}

impl fmt::Display for SettingValue {
    /// The TOML representation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_toml_repr())
    }
}

impl From<bool> for SettingValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<i64> for SettingValue {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<&str> for SettingValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<String> for SettingValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<TomlLiteral> for SettingValue {
    fn from(lit: TomlLiteral) -> Self {
        match lit {
            TomlLiteral::Str(s) => Self::String(s.to_owned()),
            TomlLiteral::Bool(b) => Self::Bool(b),
        }
    }
}

impl Serialize for SettingValue {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Bool(b) => s.serialize_bool(*b),
            Self::Integer(i) => s.serialize_i64(*i),
            Self::Float(x) => s.serialize_f64(*x),
            Self::String(v) => s.serialize_str(v),
            Self::Array(items) => items.serialize(s),
            Self::Table(map) => map.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for SettingValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = SettingValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a boolean, number, string, array or table")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<SettingValue, E> {
        Ok(SettingValue::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<SettingValue, E> {
        Ok(SettingValue::Integer(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<SettingValue, E> {
        i64::try_from(v)
            .map(SettingValue::Integer)
            .map_err(|_| E::custom(format!("integer {v} is out of range")))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<SettingValue, E> {
        SettingValue::float(v).ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<SettingValue, E> {
        Ok(SettingValue::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<SettingValue, E> {
        Ok(SettingValue::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<SettingValue, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(SettingValue::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<SettingValue, A::Error> {
        let mut out = BTreeMap::new();
        while let Some((k, v)) = map.next_entry::<String, SettingValue>()? {
            out.insert(k, v);
        }
        Ok(SettingValue::Table(out))
    }
}

/// What a profile wants for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingEntry {
    /// Write this value.
    Set(SettingValue),
    /// Remove the key, so Codex uses its default (`null` in `profiles.json`).
    Unset,
}

impl SettingEntry {
    /// The value to write, `None` for [`SettingEntry::Unset`].
    pub fn value(&self) -> Option<&SettingValue> {
        match self {
            Self::Set(v) => Some(v),
            Self::Unset => None,
        }
    }

    /// TOML representation of the result (`None` = key absent).
    pub fn to_toml_repr(&self) -> Option<String> {
        self.value().map(SettingValue::to_toml_repr)
    }
}

impl Serialize for SettingEntry {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for SettingEntry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match Option::<SettingValue>::deserialize(d)? {
            Some(v) => Self::Set(v),
            None => Self::Unset,
        })
    }
}

/// The Codex settings of a profile (decision 3): config path -> entry. Serialized
/// as a JSON object (`{"web_search": "live", "approval_policy": null}`).
///
/// Paths are dotted bare keys (`features.fast_mode`, `tui.notifications`); map
/// entries use the concrete name (`mcp_servers.docs.enabled`). Paths that
/// [`reserved_owner`] names are managed elsewhere and refused by [`Self::set`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CodexSettings {
    entries: BTreeMap<String, SettingEntry>,
}

impl CodexSettings {
    /// No settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// True if no path is managed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of managed paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Entries in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SettingEntry)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Managed paths in order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// The entry for `path`.
    pub fn get(&self, path: &str) -> Option<&SettingEntry> {
        self.entries.get(path)
    }

    /// True if `path` is managed.
    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    /// Manage `path` with `entry`. Refuses malformed and reserved paths.
    pub fn insert(&mut self, path: &str, entry: SettingEntry) -> Result<()> {
        check_path(path)?;
        self.entries.insert(path.to_owned(), entry);
        Ok(())
    }

    /// Write `value` at `path`.
    pub fn set(&mut self, path: &str, value: impl Into<SettingValue>) -> Result<()> {
        self.insert(path, SettingEntry::Set(value.into()))
    }

    /// Remove `path` from `config.toml` (reset to Codex's default).
    pub fn unset(&mut self, path: &str) -> Result<()> {
        self.insert(path, SettingEntry::Unset)
    }

    /// Stop managing `path`; its value before Superba took it over comes back at
    /// the next switch. Returns the dropped entry.
    pub fn forget(&mut self, path: &str) -> Option<SettingEntry> {
        self.entries.remove(path)
    }

    /// Insert an entry without validation (migrations and tests that must model
    /// malformed input; planners report such paths as skipped).
    pub fn insert_unchecked(&mut self, path: impl Into<String>, entry: SettingEntry) {
        self.entries.insert(path.into(), entry);
    }

    /// Keys the Krill tutorial recommends ([`PAT_RECOMMENDED_KEYS`]), the preset of
    /// new Krill profiles. Users see and edit them like any other setting.
    pub fn krill_recommended() -> Self {
        let mut s = Self::new();
        for (path, lit) in PAT_RECOMMENDED_KEYS {
            s.insert_unchecked(*path, SettingEntry::Set((*lit).into()));
        }
        s
    }
}

impl FromIterator<(String, SettingEntry)> for CodexSettings {
    fn from_iter<I: IntoIterator<Item = (String, SettingEntry)>>(iter: I) -> Self {
        Self {
            entries: iter.into_iter().collect(),
        }
    }
}

/// Who manages a path that cannot be a Codex setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReservedOwner {
    /// Routing keys written from the profile's connection (site, line, auth).
    Connection,
    /// `model`, `model_reasoning_effort`, `model_context_window`,
    /// `model_auto_compact_token_limit`, `service_tier` and `model_catalog_json`:
    /// the profile's models ([`crate::ModelDefaults`], per-model overrides, speed
    /// tier), edited on the Models page.
    ModelDefaults,
    /// Keys that would break Superba's switching (provider tables, config
    /// profiles, the credentials store).
    Superba,
}

/// The owner of a reserved path, `None` for ordinary settings.
pub fn reserved_owner(path: &str) -> Option<ReservedOwner> {
    let first = path.split('.').next().unwrap_or(path);
    match first {
        config_keys::MODEL_PROVIDER
        | config_keys::OPENAI_BASE_URL
        | config_keys::CHATGPT_BASE_URL => Some(ReservedOwner::Connection),
        config_keys::MODEL
        | config_keys::MODEL_REASONING_EFFORT
        | config_keys::MODEL_CATALOG_JSON
        | config_keys::MODEL_CONTEXT_WINDOW
        | config_keys::MODEL_AUTO_COMPACT_TOKEN_LIMIT
        | config_keys::SERVICE_TIER => Some(ReservedOwner::ModelDefaults),
        config_keys::MODEL_PROVIDERS
        | config_keys::PROFILE
        | "profiles"
        | config_keys::CLI_AUTH_CREDENTIALS_STORE => Some(ReservedOwner::Superba),
        _ => None,
    }
}

/// True if `path` is a dotted path of bare TOML keys (`[A-Za-z0-9_-]+` segments),
/// the only form Superba edits.
pub fn is_valid_setting_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
}

fn check_path(path: &str) -> Result<()> {
    if !is_valid_setting_path(path) {
        return Err(Error::invalid(format!(
            "{path:?} is not a dotted path of bare TOML keys"
        )));
    }
    if let Some(owner) = reserved_owner(path) {
        let hint = match owner {
            ReservedOwner::ModelDefaults => {
                "it is part of the profile's models: spb models (default model, effort, \
                 context window, compaction), spb speed, or the Models page"
            }
            ReservedOwner::Connection => {
                "it follows the profile's site, line and authentication (spb switch, the \
                 Connection page)"
            }
            ReservedOwner::Superba => "Superba never writes it (it would break switching)",
        };
        return Err(Error::invalid(format!(
            "{path} is not a Codex setting Superba manages here; {hint}"
        )));
    }
    Ok(())
}

/// Encode a TOML basic string.
pub(crate) fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ch if ch <= '\u{1f}' || ch == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", u32::from(ch)));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        k.to_owned()
    } else {
        toml_string(k)
    }
}
