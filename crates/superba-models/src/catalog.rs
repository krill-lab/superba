//! [`build_catalog`]: a Codex `model_catalog_json` file for one consumer version.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use superba_core::{
    Error, MIN_CONTEXT_WINDOW, ModelOverride, ModelSpec, Notice, Result, ServiceTierSpec,
    SpeedTier, effective_auto_compact,
};

use crate::bundled::{effective_required, fill_missing, required_fields, slug_of, template_fields};
use crate::{BundledCatalog, CatalogFile, CatalogInput, CatalogSource, content_name, text};

/// Codex's fallback window for unknown models (`model_info_from_slug`).
const FALLBACK_CONTEXT_WINDOW: u64 = 272_000;
/// Codex's default usable share of the window.
const FALLBACK_EFFECTIVE_PERCENT: u64 = 95;
/// Priority of generic entries: after every bundled model.
const GENERIC_PRIORITY_BASE: i64 = 100;

/// One entry of the file being built.
struct Entry {
    slug: String,
    /// The model a tier-suffix entry was derived from.
    base: Option<String>,
    /// Tier of a tier-suffix entry.
    tier: Option<SpeedTier>,
    json: Map<String, Value>,
}

struct Builder<'a> {
    input: &'a CatalogInput<'a>,
    /// Generic base: the template's first listed entry.
    base: &'a Map<String, Value>,
    entries: Vec<Entry>,
    seen: BTreeSet<String>,
    notes: Vec<Notice>,
}

/// Build the catalog file for one consumer version (see the crate docs).
///
/// Errors (nothing is produced): a template that does not fit the consumer (another
/// version's dump, or entries without a field the consumer requires), an empty
/// result, an override window below [`MIN_CONTEXT_WINDOW`] or a zero compaction
/// threshold.
pub fn build_catalog(input: &CatalogInput<'_>) -> Result<CatalogFile> {
    let base = check_template(input)?;
    let mut b = Builder {
        input,
        base,
        entries: Vec::new(),
        seen: BTreeSet::new(),
        notes: Vec::new(),
    };
    b.add_source();
    b.add_suffix_entries();
    b.ensure_configured_model();
    b.speed_notes();
    b.apply_overrides()?;
    b.complete_required()?;
    if b.entries.is_empty() {
        return Err(Error::invalid(
            "the model catalog would be empty (Codex refuses an empty catalog)",
        ));
    }
    let slugs: Vec<String> = b.entries.iter().map(|e| e.slug.clone()).collect();
    let models: Vec<Value> = b
        .entries
        .into_iter()
        .map(|e| Value::Object(e.json))
        .collect();
    let bytes = serde_json::to_vec(&json!({ "models": models }))
        .map_err(|e| Error::other(format!("model catalog: {e}")))?;
    let (sha8, _) = content_name(&bytes)?;
    let mut notes: Vec<Notice> = Vec::new();
    for n in b.notes {
        if !notes.contains(&n) {
            notes.push(n);
        }
    }
    Ok(CatalogFile {
        bytes,
        sha8,
        slugs,
        notes,
    })
}

/// The template must be the consumer's own dump: same version (when known) and
/// every field the consumer requires present in every entry. Returns the generic
/// base entry.
fn check_template<'a>(input: &'a CatalogInput<'a>) -> Result<&'a Map<String, Value>> {
    let t: &BundledCatalog = input.template;
    if let Some(v) = &t.codex_version
        && v != &input.consumer
    {
        return Err(Error::invalid(format!(
            "the template is the built-in catalog of Codex {v}, not of Codex {}",
            input.consumer
        )));
    }
    let fields = template_fields(t);
    let missing: Vec<&str> = required_fields(&input.consumer)
        .into_iter()
        .filter(|f| !fields.iter().any(|x| x == f))
        .collect();
    if !missing.is_empty() {
        return Err(Error::invalid(format!(
            "the template does not fit Codex {}: its entries lack {}",
            input.consumer,
            missing.join(", ")
        )));
    }
    let mut objects = t.entries.iter().filter_map(Value::as_object);
    let listed = t
        .entries
        .iter()
        .filter_map(Value::as_object)
        .find(|o| o.get("visibility").and_then(Value::as_str) == Some("list"));
    listed
        .or_else(|| objects.next())
        .ok_or_else(|| Error::invalid("the built-in catalog of the consumer is empty"))
}

