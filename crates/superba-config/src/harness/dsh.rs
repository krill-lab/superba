//! dsh, the DeepSeek Harness (`$DSH_HOME`, default `~/.dsh`).
//!
//! Superba writes into the **per-profile** patch layer
//! `profiles/<profile>/cordis.patch.yml` of every profile there is (and of
//! the shipped `web` and `headless` profiles, which dsh completes around the
//! patch on first start):
//!
//! - `llm-pi-ai` entry → `config.providers.superba` (`api`, `baseURL`,
//!   `apiKeyEnv`, `models`; the entry itself is created when the profile has
//!   none — the bundle layers give `llm-pi-ai` no `config`, so nothing is
//!   lost);
//! - the `agent-default-model` entry (`provider: superba`, `model`);
//! - with telemetry off: `session-telemetry-otel` disabled and
//!   `session-log-deepseek` `enabled: false`.
//!
//! The key goes into `$DSH_HOME/.env` as [`DSH_KEY_ENV`] (the only place
//! every profile reads: the headless profile ignores `.credentials.yaml`;
//! `DSH_*` names are refused in `.env` files). The home-level
//! `$DSH_HOME/cordis.patch.yml` is never written (it would replace the whole
//! `llm-pi-ai` config of every profile and make dsh's Models page refuse
//! edits); when it overrides Superba's entries a diagnostic says so.
//! `reasoningEfforts` is declared only for models that have efforts.
//! YAML edits keep comments and `!!js` expressions (`super::yaml`).

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use superba_core::harness::{
    DSH_KEY_ENV, DshTelemetry, ENTRY_ID, ENV_DSH_HOME, HarnessId, HarnessModel, HarnessOptions,
    HarnessPlanKind, HarnessProtocol, HarnessTarget,
};
use superba_core::{Error, LocalContext, Notice, Result, Secret};

use super::doc::{Format, HarnessFile, ReadFile};
use super::slot::{Seg, SlotPath};
use super::state::DesiredFile;
use super::{HarnessSpec, dir_from_env, dotenv, text, yaml};

/// The provider plugin's entry id.
pub const LLM_ENTRY: &str = "llm-pi-ai";
/// The default model entry id.
pub const DEFAULT_MODEL_ENTRY: &str = "agent-default-model";
/// The OpenTelemetry feedback upload entry.
pub const TELEMETRY_ENTRY: &str = "session-telemetry-otel";
/// The DeepSeek session-log upload entry.
pub const SESSION_LOG_ENTRY: &str = "session-log-deepseek";
/// The profiles dsh ships, written even before their first start.
pub const SHIPPED_PROFILES: [&str; 2] = ["web", "headless"];
/// Key of `.env`.
pub const DOTENV: &str = ".env";

/// `$DSH_HOME`.
pub fn dsh_home(ctx: &LocalContext) -> PathBuf {
    dir_from_env(ctx, ENV_DSH_HOME, ctx.home_dir.join(".dsh"))
}

/// The profiles: the directories under `profiles/` plus the shipped ones.
pub fn profiles(ctx: &LocalContext) -> Vec<String> {
    let mut out: Vec<String> = SHIPPED_PROFILES.iter().map(|s| (*s).to_owned()).collect();
    if let Ok(rd) = std::fs::read_dir(dsh_home(ctx).join("profiles")) {
        for e in rd.flatten() {
            if e.path().is_dir()
                && let Some(n) = e.file_name().to_str()
                && !n.starts_with('.')
                && !out.iter().any(|o| o == n)
            {
                out.push(n.to_owned());
            }
        }
    }
    out.sort();
    out
}

/// dsh's `api` for a protocol.
pub fn api_name(p: HarnessProtocol) -> &'static str {
    match p {
        HarnessProtocol::Responses => "openai-responses",
        HarnessProtocol::Chat => "openai-completions",
        HarnessProtocol::Anthropic => "anthropic-messages",
    }
}

fn model_entry(m: &HarnessModel) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(m.id));
    if let Some(n) = &m.name {
        o.insert("name".into(), json!(n));
    }
    if let Some(w) = m.context_window {
        o.insert("contextWindow".into(), json!(w));
    }
    if m.images {
        o.insert("input".into(), json!(["text", "image"]));
    }
    let levels = m.named_levels();
    if !levels.is_empty() {
        let mut efforts = Map::new();
        for l in levels {
            efforts.insert(l.clone(), json!(l));
        }
        o.insert("reasoningEfforts".into(), Value::Object(efforts));
    }
    Value::Object(o)
}

/// Superba's route of the `llm-pi-ai` entry.
pub fn provider_entry(t: &HarnessTarget) -> Value {
    let mut o = Map::new();
    o.insert("displayName".into(), json!(t.display_name()));
    o.insert("apiKeyEnv".into(), json!(DSH_KEY_ENV));
    o.insert("api".into(), json!(api_name(t.endpoint.protocol)));
    o.insert("baseURL".into(), json!(t.endpoint.base_text()));
    o.insert(
        "models".into(),
        Value::Array(t.models.iter().map(model_entry).collect()),
    );
    Value::Object(o)
}

fn entry(id: &str) -> SlotPath {
    SlotPath::find(&[("id", id)])
}

/// dsh.
pub struct Dsh;

