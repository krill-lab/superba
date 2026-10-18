//! # superba-models
//!
//! Model catalogs for Codex (v0.3 Models page, speed tiers, router model lists).
//! Pure: no network, no file IO; every function works on the bytes and values it
//! is given. `superba-switch` fetches the inputs (provider list, feeds,
//! `codex debug models --bundled` of the consumer binary), writes the result to
//! `<CODEX_HOME>/superba/models-<sha8>.json` and preflights it with that binary.
//!
//! Rules the implementation follows (spike A6, plan section 11):
//! - a catalog is built for **one consumer Codex version** from that binary's own
//!   bundled dump, so its schema matches; a template that lacks a field the
//!   consumer requires is rejected before anything is written;
//! - Codex-native upstream entries are kept verbatim (unknown fields preserved);
//!   OpenAI-list ids take feed metadata when the slug matches, else a generic
//!   template (Codex's unknown-model fallback, the version's `prompt.md`);
//! - overrides: `context_window`, `max_context_window = max(max, window)`,
//!   `auto_compact_token_limit`, `display_name`, `visibility = "hide"`;
//! - speed tiers: `service_tiers` lists only tiers the line declares; with
//!   [`superba_core::SiteCapabilities::tier_model_suffix`], `<slug>-fast` /
//!   `<slug>-ultrafast` entries with empty `service_tiers`;
//! - the configured `model` is always present; serialization is deterministic
//!   so the content address ([`content_name`]) is stable.
//!
//! The caller writes [`CatalogFile::bytes`] to [`CatalogFile::file_name`] and
//! preflights it with the consumer binary (`codex debug models -c
//! model_catalog_json=...`) before `config.toml` names it; [`build_catalog`]
//! already refuses a template that does not fit the consumer (another version's
//! dump, or entries lacking a field the consumer requires,
//! [`required_fields`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bundled;
mod catalog;
pub mod price;
mod text;

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use superba_core::{
    CodexVersion, Error, ModelList, ModelListSource, ModelOverride, ModelSpec, Notice, Result,
    SpeedTier, check_compact, check_window, effective_auto_compact,
};

pub use bundled::{parse_bundled, required_fields};
pub use catalog::build_catalog;

/// The consumer's own catalog: `codex debug models --bundled` of one binary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BundledCatalog {
    /// Version of the binary, when known.
    pub codex_version: Option<CodexVersion>,
    /// Entries in order, verbatim.
    pub entries: Vec<serde_json::Value>,
    /// Fields every entry of this version carries (detects templates from
    /// another version).
    pub required_fields: Vec<String>,
}

/// Which models the catalog lists and where their metadata comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogSource {
    /// Codex-native `/models` entries ([`ModelSpec::raw`] kept verbatim).
    Native(Vec<ModelSpec>),
    /// OpenAI-list ids with whatever metadata the site sent.
    OpenAiList(Vec<ModelSpec>),
    /// Superba's feed entries.
    Feed(Vec<ModelSpec>),
    /// Only the consumer's bundled entries (overrides and tiers on top).
    Bundled,
}

/// Everything [`build_catalog`] needs.
#[derive(Clone, Debug)]
pub struct CatalogInput<'a> {
    /// Codex version the file is for.
    pub consumer: CodexVersion,
    /// That version's bundled catalog (schema template).
    pub template: &'a BundledCatalog,
    /// Models to list.
    pub source: CatalogSource,
    /// Feed entries used as metadata for OpenAI-list ids with a matching slug.
    pub feed: &'a [ModelSpec],
    /// `prompt.md` of the consumer version (instructions of generic entries).
    pub prompt: Option<&'a str>,
    /// Per-model overrides by slug.
    pub overrides: &'a BTreeMap<String, ModelOverride>,
    /// The profile's speed tier.
    pub speed: SpeedTier,
    /// The site maps tier-suffix model names.
    pub tier_model_suffix: bool,
    /// The configured `model` (always listed).
    pub model: Option<&'a str>,
}

/// A generated catalog file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogFile {
    /// `{"models":[...]}` bytes, deterministic.
    pub bytes: Vec<u8>,
    /// First 8 hex digits of the bytes' sha256.
    pub sha8: String,
    /// Slugs in file order.
    pub slugs: Vec<String>,
    /// Notes (generic template used, override clamped, tier not offered).
    pub notes: Vec<Notice>,
}

impl CatalogFile {
    /// `models-<sha8>.json`.
    pub fn file_name(&self) -> String {
        format!("models-{}.json", self.sha8)
    }
}

