//! View model of the Models page (UX §3.4, plan §11): the rows a provider's model
//! list gives, the pending edits (defaults, per-model overrides, picker mode,
//! price overrides), their validation and the [`ModelsEdit`] they turn into.
//! Pure data and logic, so the drawing code and the tests need no service.

use std::collections::BTreeMap;

use superba_core::{
    LimitError, LimitField, LimitSource, ModelDefaults, ModelLimits, ModelOverride, ModelPicker,
    ModelSpec, ModelVisibility, ModelsEdit, ModelsView, PriceEntry, PriceTable, SpeedEffect,
    SpeedTier, check_all_models, check_model_override, model_limits,
};

use crate::design::widgets::token_field::{TokenInput, short_tokens};

/// Which value of a row is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Field {
    /// Context window.
    Window,
    /// Auto-compact threshold.
    Compact,
    /// The site's name for the model (custom sites).
    Upstream,
}

/// Row key of the "all models (default)" row in buffers and diffs.
pub const ALL_MODELS: &str = "";

/// What is saved now: the service's view plus the site's model map and the
/// profile's price overrides (neither is part of [`ModelsView`]).
#[derive(Clone, Copy, Debug)]
pub struct Saved<'a> {
    /// The loaded view.
    pub view: &'a ModelsView,
    /// `Site::model_map` of the provider's site (custom sites).
    pub model_map: &'a BTreeMap<String, String>,
    /// Absolute price overrides of the profile by slug.
    pub prices: &'a BTreeMap<String, PriceEntry>,
}

impl Saved<'_> {
    /// The saved override of `slug`, with its upstream name from the model map.
    pub fn override_of(&self, slug: &str) -> ModelOverride {
        let mut o = self.view.overrides.get(slug).cloned().unwrap_or_default();
        o.upstream_name = self.model_map.get(slug).cloned();
        o
    }

    /// The model's spec.
    pub fn spec(&self, slug: &str) -> Option<&ModelSpec> {
        self.view.list.model(slug)
    }
}

/// Edits not saved yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    /// New defaults (model, effort, speed, all-models window and compaction).
    pub defaults: Option<ModelDefaults>,
    /// New override per slug (an empty override removes the saved one).
    pub overrides: BTreeMap<String, ModelOverride>,
    /// New picker mode.
    pub picker: Option<ModelPicker>,
    /// New price override per slug (`None` removes the saved one).
    pub prices: BTreeMap<String, Option<PriceEntry>>,
}

impl Pending {
    /// Defaults after the pending edits.
    pub fn defaults(&self, saved: &Saved<'_>) -> ModelDefaults {
        self.defaults
            .clone()
            .unwrap_or_else(|| saved.view.defaults.clone())
    }

    /// Replace the defaults (dropped again when equal to the saved ones).
    pub fn set_defaults(&mut self, saved: &Saved<'_>, defaults: ModelDefaults) {
        self.defaults = (defaults != saved.view.defaults).then_some(defaults);
    }

    /// The override of `slug` after the pending edits.
    pub fn override_of(&self, saved: &Saved<'_>, slug: &str) -> ModelOverride {
        self.overrides
            .get(slug)
            .cloned()
            .unwrap_or_else(|| saved.override_of(slug))
    }

    /// Replace the override of `slug` (dropped again when equal to the saved one).
    pub fn set_override(&mut self, saved: &Saved<'_>, slug: &str, o: ModelOverride) {
        if o == saved.override_of(slug) {
            self.overrides.remove(slug);
        } else {
            self.overrides.insert(slug.to_owned(), o);
        }
    }

    /// The picker mode after the pending edits.
    pub fn picker(&self, saved: &Saved<'_>) -> ModelPicker {
        self.picker.unwrap_or(saved.view.picker)
    }

    /// Set the picker mode.
    pub fn set_picker(&mut self, saved: &Saved<'_>, picker: ModelPicker) {
        self.picker = (picker != saved.view.picker).then_some(picker);
    }

