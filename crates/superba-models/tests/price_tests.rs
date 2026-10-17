//! Price lookup and cost formula (micro-dollar integer math).

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use serde::Deserialize;
use superba_core::{
    LongContextPrice, Multiplier, PriceEntry, PriceSettings, PriceTable, ProfileId, TierPrice, Usd,
};
use superba_models::price::{
    PriceBasis, UsageTokens, canonical_tier, cost, lookup, rates, ratio_of, source_label,
    tier_multiplier,
};

fn usd(s: &str) -> Usd {
    Usd::parse_dollars(s).unwrap()
}

fn entry() -> PriceEntry {
    PriceEntry {
        input: usd("1.25"),
        cached_input: Some(usd("0.125")),
        cache_write: None,
        output: usd("10"),
        long_context: Vec::new(),
        tiers: BTreeMap::from([
            ("priority".to_owned(), Multiplier::from_f64(2.0).unwrap()),
            ("flex".to_owned(), Multiplier::from_f64(0.5).unwrap()),
        ]),
        tier_prices: BTreeMap::new(),
        source: Some("https://example.com/pricing".into()),
        rev: Some("2026-10-01".into()),
    }
}

#[test]
fn cost_follows_the_formula() {
    // fresh 600k * 1.25 + cached 400k * 0.125 + output 10k * 10
    // = 0.75 + 0.05 + 0.10 = 0.90 USD
    let usage = UsageTokens {
        input: 1_000_000,
        cached: 400_000,
        cache_write: 0,
        output: 10_000,
    };
    assert_eq!(
        cost(&entry(), &usage, None, Multiplier::ONE).unwrap(),
        usd("0.9")
    );
    assert_eq!(
        cost(&entry(), &usage, Some("priority"), Multiplier::ONE).unwrap(),
        usd("1.8")
    );
    assert_eq!(
        cost(&entry(), &usage, Some("flex"), Multiplier::ONE).unwrap(),
        usd("0.45")
    );
    // Standard spellings and tiers without a multiplier cost 1x.
    for t in ["default", "auto", "", "ultrafast"] {
        assert_eq!(
            cost(&entry(), &usage, Some(t), Multiplier::ONE).unwrap(),
            usd("0.9"),
            "{t}"
        );
    }
    let ratio = Multiplier::from_f64(0.15).unwrap();
    assert_eq!(cost(&entry(), &usage, None, ratio).unwrap(), usd("0.135"));
}

#[test]
fn cache_writes_default_to_the_input_price_and_rounding_is_to_the_micro_dollar() {
    let usage = UsageTokens {
        input: 3,
        cached: 0,
        cache_write: 3,
        output: 0,
    };
    // 3 tokens * 1.25 USD / 1M = 3.75 micro-dollars -> 4
    assert_eq!(
        cost(&entry(), &usage, None, Multiplier::ONE).unwrap(),
        Usd::from_micros(4)
    );
    let mut e = entry();
    e.cache_write = Some(usd("2.5"));
    assert_eq!(
        cost(&e, &usage, None, Multiplier::ONE).unwrap(),
        Usd::from_micros(8)
    );
}

#[test]
fn long_context_tiers_apply_above_their_threshold() {
    let mut e = entry();
    e.long_context = vec![LongContextPrice {
        above_input_tokens: 272_000,
        input: usd("2.5"),
        cached_input: None,
        cache_write: None,
        output: usd("15"),
    }];
    let small = UsageTokens {
        input: 272_000,
        output: 0,
        ..Default::default()
    };
    assert_eq!(
        cost(&e, &small, None, Multiplier::ONE).unwrap(),
        usd("0.34")
    );
    let big = UsageTokens {
        input: 400_000,
        cached: 100_000,
        output: 1_000,
        ..Default::default()
    };
    // 300k * 2.5 + 100k * 2.5 (cached defaults to the tier input) + 1k * 15
    assert_eq!(cost(&e, &big, None, Multiplier::ONE).unwrap(), usd("1.015"));
}

