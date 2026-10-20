//! Price lookups and cost estimates shared by the router (per request and the
//! stats line) and the switch service (statistics, Models page): micro-dollar
//! integer math, unknown price = no cost (never zero).
//!
//! ```text
//! fresh = input - cached - cache_write      (additive usage normalised first)
//! rates = the entry's prices for the response's service tier, prompt size
//!         and request time
//! cost  = (fresh*P_in + cached*P_cached + (cache_write-1h)*P_write
//!          + 1h*P_write_1h + output*P_out) / 1e6 * profile ratio
//! ```
//!
//! Which rates apply, first match wins: a service tier with absolute prices
//! (its own long-context bands, else the Standard band), the highest
//! long-context band the prompt is above (`input > above_input_tokens`), a
//! daily time window containing the request's start
//! ([`superba_core::PriceWindow`], in the provider's time zone, possibly
//! across midnight), then the base prices.
//!
//! Prices are USD per 1M tokens ([`superba_core::PriceEntry`]): OpenAI's list
//! prices from the built-in feed, unless the user set a price for the model on
//! the Models page (that override wins). Every cost is an estimate at those
//! prices times the provider's ratio (default 1x).
//!
//! The tier is the response's `service_tier`. None, `default`, `auto` and
//! `standard` are Standard; `fast` is `priority` (OpenAI renamed Priority
//! processing to Fast mode and reports `fast` for newer models). A tier with
//! absolute prices ([`superba_core::PriceEntry::tier_prices`]) is priced from
//! them, long-context bands included; a tier without long-context bands has no
//! long-context price, so a prompt in one of the entry's Standard bands is
//! priced at that band (OpenAI serves such prompts at Standard). Any other tier
//! costs Standard times its multiplier ([`superba_core::PriceEntry::tiers`],
//! 1x when missing). Cached input defaults to the input price, and so do cache
//! writes (OpenAI publishes a separate cache-write price only for GPT-5.6 and
//! later models).

use superba_core::{
    LongContextPrice, Multiplier, PriceEntry, PriceSettings, PriceTable, ProfileId, Result, Usd,
};

/// Token counts of one request (OpenAI subset semantics: `input` includes cached
/// and cache-write tokens).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTokens {
    /// `input_tokens`.
    pub input: u64,
    /// Cached tokens.
    pub cached: u64,
    /// Cache-write tokens (all lifetimes).
    pub cache_write: u64,
    /// Of `cache_write`, one-hour writes (Anthropic `ephemeral_1h_input_tokens`).
    pub cache_write_1h: u64,
    /// `output_tokens`.
    pub output: u64,
    /// When the request started (Unix ms), for time-window prices; `None`
    /// ignores the windows.
    pub at_ms: Option<i64>,
}

impl UsageTokens {
    /// From additive usage (Anthropic style: `input_tokens` excludes cache reads
    /// and cache writes, which are reported separately).
    pub fn from_additive(input: u64, cache_read: u64, cache_write: u64, output: u64) -> Self {
        Self {
            input: input.saturating_add(cache_read).saturating_add(cache_write),
            cached: cache_read,
            cache_write,
            output,
            ..Self::default()
        }
    }

    /// Input tokens that were neither cache reads nor cache writes.
    pub fn fresh(&self) -> u64 {
        self.input
            .saturating_sub(self.cached)
            .saturating_sub(self.cache_write)
    }
}

/// The prices of `model` for a profile: its absolute override, else the table's
/// entry; `None` when neither has one. A tier-suffix name
/// (`gpt-6-astra-ultrafast`) falls back to its base model's prices.
pub fn lookup(
    table: &PriceTable,
    settings: &PriceSettings,
    profile: &ProfileId,
    model: &str,
) -> Result<Option<PriceEntry>> {
    let overrides = settings.overrides.get(profile);
    let find = |slug: &str| {
        overrides
            .and_then(|o| o.get(slug))
            .or_else(|| table.get(slug))
            .cloned()
    };
    Ok(find(model).or_else(|| crate::split_tier_suffix(model).and_then(|(base, _)| find(base))))
}