    /// The price override of `slug` after the pending edits.
    pub fn price_of(&self, saved: &Saved<'_>, slug: &str) -> Option<PriceEntry> {
        match self.prices.get(slug) {
            Some(p) => p.clone(),
            None => saved.prices.get(slug).cloned(),
        }
    }

    /// Set or remove the price override of `slug`.
    pub fn set_price(&mut self, saved: &Saved<'_>, slug: &str, price: Option<PriceEntry>) {
        if price.as_ref() == saved.prices.get(slug) {
            self.prices.remove(slug);
        } else {
            self.prices.insert(slug.to_owned(), price);
        }
    }

    /// Reset a row: its override and its price override (the all-models row:
    /// the global window and compaction).
    pub fn reset(&mut self, saved: &Saved<'_>, slug: &str) {
        if slug == ALL_MODELS {
            let mut d = self.defaults(saved);
            d.context_window = None;
            d.auto_compact = None;
            self.set_defaults(saved, d);
            return;
        }
        // A saved upstream name is cleared with an empty one (see `edit`).
        let o = ModelOverride {
            upstream_name: saved.override_of(slug).upstream_name.map(|_| String::new()),
            ..ModelOverride::default()
        };
        self.set_override(saved, slug, o);
        self.set_price(saved, slug, None);
    }

    /// The row differs from Codex's defaults after the pending edits (row marker).
    pub fn overridden(&self, saved: &Saved<'_>, slug: &str) -> bool {
        if slug == ALL_MODELS {
            let d = self.defaults(saved);
            return d.context_window.is_some() || d.auto_compact.is_some();
        }
        let o = self.override_of(saved, slug);
        let upstream = o.upstream_name.as_deref().is_some_and(|u| !u.is_empty());
        let rest = ModelOverride {
            upstream_name: None,
            ..o
        };
        !rest.is_empty() || upstream || self.price_of(saved, slug).is_some()
    }

    /// The row has a pending edit.
    pub fn touched(&self, slug: &str) -> bool {
        if slug == ALL_MODELS {
            return self.defaults.is_some();
        }
        self.overrides.contains_key(slug) || self.prices.contains_key(slug)
    }

    /// Number of pending changes (each changed default, override, picker and
    /// price counts once).
    pub fn count(&self, saved: &Saved<'_>) -> usize {
        let defaults = self.defaults.as_ref().map_or(0, |d| {
            let s = &saved.view.defaults;
            [
                d.model != s.model,
                d.reasoning_effort != s.reasoning_effort,
                d.speed != s.speed,
                d.context_window != s.context_window,
                d.auto_compact != s.auto_compact,
            ]
            .into_iter()
            .filter(|b| *b)
            .count()
            .max(1)
        });
        defaults + self.overrides.len() + usize::from(self.picker.is_some()) + self.prices.len()
    }

    /// No pending changes.
    pub fn is_empty(&self) -> bool {
        self.defaults.is_none()
            && self.overrides.is_empty()
            && self.picker.is_none()
            && self.prices.is_empty()
    }

    /// Drop edits that a reloaded view already contains.
    pub fn rebase(&mut self, saved: &Saved<'_>) {
        if self.defaults.as_ref() == Some(&saved.view.defaults) {
            self.defaults = None;
        }
        self.overrides
            .retain(|slug, o| *o != saved.override_of(slug));
        if self.picker == Some(saved.view.picker) {
            self.picker = None;
        }
        self.prices
            .retain(|slug, p| p.as_ref() != saved.prices.get(slug));
    }

    /// The service edit for the pending changes (prices excluded: they belong to
    /// the router's stats settings).
    pub fn edit(&self, saved: &Saved<'_>) -> ModelsEdit {
        let mut set = BTreeMap::new();
        for (slug, o) in &self.overrides {
            let before = saved.override_of(slug);
            let mut o = o.clone();
            // Only send an upstream name when it changed; an empty one clears the
            // site's mapping.
            if o.upstream_name == before.upstream_name {
                o.upstream_name = None;
            } else if o.upstream_name.is_none() {
                o.upstream_name = Some(String::new());
            }
            set.insert(slug.clone(), o);
        }
        ModelsEdit {
            profile_id: Some(saved.view.profile_id.clone()),
            defaults: self.defaults.clone(),
            set,
            reset: Vec::new(),
            reset_all: false,
            picker: self.picker,
        }
    }

