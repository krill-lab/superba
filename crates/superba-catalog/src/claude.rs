//! The Claude Code settings catalog (user bug 79): every key of Claude Code's
//! `settings.json` and every documented environment variable of its `env`
//! block, with the official English text, a Chinese name and explanation
//! written by people, the group it is listed under and how it is edited.
//!
//! Layers (repository `catalog/claude/`, refreshed by
//! `scripts/catalog/claude_catalog.py`):
//!
//! - `claude-code-settings.schema.json`: the official schema from
//!   schemastore.org (142 keys, 340 environment variables);
//! - `binary-keys.json`: keys only the installed Claude Code's own schema
//!   knows (`modelSettings`, `idleCompaction`, `modelPicker`, ...; its
//!   `@internal` keys left out);
//! - `zh.json`: Chinese names, explanations and groups.
//!
//! Keys found in a file that neither source knows are listed too
//! ([`ClaudeCatalog::file_entry`], group "Other", JSON editor).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Map, Value, json};

const SCHEMA: &str = include_str!("../../../catalog/claude/claude-code-settings.schema.json");
const BINARY: &str = include_str!("../../../catalog/claude/binary-keys.json");
const ZH: &str = include_str!("../../../catalog/claude/zh.json");

/// Where a catalog entry comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaudeSource {
    /// The official schema (schemastore.org).
    Schemastore,
    /// Only the installed Claude Code's own schema.
    Binary,
    /// Only the user's file (no source knows it).
    File,
}

/// The groups of the Claude Code Settings page, in rail order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClaudeGroup {
    /// Endpoint, keys, headers, timeouts, providers.
    Connection,
    /// Which models are used and listed.
    Models,
    /// Effort, thinking, capabilities of models.
    Capabilities,
    /// Output and context sizes, compaction, caching.
    OutputContext,
    /// Telemetry, updates, nonessential traffic.
    TrafficPrivacy,
    /// Everything else.
    Other,
}

impl ClaudeGroup {
    /// Every group in rail order.
    pub const ALL: [ClaudeGroup; 6] = [
        ClaudeGroup::Connection,
        ClaudeGroup::Models,
        ClaudeGroup::Capabilities,
        ClaudeGroup::OutputContext,
        ClaudeGroup::TrafficPrivacy,
        ClaudeGroup::Other,
    ];

    /// The id used in `zh.json`.
    pub fn id(self) -> &'static str {
        match self {
            ClaudeGroup::Connection => "connection",
            ClaudeGroup::Models => "models",
            ClaudeGroup::Capabilities => "capabilities",
            ClaudeGroup::OutputContext => "output_context",
            ClaudeGroup::TrafficPrivacy => "traffic_privacy",
            ClaudeGroup::Other => "other",
        }
    }

    fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|g| g.id() == s)
            .unwrap_or(ClaudeGroup::Other)
    }
}

/// How a value is edited.
#[derive(Clone, Debug, PartialEq)]
pub enum ClaudeKind {
    /// `true` / `false`.
    Bool,
    /// Free text.
    Text,
    /// A number.
    Number {
        /// Whole numbers only.
        integer: bool,
        /// Minimum.
        min: Option<f64>,
        /// Maximum.
        max: Option<f64>,
    },
    /// One of these strings; `open` = other text is accepted too.
    Enum {
        /// The values.
        values: Vec<String>,
        /// Free text is accepted as well.
        open: bool,
    },
    /// An environment variable that is on when `"1"` (unset = off).
    Flag,
    /// A list of strings.
    List,
    /// Anything else (objects): a structured editor or JSON.
    Json,
}

/// The editor a key gets on the Settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaudeEditor {
    /// A scalar or list editor (from [`ClaudeKind`]).
    Value,
    /// `env`: variables as key / value rows with catalog help.
    Env,
    /// `hooks`: event -> matcher -> commands.
    Hooks,
    /// A map of names to `true` / `false` (`enabledPlugins`).
    BoolMap,
    /// A map of model names to settings (`modelSettings`).
    ModelSettings,
    /// An object with known fields (`permissions`, `statusLine`,
    /// `sandbox`, `autoMode`, ...): one editor per field.
    Object,
    /// JSON text.
    Json,
}

/// One catalog entry.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeEntry {
    /// Dotted path: a top-level key or `env.<NAME>`.
    pub path: String,
    /// An environment variable of `env`.
    pub env: bool,
    /// Where it comes from.
    pub source: ClaudeSource,
    /// Rail group.
    pub group: ClaudeGroup,
    /// Value editor.
    pub kind: ClaudeKind,
    /// Page editor.
    pub editor: ClaudeEditor,
    /// Official English text.
    pub en: Option<String>,
    /// Chinese name.
    pub name_zh: Option<String>,
    /// Chinese explanation.
    pub zh: Option<String>,
    /// Upstream marks it deprecated.
    pub deprecated: bool,
    /// Default value, when documented.
    pub default: Option<Value>,
    /// Examples from the schema.
    pub examples: Vec<Value>,
    /// The schema of the value, `$ref`s resolved (objects and arrays).
    pub schema: Option<Value>,
}