impl<'a> Builder<'a> {
    fn template_entry(&self, slug: &str) -> Option<&'a Map<String, Value>> {
        self.input
            .template
            .entries
            .iter()
            .find(|e| slug_of(e) == Some(slug))
            .and_then(Value::as_object)
    }

    fn feed_spec(&self, slug: &str) -> Option<&'a ModelSpec> {
        self.input.feed.iter().find(|m| m.slug == slug)
    }

    fn push(&mut self, slug: String, json: Map<String, Value>) {
        self.push_entry(Entry {
            slug,
            base: None,
            tier: None,
            json,
        });
    }

    fn push_entry(&mut self, entry: Entry) {
        if self.seen.insert(entry.slug.clone()) {
            self.entries.push(entry);
        } else {
            self.notes.push(text::duplicate(&entry.slug));
        }
    }

    fn next_generic_priority(&self) -> i64 {
        GENERIC_PRIORITY_BASE + i64::try_from(self.entries.len()).unwrap_or(0)
    }

    fn generic(&mut self, spec: &ModelSpec, tiers: &[ServiceTierSpec]) -> Map<String, Value> {
        let priority = spec
            .priority
            .unwrap_or_else(|| self.next_generic_priority());
        if self.input.prompt.is_none() {
            self.notes.push(text::generic_without_prompt(&spec.slug));
        } else {
            self.notes.push(text::generic_template(&spec.slug));
        }
        let legacy = required_fields(&self.input.consumer).contains(&"base_instructions");
        generic_entry(self.base, spec, tiers, self.input.prompt, priority, legacy)
    }

    fn add_source(&mut self) {
        match &self.input.source {
            CatalogSource::Native(specs) => {
                for spec in specs {
                    match spec.raw.as_ref().and_then(Value::as_object) {
                        Some(raw) => self.push(spec.slug.clone(), raw.clone()),
                        None => self.add_listed(spec, &spec.service_tiers, true),
                    }
                }
            }
            CatalogSource::OpenAiList(specs) => {
                for spec in specs {
                    self.add_listed(spec, &spec.service_tiers, true);
                }
            }
            CatalogSource::Feed(specs) => {
                for spec in specs {
                    self.add_listed(spec, &spec.service_tiers, false);
                }
            }
            CatalogSource::Bundled => {
                for e in &self.input.template.entries {
                    if let (Some(slug), Some(o)) = (slug_of(e), e.as_object()) {
                        self.push(slug.to_owned(), o.clone());
                    }
                }
            }
        }
    }

    /// A model the line (or the feed) lists without a Codex-native entry: the
    /// consumer's own entry when it has the slug, else a generic entry with feed
    /// metadata. `line_tiers`: the tiers become exactly the ones the line declared.
    fn add_listed(&mut self, spec: &ModelSpec, tiers: &[ServiceTierSpec], line_tiers: bool) {
        if let Some(t) = self.template_entry(&spec.slug) {
            let mut json = t.clone();
            if line_tiers {
                apply_site_metadata(&mut json, spec);
                set_tiers(&mut json, tiers, spec.default_service_tier.as_deref());
            }
            self.push(spec.slug.clone(), json);
            return;
        }
        let merged = match self.feed_spec(&spec.slug) {
            Some(feed) => merge_metadata(spec, feed),
            None => spec.clone(),
        };
        let json = self.generic(&merged, tiers);
        self.push(spec.slug.clone(), json);
    }

    /// `<slug>-fast` / `<slug>-ultrafast` entries for the profile's tier on a line
    /// that maps tier-suffix model names.
    fn add_suffix_entries(&mut self) {
        let tier = self.input.speed;
        if !self.input.tier_model_suffix || tier == SpeedTier::Standard {
            return;
        }
        let mut out = Vec::new();
        for e in &self.entries {
            if e.tier.is_none() && entry_offers(&e.json, tier) {
                out.push(suffix_entry(e, tier));
            }
        }
        for entry in out {
            if !self.seen.contains(&entry.slug) {
                self.push_entry(entry);
            }
        }
    }

    fn ensure_configured_model(&mut self) {
        let Some(model) = self.input.model.map(str::trim).filter(|m| !m.is_empty()) else {
            return;
        };
        if self.seen.contains(model) {
            return;
        }
        if self.input.tier_model_suffix
            && let Some((base, tier)) = split_tier_suffix(model)
            && let Some(e) = self
                .entries
                .iter()
                .find(|e| e.slug == base && e.tier.is_none())
        {
            if !entry_offers(&e.json, tier) {
                self.notes.push(text::tier_not_offered_by_model(base, tier));
            }
            let entry = suffix_entry(e, tier);
            self.push_entry(entry);
            return;
        }
        self.notes.push(text::model_added(model));
        if let Some(t) = self.template_entry(model) {
            self.push(model.to_owned(), t.clone());
        } else {
            let spec = self
                .feed_spec(model)
                .cloned()
                .unwrap_or_else(|| ModelSpec::new(model));
            let tiers = spec.service_tiers.clone();
            let json = self.generic(&spec, &tiers);
            self.push(model.to_owned(), json);
        }
    }

    fn speed_notes(&mut self) {
        let tier = self.input.speed;
        if tier == SpeedTier::Standard {
            return;
        }
        if !self.entries.iter().any(|e| entry_offers(&e.json, tier)) {
            self.notes.push(text::tier_not_offered(tier));
            return;
        }
        let Some(model) = self.input.model.map(str::trim) else {
            return;
        };
        if let Some(e) = self.entries.iter().find(|e| e.slug == model)
            && e.tier.is_none()
            && !entry_offers(&e.json, tier)
        {
            self.notes
                .push(text::tier_not_offered_by_model(model, tier));
        }
    }

    fn apply_overrides(&mut self) -> Result<()> {
        for (slug, ov) in self.input.overrides {
            if ov.is_empty() {
                continue;
            }
            check_override_values(slug, ov)?;
            let mut found = false;
            let mut notes = Vec::new();
            for e in self
                .entries
                .iter_mut()
                .filter(|e| &e.slug == slug || e.base.as_deref() == Some(slug.as_str()))
            {
                found = true;
                apply_override(e, ov, &mut notes);
            }
            if found {
                for n in notes {
                    if !self.notes.contains(&n) {
                        self.notes.push(n);
                    }
                }
            } else {
                self.notes.push(text::override_unknown_model(slug));
            }
        }
        Ok(())
    }

    /// Every entry gets the fields the consumer requires (from its own entry of
    /// the same slug, else the generic base) and instructions.
    fn complete_required(&mut self) -> Result<()> {
        let required = effective_required(&self.input.consumer, self.input.template);
        let version = self.input.consumer.to_string();
        let base = self.base;
        let template = self.input.template;
        for e in &mut self.entries {
            let source_slug = e.base.clone().unwrap_or_else(|| e.slug.clone());
            let from = template
                .entries
                .iter()
                .find(|t| slug_of(t) == Some(source_slug.as_str()))
                .and_then(Value::as_object)
                .unwrap_or(base);
            let mut filled = fill_missing(&mut e.json, from, &required);
            if !has_instructions(&e.json) {
                filled.extend(fill_missing(
                    &mut e.json,
                    from,
                    &["base_instructions".to_owned(), "model_messages".to_owned()],
                ));
            }
            if let Some(f) = required.iter().find(|f| !e.json.contains_key(*f)) {
                return Err(Error::invalid(format!(
                    "catalog entry {} lacks `{f}`, which Codex {version} requires",
                    e.slug
                )));
            }
            if !filled.is_empty() {
                self.notes
                    .push(text::filled_fields(&e.slug, &version, &filled));
            }
        }
        Ok(())
    }
}

