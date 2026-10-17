//! Many sessions: migration and rollback stay exact and reasonably fast.

#![allow(missing_docs)]

mod common;

use std::time::Instant;

use common::{Fixture, Thread};
use superba_core::*;

fn build(n: u32) -> Fixture {
    let mut f = Fixture::new();
    for i in 0..n {
        let mut t = Thread::new(i, if i % 5 == 0 { "openai" } else { "krill" });
        if i % 3 == 0 {
            t = t.zst();
        }
        if i % 7 == 0 {
            t = t.archived();
        }
        f.add(t);
    }
    f
}

async fn round_trip(n: u32) {
    let f = build(n);
    let before = f.snapshot();
    let m = f.migrator();
    let started = Instant::now();
    let request = MigrationRequest {
        to_provider: "openai".into(),
        selectors: vec![MigrationSelector::Provider {
            provider: "krill".into(),
        }],
        include_archived: true,
        exclude: vec![],
    };
    let plan = m.plan(&f.home, &request).await.unwrap();
    let expected = (0..n).filter(|i| i % 5 != 0).count() as u32;
    assert_eq!(plan.items.len() as u32, expected);
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    assert_eq!(receipt.rollouts, expected);
    assert_eq!(receipt.threads, expected);
    let report = m.scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["openai"].distinct, n);
    f.assert_projection_consistent();
    m.rollback(&f.home, &plan.id).await.unwrap();
    f.snapshot().assert_same(&before);
    eprintln!(
        "{n} sessions: migrate + rollback in {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn five_hundred_sessions_round_trip() {
    round_trip(500).await;
}

/// Plan M4 acceptance size; run with `cargo test -- --ignored`.
#[tokio::test]
#[ignore = "slow: 10k sessions"]
async fn ten_thousand_sessions_round_trip() {
    round_trip(10_000).await;
}