impl ClaudeEntry {
    /// The key or variable name (the last segment of the path).
    pub fn name(&self) -> &str {
        self.path.strip_prefix("env.").unwrap_or(&self.path)
    }

    /// Text matched by search.
    pub fn haystack(&self) -> impl Iterator<Item = &str> {
        [
            Some(self.path.as_str()),
            self.en.as_deref(),
            self.name_zh.as_deref(),
            self.zh.as_deref(),
        ]
        .into_iter()
        .flatten()
    }
}

/// The catalog.
#[derive(Debug)]
pub struct ClaudeCatalog {
    entries: Vec<ClaudeEntry>,
    index: BTreeMap<String, usize>,
    /// The Claude Code version `binary-keys.json` was taken from.
    pub binary_version: Option<String>,
    defs: Map<String, Value>,
}

#[derive(Deserialize)]
struct ZhFile {
    keys: BTreeMap<String, ZhEntry>,
    env: BTreeMap<String, ZhEntry>,
}

#[derive(Deserialize)]
struct ZhEntry {
    name_zh: String,
    zh: String,
    group: String,
}

#[derive(Deserialize)]
struct BinaryFile {
    #[serde(default)]
    claude_code_version: Option<String>,
    keys: Vec<BinaryKey>,
}

#[derive(Deserialize)]
struct BinaryKey {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    description: String,
    #[serde(default, rename = "enum")]
    values: Vec<String>,
}

/// Curated keys of the "Common" rail item, in order.
pub const COMMON: &[&str] = &[
    "model",
    "env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "env.API_TIMEOUT_MS",
    "env.CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS",
    "env.CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "env.MAX_THINKING_TOKENS",
    "env.CLAUDE_CODE_EFFORT_LEVEL",
    "effortLevel",
    "alwaysThinkingEnabled",
    "permissions",
    "hooks",
    "statusLine",
    "enabledPlugins",
    "theme",
    "language",
    "cleanupPeriodDays",
    "includeCoAuthoredBy",
    "autoCompactEnabled",
    "env.DISABLE_TELEMETRY",
    "env.DISABLE_AUTOUPDATER",
];

/// Hand-written schemas of structured keys the official schema does not
/// describe (shapes from the installed Claude Code; descriptions are our own).
fn binary_schema(key: &str) -> Option<Value> {
    let effort = json!(["low", "medium", "high", "xhigh"]);
    match key {
        "modelSettings" => Some(json!({
            "type": "object",
            "additionalProperties": {
                "type": "object",
                "properties": {
                    "effortLevel": {"type": "string", "enum": effort,
                        "description": "Effort level saved for this model."},
                    "maxEffortLevel": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"],
                        "description": "Highest effort level offered for this model."},
                    "autoCompactWindow": {"description": "Context size, in tokens, at which this model's conversation is compacted, or \"auto\"."}
                }
            }
        })),
        "modelPicker" => Some(json!({
            "type": "object",
            "properties": {
                "options": {"type": "array", "description": "Models listed in /model, in this order.",
                    "items": {"type": "object", "properties": {
                        "model": {"type": "string"}, "label": {"type": "string"},
                        "description": {"type": "string"}, "behavesAs": {"type": "string"}}}},
                "replaceBuiltInOptions": {"type": "boolean",
                    "description": "Hide the built-in models and list only these."}
            }
        })),
        "spellcheck" => Some(json!({
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean", "description": "Spell-check the prompt as you type."},
                "checker": {"type": "string"}
            }
        })),
        "remote" => Some(json!({
            "type": "object",
            "properties": {"defaultEnvironmentId": {"type": "string",
                "description": "Environment used by default for cloud sessions."}}
        })),
        _ => None,
    }
}

/// Resolve `#/$defs/...` references (one level of nesting at a time).
fn resolve(v: &Value, defs: &Map<String, Value>, depth: usize) -> Value {
    if depth > 8 {
        return v.clone();
    }
    match v {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(Value::as_str)
                && let Some(name) = r.strip_prefix("#/$defs/")
                && let Some(d) = defs.get(name)
            {
                let mut merged = resolve(d, defs, depth + 1);
                if let Value::Object(m) = &mut merged {
                    for (k, x) in o {
                        if k != "$ref" {
                            m.insert(k.clone(), resolve(x, defs, depth + 1));
                        }
                    }
                }
                return merged;
            }
            Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), resolve(x, defs, depth + 1)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| resolve(x, defs, depth + 1)).collect()),
        other => other.clone(),
    }
}

