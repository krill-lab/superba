//! Live checks against real endpoints. Skipped unless `SUPERBA_NET_LIVE=1`.
//!
//! - `live_public_krill_lines`: unauthenticated probes of the built-in Krill lines
//!   (no key is ever sent).
//! - `live_keyed_line`: additionally needs `SUPERBA_NET_LIVE_ORIGIN` (a loopback
//!   forwarder such as `http://127.0.0.1:18001`, or one of the built-in Krill line
//!   origins) and `SUPERBA_NET_LIVE_KEY_ENV` naming the variable that holds the key.
//!   Costs one request of quota per key check and a few tokens for the Responses
//!   probe when `SUPERBA_NET_LIVE_RESPONSES_MODEL` is set.
//!
//! Run with `cargo test -p superba-net --test live_tests -- --nocapture`.

use superba_core::catalog::krill_site;
use superba_core::{
    AuthMode, CodexPath, LineId, LineProber, LineSource, LocalizedText, Origin, ResolvedEndpoints,
    Secret, Site,
};
use superba_net::HttpLineProber;

fn live() -> bool {
    std::env::var("SUPERBA_NET_LIVE").is_ok_and(|v| v == "1")
}

fn text(t: &Option<LocalizedText>) -> String {
    t.as_ref()
        .map(LocalizedText::render_fallback)
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn live_public_krill_lines() {
    if !live() {
        eprintln!("skipped: set SUPERBA_NET_LIVE=1");
        return;
    }
    let prober = HttpLineProber::default();
    let site = krill_site();
    let caps = prober.probe_all(&site).await.expect("probe_all");
    assert_eq!(caps.len(), site.lines.len());
    for c in &caps {
        eprintln!(
            "{:>9} reachable={} latency={:?}ms grade={:?} pat_ok={} apikey_ok={} path={:?} edge={} | {}",
            c.line_id.as_str(),
            c.reachable,
            c.latency_ms,
            c.latency_grade(),
            c.pat_ok,
            c.apikey_ok,
            c.codex_path,
            c.blocked_by_edge,
            text(&c.detail)
        );
        for chk in &c.checks {
            eprintln!(
                "          {:?} status={:?} ok={} edge={} {:?}",
                chk.endpoint, chk.status, chk.ok, chk.edge_blocked, chk.detail
            );
        }
    }
    assert!(caps.iter().any(|c| c.reachable), "no Krill line reachable");
}

/// The Krill site with the live origin as its only extra line (a loopback origin
/// becomes a test line; a built-in origin is used as is).
fn site_with(origin: &Origin) -> (Site, LineId) {
    let mut site = krill_site();
    if let Some(l) = site.lines.iter().find(|l| &l.origin == origin) {
        let id = l.id.clone();
        return (site, id);
    }
    assert!(
        origin.is_loopback(),
        "live origin must be loopback or a Krill line"
    );
    let id = LineId::new("live");
    site.lines.push(superba_core::Line {
        id: id.clone(),
        name: LocalizedText::new("", "Live"),
        origin: origin.clone(),
        source: LineSource::Test,
    });
    superba_net::validate_site(&site).expect("live site validates");
    (site, id)
}

#[tokio::test(flavor = "multi_thread")]
async fn live_keyed_line() {
    if !live() {
        eprintln!("skipped: set SUPERBA_NET_LIVE=1");
        return;
    }
    let (Ok(origin), Ok(key_var)) = (
        std::env::var("SUPERBA_NET_LIVE_ORIGIN"),
        std::env::var("SUPERBA_NET_LIVE_KEY_ENV"),
    ) else {
        eprintln!("skipped: set SUPERBA_NET_LIVE_ORIGIN and SUPERBA_NET_LIVE_KEY_ENV");
        return;
    };
    let key = Secret::new(std::env::var(&key_var).expect("key variable is set"));
    let origin = Origin::parse(&origin).expect("origin");
    let (site, line_id) = site_with(&origin);
    let line = site.line(&line_id).expect("line").clone();
    let prober = HttpLineProber::default();

    let caps = prober
        .probe_with_key(&site, &line, &key)
        .await
        .expect("probe_with_key");
    eprintln!(
        "probe_with_key: reachable={} latency={:?} pat_ok={} apikey_ok={} path={:?} key_access={:?} | {}",
        caps.reachable,
        caps.latency_ms,
        caps.pat_ok,
        caps.apikey_ok,
        caps.codex_path,
        caps.key_access,
        text(&caps.detail)
    );
    for mode in [AuthMode::Pat, AuthMode::ApiKey] {
        let check = prober
            .check_key(&site, &line, mode, &key)
            .await
            .expect("check_key");
        eprintln!(
            "check_key {mode:?}: access={:?} whoami={:?} identity={:?} path={:?} last4={}",
            check.access, check.whoami, check.identity, check.codex_path, check.key_last4
        );
    }
    let health = prober
        .check_authapi(&site, &line.origin, &key)
        .await
        .expect("check_authapi");
    eprintln!("check_authapi: {health:?}");
    let summary = prober
        .account_summary(&site, &line, &key)
        .await
        .expect("account_summary");
    eprintln!("account_summary: {summary:?}");

    let path = caps.codex_path.unwrap_or(CodexPath::BackendApiCodex);
    let endpoints = ResolvedEndpoints::for_line(&line.origin, path, AuthMode::ApiKey).unwrap();
    let models = prober
        .list_models(&endpoints.openai_base_url, &key)
        .await
        .expect("list_models");
    eprintln!(
        "list_models ({} models): {:?}",
        models.len(),
        models
            .iter()
            .take(8)
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
    );
    if let Ok(model) = std::env::var("SUPERBA_NET_LIVE_RESPONSES_MODEL") {
        let probe = prober
            .probe_responses(&endpoints, &key, &model)
            .await
            .expect("probe_responses");
        eprintln!("probe_responses: {probe:?}");
    }
    assert!(caps.reachable);
}

/// Transport probe (WebSocket handshake + zstd) of a live line:
/// `SUPERBA_NET_LIVE_TRANSPORT_ORIGIN` (an https line origin or a loopback
/// forwarder) with the key in the variable `SUPERBA_NET_LIVE_KEY_ENV` names.
/// Costs no tokens.
#[tokio::test(flavor = "multi_thread")]
async fn live_transports() {
    if !live() {
        eprintln!("skipped: set SUPERBA_NET_LIVE=1");
        return;
    }
    let (Ok(origin), Ok(key_var)) = (
        std::env::var("SUPERBA_NET_LIVE_TRANSPORT_ORIGIN"),
        std::env::var("SUPERBA_NET_LIVE_KEY_ENV"),
    ) else {
        eprintln!("skipped: set SUPERBA_NET_LIVE_TRANSPORT_ORIGIN and SUPERBA_NET_LIVE_KEY_ENV");
        return;
    };
    let key = Secret::new(std::env::var(&key_var).expect("key variable is set"));
    let origin = Origin::parse(&origin).expect("origin");
    let endpoints =
        ResolvedEndpoints::for_line(&origin, CodexPath::BackendApiCodex, AuthMode::ApiKey).unwrap();
    let probe = HttpLineProber::default()
        .probe_transports(&endpoints, &key)
        .await
        .expect("probe_transports");
    eprintln!("websocket: {:?}", probe.websocket);
    eprintln!("zstd: {:?}", probe.zstd);
    let text = format!("{probe:?}");
    assert!(!text.contains(key.expose()));
}
