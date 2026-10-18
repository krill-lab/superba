//! opencode's global configuration (`~/.config/opencode/opencode.jsonc` when
//! it exists, else `opencode.json`; `OPENCODE_CONFIG_DIR` /
//! `XDG_CONFIG_HOME` move it): Superba's provider and the default `model`.
//!
//! Two products share the name and the file (opencode research §0):
//!
//! - **OpenCode 2** (default): `providers.superba = {name, package, settings:
//!   {baseURL, apiKey}, models}` with `@opencode/ai/providers/<kind>`; it
//!   watches the file and applies changes at once.
//! - **OpenCode 1**: `provider.superba = {name, npm, options: {baseURL,
//!   apiKey}, models}` with `@ai-sdk/<kind>`; it reads the file once and
//!   **drops** an OpenCode 2 `providers` block, so the shape follows the
//!   installed major; it needs a restart.
//!
//! The Anthropic packages append `/messages`, so their base is the root plus
//! `/v1`. `{env:…}` / `{file:…}` are substituted in the raw text, so keys
//! holding them are refused.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use superba_core::harness::{
    ENTRY_ID, ENV_OPENCODE_CONFIG_DIR, ENV_XDG_CONFIG_HOME, HarnessId, HarnessModel,
    HarnessOptions, HarnessPlanKind, HarnessProtocol, HarnessTarget, OpencodeMajor,
};
use superba_core::{Error, LocalContext, Notice, Result, Secret};

use super::doc::{Format, HarnessFile, ReadFile};
use super::jsonc::JsonFlavor;
use super::slot::SlotPath;
use super::state::DesiredFile;
use super::{HarnessSpec, text};

/// opencode's configuration schema URL (written into a file Superba creates;
/// OpenCode 1 adds it to a file without one).
pub const SCHEMA_URL: &str = "https://opencode.ai/config.json";
/// OpenCode 1 caps the output at 32 000 tokens (`OUTPUT_TOKEN_MAX`); its
/// `limit` needs an output value next to the context.
pub const V1_OUTPUT_LIMIT: u64 = 32_000;

/// opencode's global configuration directory.
pub fn config_dir(ctx: &LocalContext) -> PathBuf {
    if let Some(d) = ctx.env.var_non_empty(ENV_OPENCODE_CONFIG_DIR) {
        return PathBuf::from(d.trim());
    }
    match ctx.env.var_non_empty(ENV_XDG_CONFIG_HOME) {
        Some(x) => PathBuf::from(x.trim()).join("opencode"),
        None => ctx.home_dir.join(".config").join("opencode"),
    }
}

/// The package (OpenCode 2) or npm module (OpenCode 1) of a protocol.
pub fn package(major: OpencodeMajor, p: HarnessProtocol) -> &'static str {
    match (major, p) {
        (OpencodeMajor::V2, HarnessProtocol::Responses) => "@opencode/ai/providers/openai",
        (OpencodeMajor::V2, HarnessProtocol::Chat) => "@opencode/ai/providers/openai-compatible",
        (OpencodeMajor::V2, HarnessProtocol::Anthropic) => {
            "@opencode/ai/providers/anthropic-compatible"
        }
        (OpencodeMajor::V1, HarnessProtocol::Responses) => "@ai-sdk/openai",
        (OpencodeMajor::V1, HarnessProtocol::Chat) => "@ai-sdk/openai-compatible",
        (OpencodeMajor::V1, HarnessProtocol::Anthropic) => "@ai-sdk/anthropic",
    }
}

/// The base URL opencode wants: the OpenAI base as it is, the Anthropic
/// root plus `/v1`.
pub fn base_url(t: &HarnessTarget) -> String {
    let base = t.endpoint.base_text();
    match t.endpoint.protocol {
        HarnessProtocol::Anthropic if !base.ends_with("/v1") => format!("{base}/v1"),
        _ => base,
    }
}

