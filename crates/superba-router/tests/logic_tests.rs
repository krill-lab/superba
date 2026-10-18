//! Pure logic: configuration rules and files, breakers, stickiness, candidate
//! order, model mapping, tokens and the watchdog state machine.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::HeaderMap;
use pretty_assertions::assert_eq;
use superba_core::catalog::{krill_site, official_site};
use superba_core::{CodexPath, LineId, Secret, SiteKind};
use superba_router::health::{BreakerPolicy, LineHealth};
use superba_router::members::{MemberPath, member_from_site, router_id};
use superba_router::model_map::map_model;
use superba_router::select::{Candidate, Topology, candidates};
use superba_router::sticky::{Pin, StickyTable, session_key};
use superba_router::token::{self, bearer, constant_time_eq};
use superba_router::watchdog::{
    FallbackReason, Liveness, Verdict, Watchdog, WatchdogPolicy, primary_target,
};
use superba_router::{
    LineConfig, LineState, MemberConfig, MemberKind, RouterConfig, RouterPaths, RouterSecrets,
    RuntimeInfo,
};
use url::Url;

fn line(id: &str, url: &str) -> LineConfig {
    LineConfig {
        id: id.into(),
        name: id.into(),
        base_url: Url::parse(url).unwrap(),
        probe_url: None,
        zstd: None,
        websocket: None,
    }
}

fn member(id: &str, lines: Vec<LineConfig>) -> MemberConfig {
    MemberConfig {
        id: id.into(),
        name: id.into(),
        kind: MemberKind::Krill,
        lines,
        websocket: true,
        model_map: BTreeMap::new(),
        compaction: Default::default(),
    }
}

fn valid() -> RouterConfig {
    RouterConfig::new(
        41000,
        vec![
            member(
                "krill",
                vec![
                    line("overseas", "https://api.krill-code.net/backend-api/codex"),
                    line("cdn", "https://api.cdn-krill-ai.com/backend-api/codex"),
                ],
            ),
            member("site", vec![line("main", "https://api.example.com/v1")]),
        ],
    )
}

fn invalid(mutate: impl FnOnce(&mut RouterConfig)) -> String {
    let mut c = valid();
    mutate(&mut c);
    c.validate().unwrap_err().to_string()
}

#[test]
fn config_rules() {
    valid().validate().unwrap();
    assert!(invalid(|c| c.members.clear()).contains("at least one member"));
    assert!(invalid(|c| c.members[1].id = "krill".into()).contains("duplicate member"));
    assert!(invalid(|c| c.members[0].lines.clear()).contains("has no line"));
    assert!(invalid(|c| c.members[0].lines[1].id = "overseas".into()).contains("duplicate line"));
    assert!(invalid(|c| c.members[0].id = "Krill!".into()).contains("member id"));
    assert!(invalid(|c| c.format = 2).contains("format"));
    assert!(invalid(|c| c.tuning.connect_timeout_ms = 0).contains("positive"));
    let msg = invalid(|c| {
        c.members[1].model_map.insert("gpt".into(), " ".into());
    });
    assert!(msg.contains("empty model name"));
    // Plain http only to loopback; no credentials, query, fragment.
    assert!(
        invalid(|c| c.members[1].lines[0] = line("main", "http://api.example.com/v1"))
            .contains("https required")
    );
    let mut c = valid();
    c.members[1].lines[0] = line("main", "http://127.0.0.1:9000/v1");
    c.validate().unwrap();
    assert!(
        invalid(|c| c.members[1].lines[0] = line("main", "https://u:p@api.example.com/v1"))
            .contains("credentials")
    );
    assert!(
        invalid(|c| c.members[1].lines[0] = line("main", "https://api.example.com/v1?k=1"))
            .contains("query")
    );
}

