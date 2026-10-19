//! ZCode's personal providers (`~/.zcode/v2/provider_config.json`, not the
//! legacy `v2/config.json` the docs still name): Superba's provider rule,
//! its place first in `providerOrder` (the agent CLI uses the first
//! provider), one model rule per model it knows something about, and the
//! default model selection.
//!
//! Facts (ZCode 3.15.3): the key is stored in plain text in this file; the
//! desktop watches it and shows changes within about 8 s; it lists no models
//! for custom providers, so Superba supplies the model ids
//! (`personalModelIds`). OpenAI types take the `…/v1` base, Anthropic the
//! root.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use superba_core::harness::{
    ENTRY_ID, ENV_ZCODE_PROVIDER_FILE, HarnessId, HarnessModel, HarnessOptions, HarnessPlanKind,
    HarnessProtocol, HarnessTarget,
};
use superba_core::{Error, LocalContext, Notice, Result};

use super::doc::{Format, HarnessFile, ReadFile};
use super::jsonc::JsonFlavor;
use super::slot::SlotPath;
use super::state::DesiredFile;
use super::{HarnessSpec, text};

/// Key of `provider_config.json`.
pub const PROVIDER_CONFIG: &str = "provider_config.json";
/// The file's schema version.
pub const SCHEMA_VERSION: u64 = 1;
/// The group of custom providers.
pub const GROUP: &str = "standard-personal";

/// `provider_config.json`.
pub fn provider_config(ctx: &LocalContext) -> PathBuf {
    match ctx.env.var_non_empty(ENV_ZCODE_PROVIDER_FILE) {
        Some(p) => PathBuf::from(p.trim()),
        None => ctx
            .home_dir
            .join(".zcode")
            .join("v2")
            .join("provider_config.json"),
    }
}

/// ZCode's `api.type`.
pub fn api_type(p: HarnessProtocol) -> &'static str {
    match p {
        HarnessProtocol::Responses => "openai-responses",
        HarnessProtocol::Chat => "openai-chat-completions",
        HarnessProtocol::Anthropic => "anthropic-messages",
        HarnessProtocol::Gemini => "unsupported",
    }
}

/// Superba's provider rule.
pub fn provider_rule(t: &HarnessTarget) -> Value {
    json!({
        "providerId": ENTRY_ID,
        "providerName": t.display_name(),
        "config": {
            "group": GROUP,
            "access": { "type": "api-key", "apiKey": t.key.expose() },
            "api": { "type": api_type(t.endpoint.protocol), "baseUrl": t.endpoint.base_text() },
            "personalModelIds": t.models.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        }
    })
}

/// A model's rule (`None` when nothing is known beyond its id).
pub fn model_rule(m: &HarnessModel) -> Option<Value> {
    let mut props = Map::new();
    if let Some(w) = m.context_window {
        props.insert("contextWindow".into(), json!(w));
    }
    if m.images {
        props.insert(
            "inputFormat".into(),
            json!({ "supportsText": true, "supportsImage": true }),
        );
    }
    let mut specs = Map::new();
    let levels = m.named_levels();
    if !levels.is_empty() {
        specs.insert("reasoningLevel".into(), json!({ "values": levels }));
    }
    if props.is_empty() && specs.is_empty() {
        return None;
    }
    let mut config = Map::new();
    if !props.is_empty() {
        config.insert("properties".into(), Value::Object(props));
    }
    if !specs.is_empty() {
        config.insert("optionSpecs".into(), Value::Object(specs));
    }
    Some(json!({ "providerId": ENTRY_ID, "modelId": m.id, "config": config }))
}

fn rule_path(model: &str) -> SlotPath {
    SlotPath::keys(&["config", "modelConfigRules", "providerModelRules"])
        .then_find(&[("providerId", ENTRY_ID), ("modelId", model)])
}

/// ZCode.
pub struct ZCode;

impl HarnessSpec for ZCode {
    fn id(&self) -> HarnessId {
        HarnessId::ZCode
    }

    fn files(&self, ctx: &LocalContext, _options: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        Ok(vec![HarnessFile::new(
            PROVIDER_CONFIG,
            provider_config(ctx),
            Format::Json(JsonFlavor::Strict),
            true,
        )])
    }

    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        let f = files
            .first()
            .ok_or_else(|| Error::other("ZCode's provider file was not read"))?;
        let mut slots = Vec::new();
        let version = f.value().get("schemaVersion").cloned();
        if f.bytes.is_none() || version == Some(json!(SCHEMA_VERSION)) {
            slots.push((SlotPath::key("schemaVersion"), Some(json!(SCHEMA_VERSION))));
        }
        slots.push((
            SlotPath::keys(&["config", "providerOrder"]).then_first_item(ENTRY_ID),
            Some(json!(ENTRY_ID)),
        ));
        slots.push((
            SlotPath::keys(&["config", "providerConfigRules", "providerRules"])
                .then_find(&[("providerId", ENTRY_ID)]),
            Some(provider_rule(t)),
        ));
        for m in &t.models {
            if let Some(rule) = model_rule(m) {
                slots.push((rule_path(&m.id), Some(rule)));
            }
        }
        slots.push((
            SlotPath::keys(&["config", "defaultModelSelection"]),
            t.default_model_id()
                .map(|m| json!({ "providerId": ENTRY_ID, "modelId": m })),
        ));
        Ok(vec![DesiredFile {
            file: f.file.clone(),
            slots,
        }])
    }

    fn notes(&self, kind: HarnessPlanKind, _target: Option<&HarnessTarget>) -> Vec<Notice> {
        let mut out = vec![Notice::info(text(
            "config.harness.zcode.hot_reload",
            "ZCode shows the change within about 8 seconds; a session that is already open keeps its model.",
        ))];
        if kind == HarnessPlanKind::Apply {
            out.push(Notice::warning(text(
                "config.harness.zcode.plain_key",
                "ZCode keeps the key in plain text in provider_config.json (file mode 0600).",
            )));
        }
        out
    }

    fn diagnostics(&self, ctx: &LocalContext, _files: &[ReadFile]) -> Vec<Notice> {
        let legacy = ctx.home_dir.join(".zcode").join("v2").join("config.json");
        if legacy.is_file() {
            vec![Notice::info(text(
                "config.harness.zcode.legacy",
                "ZCode's legacy v2/config.json exists; ZCode imported it once and now uses provider_config.json, which is what Superba writes.",
            ))]
        } else {
            Vec::new()
        }
    }
}