/// The value kind of a JSON schema.
pub fn kind_of_schema(s: &Value) -> ClaudeKind {
    let strings = |v: &Value| -> Option<Vec<String>> {
        v.as_array()?
            .iter()
            .map(|x| x.as_str().map(str::to_owned))
            .collect()
    };
    if let Some(values) = s.get("enum").and_then(strings) {
        return ClaudeKind::Enum {
            values,
            open: false,
        };
    }
    if let Some(alts) = s
        .get("anyOf")
        .or_else(|| s.get("oneOf"))
        .and_then(Value::as_array)
    {
        // A string enum with a free-text alternative (`theme`).
        let mut values = Vec::new();
        let mut text = false;
        let mut other = false;
        for a in alts {
            match (
                a.get("type").and_then(Value::as_str),
                a.get("enum").and_then(strings),
            ) {
                (Some("string"), Some(v)) => values.extend(v),
                (Some("string"), None) => text = true,
                _ => other = true,
            }
        }
        if !other && !values.is_empty() {
            return ClaudeKind::Enum { values, open: text };
        }
        if !other && text {
            return ClaudeKind::Text;
        }
        return ClaudeKind::Json;
    }
    let ty = match s.get("type") {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or(""),
        _ => "",
    };
    let num = |k: &str| s.get(k).and_then(Value::as_f64);
    match ty {
        "boolean" => ClaudeKind::Bool,
        "string" => ClaudeKind::Text,
        "integer" | "number" => ClaudeKind::Number {
            integer: ty == "integer",
            min: num("minimum").or(num("exclusiveMinimum")),
            max: num("maximum"),
        },
        "array"
            if s.get("items").is_none_or(|i| {
                matches!(i.get("type").and_then(Value::as_str), Some("string") | None)
                    && i.get("properties").is_none()
            }) =>
        {
            ClaudeKind::List
        }
        _ => ClaudeKind::Json,
    }
}

fn editor_for(key: &str, kind: &ClaudeKind, schema: Option<&Value>) -> ClaudeEditor {
    match key {
        "env" => return ClaudeEditor::Env,
        "hooks" => return ClaudeEditor::Hooks,
        "enabledPlugins" => return ClaudeEditor::BoolMap,
        "modelSettings" => return ClaudeEditor::ModelSettings,
        _ => {}
    }
    if *kind != ClaudeKind::Json {
        return ClaudeEditor::Value;
    }
    let has_fields = schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty());
    if has_fields {
        ClaudeEditor::Object
    } else {
        ClaudeEditor::Json
    }
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn deprecated(v: &Value) -> bool {
    v.get("deprecated").and_then(Value::as_bool) == Some(true)
        || text(v, "description").is_some_and(|d| {
            d.starts_with("Deprecated") || d.starts_with("[DEPRECATED]") || d.contains("DEPRECATED")
        })
}