    /// The diff shown by "View diff".
    pub fn diff(&self, saved: &Saved<'_>) -> Vec<DiffLine> {
        let mut out = Vec::new();
        if let Some(d) = &self.defaults {
            let s = &saved.view.defaults;
            let mut push = |field: DiffField, before: Option<String>, after: Option<String>| {
                if before != after {
                    out.push(DiffLine {
                        slug: ALL_MODELS.to_owned(),
                        field,
                        before,
                        after,
                    });
                }
            };
            push(DiffField::Model, s.model.clone(), d.model.clone());
            push(
                DiffField::Effort,
                s.reasoning_effort.clone(),
                d.reasoning_effort.clone(),
            );
            push(
                DiffField::Speed,
                s.speed.map(|t| t.config_value().to_owned()),
                d.speed.map(|t| t.config_value().to_owned()),
            );
            push(
                DiffField::Window,
                s.context_window.map(short_tokens),
                d.context_window.map(short_tokens),
            );
            push(
                DiffField::Compact,
                s.auto_compact.map(short_tokens),
                d.auto_compact.map(short_tokens),
            );
        }
        for (slug, o) in &self.overrides {
            let b = saved.override_of(slug);
            let pairs = [
                (
                    DiffField::Window,
                    b.context_window.map(short_tokens),
                    o.context_window.map(short_tokens),
                ),
                (
                    DiffField::Compact,
                    b.auto_compact.map(short_tokens),
                    o.auto_compact.map(short_tokens),
                ),
                (
                    DiffField::Upstream,
                    b.upstream_name.clone().filter(|s| !s.is_empty()),
                    o.upstream_name.clone().filter(|s| !s.is_empty()),
                ),
                (
                    DiffField::Name,
                    b.display_name.clone(),
                    o.display_name.clone(),
                ),
                (
                    DiffField::Hidden,
                    b.hidden.then(|| "hide".to_owned()),
                    o.hidden.then(|| "hide".to_owned()),
                ),
            ];
            for (field, before, after) in pairs {
                if before != after {
                    out.push(DiffLine {
                        slug: slug.clone(),
                        field,
                        before,
                        after,
                    });
                }
            }
        }
        if let Some(p) = self.picker {
            out.push(DiffLine {
                slug: ALL_MODELS.to_owned(),
                field: DiffField::Picker,
                before: Some(picker_value(saved.view.picker).to_owned()),
                after: Some(picker_value(p).to_owned()),
            });
        }
        for (slug, p) in &self.prices {
            out.push(DiffLine {
                slug: slug.clone(),
                field: DiffField::Price,
                before: saved.prices.get(slug).map(price_text),
                after: p.as_ref().map(price_text),
            });
        }
        out
    }
}

fn picker_value(p: ModelPicker) -> &'static str {
    match p {
        ModelPicker::Provider => "provider",
        _ => "codex",
    }
}

/// What a diff line changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffField {
    /// Default model.
    Model,
    /// Reasoning effort.
    Effort,
    /// Speed tier.
    Speed,
    /// Context window.
    Window,
    /// Auto-compact threshold.
    Compact,
    /// Upstream name.
    Upstream,
    /// Display name.
    Name,
    /// Hidden in the picker.
    Hidden,
    /// Picker mode.
    Picker,
    /// Price override.
    Price,
}

/// One line of the diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// Model slug ([`ALL_MODELS`] for the defaults).
    pub slug: String,
    /// What changes.
    pub field: DiffField,
    /// Saved value (`None` = default).
    pub before: Option<String>,
    /// New value (`None` = default).
    pub after: Option<String>,
}

/// `1.25 · 0.125 · 10.00` (USD per 1M tokens: input · cached input · output).
pub fn price_text(p: &PriceEntry) -> String {
    let cached = p.cached_input.unwrap_or(p.input);
    format!(
        "{} · {} · {}",
        dollars(p.input),
        dollars(cached),
        dollars(p.output)
    )
}

