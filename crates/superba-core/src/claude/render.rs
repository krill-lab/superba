//! The keys Superba owns in Claude Code's `settings.json` and the values a
//! configuration wants there.
//!
//! Paths are dotted: a top-level key (`model`, `modelPicker`) or an
//! environment variable of the `env` block (`env.ANTHROPIC_BASE_URL`).
//! Superba owns the whole set while it is applied: a key the configuration
//! does not use is removed (and restored to its value from before Superba
//! by "Restore"), so a stale slot or a second key never mixes with the
//! provider Superba set up.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{AnthropicAuth, AnthropicEndpoint, ClaudeModels, ClaudeSlot, capabilities_text};
use crate::models::ModelSpec;
use crate::secret::Secret;

/// `ANTHROPIC_BASE_URL`.
pub const ENV_BASE_URL: &str = "ANTHROPIC_BASE_URL";
/// `ANTHROPIC_AUTH_TOKEN` (Bearer).
pub const ENV_AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
/// `ANTHROPIC_API_KEY` (`x-api-key`).
pub const ENV_API_KEY: &str = "ANTHROPIC_API_KEY";
/// `ANTHROPIC_MODEL`: overrides `model` and `/model` for every session.
pub const ENV_MODEL: &str = "ANTHROPIC_MODEL";
/// `ANTHROPIC_SMALL_FAST_MODEL`: the background model.
pub const ENV_SMALL_FAST: &str = "ANTHROPIC_SMALL_FAST_MODEL";
/// `CLAUDE_CODE_SUBAGENT_MODEL`.
pub const ENV_SUBAGENT: &str = "CLAUDE_CODE_SUBAGENT_MODEL";
/// `CLAUDE_CODE_GATEWAY_HINT_HEADERS`: Claude Code sends its prompt id, request
/// class and agent type to a gateway (written while it goes through the local
/// router, which records them).
pub const ENV_GATEWAY_HINT_HEADERS: &str = "CLAUDE_CODE_GATEWAY_HINT_HEADERS";
/// The default model.
pub const KEY_MODEL: &str = "model";
/// The `/model` picker rows.
pub const KEY_MODEL_PICKER: &str = "modelPicker";
/// The block of environment variables.
pub const KEY_ENV: &str = "env";

/// `env.<name>`.
pub fn env_path(name: &str) -> String {
    format!("{KEY_ENV}.{name}")
}

/// The environment variable of an `env.<name>` path.
pub fn env_name(path: &str) -> Option<&str> {
    path.strip_prefix("env.")
}

/// Every path Superba owns while it is applied.
pub fn managed_paths() -> Vec<String> {
    let mut out = vec![
        env_path(ENV_BASE_URL),
        env_path(ENV_AUTH_TOKEN),
        env_path(ENV_API_KEY),
        env_path(ENV_MODEL),
        env_path(ENV_SMALL_FAST),
        env_path(ENV_SUBAGENT),
        env_path(ENV_GATEWAY_HINT_HEADERS),
    ];
    for slot in ClaudeSlot::ALL {
        out.extend(slot.env_vars().iter().map(|v| env_path(v)));
    }
    out.push(KEY_MODEL.to_owned());
    out.push(KEY_MODEL_PICKER.to_owned());
    out
}

/// A path that holds a key.
pub fn is_secret_path(path: &str) -> bool {
    matches!(env_name(path), Some(ENV_AUTH_TOKEN | ENV_API_KEY))
}

/// What one configuration wants in every owned path (`None` = absent), in
/// [`managed_paths`] order (new keys are added to the file in this order).
#[derive(Clone, Default, PartialEq)]
pub struct ClaudeDesired {
    values: Vec<(String, Option<Value>)>,
}

impl std::fmt::Debug for ClaudeDesired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut m = f.debug_map();
        for (k, v) in &self.values {
            if is_secret_path(k) && v.is_some() {
                m.entry(k, &"<redacted>");
            } else {
                m.entry(k, v);
            }
        }
        m.finish()
    }
}