#[test]
fn the_official_chatgpt_login_is_never_a_member() {
    for host in [
        "https://chatgpt.com/backend-api/codex",
        "https://ab.chatgpt.com/backend-api/codex",
        "https://chat.openai.com/backend-api",
        "https://chatgpt.com./backend-api",
    ] {
        let msg = invalid(|c| c.members[1].lines[0] = line("main", host));
        assert!(msg.contains("never routed"), "{host}: {msg}");
    }
    assert!(MemberKind::for_site(SiteKind::Official).is_err());
    assert_eq!(
        MemberKind::for_site(SiteKind::Krill).unwrap(),
        MemberKind::Krill
    );
    assert_eq!(
        MemberKind::for_site(SiteKind::Compatible).unwrap(),
        MemberKind::Compatible
    );
    // A config file naming the official kind does not even parse.
    let json = serde_json::to_string(&valid()).unwrap().replacen(
        "\"krill\",\"lines\"",
        "\"official\",\"lines\"",
        1,
    );
    assert!(serde_json::from_str::<RouterConfig>(&json).is_err());
}

#[test]
fn probe_url_defaults_to_models_below_the_base() {
    let l = line("a", "https://api.example.com/v1/");
    assert_eq!(
        l.effective_probe_url().as_str(),
        "https://api.example.com/v1/models"
    );
}

#[test]
fn files_round_trip_with_private_modes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::from_dir(dir.path().join("router"));
    let cfg = valid();
    cfg.save(&paths).unwrap();
    assert_eq!(RouterConfig::load(&paths).unwrap(), cfg);
    let keys: BTreeMap<String, Secret> = [
        ("krill".to_owned(), Secret::new("nb_krill_key_123456")),
        ("site".to_owned(), Secret::new("sk-site-key-123456")),
    ]
    .into();
    let secrets = RouterSecrets::generate(keys);
    assert!(secrets.router_token.expose().starts_with("sbr_"));
    assert_eq!(secrets.router_token.expose().len(), 4 + 64);
    secrets.validate_for(&cfg).unwrap();
    secrets.save(&paths).unwrap();
    let back = RouterSecrets::load(&paths).unwrap();
    assert_eq!(back.router_token, secrets.router_token);
    assert_eq!(back.key("site").unwrap().expose(), "sk-site-key-123456");
    // The configuration never holds a key.
    let config_text = std::fs::read_to_string(paths.config_json()).unwrap();
    assert!(!config_text.contains("nb_krill_key"));
    // A member without a key is refused.
    let mut partial = back.clone();
    partial.member_keys.remove("site");
    assert!(partial.validate_for(&cfg).is_err());
    // Runtime record.
    let info = RuntimeInfo {
        pid: 42,
        port: 41000,
        instance_id: "abc".into(),
        admin_token: Secret::new("sba_admin"),
        started_at_ms: 1,
        version: "0.1.0".into(),
    };
    info.save(&paths).unwrap();
    let r = RuntimeInfo::load(&paths).unwrap();
    assert_eq!(
        (r.pid, r.port, r.admin_token.expose()),
        (42, 41000, "sba_admin")
    );
    RuntimeInfo::remove_if_owned(&paths, "other");
    assert!(paths.runtime_json().exists());
    RuntimeInfo::remove_if_owned(&paths, "abc");
    assert!(!paths.runtime_json().exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in [paths.config_json(), paths.secrets_json()] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", p.display());
        }
        let mode = std::fs::metadata(paths.dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
    // A corrupt secrets file does not echo its content in the error.
    std::fs::write(paths.secrets_json(), "{\"router_token\": \"sbr_leak").unwrap();
    let err = RouterSecrets::load(&paths).unwrap_err().to_string();
    assert!(!err.contains("sbr_leak"), "{err}");
}

