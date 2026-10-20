//! The configurable failover and recovery rules (user bug 117): the statuses
//! that fail over, returning to the preferred member, the cooldowns and the
//! idle time of a pin, as written by `Tuning::set_failover`.

mod common;

use std::time::Duration;

use common::*;
use superba_core::RouterFailover;
use superba_router::{LineState, MemberKind};

const OK: &str = r#"{"ok":true}"#;
const BODY: &str = r#"{"model":"gpt-x","input":"hello"}"#;

fn two_members(first: &Upstream, second: &Upstream) -> superba_router::RouterConfig {
    test_config(vec![
        member(
            "x",
            MemberKind::Krill,
            false,
            vec![line("x1", first.base("/up"))],
        ),
        member(
            "y",
            MemberKind::Compatible,
            false,
            vec![line("y1", second.base("/v1"))],
        ),
    ])
}

#[test]
fn defaults_are_todays_rules_and_round_trip() {
    let t = superba_router::Tuning::default();
    assert_eq!(t.failover(), RouterFailover::default());
    let mut t = t;
    let f = RouterFailover {
        cooldown_secs: 10,
        max_cooldown_secs: 120,
        failover_statuses: vec![503, 429, 502, 429],
        sticky_idle_minutes: 30,
        return_to_preferred: true,
    };
    t.set_failover(&f).unwrap();
    assert_eq!(t.breaker_base_ms, 10_000);
    assert_eq!(t.breaker_max_ms, 120_000);
    assert_eq!(t.sticky_ttl_secs, 1_800);
    assert_eq!(t.failover_statuses, [429, 502, 503]);
    assert!(t.return_to_preferred);
}

#[test]
fn out_of_bounds_values_are_refused() {
    let mut t = superba_router::Tuning::default();
    for f in [
        RouterFailover {
            cooldown_secs: 0,
            ..RouterFailover::default()
        },
        RouterFailover {
            cooldown_secs: 30,
            max_cooldown_secs: 10,
            ..RouterFailover::default()
        },
        RouterFailover {
            sticky_idle_minutes: 1,
            ..RouterFailover::default()
        },
        RouterFailover {
            failover_statuses: vec![404],
            ..RouterFailover::default()
        },
    ] {
        assert!(t.set_failover(&f).is_err(), "{f:?}");
    }
    assert_eq!(t, superba_router::Tuning::default());
}

#[tokio::test]
async fn a_configured_429_fails_over() {
    let first = upstream(Mode::Json(429, r#"{"error":{"message":"slow down"}}"#)).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&first, &second);
    cfg.tuning
        .set_failover(&RouterFailover {
            failover_statuses: vec![429, 502, 503, 504],
            ..RouterFailover::default()
        })
        .unwrap();
    let r = start(cfg).await;
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(route(&resp), "y/y1");
    assert_eq!(r.status().await.totals.failovers, 1);
}

#[tokio::test]
async fn a_status_left_out_is_passed_back() {
    let first = upstream(Mode::Json(503, "{}")).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&first, &second);
    cfg.tuning
        .set_failover(&RouterFailover {
            failover_statuses: vec![502, 504],
            ..RouterFailover::default()
        })
        .unwrap();
    let r = start(cfg).await;
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(route(&resp), "x/x1");
    assert_eq!(second.hits(), 0);
    assert_eq!(
        r.status().await.members[0].lines[0].state,
        LineState::Healthy
    );
}

#[tokio::test]
async fn return_to_preferred_moves_a_pinned_conversation_back() {
    let x = upstream(Mode::Json(200, OK)).await;
    let y = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&x, &y);
    cfg.tuning
        .set_failover(&RouterFailover {
            return_to_preferred: true,
            ..RouterFailover::default()
        })
        .unwrap();
    let r = start(cfg).await;
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "x/x1");
    x.set(Mode::Json(502, "{}"));
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "y/y1");
    x.set(Mode::Json(200, OK));
    // Off (the default) s1 would stay on y; on, it goes back to x.
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "x/x1");
}

#[tokio::test]
async fn the_configured_cooldown_keeps_a_failed_line_closed() {
    let first = upstream(Mode::Json(502, "{}")).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&first, &second);
    cfg.tuning.breaker_threshold = 1;
    cfg.tuning
        .set_failover(&RouterFailover {
            cooldown_secs: 1,
            max_cooldown_secs: 1,
            ..RouterFailover::default()
        })
        .unwrap();
    let r = start(cfg).await;
    assert_eq!(route(&r.post(Some("a"), BODY).await), "y/y1");
    first.set(Mode::Json(200, OK));
    // Within the 1 s cooldown a new conversation skips x.
    assert_eq!(route(&r.post(Some("b"), BODY).await), "y/y1");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert_eq!(route(&r.post(Some("c"), BODY).await), "x/x1");
}
