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
        ..RouterFailover::default()
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

#[test]
fn old_settings_enable_failover_and_fixed_exits_survive_reordering() {
    let old: RouterFailover = serde_json::from_str(r#"{"cooldown_secs":7}"#).unwrap();
    assert!(old.enabled);
    assert_eq!(old.fixed_route, None);
    let old: superba_router::Tuning = serde_json::from_str("{}").unwrap();
    assert!(old.failover_enabled);
    let mut cfg = test_config(vec![
        member(
            "x",
            MemberKind::Krill,
            false,
            vec![line("a", "http://example.test/up".parse().unwrap())],
        ),
        member(
            "y",
            MemberKind::Compatible,
            false,
            vec![line("b", "http://example.test/v1".parse().unwrap())],
        ),
    ]);
    cfg.tuning.failover_enabled = false;
    cfg.reconcile_fixed_exit();
    assert_eq!(cfg.tuning.fixed_route.as_ref().unwrap().member_id, "x");
    cfg.members.swap(0, 1);
    cfg.reconcile_fixed_exit();
    assert_eq!(cfg.fixed_exit(), Some((1, 0)));
    let copy: superba_router::RouterConfig =
        serde_json::from_slice(&serde_json::to_vec(&cfg).unwrap()).unwrap();
    assert_eq!(copy.tuning, cfg.tuning);
    cfg.members.pop();
    cfg.reconcile_fixed_exit();
    assert_eq!(cfg.tuning.fixed_route.as_ref().unwrap().member_id, "y");
}

#[tokio::test]
async fn disabled_failover_passes_errors_and_edges_without_retry_or_probes() {
    let first = upstream(Mode::Json(200, OK)).await;
    let second = upstream(Mode::Json(200, OK)).await;
    let mut cfg = two_members(&first, &second);
    cfg.tuning.failover_enabled = false;
    cfg.tuning.active_probes = true;
    cfg.tuning.breaker_threshold = 1;
    cfg.tuning.failover_statuses = RouterFailover::STATUSES.to_vec();
    // Production configurations have a merged model catalog. Forwarding must
    // not fetch it as a request-time availability check in fixed mode.
    cfg.catalog = Some(Default::default());
    let r = start(cfg).await;
    for code in [451, 500, 503, 503] {
        first.set(Mode::Json(code, r#"{"error":"original"}"#));
        let resp = r.post(Some("same-session"), BODY).await;
        assert_eq!(resp.status().as_u16(), code);
        assert_eq!(route(&resp), "x/x1");
        assert_eq!(resp.text().await.unwrap(), r#"{"error":"original"}"#);
    }
    first.set(Mode::JsonHeader(200, OK, "cf-mitigated", "challenge"));
    assert_eq!(
        r.post(Some("new-session"), BODY).await.status().as_u16(),
        200
    );
    first.set(Mode::Json(200, ""));
    assert_eq!(r.post(None, BODY).await.status().as_u16(), 200);
    let resp = r
        .http
        .post(r.url("/_superba/v1/probe"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert_eq!(
        first.hits(),
        6,
        "one upstream request per client request, no model discovery"
    );
    assert_eq!(second.hits(), 0);
    assert_eq!(first.probes.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(second.probes.load(std::sync::atomic::Ordering::Relaxed), 0);
    let status = r.status().await;
    assert_eq!(status.totals.failovers, 0);
    assert_eq!(status.members[0].lines[0].failovers_away, 0);
    assert_eq!(status.members[0].lines[0].last_error, None);
    assert_eq!(status.sticky_sessions, 0);
}

#[tokio::test]
async fn a_fixed_backup_line_is_used_for_every_conversation_and_is_recorded() {
    let first = upstream(Mode::Json(200, OK)).await;
    let second = upstream(Mode::Json(500, r#"{"error":"chosen exit"}"#)).await;
    let mut cfg = two_members(&first, &second);
    cfg.members[1]
        .lines
        .insert(0, line("y0", first.base("/v1")));
    cfg.tuning
        .set_failover(&RouterFailover {
            enabled: false,
            fixed_route: Some(superba_core::RouterFixedRoute {
                member_id: "y".into(),
                line_id: "y1".into(),
            }),
            ..Default::default()
        })
        .unwrap();
    let running = common::openai::start_recording(cfg, Default::default()).await;
    for session in [Some("a"), Some("b"), Some("a"), None] {
        let resp = running.router.post(session, BODY).await;
        assert_eq!(route(&resp), "y/y1");
        assert_eq!(resp.status().as_u16(), 500);
        assert_eq!(resp.text().await.unwrap(), r#"{"error":"chosen exit"}"#);
    }
    let rows = common::openai::logged(&running.paths, 4).await;
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r.member_id.as_str() == "y"
        && r.line_id.as_deref() == Some("y1")
        && r.http_status == Some(500)
        && r.failovers == 0));
    assert_eq!(first.hits(), 0);
    assert_eq!(second.hits(), 4);
}

#[tokio::test]
async fn a_fixed_exit_connection_failure_never_tries_a_working_line() {
    let good = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "x",
        MemberKind::Krill,
        false,
        vec![line("dead", dead_base()), line("good", good.base("/up"))],
    )]);
    cfg.tuning.failover_enabled = false;
    let r = start(cfg).await;
    for _ in 0..2 {
        assert_eq!(r.post(Some("s"), BODY).await.status().as_u16(), 502);
    }
    assert_eq!(good.hits(), 0);
    assert_eq!(r.status().await.totals.failovers, 0);
}

#[tokio::test]
async fn fixed_mode_applies_to_chat_messages_and_gemini_without_model_discovery() {
    use superba_router::claude::{AnthropicAuth, ClaudeConfig, ClaudeMember, Endpoint};
    use superba_router::openai::{OpenAiConfig, OpenAiEndpoint, OpenAiMember};
    use superba_router::translate::routing::{GeminiConfig, GeminiMember};
    let first = upstream(Mode::Json(200, OK)).await;
    let second = upstream(Mode::Json(503, r#"{"error":{"message":"chosen exit"}}"#)).await;
    let mut cfg = two_members(&first, &second);
    cfg.openai = Some(OpenAiConfig {
        members: ["x", "y"]
            .into_iter()
            .map(|id| {
                (
                    id.into(),
                    OpenAiMember {
                        responses: Some(OpenAiEndpoint::line_base()),
                        chat: Some(OpenAiEndpoint::line_base()),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.claude = Some(ClaudeConfig {
        members: ["x", "y"]
            .into_iter()
            .map(|id| {
                (
                    id.into(),
                    ClaudeMember {
                        anthropic: Some(Endpoint::default()),
                        auth: AnthropicAuth::Bearer,
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.gemini = Some(GeminiConfig {
        members: ["x", "y"]
            .into_iter()
            .map(|id| {
                (
                    id.into(),
                    GeminiMember {
                        endpoint: Some(OpenAiEndpoint::at("/v1beta")),
                        models: vec![],
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.tuning.failover_enabled = false;
    cfg.tuning.fixed_route = Some(superba_core::RouterFixedRoute {
        member_id: "y".into(),
        line_id: "y1".into(),
    });
    let r = start(cfg).await;
    for (path, body) in [
        (
            "/openai/v1/chat/completions",
            r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#,
        ),
        ("/openai/v1/responses", BODY),
        (
            "/claude/v1/messages",
            r#"{"model":"test","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}"#,
        ),
        (
            "/gemini/v1beta/models/test:generateContent",
            r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#,
        ),
    ] {
        let resp = r
            .http
            .post(r.url(path))
            .bearer_auth(ROUTER_TOKEN)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 503, "{path}");
        assert_eq!(route(&resp), "y/y1", "{path}");
        let _ = resp.text().await.unwrap();
    }
    assert_eq!(first.hits(), 0);
    assert_eq!(second.hits(), 4, "no request-time model list lookups");
}

#[tokio::test]
async fn fixed_websocket_exit_passes_failure_and_ignores_cached_refusals() {
    use tokio_tungstenite::tungstenite::{Error as WsError, client::IntoClientRequest};
    let first = upstream(Mode::WsEcho).await;
    let second = upstream(Mode::Json(503, "{}")).await;
    let mut cfg = two_members(&first, &second);
    for m in &mut cfg.members {
        m.websocket = true;
    }
    cfg.tuning.failover_enabled = false;
    cfg.tuning.fixed_route = Some(superba_core::RouterFixedRoute {
        member_id: "y".into(),
        line_id: "y1".into(),
    });
    let r = start(cfg).await;
    let request = || {
        let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
        );
        req
    };
    for status in [503, 426] {
        second.set(Mode::Json(status, "{}"));
        let error = tokio_tungstenite::connect_async(request())
            .await
            .unwrap_err();
        let WsError::Http(resp) = error else {
            panic!("expected the upstream HTTP error")
        };
        assert_eq!(resp.status().as_u16(), status);
        assert_eq!(resp.headers()[superba_router::ROUTE_HEADER], "y/y1");
    }
    second.set(Mode::WsEcho);
    let (mut socket, resp) = tokio_tungstenite::connect_async(request()).await.unwrap();
    assert_eq!(resp.headers()[superba_router::ROUTE_HEADER], "y/y1");
    socket.close(None).await.unwrap();
    assert_eq!(first.hits(), 0);
    assert_eq!(second.hits(), 3);
    assert_eq!(r.status().await.totals.failovers, 0);
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
