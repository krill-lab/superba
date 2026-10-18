//! The writes of the other programs that manage Codex's configuration,
//! reproduced from their sources (GH#5; cc-switch 4.0.15 and AI Toolbox HEAD,
//! both 2026-10-18), for tests that let them change what Superba wrote. Shared
//! with superba-switch's tests (`#[path]`).

#![allow(dead_code)]

use superba_config::toml_keys::ConfigDoc;

/// A provider as the other tool stores it.
pub struct Provider<'a> {
    pub name: &'a str,
    pub base_url: &'a str,
    pub key: &'a str,
    pub model: Option<&'a str>,
}

/// Kimi in cc-switch's schema 20 fixture (its current Codex provider).
pub const CCS_KIMI: Provider<'static> = Provider {
    name: "kimi",
    base_url: "https://api.moonshot.example/v1",
    key: "sk-test-v20-kimi-000000000000000000",
    model: Some("kimi-k3"),
};

/// "Example Relay" in AI Toolbox's schema 29 fixture (its applied provider).
pub const AITB_RELAY: Provider<'static> = Provider {
    name: "OpenAI",
    base_url: "https://relay.example.com/v1",
    key: "sk-test-relay-4f9c2a7e1b",
    model: Some("gpt-5.5"),
};

fn string(s: &str) -> String {
    format!("{s:?}")
}

/// cc-switch switching Codex to a third-party provider
/// (`services/provider/codex_direct.rs` `write_direct`): `live/floor.rs`
/// clears the top-level key fields the target does not set and the nested
/// model keys; `live/project/codex.rs` `write_route` writes
/// `model_provider = "custom"` and `[model_providers.custom]` with the key as
/// `experimental_bearer_token` (`requires_openai_auth = false`: no login left on
/// disk); the target's values are written in place or appended. Everything
/// else stays byte for byte. `auth.json` is deleted (`codex_login::plan`,
/// "preserve official auth" off, the default). Returns the new files.
pub fn cc_switch(config: Option<&str>, p: &Provider<'_>) -> (String, Option<String>) {
    let mut doc = match config {
        Some(c) => ConfigDoc::parse(c.as_bytes()).unwrap(),
        None => ConfigDoc::empty(),
    };
    let target = [
        "model_provider",
        "model",
        "model_reasoning_effort",
        "disable_response_storage",
    ];
    for key in [
        "model_provider",
        "openai_base_url",
        "model",
        "review_model",
        "model_reasoning_effort",
        "plan_mode_reasoning_effort",
        "disable_response_storage",
        "model_catalog_json",
        "experimental_bearer_token",
        "base_url",
        "wire_api",
        "agents.default_subagent_model",
        "agents.default_subagent_reasoning_effort",
        "memories.extract_model",
        "memories.consolidation_model",
    ] {
        if !target.contains(&key) {
            doc.remove(key);
        }
    }
    doc.set_repr("model_provider", &string("custom")).unwrap();
    if let Some(m) = p.model {
        doc.set_repr("model", &string(m)).unwrap();
    }
    doc.set_repr("model_reasoning_effort", &string("high"))
        .unwrap();
    doc.set_repr("disable_response_storage", "true").unwrap();
    for (k, v) in [
        ("name", string(p.name)),
        ("base_url", string(p.base_url)),
        ("wire_api", string("responses")),
        ("requires_openai_auth", "false".to_owned()),
        ("experimental_bearer_token", string(p.key)),
    ] {
        doc.set_repr(&format!("model_providers.custom.{k}"), &v)
            .unwrap();
    }
    (String::from_utf8(doc.to_bytes()).unwrap(), None)
}

/// AI Toolbox applying a Codex provider (`coding/codex/commands.rs`
/// `apply_config_internal_with_events`): its managed TOML is merged over the
/// file (`merge_toml_tables`: values replaced in place, new keys appended;
/// keys it does not manage, such as `openai_base_url`, stay),
/// `requires_openai_auth = true` with a key, and `#:schema none` prepended
/// (`render_codex_config_document`). `auth.json` is merged
/// (`merge_codex_auth_json`): `OPENAI_API_KEY` replaced or appended,
/// `auth_mode = "apikey"`, other fields kept; pretty JSON without a trailing
/// newline. Returns the new files.
pub fn ai_toolbox(config: Option<&str>, auth: Option<&str>, p: &Provider<'_>) -> (String, String) {
    let mut doc = match config {
        Some(c) => ConfigDoc::parse(c.as_bytes()).unwrap(),
        None => ConfigDoc::empty(),
    };
    if let Some(m) = p.model {
        doc.set_repr("model", &string(m)).unwrap();
    }
    doc.set_repr("model_provider", &string("custom")).unwrap();
    for (k, v) in [
        ("name", string(p.name)),
        ("wire_api", string("responses")),
        ("base_url", string(p.base_url)),
        ("requires_openai_auth", "true".to_owned()),
    ] {
        doc.set_repr(&format!("model_providers.custom.{k}"), &v)
            .unwrap();
    }
    let mut text = String::from_utf8(doc.to_bytes()).unwrap();
    if !text.starts_with("#:schema") {
        text = format!("#:schema none\n{text}");
    }
    let mut m: serde_json::Map<String, serde_json::Value> = auth
        .and_then(|a| serde_json::from_str(a).ok())
        .unwrap_or_default();
    m.insert("OPENAI_API_KEY".into(), p.key.into());
    m.insert("auth_mode".into(), "apikey".into());
    (text, serde_json::to_string_pretty(&m).unwrap())
}

/// The Codex app's model picker: `model` (and effort) written in place.
pub fn codex_model_picker(config: &str, model: &str) -> String {
    let mut doc = ConfigDoc::parse(config.as_bytes()).unwrap();
    doc.set_repr("model", &string(model)).unwrap();
    String::from_utf8(doc.to_bytes()).unwrap()
}
