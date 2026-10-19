//! pi (`~/.pi/agent`, or `PI_CODING_AGENT_DIR`): Superba's provider in
//! `models.json` (`providers.superba`) and the default in `settings.json`
//! (`defaultProvider`, `defaultModel`).
//!
//! Facts (pi 1.3.0): one value that does not match pi's schema makes pi drop
//! the whole `models.json`, every custom provider with it, so the file is
//! checked against the vendored schema before it is written. `apiKey` and
//! header values are templates (`$NAME` / `${NAME}` read the environment, a
//! leading `!` runs a command; `$$` and `$!` are a literal `$` / `!`), so
//! Superba escapes the key. `baseUrl` is used as written. `models.json` may
//! hold `//` comments and trailing commas (kept). pi reads `models.json` when
//! it starts and when `/model` opens; there is no hot reload.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use superba_core::harness::{
    ENTRY_ID, ENV_PI_AGENT_DIR, HarnessId, HarnessModel, HarnessOptions, HarnessPlanKind,
    HarnessProtocol, HarnessTarget,
};
use superba_core::{Error, LocalContext, Notice, Result};

use super::doc::{Format, HarnessFile, ReadFile};
use super::jsonc::JsonFlavor;
use super::slot::SlotPath;
use super::state::DesiredFile;
use super::{HarnessSpec, dir_from_env, file_of, schema, text};

/// pi's `models.json` schema (vendored, `catalog/harness/pi`).
pub const MODELS_SCHEMA: &str = include_str!("../../../../catalog/harness/pi/models.schema.json");

/// Key of `models.json`.
pub const MODELS: &str = "models.json";
/// Key of `settings.json`.
pub const SETTINGS: &str = "settings.json";

/// pi's agent directory.
pub fn agent_dir(ctx: &LocalContext) -> PathBuf {
    dir_from_env(
        ctx,
        ENV_PI_AGENT_DIR,
        ctx.home_dir.join(".pi").join("agent"),
    )
}

/// A key as a pi config value: `$` and `!` escaped, so pi never reads the
/// environment or runs a command for it.
pub fn escape_value(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 2);
    for c in key.chars() {
        match c {
            '$' => out.push_str("$$"),
            '!' => out.push_str("$!"),
            c => out.push(c),
        }
    }
    out
}

/// pi's `api` for a protocol.
pub fn api_name(p: HarnessProtocol) -> &'static str {
    match p {
        HarnessProtocol::Responses => "openai-responses",
        HarnessProtocol::Chat => "openai-completions",
        HarnessProtocol::Anthropic => "anthropic-messages",
        HarnessProtocol::Gemini => "unsupported",
    }
}

/// pi's thinking levels for a model's efforts: `low` / `medium` / `high`
/// hidden (`null`) when the model lacks them, `xhigh` / `max` shown only
/// when it has them, `off` / `minimal` only for `none` / `minimal`.
pub fn thinking_map(m: &HarnessModel) -> Map<String, Value> {
    let has = |l: &str| m.reasoning_levels.iter().any(|x| x == l);
    let mut map = Map::new();
    if !has("none") {
        map.insert("off".into(), Value::Null);
    }
    map.insert(
        "minimal".into(),
        if has("minimal") {
            json!("minimal")
        } else {
            Value::Null
        },
    );
    for l in ["low", "medium", "high"] {
        if !has(l) {
            map.insert(l.into(), Value::Null);
        }
    }
    for l in ["xhigh", "max"] {
        if has(l) {
            map.insert(l.into(), json!(l));
        }
    }
    map
}

/// One model entry.
pub fn model_entry(m: &HarnessModel) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(m.id));
    if let Some(n) = &m.name {
        o.insert("name".into(), json!(n));
    }
    if m.reasons() {
        o.insert("reasoning".into(), json!(true));
        o.insert("thinkingLevelMap".into(), Value::Object(thinking_map(m)));
    }
    o.insert(
        "input".into(),
        if m.images {
            json!(["text", "image"])
        } else {
            json!(["text"])
        },
    );
    if let Some(w) = m.context_window {
        o.insert("contextWindow".into(), json!(w));
    }
    Value::Object(o)
}

