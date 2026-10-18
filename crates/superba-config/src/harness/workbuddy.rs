//! WorkBuddy's custom models (`~/.workbuddy/models.json` for the China build,
//! `~/.workbuddy-ai/models.json` for the international one;
//! `WORKBUDDY_CONFIG_DIR` moves it): one `models[]` entry per model with
//! Superba's endpoint and key, and its id in `availableModels`.
//!
//! Facts (WorkBuddy 5.8): Chat Completions only, with the `…/v1` base (it
//! appends `/chat/completions`); `${VAR}` in `url` / `apiKey` reads the
//! environment, so keys holding `${` are refused; the key is stored in plain
//! text; the app reloads the file within seconds. A fresh install writes a
//! bare `[]`, which Superba replaces with the object form (and puts back on
//! restore). The app needs a signed-in Tencent account before it shows custom
//! models at all, and it reports each custom model's domain to Tencent.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use superba_core::harness::{
    ENV_WORKBUDDY_CONFIG_DIR, HarnessId, HarnessModel, HarnessOptions, HarnessPlanKind,
    HarnessTarget, WorkBuddyEdition,
};
use superba_core::{Error, LocalContext, Notice, Result, Secret};

use super::doc::{Format, HarnessFile, ReadFile};
use super::jsonc::JsonFlavor;
use super::slot::SlotPath;
use super::state::DesiredFile;
use super::{HarnessSpec, text};

/// The configuration directory of an edition.
pub fn config_dir(ctx: &LocalContext, edition: WorkBuddyEdition) -> PathBuf {
    match ctx.env.var_non_empty(ENV_WORKBUDDY_CONFIG_DIR) {
        // One override for whichever build runs; it applies to the China
        // build (the variable is the app's own).
        Some(d) if edition == WorkBuddyEdition::Cn => PathBuf::from(d.trim()),
        _ => ctx.home_dir.join(edition.dir_name()),
    }
}

/// The editions to configure: the chosen ones, else those whose directory
/// exists, else the China build.
pub fn editions(ctx: &LocalContext, options: &HarnessOptions) -> Vec<WorkBuddyEdition> {
    if !options.workbuddy_editions.is_empty() {
        let mut e = options.workbuddy_editions.clone();
        e.sort();
        e.dedup();
        return e;
    }
    let found: Vec<WorkBuddyEdition> = WorkBuddyEdition::ALL
        .into_iter()
        .filter(|e| config_dir(ctx, *e).is_dir())
        .collect();
    if found.is_empty() {
        vec![WorkBuddyEdition::Cn]
    } else {
        found
    }
}

/// One model entry.
pub fn model_entry(t: &HarnessTarget, m: &HarnessModel) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(m.id));
    let name = m.name.clone().unwrap_or_else(|| m.id.clone());
    o.insert("name".into(), json!(format!("{name} (Superba)")));
    o.insert("vendor".into(), json!("Custom"));
    o.insert("url".into(), json!(t.endpoint.base_text()));
    o.insert("apiKey".into(), json!(t.key.expose()));
    o.insert("supportsToolCall".into(), json!(true));
    o.insert("supportsImages".into(), json!(m.images));
    let levels = m.named_levels();
    if !levels.is_empty() {
        o.insert("supportsReasoning".into(), json!(true));
        let default = m
            .default_reasoning
            .clone()
            .filter(|d| levels.contains(d))
            .or_else(|| levels.iter().find(|l| *l == "medium").cloned())
            .unwrap_or_else(|| levels[0].clone());
        o.insert(
            "reasoning".into(),
            json!({ "defaultEffort": default, "supportedEfforts": levels }),
        );
    }
    if let Some(w) = m.context_window {
        o.insert("maxInputTokens".into(), json!(w));
    }
    Value::Object(o)
}

/// WorkBuddy (both builds).
pub struct WorkBuddy;

impl HarnessSpec for WorkBuddy {
    fn id(&self) -> HarnessId {
        HarnessId::WorkBuddy
    }

    fn files(&self, ctx: &LocalContext, options: &HarnessOptions) -> Result<Vec<HarnessFile>> {
        Ok(editions(ctx, options)
            .into_iter()
            .map(|e| {
                HarnessFile::new(
                    format!("{}/models.json", e.as_str()),
                    config_dir(ctx, e).join("models.json"),
                    Format::Json(JsonFlavor::Strict),
                    true,
                )
            })
            .collect())
    }

    fn desired(&self, t: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>> {
        if t.endpoint.protocol != superba_core::harness::HarnessProtocol::Chat {
            return Err(Error::invalid(
                "WorkBuddy speaks Chat Completions only: pick a provider with a Chat Completions endpoint",
            ));
        }
        Ok(files
            .iter()
            .map(|f| {
                let mut slots = Vec::new();
                for m in &t.models {
                    slots.push((
                        SlotPath::key("models").then_find(&[("id", &m.id)]),
                        Some(model_entry(t, m)),
                    ));
                    slots.push((
                        SlotPath::key("availableModels").then_item(&m.id),
                        Some(json!(m.id)),
                    ));
                }
                DesiredFile {
                    file: f.file.clone(),
                    slots,
                }
            })
            .collect())
    }

    fn check_key(&self, key: &Secret) -> Result<()> {
        superba_core::harness::check_key_text(key).map_err(|_| {
            Error::invalid("the key contains characters WorkBuddy's file cannot hold")
        })?;
        if key.expose().contains("${") {
            return Err(Error::invalid(
                "the key contains ${, which WorkBuddy would read as an environment variable",
            ));
        }
        Ok(())
    }

    fn notes(&self, kind: HarnessPlanKind, _target: Option<&HarnessTarget>) -> Vec<Notice> {
        let mut out = vec![Notice::info(text(
            "config.harness.workbuddy.hot_reload",
            "WorkBuddy reloads models.json within seconds.",
        ))];
        if kind == HarnessPlanKind::Apply {
            out.push(Notice::warning(text(
                "config.harness.workbuddy.login",
                "WorkBuddy shows custom models only after you sign in with a Tencent CodeBuddy / WorkBuddy account, and it sends each custom model's domain to Tencent.",
            )));
            out.push(Notice::info(text(
                "config.harness.workbuddy.plain_key",
                "WorkBuddy keeps the key in plain text in models.json (file mode 0600).",
            )));
        }
        out
    }
}