/// Reject override values that can never be valid (the window minimum, a zero
/// compaction threshold).
fn check_override_values(slug: &str, ov: &ModelOverride) -> Result<()> {
    if let Some(w) = ov.context_window
        && w < MIN_CONTEXT_WINDOW
    {
        return Err(Error::invalid(format!(
            "{slug}: a context window of {w} tokens is below the minimum of {MIN_CONTEXT_WINDOW}"
        )));
    }
    if ov.auto_compact == Some(0) {
        return Err(Error::invalid(format!(
            "{slug}: the auto-compact threshold must be above zero"
        )));
    }
    Ok(())
}

fn apply_override(e: &mut Entry, ov: &ModelOverride, notes: &mut Vec<Notice>) {
    let label = e.base.clone().unwrap_or_else(|| e.slug.clone());
    if let Some(w) = ov.context_window {
        let max = get_u64(&e.json, "max_context_window");
        e.json.insert("context_window".into(), json!(w));
        if max.is_none_or(|m| m < w) {
            e.json.insert("max_context_window".into(), json!(w));
            if max.is_some() {
                notes.push(text::max_raised(&label, w));
            }
        }
    }
    if let Some(limit) = ov.auto_compact {
        e.json
            .insert("auto_compact_token_limit".into(), json!(limit));
        let window =
            get_u64(&e.json, "context_window").or_else(|| get_u64(&e.json, "max_context_window"));
        if let Some(w) = window {
            let effective = effective_auto_compact(w, Some(limit));
            if effective < limit {
                notes.push(text::compact_clamped(&label, limit, effective));
            }
        }
    }
    if let Some(name) = ov
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let name = match e.tier {
            Some(t) => format!("{name}{}", suffix_label(t)),
            None => name.to_owned(),
        };
        e.json.insert("display_name".into(), json!(name));
    }
    if ov.hidden {
        e.json.insert("visibility".into(), json!("hide"));
    }
}

