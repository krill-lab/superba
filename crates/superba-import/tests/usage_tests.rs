//! Usage history of cc-switch and AI Toolbox (GitHub issue 1): both tools'
//! request and daily-total tables at their current and an old schema, Codex
//! rows only, token semantics normalised, recorded costs kept (a zero cost of
//! a row with tokens is "not recorded"), rows read from Codex's own session
//! files marked, provider names from the tools' provider tables, and nothing in
//! the tools' directories changes.

mod support;

use superba_core::{EnvSource, ImportSource, StoreStatus, Usd};
use superba_import::usage::{ForeignRequest, UsageRead, read_usage, total_input};
use superba_import::{Os, ScanEnv};
use support::{Home, fixture, listing};

const CCS_DB: &str = ".cc-switch/cc-switch.db";
const ATB_DB: &str = ".local/share/com.ai-toolbox/ai-toolbox.db";
const ON_DISK: &str = "019a7c00-0000-7000-8000-0000000d15c0";
const GONE: &str = "019a7c00-0000-7000-8000-00000000901e";

fn read(h: &Home, source: ImportSource) -> UsageRead {
    let home = h.home();
    let scratch = h.scratch();
    let env: &dyn EnvSource = h.env.as_ref();
    let scan = ScanEnv {
        home: &home,
        env,
        os: Os::Linux,
        scratch: &scratch,
    };
    read_usage(source, &scan)
}

fn by_id<'a>(r: &'a UsageRead, id: &str) -> &'a ForeignRequest {
    r.requests
        .iter()
        .find(|x| x.source_id == id)
        .unwrap_or_else(|| {
            panic!(
                "{id} not in {:?}",
                r.requests.iter().map(|x| &x.source_id).collect::<Vec<_>>()
            )
        })
}

#[test]
fn nothing_installed_is_a_missing_store() {
    let h = Home::new();
    for source in ImportSource::ALL {
        let r = read(&h, source);
        assert_eq!(r.stores.len(), 1);
        assert_eq!(r.stores[0].status, StoreStatus::Missing);
        assert!(r.requests.is_empty() && r.daily.is_empty());
    }
}

#[test]
fn cc_switch_v20_reads_codex_requests_and_days_with_normalised_tokens() {
    let h = Home::new();
    let sql = fixture("ccs/schema-v20.sql") + &fixture("usage/ccs-v20.sql");
    h.db(CCS_DB, &sql, false);
    let before = listing(&h.path(".cc-switch"));
    let r = read(&h, ImportSource::Ccs);
    assert_eq!(listing(&h.path(".cc-switch")), before, "read-only");
    assert_eq!(r.stores[0].status, StoreStatus::Read);
    assert_eq!(r.stores[0].format, "CC Switch database, schema 20");
    // Claude Code's request and day are counted, not returned.
    assert_eq!(r.other_apps, 2);
    assert_eq!(r.requests.len(), 5);

    // Semantics 1: input already includes cache reads.
    let total = by_id(&r, "req-total");
    assert_eq!(total.provider_id, "3f6a9b2c-2222-4d1e-8f00-000000000002");
    assert_eq!(r.provider_name(&total.provider_id), "PackyCode");
    assert_eq!(total.input, 12_000);
    assert_eq!(total.cached, 9_000);
    assert_eq!(total.output, 800);
    assert_eq!(total.cost, Some(Usd::from_micros(31_250)));
    assert_eq!(total.ended_ms, 1_791_619_200_000);
    assert_eq!(total.started_ms, 1_791_619_196_000);
    assert_eq!(total.first_token_ms, Some(900));
    assert_eq!(total.status, Some(200));
    assert!(!total.from_sessions);
    assert_eq!(
        total.thread_id.as_deref(),
        Some("019a7c00-0000-7000-8000-00000000aaa1")
    );
    // Semantics 2: fresh input, cache reads and writes added; a zero cost
    // with tokens is "not recorded".
    let fresh = by_id(&r, "req-fresh");
    assert_eq!(fresh.input, 3_000 + 1_000 + 500);
    assert_eq!(fresh.cache_write, 500);
    assert_eq!(fresh.cost, None);
    assert_eq!(r.provider_name(&fresh.provider_id), "My NewAPI");
    // A failed request keeps its status, never its error text.
    let failed = by_id(&r, "req-failed");
    assert_eq!(failed.status, Some(502));
    assert_eq!(
        failed.cost,
        Some(Usd::ZERO),
        "zero without tokens is a recorded zero"
    );
    // Rows cc-switch read from Codex's session files.
    for t in [ON_DISK, GONE] {
        let s = by_id(&r, &format!("codex_session:thread-v1:{t}:0"));
        assert!(s.from_sessions);
        assert_eq!(s.thread_id.as_deref(), Some(t));
        assert_eq!(r.provider_name(&s.provider_id), "Codex sessions");
    }

    assert_eq!(r.daily.len(), 2);
    let proxy_day = r.daily.iter().find(|d| !d.from_sessions).unwrap();
    assert_eq!(proxy_day.day, "2026-08-01");
    assert_eq!((proxy_day.requests, proxy_day.ok), (40, 39));
    assert_eq!(proxy_day.input, 400_000);
    assert_eq!(proxy_day.cost, Some(Usd::from_micros(1_250_000)));
    assert!(
        !proxy_day.variant.is_empty(),
        "the request / pricing model are part of its key"
    );
    let session_day = r.daily.iter().find(|d| d.from_sessions).unwrap();
    assert_eq!(session_day.requests, 12);
}