/// Superba's provider entry.
pub fn provider_entry(t: &HarnessTarget) -> Value {
    let mut o = Map::new();
    o.insert("name".into(), json!(t.display_name()));
    o.insert("baseUrl".into(), json!(t.endpoint.base_text()));
    o.insert("api".into(), json!(api_name(t.endpoint.protocol)));
    o.insert("apiKey".into(), json!(escape_value(t.key.expose())));
    o.insert(
        "models".into(),
        Value::Array(t.models.iter().map(model_entry).collect()),
    );
    Value::Object(o)
}

/// pi.
pub struct Pi;

impl HarnessSpec for Pi {
    fn id(&self) -> HarnessId {
        HarnessId::Pi
    }

    fn files(&self, ctx: &LocalContext, _options: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        let dir = agent_dir(ctx);
        Ok(vec![
            HarnessFile::new(
                MODELS,
                dir.join("models.json"),
                Format::Json(JsonFlavor::Jsonc),
                true,
            ),
            HarnessFile::new(
                SETTINGS,
                dir.join("settings.json"),
                Format::Json(JsonFlavor::Strict),
                false,
            ),
        ])
    }

    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        let models = file_of(files, MODELS)?;
        let settings = file_of(files, SETTINGS)?;
        Ok(vec![
            DesiredFile {
                file: models,
                slots: vec![(
                    SlotPath::keys(&["providers", ENTRY_ID]),
                    Some(provider_entry(t)),
                )],
            },
            DesiredFile {
                file: settings,
                slots: vec![
                    (SlotPath::key("defaultProvider"), Some(json!(ENTRY_ID))),
                    (
                        SlotPath::key("defaultModel"),
                        t.default_model_id().map(|m| json!(m)),
                    ),
                ],
            },
        ])
    }

    fn check(&self, after: &[(HarnessFile, Value)]) -> Result<()> {
        let schema: Value = serde_json::from_str(MODELS_SCHEMA)
            .map_err(|e| Error::other(format!("pi's vendored schema: {e}")))?;
        for (file, value) in after {
            if file.key != MODELS || value.is_null() {
                continue;
            }
            if let Err((path, msg)) = schema::check(&schema, value) {
                return Err(Error::invalid(format!(
                    "{}: pi would reject the whole file ({path}: {msg}), hiding every custom provider; nothing was written",
                    file.path.display()
                )));
            }
        }
        Ok(())
    }

    fn notes(&self, kind: HarnessPlanKind, _target: Option<&HarnessTarget>) -> Vec<Notice> {
        let mut out = vec![Notice::info(text(
            "config.harness.pi.no_hot_reload",
            "pi reads models.json when it starts and when /model opens: running sessions keep the old provider until you pick the model again in /model.",
        ))];
        if kind == HarnessPlanKind::Apply {
            out.push(Notice::info(text(
                "config.harness.pi.plain_key",
                "pi keeps the key in plain text in models.json (file mode 0600).",
            )));
        }
        out
    }

    fn diagnostics(&self, ctx: &LocalContext, _files: &[ReadFile]) -> Vec<Notice> {
        let mut out = Vec::new();
        let auth = agent_dir(ctx).join("auth.json");
        let has_login = std::fs::read(&auth)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|v| v.get(ENTRY_ID).is_some());
        if has_login {
            out.push(Notice::warning(
                text(
                    "config.harness.pi.auth_override",
                    "pi's auth.json holds a credential for the provider \"{id}\": it wins over the key Superba writes. Remove it with /logout in pi.",
                )
                .arg("id", ENTRY_ID),
            ));
        }
        out
    }
}
