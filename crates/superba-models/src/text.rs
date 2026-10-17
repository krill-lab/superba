//! Notes of this crate (`models_note.*` keys in superba-core's
//! `locales/core.models.*.toml`).

use superba_core::{LocalizedText, Notice, SpeedTier};

pub(crate) fn generic_template(model: &str) -> Notice {
    Notice::info(
        LocalizedText::new(
            "models_note.generic_template",
            "{model}: no catalog entry is known; Codex's settings for unknown models are used",
        )
        .arg("model", model),
    )
}

pub(crate) fn generic_without_prompt(model: &str) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "models_note.generic_without_prompt",
            "{model}: the generic instructions of this Codex version are not available; another model's instructions are used",
        )
        .arg("model", model),
    )
}

pub(crate) fn filled_fields(model: &str, version: &str, fields: &[String]) -> Notice {
    Notice::info(
        LocalizedText::new(
            "models_note.filled_fields",
            "{model}: fields Codex {version} requires were taken from its built-in catalog ({fields})",
        )
        .arg("model", model)
        .arg("version", version)
        .arg("fields", fields.join(", ")),
    )
}

pub(crate) fn compact_clamped(model: &str, limit: u64, effective: u64) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "models_note.compact_clamped",
            "{model}: auto-compact at {limit} tokens is above 90% of the context window; Codex compacts at {effective}",
        )
        .arg("model", model)
        .arg("limit", limit)
        .arg("effective", effective),
    )
}

pub(crate) fn max_raised(model: &str, window: u64) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "models_note.max_raised",
            "{model}: the maximum context window was raised to {window} tokens; the upstream may refuse longer prompts",
        )
        .arg("model", model)
        .arg("window", window),
    )
}

pub(crate) fn override_unknown_model(model: &str) -> Notice {
    Notice::info(
        LocalizedText::new(
            "models_note.override_unknown_model",
            "The settings for {model} are kept but not used: the model is not in the list",
        )
        .arg("model", model),
    )
}

pub(crate) fn duplicate(model: &str) -> Notice {
    Notice::info(
        LocalizedText::new(
            "models_note.duplicate",
            "{model} is listed more than once; the first entry is used",
        )
        .arg("model", model),
    )
}

pub(crate) fn model_added(model: &str) -> Notice {
    Notice::info(
        LocalizedText::new(
            "models_note.model_added",
            "{model} (the default model) is not in the list; it was added",
        )
        .arg("model", model),
    )
}

pub(crate) fn tier_not_offered(tier: SpeedTier) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "models_note.tier_not_offered",
            "No listed model offers {tier}; Codex runs Standard",
        )
        .arg_text("tier", tier.text()),
    )
}

pub(crate) fn tier_not_offered_by_model(model: &str, tier: SpeedTier) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "models_note.tier_not_offered_by_model",
            "{model} does not offer {tier}; it runs Standard",
        )
        .arg("model", model)
        .arg_text("tier", tier.text()),
    )
}