fn get_u64(o: &Map<String, Value>, key: &str) -> Option<u64> {
    o.get(key).and_then(Value::as_u64)
}

fn has_instructions(o: &Map<String, Value>) -> bool {
    let legacy = o
        .get("base_instructions")
        .is_some_and(|v| v.as_str().is_some());
    let template = o
        .get("model_messages")
        .and_then(|m| m.get("instructions_template"))
        .is_some_and(|v| v.as_str().is_some());
    legacy || template
}

/// The tiers an entry declares (`service_tiers[].id`), as speed tiers.
fn entry_tiers(o: &Map<String, Value>) -> Vec<SpeedTier> {
    o.get("service_tiers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| t.get("id").and_then(Value::as_str))
                .filter_map(SpeedTier::from_catalog_id)
                .collect()
        })
        .unwrap_or_default()
}

fn entry_offers(o: &Map<String, Value>, tier: SpeedTier) -> bool {
    entry_tiers(o).contains(&tier)
}

fn suffix_label(tier: SpeedTier) -> &'static str {
    match tier {
        SpeedTier::Ultrafast => " Ultrafast",
        _ => " Fast",
    }
}

/// `gpt-6-astra-ultrafast` -> (`gpt-6-astra`, Ultrafast).
pub(crate) fn split_tier_suffix(model: &str) -> Option<(&str, SpeedTier)> {
    for tier in [SpeedTier::Ultrafast, SpeedTier::Fast] {
        if let Some(suffix) = tier.model_suffix()
            && let Some(base) = model.strip_suffix(suffix)
            && !base.is_empty()
        {
            return Some((base, tier));
        }
    }
    None
}