#[test]
fn additive_usage_is_normalised() {
    let u = UsageTokens::from_additive(100, 900, 50, 7);
    assert_eq!(u.input, 1_050);
    assert_eq!(u.cached, 900);
    assert_eq!(u.cache_write, 50);
    assert_eq!(u.fresh(), 100);
    // Inconsistent upstream numbers never underflow.
    let odd = UsageTokens {
        input: 10,
        cached: 20,
        cache_write: 5,
        output: 0,
    };
    assert_eq!(odd.fresh(), 0);
}

#[test]
fn lookup_prefers_the_profile_override_then_the_table_then_the_base_model() {
    let table = PriceTable {
        rev: "t1".into(),
        entries: BTreeMap::from([("gpt-6-astra".to_owned(), entry())]),
    };
    let profile: ProfileId = "krill".into();
    let mut settings = PriceSettings::default();
    assert_eq!(
        lookup(&table, &settings, &profile, "gpt-6-astra").unwrap(),
        Some(entry())
    );
    assert_eq!(
        lookup(&table, &settings, &profile, "gpt-6-astra-ultrafast").unwrap(),
        Some(entry())
    );
    assert_eq!(
        lookup(&table, &settings, &profile, "unknown").unwrap(),
        None
    );
    let mut own = entry();
    own.input = usd("1");
    settings
        .overrides
        .entry(profile.clone())
        .or_default()
        .insert("gpt-6-astra".into(), own.clone());
    assert_eq!(
        lookup(&table, &settings, &profile, "gpt-6-astra").unwrap(),
        Some(own)
    );
    assert_eq!(
        lookup(&table, &settings, &"other".into(), "gpt-6-astra").unwrap(),
        Some(entry())
    );
    assert_eq!(ratio_of(&settings, &profile), Multiplier::ONE);
    settings
        .ratio_by_profile
        .insert(profile.clone(), Multiplier::from_f64(0.2).unwrap());
    assert_eq!(ratio_of(&settings, &profile).as_f64(), 0.2);
    assert_eq!(
        tier_multiplier(&entry(), Some("priority")),
        Multiplier::from_f64(2.0).unwrap()
    );
}

// ---------------------------------------------------------------------------
// The built-in table (catalog/feeds/prices.json): OpenAI's official prices.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Feed {
    rev: String,
    entries: BTreeMap<String, PriceEntry>,
}

#[derive(Deserialize)]
struct Models {
    models: Vec<Slug>,
}

#[derive(Deserialize)]
struct Slug {
    slug: String,
}

/// Models in `models.json` OpenAI publishes no API price for (kept unpriced;
/// the UI shows "—" and points to the Models page): Codex's internal reviewer
/// and the deprecated Daybreak aliases, whose model pages carry no price.
const UNPRICED: [&str; 3] = [
    "codex-auto-review",
    "gpt-daybreak-blue-latest",
    "gpt-daybreak-red-latest",
];

fn builtin() -> PriceTable {
    let feed: Feed =
        serde_json::from_str(include_str!("../../../catalog/feeds/prices.json")).unwrap();
    PriceTable {
        rev: feed.rev,
        entries: feed.entries,
    }
}

/// Cost of `usage` for `model` in the built-in table at `tier`, ratio 1.
fn list_cost(model: &str, usage: UsageTokens, tier: Option<&str>) -> Usd {
    let table = builtin();
    let e = lookup(&table, &PriceSettings::default(), &"p".into(), model)
        .unwrap()
        .unwrap_or_else(|| panic!("{model} has no price"));
    cost(&e, &usage, tier, Multiplier::ONE).unwrap()
}

fn fresh(input: u64) -> UsageTokens {
    UsageTokens {
        input,
        ..Default::default()
    }
}