fn model_v1(m: &HarnessModel) -> Value {
    let mut o = Map::new();
    if let Some(n) = &m.name {
        o.insert("name".into(), json!(n));
    }
    let levels = m.named_levels();
    if m.reasons() {
        o.insert("reasoning".into(), json!(true));
        if !levels.is_empty() {
            o.insert(
                "reasoning_options".into(),
                json!([{ "type": "effort", "values": levels }]),
            );
        }
    }
    if m.images {
        o.insert("attachment".into(), json!(true));
        o.insert(
            "modalities".into(),
            json!({ "input": ["text", "image"], "output": ["text"] }),
        );
    }
    if let Some(w) = m.context_window {
        o.insert(
            "limit".into(),
            json!({ "context": w, "output": V1_OUTPUT_LIMIT }),
        );
    }
    Value::Object(o)
}

fn model_v2(m: &HarnessModel) -> Value {
    let mut o = Map::new();
    if let Some(n) = &m.name {
        o.insert("name".into(), json!(n));
    }
    let mut caps = Map::new();
    if m.reasons() {
        let levels = m.named_levels();
        let mut r = Map::new();
        r.insert("supported".into(), json!(true));
        if !levels.is_empty() {
            r.insert("effort".into(), json!(levels));
        }
        caps.insert("reasoning".into(), Value::Object(r));
    }
    if m.images {
        caps.insert("input".into(), json!(["text", "image"]));
    }
    if !caps.is_empty() {
        o.insert("capabilities".into(), Value::Object(caps));
    }
    if let Some(w) = m.context_window {
        o.insert("limit".into(), json!({ "context": w }));
    }
    Value::Object(o)
}

/// Superba's provider entry for `major`.
pub fn provider_entry(t: &HarnessTarget, major: OpencodeMajor) -> Value {
    let mut models = Map::new();
    for m in &t.models {
        models.insert(
            m.id.clone(),
            match major {
                OpencodeMajor::V1 => model_v1(m),
                OpencodeMajor::V2 => model_v2(m),
            },
        );
    }
    let pkg = package(major, t.endpoint.protocol);
    let mut conn = Map::new();
    conn.insert("baseURL".into(), json!(base_url(t)));
    conn.insert("apiKey".into(), json!(t.key.expose()));
    if major == OpencodeMajor::V1 && t.endpoint.protocol == HarnessProtocol::Chat {
        // Chat Completions sends prompt_cache_key only with this.
        conn.insert("setCacheKey".into(), json!(true));
    }
    let mut o = Map::new();
    o.insert("name".into(), json!(t.display_name()));
    match major {
        OpencodeMajor::V1 => {
            o.insert("npm".into(), json!(pkg));
            o.insert("options".into(), Value::Object(conn));
        }
        OpencodeMajor::V2 => {
            o.insert("package".into(), json!(pkg));
            o.insert("settings".into(), Value::Object(conn));
        }
    }
    o.insert("models".into(), Value::Object(models));
    Value::Object(o)
}

/// The shape to write: the setting, else OpenCode 2.
pub fn major_of(options: &HarnessOptions) -> OpencodeMajor {
    options.opencode_major.unwrap_or(OpencodeMajor::V2)
}

/// opencode.
pub struct OpenCode;

impl HarnessSpec for OpenCode {
    fn id(&self) -> HarnessId {
        HarnessId::OpenCode
    }