/// The profile's price ratio (1x when unset).
pub fn ratio_of(settings: &PriceSettings, profile: &ProfileId) -> Multiplier {
    settings
        .ratio_by_profile
        .get(profile)
        .copied()
        .unwrap_or(Multiplier::ONE)
}

/// The tier a response's `service_tier` is priced as: `None` for Standard
/// (none, empty, `default`, `auto`, `standard`), `priority` for `fast` and
/// `priority`, else the trimmed value.
pub fn canonical_tier(service_tier: Option<&str>) -> Option<&str> {
    match service_tier.map(str::trim) {
        None | Some("" | "default" | "auto" | "standard") => None,
        Some("fast" | "priority") => Some("priority"),
        Some(t) => Some(t),
    }
}

/// The multiplier for the response's service tier on Standard prices (only
/// used for tiers without absolute prices; 1x when the entry has none).
pub fn tier_multiplier(entry: &PriceEntry, service_tier: Option<&str>) -> Multiplier {
    match canonical_tier(service_tier) {
        None => Multiplier::ONE,
        Some("priority") => entry
            .tiers
            .get("priority")
            .or_else(|| entry.tiers.get("fast"))
            .copied()
            .unwrap_or(Multiplier::ONE),
        Some(t) => entry.tiers.get(t).copied().unwrap_or(Multiplier::ONE),
    }
}

/// The four per-1M-token prices one request is billed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rates {
    /// Uncached input.
    pub input: Usd,
    /// Cached input.
    pub cached_input: Usd,
    /// Cache writes (five-minute or the only lifetime).
    pub cache_write: Usd,
    /// One-hour cache writes.
    pub cache_write_1h: Usd,
    /// Output.
    pub output: Usd,
}

impl Rates {
    fn new(input: Usd, cached: Option<Usd>, write: Option<Usd>, output: Usd) -> Self {
        let cache_write = write.unwrap_or(input);
        Rates {
            input,
            cached_input: cached.unwrap_or(input),
            cache_write,
            cache_write_1h: cache_write,
            output,
        }
    }

    fn with_1h(mut self, write_1h: Option<Usd>) -> Self {
        if let Some(w) = write_1h {
            self.cache_write_1h = w;
        }
        self
    }

    fn band(b: &LongContextPrice) -> Self {
        Self::new(b.input, b.cached_input, b.cache_write, b.output).with_1h(b.cache_write_1h)
    }
}

/// The highest band whose threshold `input_tokens` is above.
fn band_for(bands: &[LongContextPrice], input_tokens: u64) -> Option<&LongContextPrice> {
    bands
        .iter()
        .rev()
        .find(|t| input_tokens > t.above_input_tokens)
}

/// The rates of `entry` for a request with `input_tokens` answered at
/// `service_tier`, and the multiplier still to apply (1x unless the tier has no
/// absolute prices).
pub fn rates(
    entry: &PriceEntry,
    input_tokens: u64,
    service_tier: Option<&str>,
) -> (Rates, Multiplier) {
    rates_at(entry, input_tokens, service_tier, None)
}

/// [`rates`] for a request that started at `at_ms` (Unix ms): a time window
/// containing it replaces the base prices (not a long-context band).
pub fn rates_at(
    entry: &PriceEntry,
    input_tokens: u64,
    service_tier: Option<&str>,
    at_ms: Option<i64>,
) -> (Rates, Multiplier) {
    let standard_band = band_for(&entry.long_context, input_tokens);
    if let Some(tier) = canonical_tier(service_tier).and_then(|t| entry.tier_prices.get(t)) {
        let rates = match (band_for(&tier.long_context, input_tokens), standard_band) {
            (Some(b), _) => Rates::band(b),
            (None, Some(b)) if tier.long_context.is_empty() => Rates::band(b),
            _ => Rates::new(tier.input, tier.cached_input, tier.cache_write, tier.output),
        };
        return (rates, Multiplier::ONE);
    }
    let window = at_ms.and_then(|at| entry.windows.iter().find(|w| w.contains(at)));
    let rates = match (standard_band, window) {
        (Some(b), _) => Rates::band(b),
        (None, Some(w)) => Rates::new(w.input, w.cached_input, w.cache_write, w.output),
        (None, None) => Rates::new(
            entry.input,
            entry.cached_input,
            entry.cache_write,
            entry.output,
        )
        .with_1h(entry.cache_write_1h),
    };
    (rates, tier_multiplier(entry, service_tier))
}

