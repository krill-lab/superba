//! Codex `config.toml` snippets and `auth.json` contents as the other tools keep
//! them for a provider: which base URL, model, wire API and key they configure.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::foreign::ForeignKey;

/// One `[model_providers.<id>]` table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderTable {
    /// Its id.
    pub id: String,
    /// `name`.
    pub name: Option<String>,
    /// `base_url`.
    pub base_url: Option<String>,
    /// `wire_api`.
    pub wire_api: Option<String>,
    /// `api_format` (AI Toolbox's gateway alias of `wire_api`).
    pub api_format: Option<String>,
    /// `env_key`.
    pub env_key: Option<String>,
    /// `experimental_bearer_token` (a key in the TOML).
    pub bearer_token: Option<String>,
}

/// What a Codex `config.toml` snippet configures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexToml {
    /// `model_provider` (absent = Codex's default, `openai`).
    pub provider_id: Option<String>,
    /// The table `model_provider` selects.
    pub active: Option<ProviderTable>,
    /// The first `[model_providers.*]` table that has a `base_url`.
    pub first_with_base_url: Option<ProviderTable>,
    /// Top-level `base_url` (not a Codex key; some tools write it).
    pub root_base_url: Option<String>,
    /// Top-level `openai_base_url` (Codex's own key for provider `openai`).
    pub openai_base_url: Option<String>,
    /// Top-level `wire_api` / `api_format`.
    pub root_wire_api: Option<String>,
    /// Top-level `experimental_bearer_token`.
    pub root_bearer_token: Option<String>,
    /// `model`.
    pub model: Option<String>,
    /// `[chat].model` (AI Toolbox's legacy place for the model).
    pub chat_model: Option<String>,
    /// `model_reasoning_effort`.
    pub reasoning_effort: Option<String>,
    /// Top-level keys Superba does not import.
    pub ignored: BTreeSet<String>,
}

/// Top-level keys that describe the connection (imported or implied).
const ROUTING_KEYS: &[&str] = &[
    "model_provider",
    "model_providers",
    "model",
    "model_reasoning_effort",
    "openai_base_url",
    "chatgpt_base_url",
    "base_url",
    "wire_api",
    "api_format",
    "experimental_bearer_token",
    "chat",
];

/// Model provider ids Codex reserves for its built-in providers (their tables
/// cannot carry a key, so cc-switch does not read one from them).
pub const RESERVED_PROVIDER_IDS: &[&str] = &[
    "amazon-bedrock",
    "amazon-bedrock-runtime",
    "openai",
    "ollama",
    "lmstudio",
];