/// Result of checking an override against a model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverrideCheck {
    /// Window Codex will use.
    pub effective_window: Option<u64>,
    /// Compaction threshold Codex will use (after the 90 % clamp).
    pub effective_auto_compact: Option<u64>,
    /// The compaction threshold was clamped.
    pub clamped: bool,
    /// Notes for the UI.
    pub notes: Vec<Notice>,
}

/// Check an override against a model with the bounds the Models page and
/// `spb models` share ([`superba_core::check_window`],
/// [`superba_core::check_compact`]): a window of at least
/// [`superba_core::MIN_CONTEXT_WINDOW`] and at most the model's maximum, a
/// compaction threshold of at least [`superba_core::MIN_AUTO_COMPACT`] and below
/// the window, a display name or upstream name non-empty. Reports the 90 %
/// compaction clamp. [`OverrideCheck::effective_window`] is the context window
/// the catalog gives Codex (the usable share is
/// `effective_context_window_percent` of it).
pub fn validate_override(
    spec: &ModelSpec,
    model_override: &ModelOverride,
) -> Result<OverrideCheck> {
    let ov = model_override;
    let invalid =
        |e: superba_core::LimitError| Error::invalid(format!("{}: {}", spec.slug, e.message()));
    if let Some(w) = ov.context_window {
        check_window(w, spec.max_window()).map_err(invalid)?;
    }
    if let Some(c) = ov.auto_compact {
        check_compact(
            c,
            ov.context_window.or_else(|| spec.resolved_context_window()),
        )
        .map_err(invalid)?;
    }
    for (what, name) in [
        ("display name", &ov.display_name),
        ("upstream model name", &ov.upstream_name),
    ] {
        if name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(Error::invalid(format!("the {what} is empty")));
        }
    }
    let window = ov.context_window.or_else(|| spec.resolved_context_window());
    let limit = ov.auto_compact.or(spec.auto_compact_token_limit);
    let mut check = OverrideCheck {
        effective_window: window,
        effective_auto_compact: window.map(|w| effective_auto_compact(w, limit)).or(limit),
        clamped: false,
        notes: Vec::new(),
    };
    if let (Some(w), Some(l)) = (window, ov.auto_compact) {
        let effective = effective_auto_compact(w, Some(l));
        if effective < l {
            check.clamped = true;
            check
                .notes
                .push(text::compact_clamped(&spec.slug, l, effective));
        }
    }
    Ok(check)
}

/// Union of router members' lists: dedupe by slug (the first member that lists a
/// model wins), the member's label in front of the description (also in the
/// Codex-native entry, so the app's picker shows it). The source is
/// [`ModelListSource::Union`] with the member labels in order.
pub fn union(lists: &[(String, ModelList)]) -> Result<ModelList> {
    let mut seen = std::collections::BTreeSet::new();
    let mut models = Vec::new();
    for (member, list) in lists {
        for m in &list.models {
            if !seen.insert(m.slug.clone()) {
                continue;
            }
            let mut m = m.clone();
            let description = match m.description.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => format!("{member} · {d}"),
                None => member.clone(),
            };
            if let Some(serde_json::Value::Object(raw)) = m.raw.as_mut() {
                raw.insert("description".into(), serde_json::json!(description));
            }
            m.description = Some(description);
            models.push(m);
        }
    }
    Ok(ModelList {
        source: ModelListSource::Union {
            members: lists.iter().map(|(m, _)| m.clone()).collect(),
        },
        models,
        fallback_reason: None,
        sources: Vec::new(),
    })
}

/// Content address of catalog bytes: `(sha8, "models-<sha8>.json")`, where `sha8`
/// is the first 8 hex digits of the bytes' SHA-256.
pub fn content_name(bytes: &[u8]) -> Result<(String, String)> {
    let sha8 = hex::encode(Sha256::digest(bytes))[..8].to_owned();
    let name = format!("models-{sha8}.json");
    Ok((sha8, name))
}

/// Speed tiers a model offers, Standard first: the tiers its catalog entry
/// declares. A tier-suffix capability does not add tiers (it changes how the
/// desktop app reaches them: `<slug>-fast` / `<slug>-ultrafast` model names); a
/// tier-suffix model itself (`gpt-6-astra-ultrafast`) offers only Standard
/// unless its entry declares tiers.
pub fn speed_tiers_of(spec: &ModelSpec, tier_model_suffix: bool) -> Result<Vec<SpeedTier>> {
    let _ = tier_model_suffix;
    Ok(spec.speed_tiers())
}

/// `gpt-6-astra-ultrafast` -> `("gpt-6-astra", Ultrafast)`; `None` for a name
/// without a tier suffix.
pub fn split_tier_suffix(model: &str) -> Option<(&str, SpeedTier)> {
    catalog::split_tier_suffix(model)
}
