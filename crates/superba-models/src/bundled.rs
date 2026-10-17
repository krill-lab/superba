//! The consumer's own catalog (`codex debug models --bundled`) and what each
//! Codex version requires of a catalog entry.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use superba_core::{CodexVersion, Error, Result};

use crate::BundledCatalog;

/// `ModelInfo` fields without a serde default in every Codex from 0.148.0 to
/// 0.163.x (`codex-rs/protocol/src/openai_models.rs`). A missing one makes
/// `model_catalog_json` fail to parse, which stops the app-server.
const REQUIRED_BASE: &[&str] = &[
    "slug",
    "display_name",
    "supported_reasoning_levels",
    "shell_type",
    "visibility",
    "supported_in_api",
    "priority",
    "support_verbosity",
    "truncation_policy",
    "experimental_supported_tools",
];

/// Extra required fields of older versions: (first version that no longer
/// requires it, field).
const REQUIRED_UNTIL: &[((u64, u64, u64), &str)] = &[
    ((0, 145, 0), "supports_reasoning_summaries"),
    ((0, 147, 0), "base_instructions"),
    ((0, 148, 0), "supports_parallel_tool_calls"),
];

/// Every `ModelInfo` field of Codex 0.140.0 to 0.163.x, plus fields Codex-native
/// `/models` answers add for older clients. A field outside this list that every
/// entry of a consumer's own dump carries is treated as required (a newer Codex
/// may have added it).
const KNOWN_FIELDS: &[&str] = &[
    "additional_speed_tiers",
    "apply_patch_tool_type",
    "auto_compact_token_limit",
    "auto_review_model_override",
    "availability_nux",
    "available_access_programs",
    "available_in_plans",
    "base_instructions",
    "comp_hash",
    "context_window",
    "default_reasoning_level",
    "default_reasoning_summary",
    "default_service_tier",
    "default_verbosity",
    "description",
    "display_name",
    "effective_context_window_percent",
    "experimental_supported_tools",
    "guardian",
    "include_apps_usage_instructions",
    "include_plugin_usage_instructions",
    "include_skills_usage_instructions",
    "input_modalities",
    "max_context_window",
    "minimal_client_version",
    "model_messages",
    "model_specialty",
    "multi_agent_reasoning_effort",
    "multi_agent_version",
    "node_repl_auto_review_required",
    "node_repl_disabled",
    "prefer_websockets",
    "priority",
    "service_tiers",
    "shell_type",
    "slug",
    "support_verbosity",
    "supported_in_api",
    "supported_reasoning_levels",
    "supports_experimental_context",
    "supports_image_detail_original",
    "supports_parallel_tool_calls",
    "supports_reasoning_effort_updates",
    "supports_reasoning_summaries",
    "supports_reasoning_summary_parameter",
    "supports_search_tool",
    "tool_mode",
    "truncation_policy",
    "upgrade",
    "use_responses_lite",
    "visibility",
    "web_search_tool_type",
];

/// Fields a catalog entry must carry for `consumer` (from the Codex source of
/// 0.140.0 to 0.163.x; older consumers are treated like 0.140.0).
pub fn required_fields(consumer: &CodexVersion) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = REQUIRED_BASE.to_vec();
    for (until, field) in REQUIRED_UNTIL {
        if consumer.triple() < *until {
            out.push(field);
        }
    }
    out
}

/// True if `field` is a `ModelInfo` field Superba knows.
pub(crate) fn is_known_field(field: &str) -> bool {
    KNOWN_FIELDS.contains(&field)
}

/// Parse the output of `codex debug models --bundled` (Codex 0.140-0.163):
/// `{"models":[ModelInfo...]}` (a bare array is accepted too). Text before the
/// first `{` or `[` (a warning line) is skipped. Entries without a slug are
/// dropped, later duplicates of a slug are ignored, an empty list is an error.
/// [`BundledCatalog::codex_version`] is left empty for the caller to set.
pub fn parse_bundled(output: &[u8]) -> Result<BundledCatalog> {
    let start = output
        .iter()
        .position(|b| *b == b'{' || *b == b'[')
        .ok_or_else(|| Error::parse("codex debug models", "no JSON in the output"))?;
    let mut stream = serde_json::Deserializer::from_slice(&output[start..]).into_iter::<Value>();
    let value = stream
        .next()
        .ok_or_else(|| Error::parse("codex debug models", "no JSON in the output"))?
        .map_err(|e| Error::parse("codex debug models", e))?;
    let items = match value {
        Value::Object(mut o) => match o.remove("models") {
            Some(Value::Array(a)) => a,
            _ => {
                return Err(Error::parse(
                    "codex debug models",
                    "the output has no `models` list",
                ));
            }
        },
        Value::Array(a) => a,
        _ => {
            return Err(Error::parse(
                "codex debug models",
                "the output is not a model list",
            ));
        }
    };
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for item in items {
        let Some(slug) = slug_of(&item) else { continue };
        if seen.insert(slug.to_owned()) {
            entries.push(item);
        }
    }
    if entries.is_empty() {
        return Err(Error::parse(
            "codex debug models",
            "the model list is empty",
        ));
    }
    let required_fields = common_fields(&entries);
    Ok(BundledCatalog {
        codex_version: None,
        entries,
        required_fields,
    })
}

/// The non-empty `slug` of an entry.
pub(crate) fn slug_of(entry: &Value) -> Option<&str> {
    entry
        .get("slug")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Fields every entry carries, sorted.
pub(crate) fn common_fields(entries: &[Value]) -> Vec<String> {
    let mut iter = entries.iter().filter_map(Value::as_object);
    let Some(first) = iter.next() else {
        return Vec::new();
    };
    let mut common: BTreeSet<&str> = first.keys().map(String::as_str).collect();
    for o in iter {
        common.retain(|k| o.contains_key(*k));
    }
    common.into_iter().map(str::to_owned).collect()
}

/// The fields every entry of a catalog for `consumer` must carry, given the
/// consumer's own template: the version table plus fields Superba does not know
/// that every template entry carries.
pub(crate) fn effective_required(
    consumer: &CodexVersion,
    template: &BundledCatalog,
) -> Vec<String> {
    let mut out: Vec<String> = required_fields(consumer)
        .into_iter()
        .map(str::to_owned)
        .collect();
    for f in template_fields(template) {
        if !is_known_field(&f) && !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// The template's common fields ([`BundledCatalog::required_fields`], computed
/// from the entries when the caller left it empty).
pub(crate) fn template_fields(template: &BundledCatalog) -> Vec<String> {
    if template.required_fields.is_empty() {
        common_fields(&template.entries)
    } else {
        template.required_fields.clone()
    }
}

/// Copy `fields` that `entry` lacks from `from`; returns the names copied.
pub(crate) fn fill_missing(
    entry: &mut Map<String, Value>,
    from: &Map<String, Value>,
    fields: &[String],
) -> Vec<String> {
    let mut filled = Vec::new();
    for f in fields {
        if !entry.contains_key(f)
            && let Some(v) = from.get(f)
        {
            entry.insert(f.clone(), v.clone());
            filled.push(f.clone());
        }
    }
    filled
}