impl ClaudeCatalog {
    /// The catalog compiled into Superba.
    pub fn builtin() -> &'static ClaudeCatalog {
        static CATALOG: OnceLock<ClaudeCatalog> = OnceLock::new();
        CATALOG.get_or_init(|| {
            Self::parse(SCHEMA, BINARY, ZH).expect("the built-in Claude Code catalog parses")
        })
    }

    /// Build from the three files (see the module docs).
    pub fn parse(schema: &str, binary: &str, zh: &str) -> Result<Self, String> {
        let schema: Value = serde_json::from_str(schema).map_err(|e| format!("schema: {e}"))?;
        let binary: BinaryFile =
            serde_json::from_str(binary).map_err(|e| format!("binary keys: {e}"))?;
        let zh: ZhFile = serde_json::from_str(zh).map_err(|e| format!("zh.json: {e}"))?;
        let defs = schema
            .get("$defs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let props = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or("schema has no properties")?;
        let mut entries = Vec::new();
        let zh_of = |m: &BTreeMap<String, ZhEntry>, k: &str| {
            m.get(k)
                .map(|z| {
                    (
                        Some(z.name_zh.clone()),
                        Some(z.zh.clone()),
                        ClaudeGroup::parse(&z.group),
                    )
                })
                .unwrap_or((None, None, ClaudeGroup::Other))
        };
        for (key, raw) in props {
            let v = resolve(raw, &defs, 0);
            let kind = kind_of_schema(&v);
            let (name_zh, zh_text, group) = zh_of(&zh.keys, key);
            let structured = matches!(
                v.get("type").and_then(Value::as_str),
                Some("object" | "array")
            ) || v.get("anyOf").is_some();
            entries.push(ClaudeEntry {
                path: key.clone(),
                env: false,
                source: ClaudeSource::Schemastore,
                group,
                editor: editor_for(key, &kind, Some(&v)),
                kind,
                en: text(&v, "description"),
                name_zh,
                zh: zh_text,
                deprecated: deprecated(&v),
                default: v.get("default").cloned(),
                examples: v
                    .get("examples")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                schema: structured.then(|| v.clone()),
            });
        }
        for b in &binary.keys {
            if props.contains_key(&b.key) {
                continue;
            }
            let schema = binary_schema(&b.key);
            let kind = match b.kind.as_str() {
                "boolean" => ClaudeKind::Bool,
                "string" => ClaudeKind::Text,
                "number" => ClaudeKind::Number {
                    integer: true,
                    min: None,
                    max: None,
                },
                "enum" => ClaudeKind::Enum {
                    values: b.values.clone(),
                    open: false,
                },
                "array" => ClaudeKind::List,
                _ => ClaudeKind::Json,
            };
            let (name_zh, zh_text, group) = zh_of(&zh.keys, &b.key);
            entries.push(ClaudeEntry {
                path: b.key.clone(),
                env: false,
                source: ClaudeSource::Binary,
                group,
                editor: editor_for(&b.key, &kind, schema.as_ref()),
                kind,
                deprecated: b.description.starts_with("Deprecated"),
                en: (!b.description.is_empty()).then(|| b.description.clone()),
                name_zh,
                zh: zh_text,
                default: None,
                examples: Vec::new(),
                schema,
            });
        }
        let env = props
            .get("env")
            .and_then(|e| e.get("properties"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (name, raw) in &env {
            let v = resolve(raw, &defs, 0);
            let kind = match v.get("enum").and_then(Value::as_array) {
                Some(values)
                    if values.iter().all(|x| matches!(x.as_str(), Some("0" | "1")))
                        && values.iter().any(|x| x.as_str() == Some("1")) =>
                {
                    ClaudeKind::Flag
                }
                _ => match kind_of_schema(&v) {
                    ClaudeKind::Enum { values, open } => ClaudeKind::Enum { values, open },
                    _ => ClaudeKind::Text,
                },
            };
            let (name_zh, zh_text, group) = zh_of(&zh.env, name);
            entries.push(ClaudeEntry {
                path: format!("env.{name}"),
                env: true,
                source: ClaudeSource::Schemastore,
                group,
                editor: ClaudeEditor::Value,
                kind,
                en: text(&v, "description"),
                name_zh,
                zh: zh_text,
                deprecated: deprecated(&v),
                default: v.get("default").cloned(),
                examples: v
                    .get("examples")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                schema: None,
            });
        }
        let index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        Ok(Self {
            entries,
            index,
            binary_version: binary.claude_code_version,
            defs,
        })
    }

    /// Every entry: top-level keys (official, then binary-only), then the
    /// environment variables.
    pub fn entries(&self) -> &[ClaudeEntry] {
        &self.entries
    }

    /// The entry of `path`.
    pub fn entry(&self, path: &str) -> Option<&ClaudeEntry> {
        self.index.get(path).map(|i| &self.entries[*i])
    }

    /// An entry for a key of the file that no source knows: group Other,
    /// edited as JSON (a variable of `env` as text).
    pub fn file_entry(path: &str) -> ClaudeEntry {
        let env = path.starts_with("env.");
        ClaudeEntry {
            path: path.to_owned(),
            env,
            source: ClaudeSource::File,
            group: ClaudeGroup::Other,
            kind: if env {
                ClaudeKind::Text
            } else {
                ClaudeKind::Json
            },
            editor: if env {
                ClaudeEditor::Value
            } else {
                ClaudeEditor::Json
            },
            en: None,
            name_zh: None,
            zh: None,
            deprecated: false,
            default: None,
            examples: Vec::new(),
            schema: None,
        }
    }

    /// The events `hooks` takes, in schema order.
    pub fn hook_events(&self) -> Vec<String> {
        self.entry("hooks")
            .and_then(|e| e.schema.as_ref())
            .and_then(|s| s.get("properties"))
            .and_then(Value::as_object)
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The description of a hook event.
    pub fn hook_event_text(&self, event: &str) -> Option<String> {
        self.entry("hooks")
            .and_then(|e| e.schema.as_ref())
            .and_then(|s| s.get("properties"))
            .and_then(|p| p.get(event))
            .and_then(|e| text(e, "description"))
    }

    /// `$defs` of the official schema (for structured editors).
    pub fn defs(&self) -> &Map<String, Value> {
        &self.defs
    }
}
