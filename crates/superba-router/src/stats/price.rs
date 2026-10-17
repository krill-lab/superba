//! Cost estimates in micro-dollars (spike RS 2), with the formula shared with
//! the switch service's statistics ([`superba_models::price`]):
//!
//! ```text
//! fresh = input - cached - cache_write          (usage normalised first)
//! cost  = (fresh*P_in + cached*P_cached + cache_write*P_write + output*P_out) / 1e6
//!         * tier multiplier(response service_tier) * member ratio
//! ```
//!
//! Prices are USD per 1M tokens (OpenAI list prices from the feed, or the
//! member's per-model override); tiers, long-context bands and the cached /
//! cache-write defaults are resolved by the shared formula. An unknown model has
//! no price, so no cost — never 0. Here prices are keyed by router member id
//! (the profile id made a router id).

use superba_core::{Multiplier, PriceEntry, Usd};
use superba_models::price::{PriceBasis, UsageTokens};

use super::PriceConfig;
use super::usage::Usage;

/// The prices of the first of `models` (requested name, then the name sent
/// upstream, then `response.model`) that `member` overrides or the table lists.
/// A tier-suffix name (`<slug>-fast`, `<slug>-ultrafast`) falls back to `<slug>`.
pub fn lookup<'a>(
    prices: &'a PriceConfig,
    member: &str,
    models: &[&str],
) -> Option<&'a PriceEntry> {
    quote(prices, member, models).map(|(e, _)| e)
}

/// [`lookup`] and whether the price is the member's override (`true`) rather
/// than the table's list price.
pub fn quote<'a>(
    prices: &'a PriceConfig,
    member: &str,
    models: &[&str],
) -> Option<(&'a PriceEntry, bool)> {
    let overrides = prices.overrides.get(member);
    let find = |name: &str| {
        overrides
            .and_then(|o| o.get(name))
            .map(|e| (e, true))
            .or_else(|| prices.table.get(name).map(|e| (e, false)))
    };
    models.iter().find_map(|m| find(m)).or_else(|| {
        models.iter().find_map(|m| {
            let (base, _) = superba_models::split_tier_suffix(m)?;
            find(base)
        })
    })
}

/// The ratio of `member` (1 when not set).
pub fn ratio(prices: &PriceConfig, member: &str) -> Multiplier {
    prices
        .ratio_by_member
        .get(member)
        .copied()
        .unwrap_or(Multiplier::ONE)
}

/// Cost of `usage` at `entry`'s prices, the response's service tier and `ratio`
/// (rounded to the micro-dollar; `None` only if the shared formula refuses).
pub fn cost(
    entry: &PriceEntry,
    usage: &Usage,
    service_tier: Option<&str>,
    ratio: Multiplier,
) -> Option<Usd> {
    let tokens = UsageTokens {
        input: usage.input,
        cached: usage.cached.unwrap_or(0),
        cache_write: usage.cache_write.unwrap_or(0),
        output: usage.output,
    };
    superba_models::price::cost(entry, &tokens, service_tier, ratio).ok()
}

/// Estimate for `member` and `models`: `(cost, price basis)` with the basis as
/// stored in `RequestRow::price_rev` ([`PriceBasis`]: list price revision and
/// source page, or `override`), or `None` when no price is known.
pub fn estimate(
    prices: &PriceConfig,
    member: &str,
    models: &[&str],
    usage: &Usage,
    service_tier: Option<&str>,
) -> Option<(Usd, Option<String>)> {
    let (entry, is_override) = quote(prices, member, models)?;
    let c = cost(entry, usage, service_tier, ratio(prices, member))?;
    let basis = if is_override {
        Some(PriceBasis::Override)
    } else {
        PriceBasis::of_list(entry, &prices.table.rev)
    };
    Some((c, basis.map(|b| b.encode())))
}