#[test]
fn breaker_opens_backs_off_and_recovers() {
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);
    let mut h = LineHealth::new(BreakerPolicy::default());
    assert_eq!(h.state(t0), LineState::Healthy);
    // One soft failure is tolerated, the second opens.
    assert!(!h.on_soft_failure(t0, "HTTP 502"));
    assert_eq!(h.state(t0), LineState::Healthy);
    assert!(h.on_soft_failure(t0, "HTTP 502"));
    assert_eq!(h.state(s(4)), LineState::Open);
    assert_eq!(h.state(s(5)), LineState::HalfOpen);
    assert!(h.is_available(s(5)));
    // Failing while half-open re-opens with a doubled backoff (10 s).
    assert!(!h.on_soft_failure(s(5), "HTTP 503"));
    assert_eq!(h.state(s(14)), LineState::Open);
    assert_eq!(h.state(s(15)), LineState::HalfOpen);
    // Hard failures keep doubling up to the 60 s cap.
    h.on_hard_failure(s(15), "connect failed"); // 20 s
    h.on_hard_failure(s(35), "connect failed"); // 40 s
    h.on_hard_failure(s(75), "connect failed"); // 60 s (cap)
    h.on_hard_failure(s(135), "connect failed"); // 60 s
    assert_eq!(h.state(s(194)), LineState::Open);
    assert_eq!(h.state(s(195)), LineState::HalfOpen);
    assert_eq!(h.last_error(), Some("connect failed"));
    // Success closes and resets the backoff.
    h.on_success(s(200), Some(Duration::from_millis(100)));
    assert_eq!(h.state(s(200)), LineState::Healthy);
    assert_eq!(h.consecutive_failures(), 0);
    assert_eq!(h.ttfb_ms(), Some(100));
    h.on_hard_failure(s(201), "connect failed");
    assert_eq!(h.state(s(205)), LineState::Open);
    assert_eq!(h.state(s(206)), LineState::HalfOpen);
    // Probes.
    assert!(h.on_probe(s(206), true, None));
    assert_eq!(h.state(s(206)), LineState::Healthy);
    assert!(h.on_probe(s(207), false, Some("probe: HTTP 502".into())));
    assert_eq!(h.state(s(207)), LineState::Open);
    assert_eq!(h.last_error(), Some("probe: HTTP 502"));
}

#[test]
fn sticky_table_ttl_refresh_and_capacity() {
    let t0 = Instant::now();
    let mut t = StickyTable::new(Duration::from_secs(10), 2);
    let p = |m, l| Pin { member: m, line: l };
    t.set("a", p(0, 0), t0);
    t.set("b", p(1, 0), t0 + Duration::from_secs(1));
    assert_eq!(t.get("a", t0 + Duration::from_secs(9)), Some(p(0, 0)));
    // "a" was refreshed at 9 s; "b" (1 s) is now the oldest and is evicted.
    t.set("c", p(1, 1), t0 + Duration::from_secs(9));
    assert_eq!(t.len(), 2);
    assert_eq!(t.get("b", t0 + Duration::from_secs(9)), None);
    assert_eq!(t.get("a", t0 + Duration::from_secs(18)), Some(p(0, 0)));
    assert_eq!(t.get("c", t0 + Duration::from_secs(19)), None);
    t.purge(t0 + Duration::from_secs(60));
    assert!(t.is_empty());
}

#[test]
fn session_key_prefers_session_id() {
    let mut h = HeaderMap::new();
    assert_eq!(session_key(&h), None);
    h.insert("x-client-request-id", "c".parse().unwrap());
    assert_eq!(session_key(&h).as_deref(), Some("x-client-request-id:c"));
    h.insert("thread-id", "t".parse().unwrap());
    assert_eq!(session_key(&h).as_deref(), Some("thread-id:t"));
    h.insert("session-id", "s".parse().unwrap());
    assert_eq!(session_key(&h).as_deref(), Some("session-id:s"));
    h.insert("session-id", "x".repeat(300).parse().unwrap());
    assert_eq!(session_key(&h).as_deref(), Some("thread-id:t"));
}

struct Topo {
    lines: Vec<usize>,
    ws: Vec<bool>,
    ws_only: bool,
    open: Vec<(usize, usize, u64)>,
    t0: Instant,
}

impl Topology for Topo {
    fn members(&self) -> usize {
        self.lines.len()
    }
    fn lines(&self, m: usize) -> usize {
        self.lines[m]
    }
    fn eligible(&self, m: usize) -> bool {
        !self.ws_only || self.ws[m]
    }
    fn available(&self, m: usize, l: usize, _: Instant) -> bool {
        !self.open.iter().any(|(om, ol, _)| *om == m && *ol == l)
    }
    fn reopens_at(&self, m: usize, l: usize) -> Option<Instant> {
        self.open
            .iter()
            .find(|(om, ol, _)| *om == m && *ol == l)
            .map(|(_, _, s)| self.t0 + Duration::from_secs(*s))
    }
}

fn order(topo: &Topo, pin: Option<(usize, usize)>) -> Vec<(usize, usize)> {
    candidates(
        topo,
        pin.map(|(member, line)| Pin { member, line }),
        topo.t0,
    )
    .into_iter()
    .map(|Candidate { member, line }| (member, line))
    .collect()
}