/// A per-1M price without trailing zeros beyond two decimals (`1.25`, `0.125`, `10.00`).
pub fn dollars(v: superba_core::Usd) -> String {
    let full = v.amount(6);
    let (int, frac) = full.split_once('.').unwrap_or((&full, ""));
    let frac = frac.trim_end_matches('0');
    let frac = if frac.len() < 2 {
        format!("{frac:0<2}")
    } else {
        frac.to_owned()
    };
    format!("{int}.{frac}")
}

/// The price shown for `slug`: the pending/saved override, else the table entry.
/// The flag is true for an override.
pub fn shown_price(
    pending: &Pending,
    saved: &Saved<'_>,
    table: Option<&PriceTable>,
    slug: &str,
) -> Option<(PriceEntry, bool)> {
    if let Some(p) = pending.price_of(saved, slug) {
        return Some((p, true));
    }
    let base = superba_models::split_tier_suffix(slug).map_or(slug, |(b, _)| b);
    table
        .and_then(|t| t.get(slug).or_else(|| t.get(base)))
        .cloned()
        .map(|p| (p, false))
}

/// Hidden from the picker by default (shown with "Show hidden models").
pub fn hidden(spec: &ModelSpec, o: &ModelOverride) -> bool {
    o.hidden
        || !spec.supported_in_api
        || !matches!(
            spec.visibility,
            ModelVisibility::List | ModelVisibility::Other
        )
}

/// The rows of the table: models in list order, hidden ones only with
/// `show_hidden`, filtered by `query` (slug or display name, any word).
pub fn rows<'a>(
    saved: &Saved<'a>,
    pending: &Pending,
    show_hidden: bool,
    query: &str,
) -> Vec<&'a ModelSpec> {
    let q = query.trim().to_lowercase();
    saved
        .view
        .list
        .models
        .iter()
        .filter(|m| show_hidden || !hidden(m, &pending.override_of(saved, &m.slug)))
        .filter(|m| {
            q.split_whitespace().all(|w| {
                m.slug.to_lowercase().contains(w)
                    || m.display_name
                        .as_deref()
                        .is_some_and(|n| n.to_lowercase().contains(w))
            })
        })
        .collect()
}

/// Number of models hidden by default.
pub fn hidden_count(saved: &Saved<'_>, pending: &Pending) -> usize {
    saved
        .view
        .list
        .models
        .iter()
        .filter(|m| hidden(m, &pending.override_of(saved, &m.slug)))
        .count()
}

/// Why a context window cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowError {
    /// Below [`superba_core::MIN_CONTEXT_WINDOW`].
    TooSmall(u64),
    /// Above the model's `max_context_window` (or, without one,
    /// [`superba_core::MAX_CONTEXT_WINDOW`]).
    OverMax(u64),
}

/// Check a context window ([`superba_core::check_window`]): at least
/// [`superba_core::MIN_CONTEXT_WINDOW`], and for one model at most its
/// `max_context_window` (Codex takes a catalog window as given and the upstream
/// would refuse a longer context). The all-models value is capped per model by
/// Codex, so only the sanity bound applies there.
pub fn check_window(value: u64, max: Option<u64>, all_models: bool) -> Result<u64, WindowError> {
    let max = if all_models { None } else { max };
    match superba_core::check_window(value, max) {
        Ok(()) => Ok(value),
        Err(LimitError::WindowTooSmall { min }) => Err(WindowError::TooSmall(min)),
        Err(LimitError::WindowOverMax { max }) => Err(WindowError::OverMax(max)),
        Err(_) => Err(WindowError::OverMax(value)),
    }
}

/// Why a compaction threshold cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactError {
    /// A percentage without a known window.
    NeedsWindow,
}