#[test]
fn every_listed_model_is_priced_or_known_to_have_no_official_price() {
    let models: Models =
        serde_json::from_str(include_str!("../../../catalog/feeds/models.json")).unwrap();
    let table = builtin();
    for m in &models.models {
        let priced = table.get(&m.slug).is_some();
        let unpriced = UNPRICED.contains(&m.slug.as_str());
        assert!(
            priced != unpriced,
            "{}: add its official price to catalog/feeds/prices.json, or list it in UNPRICED \
             when OpenAI publishes none",
            m.slug
        );
    }
    for e in table.entries.values() {
        assert_eq!(e.rev.as_deref(), Some(table.rev.as_str()));
        let src = e.source.as_deref().unwrap();
        assert!(
            src.starts_with("https://developers.openai.com/")
                || src.starts_with("https://openai.com/"),
            "{src}"
        );
    }
}

#[test]
fn the_built_in_table_prices_each_tier_from_its_own_official_table() {
    // 100k fresh input tokens (short context: at most 272k) = a tenth of the
    // per-1M price.
    let k = 100_000;
    // gpt-6-astra: Standard $10 / cached $1 / output $50; Fast 2x, Flex 0.5x,
    // Ultrafast 6x (absolute prices from each tier's table).
    assert_eq!(list_cost("gpt-6-astra", fresh(k), None), usd("1"));
    for t in ["default", "auto", "standard"] {
        assert_eq!(list_cost("gpt-6-astra", fresh(k), Some(t)), usd("1"), "{t}");
    }
    // OpenAI reports `fast` for models after GPT-5.6 and `priority` before.
    assert_eq!(list_cost("gpt-6-astra", fresh(k), Some("fast")), usd("2"));
    assert_eq!(
        list_cost("gpt-6-astra", fresh(k), Some("priority")),
        usd("2")
    );
    assert_eq!(list_cost("gpt-6-astra", fresh(k), Some("flex")), usd("0.5"));
    assert_eq!(
        list_cost("gpt-6-astra", fresh(k), Some("ultrafast")),
        usd("6")
    );
    let out = UsageTokens {
        output: 1_000_000,
        ..Default::default()
    };
    assert_eq!(list_cost("gpt-6-astra", out, None), usd("50"));
    assert_eq!(list_cost("gpt-6-astra", out, Some("fast")), usd("100"));
    assert_eq!(list_cost("gpt-6-astra", out, Some("ultrafast")), usd("300"));
    // Fast is not a uniform multiple: gpt-5.5 is $12.50 vs $5 (2.5x), gpt-5.4
    // mini $1.50 vs $0.75 (2x), so tiers are absolute prices.
    assert_eq!(
        list_cost("gpt-5.5", fresh(k), Some("priority")),
        usd("1.25")
    );
    assert_eq!(
        list_cost("gpt-5.4-mini", fresh(k), Some("priority")),
        usd("0.15")
    );
    assert_eq!(list_cost("gpt-5.4", fresh(k), Some("flex")), usd("0.125"));
    // The Codex models.
    assert_eq!(list_cost("gpt-5.3-codex", fresh(k), None), usd("0.175"));
    assert_eq!(
        list_cost("gpt-5.3-codex", fresh(k), Some("priority")),
        usd("0.35")
    );
    assert_eq!(
        list_cost("gpt-5.1-codex", fresh(k), Some("priority")),
        usd("0.25")
    );
    assert_eq!(
        list_cost("gpt-5.1-codex-mini", fresh(k), None),
        usd("0.025")
    );
    assert_eq!(list_cost("codex-mini-latest", out, None), usd("6"));
    // A tier without an official price (no Flex row for gpt-5.1-codex-max)
    // falls back to Standard x the entry's multiplier (none: 1x).
    assert_eq!(
        list_cost("gpt-5.1-codex-max", fresh(k), Some("flex")),
        usd("0.125")
    );
    // A tier-suffix name is priced as its base model.
    assert_eq!(
        list_cost("gpt-6-astra-ultrafast", fresh(k), Some("ultrafast")),
        usd("6")
    );
}