impl HarnessSpec for Dsh {
    fn id(&self) -> HarnessId {
        HarnessId::Dsh
    }

    fn files(&self, ctx: &LocalContext, _options: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        let home = dsh_home(ctx);
        let mut out: Vec<HarnessFile> = profiles(ctx)
            .into_iter()
            .map(|p| {
                HarnessFile::new(
                    format!("profiles/{p}/cordis.patch.yml"),
                    home.join("profiles").join(&p).join("cordis.patch.yml"),
                    Format::Yaml,
                    false,
                )
            })
            .collect();
        out.push(HarnessFile::new(
            DOTENV,
            home.join(".env"),
            Format::Dotenv,
            true,
        ));
        Ok(out)
    }

    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        let mut out = Vec::new();
        for f in files {
            if f.file.format == Format::Dotenv {
                out.push(DesiredFile {
                    file: f.file.clone(),
                    slots: vec![(SlotPath::key(DSH_KEY_ENV), Some(json!(t.key.expose())))],
                });
                continue;
            }
            let mut slots = vec![(
                entry(LLM_ENTRY)
                    .then_key("config")
                    .then_key("providers")
                    .then_key(ENTRY_ID),
                Some(provider_entry(t)),
            )];
            if let Some(m) = t.default_model_id() {
                slots.push((
                    entry(DEFAULT_MODEL_ENTRY),
                    Some(json!({
                        "id": DEFAULT_MODEL_ENTRY,
                        "config": { "provider": ENTRY_ID, "model": m },
                    })),
                ));
            }
            if t.options.dsh_telemetry == DshTelemetry::Off {
                slots.push((
                    entry(TELEMETRY_ENTRY),
                    Some(json!({ "id": TELEMETRY_ENTRY, "disabled": true })),
                ));
                slots.push((
                    entry(SESSION_LOG_ENTRY),
                    Some(json!({ "id": SESSION_LOG_ENTRY, "config": { "enabled": false } })),
                ));
            }
            out.push(DesiredFile {
                file: f.file.clone(),
                slots,
            });
        }
        Ok(out)
    }

    fn check_key(&self, key: &Secret) -> Result<()> {
        superba_core::harness::check_key_text(key)
            .map_err(|_| Error::invalid("the key contains characters a .env file cannot hold"))?;
        dotenv::quote(key.expose())
            .map(|_| ())
            .map_err(Error::invalid)
    }

    fn secret_slot(&self, path: &SlotPath) -> bool {
        matches!(path.0.as_slice(), [Seg::Key(k)] if k == DSH_KEY_ENV)
    }

    fn notes(&self, kind: HarnessPlanKind, target: Option<&HarnessTarget>) -> Vec<Notice> {
        let mut out = vec![Notice::info(text(
            "config.harness.dsh.reload",
            "dsh reads its profile patches and .env when it starts (a running dsh web picks up patch changes, but not the key): restart dsh to use this.",
        ))];
        if kind == HarnessPlanKind::Apply {
            out.push(Notice::info(
                text(
                    "config.harness.dsh.plain_key",
                    "The key is kept in plain text in $DSH_HOME/.env as {var} (file mode 0600).",
                )
                .arg("var", DSH_KEY_ENV),
            ));
            if target.is_some_and(|t| t.options.dsh_telemetry == DshTelemetry::Off) {
                out.push(Notice::info(text(
                    "config.harness.dsh.telemetry_off",
                    "dsh's feedback upload (OpenTelemetry) and DeepSeek session-log upload are switched off in every profile.",
                )));
            }
        }
        out
    }

    fn diagnostics(&self, ctx: &LocalContext, _files: &[ReadFile]) -> Vec<Notice> {
        let mut out = Vec::new();
        let home = dsh_home(ctx);
        // The home-level patch outranks every profile.
        if let Ok(t) = std::fs::read_to_string(home.join("cordis.patch.yml"))
            && let Ok(Value::Array(entries)) = yaml::to_value(&t)
        {
            for id in [LLM_ENTRY, DEFAULT_MODEL_ENTRY] {
                if entries
                    .iter()
                    .any(|e| e.get("id").and_then(Value::as_str) == Some(id))
                {
                    out.push(Notice::warning(
                        text(
                            "config.harness.dsh.home_patch",
                            "$DSH_HOME/cordis.patch.yml has a {entry} entry: it replaces what Superba writes into the profiles.",
                        )
                        .arg("entry", id),
                    ));
                }
            }
        }
        if ctx.env.var_non_empty(DSH_KEY_ENV).is_some() {
            out.push(Notice::warning(
                text(
                    "config.harness.dsh.env_var",
                    "{var} is set in the environment: dsh uses it instead of the key in $DSH_HOME/.env.",
                )
                .arg("var", DSH_KEY_ENV),
            ));
        }
        if std::fs::read_to_string(home.join(".credentials.yaml"))
            .is_ok_and(|t| t.contains(DSH_KEY_ENV))
        {
            out.push(Notice::warning(
                text(
                    "config.harness.dsh.credentials",
                    "dsh's .credentials.yaml stores {var}: profiles that read it (web, desktop) use that value instead of the key in .env.",
                )
                .arg("var", DSH_KEY_ENV),
            ));
        }
        out
    }
}