#[test]
fn an_old_cc_switch_table_without_the_later_columns_reads_alike() {
    let h = Home::new();
    let sql = fixture("ccs/schema-v1.sql") + &fixture("usage/ccs-v8.sql");
    h.db(CCS_DB, &sql, false);
    let r = read(&h, ImportSource::Ccs);
    assert_eq!(r.stores[0].status, StoreStatus::Read);
    assert_eq!(r.other_apps, 1);
    assert_eq!(r.requests.len(), 1);
    let old = &r.requests[0];
    // Legacy semantics: input includes cache reads.
    assert_eq!(old.input, 2_000);
    assert_eq!(old.cached, 1_500);
    assert_eq!(old.cost, Some(Usd::from_micros(2_100)));
    // No duration column: the latency stands in.
    assert_eq!(old.duration_ms, Some(3_000));
    assert!(!old.from_sessions);
    assert!(r.daily.is_empty());
}

#[test]
fn ai_toolbox_v29_reads_codex_requests_with_fresh_input_and_skips_non_requests() {
    let h = Home::new();
    let sql = fixture("ai-toolbox/schema-v29.sql") + &fixture("usage/ai-toolbox-v29.sql");
    h.db(ATB_DB, &sql, true);
    let dir = h.path(".local/share/com.ai-toolbox");
    let before = listing(&dir);
    let r = read(&h, ImportSource::AiToolbox);
    assert_eq!(listing(&dir), before, "read-only");
    assert_eq!(r.stores[0].status, StoreStatus::Read);
    assert_eq!(r.other_apps, 1);
    // gw-models (a model list, usage_request_count 0) is no request.
    let ids: Vec<&str> = r.requests.iter().map(|x| x.source_id.as_str()).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    let gw = by_id(&r, "gw-1");
    assert_eq!(r.provider_name(&gw.provider_id), "Example Relay");
    assert_eq!(gw.input, 3_000 + 9_000);
    assert_eq!(gw.cached, 9_000);
    assert_eq!(gw.cost, Some(Usd::from_micros(28_100)));
    assert_eq!(gw.duration_ms, Some(5_200));
    assert_eq!(gw.started_ms, 1_791_619_200_000 - 5_200);
    assert_eq!(gw.outcome.as_deref(), Some("completed"));
    assert_eq!(gw.service_tier.as_deref(), Some("priority"));
    let failed = by_id(&r, "gw-failed");
    assert_eq!(failed.outcome.as_deref(), Some("failed"));
    let s = by_id(&r, &format!("SESSION:codex:session:{ON_DISK}:1"));
    assert!(s.from_sessions);
    assert_eq!(s.input, 5_000);

    assert_eq!(r.daily.len(), 3);
    let projects: Vec<&str> = r.daily.iter().map(|d| d.variant.as_str()).collect();
    assert!(
        projects.contains(&"/work/a") && projects.contains(&"/work/b"),
        "{projects:?}"
    );
    let b = r.daily.iter().find(|d| d.variant == "/work/b").unwrap();
    assert_eq!(b.cost, None, "zero with tokens");
    assert_eq!(b.input, 10_000 + 40_000);
    assert!(
        r.daily
            .iter()
            .any(|d| d.from_sessions && d.provider_id == "session")
    );
}

#[test]
fn the_first_ai_toolbox_usage_tables_read_alike() {
    let h = Home::new();
    let sql = fixture("ai-toolbox/schema-v04.sql") + &fixture("usage/ai-toolbox-v02.sql");
    h.db(ATB_DB, &sql, true);
    let r = read(&h, ImportSource::AiToolbox);
    assert_eq!(r.stores[0].status, StoreStatus::Read);
    assert_eq!(r.requests.len(), 1);
    assert_eq!(r.requests[0].input, 4_000);
    assert_eq!(r.daily.len(), 1);
    assert_eq!(r.daily[0].input, 12_000);
    assert_eq!(r.daily[0].cost, Some(Usd::from_micros(15_000)));
}

#[test]
fn a_database_without_usage_tables_says_so() {
    let h = Home::new();
    h.db(CCS_DB, &fixture("ccs/schema-v20.sql"), false);
    let r = read(&h, ImportSource::Ccs);
    assert_eq!(r.stores[0].status, StoreStatus::Read);
    assert!(r.requests.is_empty());
    assert_eq!(
        r.stores[0].note.as_ref().map(|n| n.key.as_str()),
        Some("import.usage.none")
    );
}

#[test]
fn token_semantics_are_normalised_to_input_including_cache() {
    use ImportSource::*;
    assert_eq!(total_input(Ccs, Some(1), 100, 60, 10), 100);
    assert_eq!(total_input(Ccs, Some(0), 100, 60, 10), 110);
    assert_eq!(total_input(Ccs, None, 100, 60, 10), 110);
    assert_eq!(total_input(Ccs, Some(2), 100, 60, 10), 170);
    assert_eq!(total_input(AiToolbox, None, 100, 60, 10), 170);
    assert_eq!(
        superba_import::usage::parse_cost(Some("0.0042")),
        Some(Usd::from_micros(4_200))
    );
    assert_eq!(
        superba_import::usage::parse_cost(Some("1.5e-3")),
        Some(Usd::from_micros(1_500))
    );
    assert_eq!(superba_import::usage::parse_cost(Some("abc")), None);
}