#[test]
fn cached_input_and_cache_writes_are_billed_at_their_own_rates() {
    // gpt-6.1-sol: input $2, cached $0.10 (5%), cache writes $2.50 (1.25x).
    let u = UsageTokens {
        input: 100_000,
        cached: 60_000,
        cache_write: 10_000,
        output: 1_000,
    };
    // 30k*2 + 60k*0.10 + 10k*2.5 + 1k*10 = 0.06 + 0.006 + 0.025 + 0.01
    assert_eq!(list_cost("gpt-6.1-sol", u, None), usd("0.101"));
    // Fast doubles every rate.
    assert_eq!(list_cost("gpt-6.1-sol", u, Some("fast")), usd("0.202"));
    // gpt-5.5 has no cache-write price: writes cost the input rate.
    let w = UsageTokens {
        input: 100_000,
        cache_write: 100_000,
        ..Default::default()
    };
    assert_eq!(list_cost("gpt-5.5", w, None), usd("0.5"));
    let c = UsageTokens {
        input: 100_000,
        cached: 100_000,
        ..Default::default()
    };
    assert_eq!(list_cost("gpt-5.5", c, None), usd("0.05"));
    assert_eq!(list_cost("gpt-5.5", c, Some("priority")), usd("0.125"));
}

#[test]
fn long_prompts_use_the_long_context_band_of_the_tier() {
    // 400k input on gpt-6-astra: long-context $20 Standard, $40 Fast, $120
    // Ultrafast, cached $2 / $4.
    let u = UsageTokens {
        input: 400_000,
        cached: 100_000,
        cache_write: 0,
        output: 0,
    };
    // 300k*20 + 100k*2 = 6.2
    assert_eq!(list_cost("gpt-6-astra", u, None), usd("6.2"));
    assert_eq!(list_cost("gpt-6-astra", u, Some("fast")), usd("12.4"));
    assert_eq!(list_cost("gpt-6-astra", u, Some("ultrafast")), usd("37.2"));
    // Exactly 272k is short context.
    assert_eq!(list_cost("gpt-6-astra", fresh(272_000), None), usd("2.72"));
    // gpt-5.5's Fast table has no long-context price (such prompts are served
    // at Standard): the Standard long-context band applies.
    assert_eq!(
        list_cost("gpt-5.5", fresh(400_000), Some("priority")),
        usd("4")
    );
    assert_eq!(list_cost("gpt-5.5", fresh(400_000), None), usd("4"));
    assert_eq!(list_cost("gpt-5.5", fresh(400_000), Some("flex")), usd("2"));
}

#[test]
fn unpriced_models_have_no_price_and_so_no_cost() {
    let table = builtin();
    for m in UNPRICED.iter().copied().chain(["gpt-7-next", "acme-coder"]) {
        assert_eq!(
            lookup(&table, &PriceSettings::default(), &"p".into(), m).unwrap(),
            None,
            "{m}"
        );
    }
}

#[test]
fn overrides_win_and_the_ratio_scales_every_tier() {
    let table = builtin();
    let profile: ProfileId = "relay".into();
    let mut settings = PriceSettings::default();
    settings
        .ratio_by_profile
        .insert(profile.clone(), Multiplier::from_f64(0.3).unwrap());
    let ratio = ratio_of(&settings, &profile);
    let e = lookup(&table, &settings, &profile, "gpt-6-astra")
        .unwrap()
        .unwrap();
    // 100k tokens: $2 Fast x 0.3; $1 Standard x 0.3.
    assert_eq!(
        cost(&e, &fresh(100_000), Some("fast"), ratio).unwrap(),
        usd("0.6")
    );
    assert_eq!(cost(&e, &fresh(100_000), None, ratio).unwrap(), usd("0.3"));
    // Another profile keeps 1x.
    assert_eq!(ratio_of(&settings, &"other".into()), Multiplier::ONE);
    // A price set on the Models page replaces the list price (tiers included).
    let own = PriceEntry {
        input: usd("1"),
        output: usd("2"),
        ..PriceEntry::default()
    };
    settings
        .overrides
        .entry(profile.clone())
        .or_default()
        .insert("gpt-6-astra".into(), own.clone());
    let e = lookup(&table, &settings, &profile, "gpt-6-astra")
        .unwrap()
        .unwrap();
    assert_eq!(e, own);
    assert_eq!(
        cost(&e, &fresh(100_000), Some("fast"), ratio).unwrap(),
        usd("0.03")
    );
    // An override can price a model the table does not know.
    settings
        .overrides
        .entry(profile.clone())
        .or_default()
        .insert("codex-auto-review".into(), own.clone());
    assert_eq!(
        lookup(&table, &settings, &profile, "codex-auto-review").unwrap(),
        Some(own)
    );
}