/// Cost of one request with `entry`, the response's service tier and the
/// profile's ratio, rounded to the micro-dollar.
pub fn cost(
    entry: &PriceEntry,
    usage: &UsageTokens,
    service_tier: Option<&str>,
    ratio: Multiplier,
) -> Result<Usd> {
    let (r, multiplier) = rates_at(entry, usage.input, service_tier, usage.at_ms);
    let part = |tokens: u64, price: Usd| i128::from(tokens) * i128::from(price.micros());
    let one_hour = usage.cache_write_1h.min(usage.cache_write);
    let total = part(usage.fresh(), r.input)
        + part(usage.cached, r.cached_input)
        + part(usage.cache_write - one_hour, r.cache_write)
        + part(one_hour, r.cache_write_1h)
        + part(usage.output, r.output);
    let micros = (total + 500_000).div_euclid(1_000_000);
    let base = Usd::from_micros(micros.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64);
    Ok(ratio.apply(multiplier.apply(base)))
}

/// What a recorded cost was computed with, stored as text in
/// `RequestRow::price_rev` (no schema change): `"<rev>"`, `"<rev> <source>"`
/// or `"override"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PriceBasis {
    /// A list price from the price table (OpenAI's official prices in the
    /// built-in feed): its revision (date) and source page.
    List {
        /// Revision (the entry's `rev`, else the table's).
        rev: String,
        /// Public price page.
        source: Option<String>,
    },
    /// A price the user set for the model on the Models page.
    Override,
}

impl PriceBasis {
    /// The stored text of [`PriceBasis::Override`].
    pub const OVERRIDE: &'static str = "override";

    /// The basis of a table entry: its own `rev`, else the table's; `None` when
    /// neither the entry nor the table has a revision or a source.
    pub fn of_list(entry: &PriceEntry, table_rev: &str) -> Option<Self> {
        let rev = entry
            .rev
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(table_rev.trim());
        let source = entry
            .source
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
            .map(str::to_owned);
        (!rev.is_empty() || source.is_some()).then(|| PriceBasis::List {
            rev: rev.to_owned(),
            source,
        })
    }

    /// The stored text.
    pub fn encode(&self) -> String {
        match self {
            PriceBasis::Override => Self::OVERRIDE.to_owned(),
            PriceBasis::List { rev, source: None } => rev.clone(),
            PriceBasis::List {
                rev,
                source: Some(s),
            } => format!("{rev} {s}").trim().to_owned(),
        }
    }

    /// Parse the stored text (a list price without a recorded source is the
    /// revision only); `None` for empty text.
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        if t == Self::OVERRIDE {
            return Some(PriceBasis::Override);
        }
        let (rev, source) = match t.split_once(char::is_whitespace) {
            Some((r, s)) => (r, Some(s.trim())),
            None if t.contains("://") => ("", Some(t)),
            None => (t, None),
        };
        Some(PriceBasis::List {
            rev: rev.to_owned(),
            source: source.filter(|s| !s.is_empty()).map(str::to_owned),
        })
    }

    /// The revision of a list price.
    pub fn rev(&self) -> Option<&str> {
        match self {
            PriceBasis::List { rev, .. } if !rev.is_empty() => Some(rev),
            _ => None,
        }
    }
}

/// A source page for display: `developers.openai.com/api/docs/pricing` (no
/// scheme, query or trailing slash).
pub fn source_label(url: &str) -> String {
    let t = url.trim();
    let t = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
        .unwrap_or(t);
    let t = t.split(['?', '#']).next().unwrap_or(t);
    t.trim_end_matches('/').to_owned()
}