/// A tier-suffix copy of `e`: `<slug>-<tier>`, display name + tier, no tiers of
/// its own (Codex then sends no `service_tier`; the relay maps the name).
fn suffix_entry(e: &Entry, tier: SpeedTier) -> Entry {
    let suffix = tier.model_suffix().unwrap_or_default();
    let slug = format!("{}{suffix}", e.slug);
    let mut json = e.json.clone();
    json.insert("slug".into(), json!(slug));
    let name = json
        .get("display_name")
        .and_then(Value::as_str)
        .unwrap_or(e.slug.as_str())
        .to_owned();
    json.insert(
        "display_name".into(),
        json!(format!("{name}{}", suffix_label(tier))),
    );
    set_tiers(&mut json, &[], None);
    Entry {
        slug,
        base: Some(e.slug.clone()),
        tier: Some(tier),
        json,
    }
}

/// Replace the entry's tiers: `service_tiers` = `tiers`, legacy
/// `additional_speed_tiers` emptied (when present), `default_service_tier` set or
/// removed.
fn set_tiers(o: &mut Map<String, Value>, tiers: &[ServiceTierSpec], default: Option<&str>) {
    let list: Vec<Value> = tiers
        .iter()
        .map(|t| json!({"id": t.id, "name": t.name, "description": t.description}))
        .collect();
    o.insert("service_tiers".into(), Value::Array(list));
    if o.contains_key("additional_speed_tiers") {
        o.insert("additional_speed_tiers".into(), json!([]));
    }
    match default.filter(|d| tiers.iter().any(|t| t.id == *d)) {
        Some(d) => {
            o.insert("default_service_tier".into(), json!(d));
        }
        None => {
            o.shift_remove("default_service_tier");
        }
    }
}

/// A site's own metadata on the consumer's entry: its context window wins (the
/// site serves the model).
fn apply_site_metadata(o: &mut Map<String, Value>, spec: &ModelSpec) {
    if let Some(w) = spec.context_window {
        let max = spec.max_context_window.unwrap_or(w).max(w);
        o.insert("context_window".into(), json!(w));
        o.insert("max_context_window".into(), json!(max));
    }
}

/// Site metadata first, the feed's where the site has none. Tiers stay the
/// site's.
fn merge_metadata(site: &ModelSpec, feed: &ModelSpec) -> ModelSpec {
    let mut m = site.clone();
    m.display_name = site
        .display_name
        .clone()
        .or_else(|| feed.display_name.clone());
    m.description = site
        .description
        .clone()
        .or_else(|| feed.description.clone());
    if site.context_window.is_none() {
        m.context_window = feed.context_window;
        m.max_context_window = site.max_context_window.or(feed.max_context_window);
    }
    m.effective_context_window_percent = site
        .effective_context_window_percent
        .or(feed.effective_context_window_percent);
    m.auto_compact_token_limit = site
        .auto_compact_token_limit
        .or(feed.auto_compact_token_limit);
    if site.reasoning_levels.is_empty() {
        m.reasoning_levels = feed.reasoning_levels.clone();
        m.default_reasoning_level = site
            .default_reasoning_level
            .clone()
            .or_else(|| feed.default_reasoning_level.clone());
    }
    if site.input_modalities.is_empty() {
        m.input_modalities = feed.input_modalities.clone();
    }
    m
}

fn set_if_present(o: &mut Map<String, Value>, key: &str, value: Value) {
    if o.contains_key(key) {
        o.insert(key.to_owned(), value);
    }
}

