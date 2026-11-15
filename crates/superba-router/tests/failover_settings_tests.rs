//! The configurable failover and recovery rules (user bug 117): the statuses
//! that fail over, returning to the preferred member, the cooldowns and the
//! idle time of a pin, as written by `Tuning::set_failover`; and the switch
//! that turns failover off (PR #25, 故障转移开关): a request then gets one
//! line, and that line's error goes back to the tool.

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
    assert!(t.failover_enabled, "failover is on by default");
    let f = RouterFailover {
        enabled: false,
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
    assert!(!t.failover_enabled);
    assert!(!t.failover().enabled);
    // A configuration written before the switch existed has failover on.
    let old: superba_router::Tuning =
        serde_json::from_str(r#"{"failover_statuses":[502]}"#).unwrap();
    assert!(old.failover_enabled);
    let old: RouterFailover = serde_json::from_str(r#"{"cooldown_secs":9}"#).unwrap();
    assert!(old.enabled);
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

fn off() -> RouterFailover {
    RouterFailover {
        enabled: false,
        ..RouterFailover::default()
    }
}

#[tokio::test]
async fn with_failover_off_a_failing_line_returns_its_error_and_no_other_line_is_tried() {
    for mode in [
        Mode::Json(502, r#"{"error":"bad gateway"}"#),
        Mode::Json(503, r#"{"error":"unavailable"}"#),
        Mode::JsonHeader(403, r#"{"error":"blocked"}"#, "l-block", "1"),
    ] {
        let bad = upstream(mode).await;
        let other_line = upstream(Mode::Json(200, OK)).await;
        let other_member = upstream(Mode::Json(200, OK)).await;
        let mut cfg = test_config(vec![
            member(
                "x",
                MemberKind::Krill,
                false,
                vec![
                    line("x1", bad.base("/up")),
                    line("x2", other_line.base("/up")),
                ],
            ),
            member(
                "y",
                MemberKind::Compatible,
                false,
                vec![line("y1", other_member.base("/v1"))],
            ),
        ]);
        cfg.tuning.set_failover(&off()).unwrap();
        let r = start(cfg).await;
        let resp = r.post(Some("s"), BODY).await;
        assert_eq!(resp.status().as_u16(), 502);
        let text = resp.text().await.unwrap();
        assert!(text.contains("router_unavailable"), "{text}");
        assert!(text.contains("x/x1"), "names the line: {text}");
        assert_eq!(bad.hits(), 1);
        assert_eq!((other_line.hits(), other_member.hits()), (0, 0));
        let st = r.status().await;
        assert!(!st.failover);
        assert_eq!(st.totals.failovers, 0);
        assert_eq!(st.totals.unavailable, 1);
        let x1 = &st.members[0].lines[0];
        assert_eq!(x1.failovers_away, 0, "nothing moved away from x1");
        assert_eq!(x1.consecutive_failures, 1, "the failure still counts");
        assert!(x1.last_error.is_some());
    }
}

#[tokio::test]
async fn with_failover_off_a_line_that_cannot_be_reached_is_not_left_either() {
    let good = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", dead_base()), line("b", good.base("/up"))],
    )]);
    cfg.tuning.set_failover(&off()).unwrap();
    let r = start(cfg).await;
    let resp = r.post(Some("s1"), BODY).await;
    assert_eq!(resp.status().as_u16(), 502);
    assert_eq!(good.hits(), 0);
    // The breaker opens as usual (the Router page shows the line down)...
    let st = r.status().await;
    assert_eq!(st.members[0].lines[0].state, LineState::Open);
    // ...but does not move requests: a new conversation goes to a again.
    let resp = r.post(Some("s2"), BODY).await;
    assert_eq!(resp.status().as_u16(), 502);
    assert_eq!(good.hits(), 0);
    assert_eq!(r.status().await.totals.failovers, 0);
}

#[tokio::test]
async fn with_failover_off_requests_stay_on_a_cooling_line() {
    let first = upstream(Mode::Json(502, "{}")).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&first, &second);
    cfg.tuning.breaker_threshold = 1;
    cfg.tuning.set_failover(&off()).unwrap();
    let r = start(cfg).await;
    assert_eq!(r.post(Some("a"), BODY).await.status().as_u16(), 502);
    assert_eq!(r.status().await.members[0].lines[0].state, LineState::Open);
    first.set(Mode::Json(200, OK));
    // On, "b" would skip x for the cooldown; off, it goes to x at once.
    let resp = r.post(Some("b"), BODY).await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(route(&resp), "x/x1");
    assert_eq!(second.hits(), 0);
}

#[tokio::test]
async fn with_failover_on_the_same_failure_moves_to_the_next_member() {
    let first = upstream(Mode::Json(502, "{}")).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let r = start(two_members(&first, &second)).await;
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(route(&resp), "y/y1");
    let st = r.status().await;
    assert!(st.failover);
    assert_eq!(st.totals.failovers, 1);
    assert_eq!(st.members[0].lines[0].failovers_away, 1);
}

#[tokio::test]
async fn with_failover_off_statuses_left_out_still_pass_through() {
    let first = upstream(Mode::Json(429, r#"{"error":{"message":"slow down"}}"#)).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&first, &second);
    cfg.tuning.set_failover(&off()).unwrap();
    let r = start(cfg).await;
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.status().as_u16(), 429);
    assert_eq!(route(&resp), "x/x1");
    assert!(resp.text().await.unwrap().contains("slow down"));
    assert_eq!(second.hits(), 0);
}

#[tokio::test]
async fn with_failover_off_claude_code_requests_stay_on_their_member() {
    use common::claude::{Fake, anthropic, body, claude_config, claude_headers, claude_member};
    use superba_router::claude::ClaudeModel;
    let busy = anthropic(std::sync::Arc::new(|_| {
        Fake::json(
            529,
            serde_json::json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        )
    }))
    .await;
    let good = anthropic(std::sync::Arc::new(|_| {
        Fake::json(200, serde_json::json!({"type": "message", "content": []}))
    }))
    .await;
    let (ma, mut ca) = claude_member("a", &busy);
    let (mb, mut cb) = claude_member("b", &good);
    for c in [&mut ca, &mut cb] {
        c.models = vec![ClaudeModel {
            id: "m".into(),
            display_name: None,
        }];
    }
    let mut cfg = claude_config(vec![(ma, ca), (mb, cb)]);
    cfg.tuning.set_failover(&off()).unwrap();
    let r = start(cfg).await;
    let resp = r
        .http
        .post(r.url("/claude/v1/messages"))
        .headers(claude_headers("s-a", None))
        .bearer_auth(ROUTER_TOKEN)
        .body(serde_json::to_vec(&body("m", false)).unwrap())
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status().as_u16(), 200);
    assert_eq!((busy.messages().len(), good.messages().len()), (1, 0));
    assert_eq!(r.status().await.totals.failovers, 0);
}