#[test]
fn candidate_order() {
    let mut topo = Topo {
        lines: vec![2, 1, 2],
        ws: vec![true, false, true],
        ws_only: false,
        open: vec![],
        t0: Instant::now(),
    };
    assert_eq!(order(&topo, None), [(0, 0), (0, 1), (1, 0), (2, 0), (2, 1)]);
    // Pinned line first, then its member's other lines, then the others.
    assert_eq!(
        order(&topo, Some((2, 1))),
        [(2, 1), (2, 0), (0, 0), (0, 1), (1, 0)]
    );
    // Open lines are skipped.
    topo.open = vec![(2, 1, 5), (0, 0, 3)];
    assert_eq!(order(&topo, Some((2, 1))), [(2, 0), (0, 1), (1, 0)]);
    // A stale pin (config changed) is ignored.
    assert_eq!(order(&topo, Some((7, 0))), [(0, 1), (1, 0), (2, 0)]);
    // WebSocket: only capable members.
    topo.ws_only = true;
    topo.open.clear();
    assert_eq!(order(&topo, Some((1, 0))), [(0, 0), (0, 1), (2, 0), (2, 1)]);
    // Everything open: one attempt at the line that reopens first.
    topo.ws_only = false;
    topo.open = vec![(0, 0, 9), (0, 1, 4), (1, 0, 7), (2, 0, 4), (2, 1, 8)];
    assert_eq!(order(&topo, None), [(0, 1)]);
}

#[test]
fn model_mapping() {
    let map: BTreeMap<String, String> = [("gpt-x".to_owned(), "vendor-x".to_owned())].into();
    let body = Bytes::from_static(br#"{"model":"gpt-x","input":[{"model":"gpt-x"}]}"#);
    let h = HeaderMap::new();
    let out = map_model(&body, &h, &map, 1 << 20).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["model"], "vendor-x");
    assert_eq!(v["input"][0]["model"], "gpt-x", "only the top-level model");
    assert!(map_model(&body, &h, &BTreeMap::new(), 1 << 20).is_none());
    assert!(map_model(&Bytes::from_static(b"not json"), &h, &map, 1 << 20).is_none());
    let mut gz = HeaderMap::new();
    gz.insert("content-encoding", "gzip".parse().unwrap());
    assert!(map_model(&body, &gz, &map, 1 << 20).is_none());
    // zstd bodies are decoded within the limit only.
    let mut z = HeaderMap::new();
    z.insert("content-encoding", "zstd".parse().unwrap());
    let compressed = Bytes::from(zstd::stream::encode_all(&body[..], 3).unwrap());
    let out = map_model(&compressed, &z, &map, 1 << 20).unwrap();
    let plain = zstd::stream::decode_all(&out[..]).unwrap();
    assert!(String::from_utf8(plain).unwrap().contains("vendor-x"));
    assert!(map_model(&compressed, &z, &map, 10).is_none());
}

#[test]
fn tokens() {
    let a = token::generate(token::ROUTER_TOKEN_PREFIX);
    let b = token::generate(token::ROUTER_TOKEN_PREFIX);
    assert_ne!(a, b);
    assert!(a[4..].bytes().all(|c| c.is_ascii_hexdigit()));
    assert!(constant_time_eq(a.as_bytes(), a.as_bytes()));
    assert!(!constant_time_eq(a.as_bytes(), b.as_bytes()));
    assert!(!constant_time_eq(b"ab", b"abc"));
    assert_eq!(bearer(b"Bearer  tok "), Some(&b"tok"[..]));
    assert_eq!(bearer(b"Basic tok"), None);
    assert_eq!(bearer(b"Bearer "), None);
}