/// Codex's unknown-model fallback (`model_info_from_slug`) in the shape of the
/// consumer's own entries: a copy of a bundled entry with every request-shaping
/// field reset, the spec's metadata, the version's generic instructions.
fn generic_entry(
    base: &Map<String, Value>,
    spec: &ModelSpec,
    tiers: &[ServiceTierSpec],
    prompt: Option<&str>,
    priority: i64,
    legacy_instructions: bool,
) -> Map<String, Value> {
    let mut o = base.clone();
    o.insert("slug".into(), json!(spec.slug));
    o.insert("display_name".into(), json!(spec.label()));
    let description = spec.description.clone().unwrap_or_default();
    match o.get("description") {
        Some(Value::Null) if description.is_empty() => {}
        _ => {
            o.insert("description".into(), json!(description));
        }
    }
    o.insert("visibility".into(), json!("list"));
    o.insert("supported_in_api".into(), json!(true));
    o.insert("priority".into(), json!(priority));
    let levels: Vec<Value> = spec
        .reasoning_levels
        .iter()
        .map(|l| {
            json!({
                "effort": l.effort,
                "description": l.description.clone().unwrap_or_else(|| l.effort.clone()),
            })
        })
        .collect();
    o.insert("supported_reasoning_levels".into(), Value::Array(levels));
    match &spec.default_reasoning_level {
        Some(d) if !spec.reasoning_levels.is_empty() => {
            o.insert("default_reasoning_level".into(), json!(d));
        }
        _ => {
            o.shift_remove("default_reasoning_level");
        }
    }
    for (key, value) in [
        ("availability_nux", Value::Null),
        ("upgrade", Value::Null),
        ("include_skills_usage_instructions", json!(false)),
        ("include_plugin_usage_instructions", json!(false)),
        ("include_apps_usage_instructions", json!(false)),
        ("default_reasoning_summary", json!("auto")),
        ("support_verbosity", json!(false)),
        ("default_verbosity", Value::Null),
        ("apply_patch_tool_type", Value::Null),
        ("web_search_tool_type", json!("text")),
        (
            "truncation_policy",
            json!({"mode": "bytes", "limit": 10_000}),
        ),
        ("supports_image_detail_original", json!(false)),
        ("experimental_supported_tools", json!([])),
        ("supports_search_tool", json!(false)),
        ("supports_experimental_context", json!(false)),
        ("use_responses_lite", json!(false)),
        ("supports_reasoning_effort_updates", json!(false)),
        ("node_repl_auto_review_required", json!(false)),
        ("node_repl_disabled", json!(false)),
        ("supports_reasoning_summaries", json!(false)),
        ("supports_parallel_tool_calls", json!(false)),
        ("prefer_websockets", json!(false)),
    ] {
        set_if_present(&mut o, key, value);
    }
    for key in [
        "available_access_programs",
        "comp_hash",
        "guardian",
        "auto_review_model_override",
        "model_specialty",
        "tool_mode",
        "multi_agent_version",
        "multi_agent_reasoning_effort",
        "supports_reasoning_summary_parameter",
        "minimal_client_version",
        "available_in_plans",
        "auto_compact_token_limit",
    ] {
        o.shift_remove(key);
    }
    set_tiers(&mut o, tiers, spec.default_service_tier.as_deref());
    let window = spec
        .context_window
        .or(spec.max_context_window)
        .unwrap_or(FALLBACK_CONTEXT_WINDOW);
    let max = spec.max_context_window.unwrap_or(window).max(window);
    o.insert("context_window".into(), json!(window));
    o.insert("max_context_window".into(), json!(max));
    o.insert(
        "effective_context_window_percent".into(),
        json!(
            spec.effective_context_window_percent
                .map_or(FALLBACK_EFFECTIVE_PERCENT, u64::from)
        ),
    );
    if let Some(limit) = spec.auto_compact_token_limit {
        o.insert("auto_compact_token_limit".into(), json!(limit));
    }
    let modalities: Vec<Value> = if spec.input_modalities.is_empty() {
        vec![json!("text"), json!("image")]
    } else {
        spec.input_modalities.iter().map(|m| json!(m)).collect()
    };
    o.insert("input_modalities".into(), Value::Array(modalities));
    if let Some(prompt) = prompt {
        if legacy_instructions {
            // Codex before 0.147 reads `base_instructions`; its fallback has no
            // model messages.
            o.insert("base_instructions".into(), json!(prompt));
            o.shift_remove("model_messages");
        } else {
            if o.contains_key("base_instructions") {
                o.insert("base_instructions".into(), json!(prompt));
            }
            o.insert(
                "model_messages".into(),
                json!({ "instructions_template": prompt }),
            );
        }
    }
    o
}