/// Tokens for a typed compaction threshold (`None` = back to the default).
pub fn compact_tokens(input: TokenInput, window: Option<u64>) -> Result<Option<u64>, CompactError> {
    match input {
        TokenInput::Empty => Ok(None),
        TokenInput::Tokens(n) => Ok(Some(n)),
        TokenInput::Percent(p) => window
            .map(|w| Some(w.saturating_mul(u64::from(p)) / 100))
            .ok_or(CompactError::NeedsWindow),
    }
}

/// Window of `spec` after `o` (override, else the model's own).
pub fn window_of(spec: &ModelSpec, o: &ModelOverride) -> Option<u64> {
    o.context_window.or_else(|| spec.resolved_context_window())
}

/// Largest window an override may set for `spec`.
pub fn max_of(spec: &ModelSpec) -> Option<u64> {
    spec.max_window()
}

/// The compaction threshold Codex uses for `spec` with `o` alone (no
/// all-models values): `(value, clamped)`; `None` when the window is unknown.
pub fn effective_compact(spec: &ModelSpec, o: &ModelOverride) -> Option<(u64, bool)> {
    let l = model_limits(spec, Some(o), &ModelDefaults::default());
    l.window?;
    l.compact.map(|c| (c, l.clamped))
}

/// The window and compaction Codex will use for `spec` after the pending edits:
/// the model's own values (only where they reach Codex, not with a personal
/// access token), else the all-models values, else the listed ones.
pub fn limits(saved: &Saved<'_>, pending: &Pending, spec: &ModelSpec) -> ModelLimits {
    let o = pending.override_of(saved, &spec.slug);
    model_limits(
        spec,
        saved.view.per_model.then_some(&o),
        &pending.defaults(saved),
    )
}

/// [`limits`] of the saved profile (no pending edits).
pub fn saved_limits(saved: &Saved<'_>, spec: &ModelSpec) -> ModelLimits {
    let o = saved.override_of(&spec.slug);
    model_limits(
        spec,
        saved.view.per_model.then_some(&o),
        &saved.view.defaults,
    )
}

/// What Codex uses now for `spec` (the applied provider only).
pub fn applied_limits(saved: &Saved<'_>, spec: &ModelSpec) -> Option<ModelLimits> {
    saved.view.applied.as_ref().map(|a| a.limits(spec))
}

/// A row's own window / compaction values (pending edits included) that do not
/// reach Codex: per-model values with a personal access token.
pub fn unused_custom(saved: &Saved<'_>, pending: &Pending, slug: &str) -> bool {
    if saved.view.per_model || slug == ALL_MODELS {
        return false;
    }
    let o = pending.override_of(saved, slug);
    o.context_window.is_some() || o.auto_compact.is_some()
}

/// Where a row stands between editing and Codex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    /// Nothing to report (the defaults, as Codex uses them).
    Quiet,
    /// The row has edits that are not saved yet.
    Unsaved,
    /// Saved, but Codex still uses other values (the provider is applied; a
    /// switch to it applies them).
    NotApplied,
    /// Saved custom values Codex uses now.
    Applied,
    /// Saved custom values of a provider that is not in use: they apply at the
    /// next switch to it.
    NextSwitch,
    /// Saved per-model values Codex does not use (personal access token).
    NotUsed,
}

/// The state of a row (`ALL_MODELS` = the all-models row); `active` = the
/// view's provider is the applied one.
pub fn row_state(saved: &Saved<'_>, pending: &Pending, slug: &str, active: bool) -> RowState {
    if pending.touched(slug) {
        return RowState::Unsaved;
    }
    let all = slug == ALL_MODELS;
    if !all && unused_custom(saved, pending, slug) {
        return RowState::NotUsed;
    }
    let custom = if all {
        let d = &saved.view.defaults;
        d.context_window.is_some() || d.auto_compact.is_some()
    } else {
        let o = saved.override_of(slug);
        o.context_window.is_some() || o.auto_compact.is_some()
    };
    if !active {
        return if custom {
            RowState::NextSwitch
        } else {
            RowState::Quiet
        };
    }
    if saved.view.applied.is_none() {
        return RowState::Quiet;
    }
    let differs = |m: &ModelSpec| {
        applied_limits(saved, m).is_some_and(|a| a.pair() != saved_limits(saved, m).pair())
    };
    let pending_apply = if all {
        // The models without values of their own follow the all-models row (or
        // its absence).
        saved.view.list.models.iter().any(|m| {
            let l = saved_limits(saved, m);
            l.window_source != LimitSource::Custom
                && l.compact_source != LimitSource::Custom
                && differs(m)
        })
    } else {
        saved.spec(slug).is_some_and(differs)
    };
    match (pending_apply, custom) {
        (true, _) => RowState::NotApplied,
        (false, true) => RowState::Applied,
        (false, false) => RowState::Quiet,
    }
}