#[test]
fn watchdog_waits_for_the_grace_period_then_falls_back_once() {
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);
    let mut w = Watchdog::new(WatchdogPolicy {
        grace: Duration::from_secs(20),
    });
    let down = Liveness::Down {
        detail: "connection refused".into(),
    };
    assert!(matches!(w.observe(s(0), &down), Verdict::Suspect { .. }));
    assert!(matches!(
        w.observe(s(19), &down),
        Verdict::Suspect { down_for, .. } if down_for == Duration::from_secs(19)
    ));
    assert_eq!(
        w.observe(s(20), &down),
        Verdict::FallBack {
            reason: FallbackReason::Dead {
                detail: "connection refused".into()
            }
        }
    );
    assert_eq!(w.observe(s(25), &down), Verdict::FellBack);
    // A recovery resets the outage; a short blip never falls back.
    let st: superba_router::RouterStatus = serde_json::from_value(serde_json::json!({
        "service": "superba-router", "version": "0", "instance_id": "i", "pid": 1,
        "listen": "127.0.0.1:1", "base_url": "http://127.0.0.1:1/backend-api/codex",
        "started_at_ms": 0, "uptime_ms": 0, "config_generation": 1, "members": [],
        "totals": {"requests": 0, "active_requests": 0, "websockets": 0,
                   "active_websockets": 0, "failovers": 0, "unavailable": 0,
                   "rejected": 0, "ws_refused": 0},
        "sticky_sessions": 0, "recent": []
    }))
    .unwrap();
    assert_eq!(
        w.observe(s(30), &Liveness::Up(Box::new(st))),
        Verdict::Healthy
    );
    assert!(matches!(w.observe(s(31), &down), Verdict::Suspect { .. }));
    assert!(matches!(w.observe(s(45), &down), Verdict::Suspect { .. }));
}

#[test]
fn primary_target_is_the_first_line_of_the_first_member() {
    let cfg = valid();
    let (m, l) = primary_target(&cfg).unwrap();
    assert_eq!((m.id.as_str(), l.id.as_str()), ("krill", "overseas"));
}

#[test]
fn members_from_sites() {
    let krill = krill_site();
    let m = member_from_site(
        &krill,
        &[LineId::new("cdn"), LineId::new("overseas")],
        &MemberPath::Codex(CodexPath::BackendApiCodex),
    )
    .unwrap();
    assert_eq!(m.id, "krill");
    assert_eq!(m.kind, MemberKind::Krill);
    assert!(m.websocket);
    let urls: Vec<_> = m
        .lines
        .iter()
        .map(|l| {
            (
                l.id.as_str(),
                l.base_url.as_str(),
                l.effective_probe_url().to_string(),
            )
        })
        .collect();
    assert_eq!(
        urls,
        [
            (
                "cdn",
                "https://api.cdn-krill-ai.com/backend-api/codex",
                "https://api.cdn-krill-ai.com/v1/models".to_owned()
            ),
            (
                "overseas",
                "https://api.krill-code.net/backend-api/codex",
                "https://api.krill-code.net/v1/models".to_owned()
            ),
        ]
    );
    // Empty order = every line in site order.
    let all = member_from_site(&krill, &[], &MemberPath::Codex(CodexPath::CodexV1)).unwrap();
    assert_eq!(all.lines.len(), 3);
    assert_eq!(
        all.lines[0].base_url.as_str(),
        "https://api-slb.krill-code.net/codex/v1"
    );
    assert!(
        member_from_site(
            &krill,
            &[LineId::new("nope")],
            &MemberPath::Custom(String::new())
        )
        .is_err()
    );
    // The official login is refused.
    assert!(member_from_site(&official_site(), &[], &MemberPath::Custom(String::new())).is_err());
    // A compatible site keeps the base URL's path and has no WebSocket.
    let mut site = krill_site();
    site.kind = SiteKind::Compatible;
    site.id = "My Site_1".into();
    let base = Url::parse("https://api.krill-code.net/v1/").unwrap();
    let c = member_from_site(
        &site,
        &[LineId::new("overseas")],
        &MemberPath::of_base_url(&base),
    )
    .unwrap();
    assert_eq!(
        (c.id.as_str(), c.kind, c.websocket),
        ("my-site_1", MemberKind::Compatible, false)
    );
    assert_eq!(
        c.lines[0].base_url.as_str(),
        "https://api.krill-code.net/v1"
    );
    assert_eq!(
        c.lines[0].effective_probe_url().as_str(),
        "https://api.krill-code.net/v1/models"
    );
    assert!(member_from_site(&site, &[], &MemberPath::Custom("v1".into())).is_err());
    assert_eq!(router_id("Ab/中"), "ab--");
    assert_eq!(router_id(""), "member");
}