impl ClaudeDesired {
    /// Owned paths and their wanted values.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&Value>)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_ref()))
    }

    /// The wanted value of `path` (`None` = absent or not owned).
    pub fn get(&self, path: &str) -> Option<&Value> {
        self.values
            .iter()
            .find(|(k, _)| k == path)
            .and_then(|(_, v)| v.as_ref())
    }

    /// `path` is owned.
    pub fn owns(&self, path: &str) -> bool {
        self.values.iter().any(|(k, _)| k == path)
    }

    /// Set a path (a new one goes last).
    pub fn set(&mut self, path: impl Into<String>, value: Option<Value>) {
        let path = path.into();
        match self.values.iter_mut().find(|(k, _)| *k == path) {
            Some(slot) => slot.1 = value,
            None => self.values.push((path, value)),
        }
    }

    /// The owned paths.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.values.iter().map(|(k, _)| k.as_str())
    }
}

fn text(s: &str) -> Option<Value> {
    let t = s.trim();
    (!t.is_empty()).then(|| Value::String(t.to_owned()))
}

/// The values for `endpoint`, `key` and `models` in every owned path.
/// `spec` gives a model's metadata for the capability suggestions.
pub fn desired(
    endpoint: &AnthropicEndpoint,
    key: &Secret,
    models: &ClaudeModels,
    spec: &dyn Fn(&str) -> Option<ModelSpec>,
) -> ClaudeDesired {
    let mut d = ClaudeDesired::default();
    for p in managed_paths() {
        d.values.push((p, None));
    }
    d.set(env_path(ENV_BASE_URL), text(&endpoint.base_text()));
    let (used, unused) = match endpoint.auth {
        AnthropicAuth::Bearer => (ENV_AUTH_TOKEN, ENV_API_KEY),
        AnthropicAuth::ApiKey => (ENV_API_KEY, ENV_AUTH_TOKEN),
    };
    d.set(env_path(used), text(key.expose()));
    d.set(env_path(unused), None);
    for (slot, s) in &models.slots {
        let Some(model) = text(&s.model) else {
            continue;
        };
        d.set(env_path(slot.env_model()), Some(model));
        d.set(env_path(&slot.env_name()), s.name.as_deref().and_then(text));
        d.set(
            env_path(&slot.env_description()),
            s.description.as_deref().and_then(text),
        );
        let caps = models.capabilities_of(&s.model, spec(&s.model).as_ref());
        d.set(
            env_path(&slot.env_capabilities()),
            (!caps.is_empty()).then(|| Value::String(capabilities_text(&caps))),
        );
    }
    d.set(KEY_MODEL, models.main.as_deref().and_then(text));
    d.set(
        env_path(ENV_SUBAGENT),
        models.subagent.as_deref().and_then(text),
    );
    d.set(
        env_path(ENV_SMALL_FAST),
        models.small_fast.as_deref().and_then(text),
    );
    let rows: Vec<Value> = models
        .picker
        .iter()
        .filter(|r| !r.model.trim().is_empty())
        .map(|r| {
            let mut o = serde_json::Map::new();
            o.insert("model".into(), json!(r.model.trim()));
            for (k, v) in [
                ("label", &r.label),
                ("description", &r.description),
                ("behavesAs", &r.behaves_as),
            ] {
                if let Some(v) = v.as_deref().and_then(text) {
                    o.insert(k.into(), v);
                }
            }
            Value::Object(o)
        })
        .collect();
    if !rows.is_empty() {
        let mut picker = serde_json::Map::new();
        picker.insert("options".into(), Value::Array(rows));
        if models.replace_builtin {
            picker.insert("replaceBuiltInOptions".into(), Value::Bool(true));
        }
        d.set(KEY_MODEL_PICKER, Some(Value::Object(picker)));
    }
    d
}

/// A value as one line for people: strings without quotes, keys masked.
pub fn display_value(path: &str, value: &Value) -> String {
    match value {
        Value::String(s) if is_secret_path(path) => Secret::new(s.clone()).masked(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The owned paths in `settings` (a parsed `settings.json`) that hold a
/// value, with it.
pub fn owned_values(settings: &Value) -> BTreeMap<String, Value> {
    managed_paths()
        .into_iter()
        .filter_map(|p| get_path(settings, &p).cloned().map(|v| (p, v)))
        .collect()
}

/// The value at a dotted path of a JSON object (`env.X` or a top-level key).
pub fn get_path<'a>(settings: &'a Value, path: &str) -> Option<&'a Value> {
    match path.split_once('.') {
        Some((KEY_ENV, name)) => settings.get(KEY_ENV)?.get(name),
        _ => settings.get(path),
    }
}