fn string(v: Option<&toml::Value>) -> Option<String> {
    v.and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Parse TOML text into a table. `Err` carries the parser message (positions,
/// not values: no secrets).
pub fn parse_table(text: &str) -> Result<toml::Table, String> {
    toml::from_str(text).map_err(|e| e.message().to_owned())
}

/// Lay `over` on top of `base`: tables merge key by key, anything else in
/// `over` replaces what `base` has.
pub fn merge(base: &mut toml::Table, over: &toml::Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

fn provider_table(id: &str, t: &toml::Table) -> ProviderTable {
    ProviderTable {
        id: id.to_owned(),
        name: string(t.get("name")),
        base_url: string(t.get("base_url")),
        wire_api: string(t.get("wire_api")),
        api_format: string(t.get("api_format")),
        env_key: string(t.get("env_key")),
        bearer_token: string(t.get("experimental_bearer_token")),
    }
}

/// Read a parsed snippet.
pub fn read_toml(table: &toml::Table) -> CodexToml {
    let mut out = CodexToml {
        provider_id: string(table.get("model_provider")),
        root_base_url: string(table.get("base_url")),
        openai_base_url: string(table.get("openai_base_url")),
        root_wire_api: string(table.get("wire_api")).or_else(|| string(table.get("api_format"))),
        root_bearer_token: string(table.get("experimental_bearer_token")),
        model: string(table.get("model")),
        chat_model: table
            .get("chat")
            .and_then(toml::Value::as_table)
            .and_then(|t| string(t.get("model"))),
        reasoning_effort: string(table.get("model_reasoning_effort")),
        ..CodexToml::default()
    };
    for key in table.keys() {
        if !ROUTING_KEYS.contains(&key.as_str()) {
            out.ignored.insert(key.clone());
        }
    }
    let providers = table.get("model_providers").and_then(toml::Value::as_table);
    let id = out.provider_id.clone().unwrap_or_else(|| "openai".into());
    out.active = providers
        .and_then(|p| p.get(&id))
        .and_then(toml::Value::as_table)
        .map(|t| provider_table(&id, t));
    out.first_with_base_url = providers.and_then(|p| {
        p.iter().find_map(|(k, v)| {
            v.as_table()
                .map(|t| provider_table(k, t))
                .filter(|t| t.base_url.is_some())
        })
    });
    out
}

/// Parse a snippet.
pub fn parse_toml(text: &str) -> Result<CodexToml, String> {
    parse_table(text).map(|t| read_toml(&t))
}

impl CodexToml {
    /// The active table's id, when it is not one of Codex's built-in ids.
    pub fn custom_active(&self) -> Option<&ProviderTable> {
        self.active
            .as_ref()
            .filter(|t| !RESERVED_PROVIDER_IDS.contains(&t.id.as_str()))
    }

    /// The wire API of the active table, else the top level.
    pub fn wire_api(&self) -> Option<&str> {
        self.active
            .as_ref()
            .and_then(|t| t.wire_api.as_deref().or(t.api_format.as_deref()))
            .or(self.root_wire_api.as_deref())
    }

    /// The `env_key` of the active table.
    pub fn env_key(&self) -> Option<&str> {
        self.active.as_ref().and_then(|t| t.env_key.as_deref())
    }

    /// The key written into the TOML: the active (non built-in) table's
    /// `experimental_bearer_token`, else a top-level one.
    pub fn bearer_token(&self) -> Option<&str> {
        self.custom_active()
            .and_then(|t| t.bearer_token.as_deref())
            .or(self.root_bearer_token.as_deref())
    }

    /// Top-level keys not imported, for a note (`None` when there are none).
    pub fn ignored_list(&self) -> Option<String> {
        (!self.ignored.is_empty())
            .then(|| self.ignored.iter().cloned().collect::<Vec<_>>().join(", "))
    }
}

/// Wire API / protocol names the tools use, normalised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// OpenAI Responses (what Codex speaks).
    Responses,
    /// OpenAI Chat Completions.
    Chat,
    /// Anthropic Messages.
    Anthropic,
    /// Gemini native.
    Gemini,
}

/// Normalise a protocol name (`None` for unknown names).
pub fn protocol(name: &str) -> Option<Protocol> {
    match name.trim().to_ascii_lowercase().as_str() {
        "responses" | "response" | "openai_responses" => Some(Protocol::Responses),
        "chat" | "chat_completions" | "openai_chat" | "ollama" | "ollama_chat" => {
            Some(Protocol::Chat)
        }
        "anthropic" | "anthropic_messages" | "claude" | "claude_messages" | "messages" => {
            Some(Protocol::Anthropic)
        }
        "gemini" | "gemini_native" => Some(Protocol::Gemini),
        _ => None,
    }
}

/// Key values the tools write as placeholders, never real keys.
const KEY_PLACEHOLDERS: &[&str] = &["PROXY_MANAGED", "cc-switch-editor-pending-key"];

/// Prefixes of a reference to an OS keychain entry instead of a key (AI Toolbox
/// writes `keyring:<name>` references for the secrets it keeps in the keyring).
const KEYCHAIN_PREFIXES: &[&str] = &["keyring:", "keychain:", "credential-manager:"];

/// Fields that may hold a reference to a keychain entry.
const KEY_REF_FIELDS: &[&str] = &[
    "credential_ref",
    "api_key_ref",
    "key_ref",
    "keyRef",
    "credentialRef",
];

/// A key value as found: a key, a keychain reference, or nothing.
pub fn key_value(value: Option<&str>) -> ForeignKey {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return ForeignKey::Missing;
    };
    if KEY_PLACEHOLDERS.contains(&v) {
        return ForeignKey::Missing;
    }
    let lower = v.to_ascii_lowercase();
    if KEYCHAIN_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return ForeignKey::Keychain;
    }
    ForeignKey::from_text(Some(v))
}

/// What a Codex `auth.json` object holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAuth {
    /// `OPENAI_API_KEY` (or the `env_key` field, or another `*_API_KEY`), or a
    /// keychain reference.
    pub key: ForeignKey,
    /// A ChatGPT login (`tokens`, `auth_mode: chatgpt`, `last_refresh`).
    pub chatgpt_login: bool,
}

/// Read an `auth.json` value. `env_key` names the variable the provider table
/// reads its key from, which the tools may also store as a field.
pub fn parse_auth(value: Option<&Value>, env_key: Option<&str>) -> CodexAuth {
    let Some(obj) = value.and_then(Value::as_object) else {
        return CodexAuth {
            key: ForeignKey::Missing,
            chatgpt_login: false,
        };
    };
    let field = |name: &str| obj.get(name).and_then(Value::as_str);
    let mut key = key_value(field("OPENAI_API_KEY"));
    if key == ForeignKey::Missing
        && let Some(k) = env_key
    {
        key = key_value(field(k));
    }
    if key == ForeignKey::Missing {
        key = obj
            .iter()
            .filter(|(k, _)| k.ends_with("_API_KEY"))
            .map(|(_, v)| key_value(v.as_str()))
            .find(|k| *k != ForeignKey::Missing)
            .unwrap_or(ForeignKey::Missing);
    }
    if key == ForeignKey::Missing
        && KEY_REF_FIELDS
            .iter()
            .any(|f| key_value(field(f)) == ForeignKey::Keychain)
    {
        key = ForeignKey::Keychain;
    }
    let chatgpt_login = obj
        .get("tokens")
        .and_then(Value::as_object)
        .is_some_and(|t| !t.is_empty())
        || field("auth_mode").is_some_and(|m| m.eq_ignore_ascii_case("chatgpt"))
        || obj.get("last_refresh").is_some_and(|v| !v.is_null());
    CodexAuth { key, chatgpt_login }
}

/// A keychain reference anywhere among `fields` of `obj` (provider settings that
/// point at a keychain entry instead of carrying the key).
pub fn keychain_ref_in(obj: Option<&Value>) -> bool {
    obj.and_then(Value::as_object).is_some_and(|o| {
        KEY_REF_FIELDS
            .iter()
            .any(|f| key_value(o.get(*f).and_then(Value::as_str)) == ForeignKey::Keychain)
    })
}