    fn files(&self, ctx: &LocalContext, _options: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        let dir = config_dir(ctx);
        let jsonc = dir.join("opencode.jsonc");
        let path = if jsonc.is_file() {
            jsonc
        } else {
            dir.join("opencode.json")
        };
        let key = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "opencode.json".into());
        Ok(vec![HarnessFile::new(
            key,
            path,
            Format::Json(JsonFlavor::Jsonc),
            true,
        )])
    }

    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        let f = files
            .first()
            .ok_or_else(|| Error::other("opencode's configuration file was not read"))?;
        let major = major_of(&t.options);
        let (block, other) = match major {
            OpencodeMajor::V1 => ("provider", "providers"),
            OpencodeMajor::V2 => ("providers", "provider"),
        };
        let mut slots = Vec::new();
        // A file Superba creates names the schema (OpenCode 1 would add it).
        let schema_now = f.value().get("$schema").cloned();
        if f.bytes.is_none() || schema_now.as_ref() == Some(&json!(SCHEMA_URL)) {
            slots.push((SlotPath::key("$schema"), Some(json!(SCHEMA_URL))));
        }
        slots.push((
            SlotPath::keys(&[block, ENTRY_ID]),
            Some(provider_entry(t, major)),
        ));
        // The other major's entry would name the provider twice.
        slots.push((SlotPath::keys(&[other, ENTRY_ID]), None));
        slots.push((
            SlotPath::key("model"),
            t.default_model_id()
                .map(|m| json!(format!("{ENTRY_ID}/{m}"))),
        ));
        Ok(vec![DesiredFile {
            file: f.file.clone(),
            slots,
        }])
    }

    fn check_key(&self, key: &Secret) -> Result<()> {
        superba_core::harness::check_key_text(key).map_err(|_| {
            Error::invalid("the key contains characters opencode's file cannot hold")
        })?;
        if key.expose().contains("{env:") || key.expose().contains("{file:") {
            return Err(Error::invalid(
                "the key contains {env: or {file:, which opencode would substitute",
            ));
        }
        Ok(())
    }

    fn notes(&self, kind: HarnessPlanKind, target: Option<&HarnessTarget>) -> Vec<Notice> {
        let major = target.map_or(OpencodeMajor::V2, |t| major_of(&t.options));
        let mut out = vec![match major {
            OpencodeMajor::V2 => Notice::info(text(
                "config.harness.opencode.v2_live",
                "OpenCode 2 picks this up at once (its background service reloads the file).",
            )),
            OpencodeMajor::V1 => Notice::info(text(
                "config.harness.opencode.v1_restart",
                "OpenCode 1 reads its configuration once: restart opencode to use this.",
            )),
        }];
        if kind == HarnessPlanKind::Apply {
            out.push(Notice::info(text(
                "config.harness.opencode.plain_key",
                "opencode keeps the key in plain text in its configuration file (file mode 0600).",
            )));
            out.push(Notice::info(text(
                "config.harness.opencode.project_override",
                "An opencode.json in a project overrides these settings while you work in that project.",
            )));
        }
        out
    }

    fn diagnostics(&self, ctx: &LocalContext, files: &[ReadFile]) -> Vec<Notice> {
        let mut out = Vec::new();
        for var in ["OPENCODE_CONFIG", "OPENCODE_CONFIG_CONTENT"] {
            if ctx.env.var_non_empty(var).is_some() {
                out.push(Notice::warning(
                    text(
                        "config.harness.opencode.env_override",
                        "{var} is set in Superba's environment: opencode loads it after the global file, so its model or provider may win.",
                    )
                    .arg("var", var),
                ));
            }
        }
        let managed: &[&str] = if cfg!(windows) {
            &[r"C:\ProgramData\opencode"]
        } else if cfg!(target_os = "macos") {
            &["/Library/Application Support/opencode"]
        } else {
            &["/etc/opencode"]
        };
        for dir in managed {
            for name in ["opencode.json", "opencode.jsonc"] {
                let p = PathBuf::from(dir).join(name);
                if p.is_file() {
                    out.push(Notice::warning(
                        text(
                            "config.harness.opencode.managed",
                            "Managed opencode settings in {path} override everything Superba writes.",
                        )
                        .arg("path", superba_core::paths::display_path(&p)),
                    ));
                }
            }
        }
        if let Some(f) = files.first() {
            let both = f.file.key == "opencode.jsonc"
                && f.file.path.with_file_name("opencode.json").is_file();
            if both {
                out.push(Notice::info(text(
                    "config.harness.opencode.both_files",
                    "Both opencode.json and opencode.jsonc exist; Superba writes opencode.jsonc, which opencode reads last.",
                )));
            }
        }
        out
    }
}