#[test]
fn tier_prices_win_over_multipliers_and_fast_is_priority() {
    let mut e = entry();
    assert_eq!(canonical_tier(Some(" fast ")), Some("priority"));
    assert_eq!(canonical_tier(Some("default")), None);
    assert_eq!(canonical_tier(Some("flex")), Some("flex"));
    // Multipliers: `fast` reads the `priority` multiplier.
    assert_eq!(
        tier_multiplier(&e, Some("fast")),
        Multiplier::from_f64(2.0).unwrap()
    );
    e.tier_prices.insert(
        "priority".into(),
        TierPrice {
            input: usd("3"),
            output: usd("30"),
            ..TierPrice::default()
        },
    );
    let (r, m) = rates(&e, 1_000, Some("fast"));
    assert_eq!(
        (r.input, r.cached_input, r.cache_write, r.output),
        (usd("3"), usd("3"), usd("3"), usd("30"))
    );
    assert_eq!(m, Multiplier::ONE);
    // Flex still uses its multiplier on Standard.
    let (r, m) = rates(&e, 1_000, Some("flex"));
    assert_eq!(r.input, usd("1.25"));
    assert_eq!(m, Multiplier::from_f64(0.5).unwrap());
}

#[test]
fn long_context_cache_writes_use_the_band() {
    let mut e = entry();
    e.cache_write = Some(usd("1.5625"));
    e.long_context = vec![LongContextPrice {
        above_input_tokens: 272_000,
        input: usd("2.5"),
        cached_input: Some(usd("0.25")),
        cache_write: Some(usd("3.125")),
        output: usd("15"),
    }];
    let (r, _) = rates(&e, 300_000, None);
    assert_eq!(r.cache_write, usd("3.125"));
    e.long_context[0].cache_write = None;
    let (r, _) = rates(&e, 300_000, None);
    assert_eq!(r.cache_write, usd("2.5"), "defaults to the band's input");
    let (r, _) = rates(&e, 1_000, None);
    assert_eq!(r.cache_write, usd("1.5625"));
}

#[test]
fn the_price_basis_round_trips_through_the_stored_text() {
    let table = builtin();
    let e = table.get("gpt-6-astra").unwrap();
    let b = PriceBasis::of_list(e, &table.rev).unwrap();
    assert_eq!(
        b,
        PriceBasis::List {
            rev: "2026-10-17".into(),
            source: Some("https://developers.openai.com/api/docs/pricing".into()),
        }
    );
    let text = b.encode();
    assert_eq!(
        text,
        "2026-10-17 https://developers.openai.com/api/docs/pricing"
    );
    assert_eq!(PriceBasis::parse(&text), Some(b.clone()));
    assert_eq!(b.rev(), Some("2026-10-17"));
    // Rows written before sources were recorded hold the revision only.
    assert_eq!(
        PriceBasis::parse("2026-10"),
        Some(PriceBasis::List {
            rev: "2026-10".into(),
            source: None
        })
    );
    assert_eq!(PriceBasis::parse("override"), Some(PriceBasis::Override));
    assert_eq!(PriceBasis::Override.encode(), "override");
    assert_eq!(PriceBasis::Override.rev(), None);
    assert_eq!(PriceBasis::parse("  "), None);
    // An entry without its own rev takes the table's; nothing at all = none.
    let bare = PriceEntry::default();
    assert_eq!(
        PriceBasis::of_list(&bare, "t1"),
        Some(PriceBasis::List {
            rev: "t1".into(),
            source: None
        })
    );
    assert_eq!(PriceBasis::of_list(&bare, ""), None);
    assert_eq!(
        source_label("https://developers.openai.com/api/docs/pricing/?x=1"),
        "developers.openai.com/api/docs/pricing"
    );
}