/// Saved rows Codex does not use yet on the applied provider.
pub fn not_applied_count(saved: &Saved<'_>, pending: &Pending) -> usize {
    std::iter::once(ALL_MODELS)
        .chain(saved.view.list.models.iter().map(|m| m.slug.as_str()))
        .filter(|slug| row_state(saved, &Pending::default(), slug, true) == RowState::NotApplied)
        .filter(|slug| !pending.touched(slug))
        .count()
}

/// Window / compaction values that cannot be saved together, by row and field:
/// the all-models values, and every model's own values against the window it
/// ends up with (a lowered window can make a saved threshold invalid).
pub fn limit_errors(saved: &Saved<'_>, pending: &Pending) -> Vec<(String, Field, LimitError)> {
    let field = |e: &LimitError| match e.field() {
        LimitField::Window => Field::Window,
        _ => Field::Compact,
    };
    let mut out = Vec::new();
    let d = pending.defaults(saved);
    if let Err(e) = check_all_models(&d) {
        out.push((ALL_MODELS.to_owned(), field(&e), e));
    }
    if !saved.view.per_model {
        return out;
    }
    for m in &saved.view.list.models {
        let o = pending.override_of(saved, &m.slug);
        if o.context_window.is_none() && o.auto_compact.is_none() {
            continue;
        }
        if let Err(e) = check_model_override(Some(m), &o, &d) {
            out.push((m.slug.clone(), field(&e), e));
        }
    }
    out
}

/// The edits change what Codex reads (so the applied provider needs a
/// restart): defaults, the picker mode, or per-model values where they reach
/// Codex (upstream names change the site's model map, which the switch writes).
pub fn affects_codex(edit: &ModelsEdit, per_model: bool) -> bool {
    edit.defaults.is_some()
        || edit.picker.is_some()
        || edit.reset_all
        || edit.set.values().any(|o| {
            o.upstream_name.is_some()
                || (per_model
                    && (o.context_window.is_some()
                        || o.auto_compact.is_some()
                        || o.display_name.is_some()
                        || o.hidden))
        })
        || (per_model && (!edit.reset.is_empty() || edit.set.values().any(ModelOverride::is_empty)))
}

/// Text of a token count in a field buffer (`400k` style stays readable).
pub fn buffer_text(n: Option<u64>) -> String {
    n.map(short_tokens).unwrap_or_default()
}

/// The tiers the model offers (read-only column).
pub fn tiers(spec: &ModelSpec) -> Vec<SpeedTier> {
    spec.speed_tiers()
}

/// What `tier` does in terminal Codex and in the desktop app with the pending
/// `defaults` (review ux-visual #0): the service's effect of that tier (not the
/// saved tier's), with a tier model name renamed to the pending default model
/// when that changed.
pub fn speed_effect(
    view: &ModelsView,
    defaults: &ModelDefaults,
    tier: SpeedTier,
) -> (SpeedEffect, SpeedEffect) {
    let (cli, app) = view.speed.effect(tier);
    let rename = |e: &SpeedEffect| match (e, &defaults.model, tier.model_suffix()) {
        (SpeedEffect::ViaModelName { .. }, Some(m), Some(suffix))
            if defaults.model != view.defaults.model =>
        {
            SpeedEffect::ViaModelName {
                model: format!("{m}{suffix}"),
            }
        }
        _ => e.clone(),
    };
    (rename(cli), rename(app))
}
