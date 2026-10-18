//! Codex inside WSL (user bug 45): per-host source tokens, the Codex host in the
//! request log, and the token-gated WSL listener (no admin API there).

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use common::{Mode, ROUTER_TOKEN, line, member, secrets_for, test_config, upstream};
use pretty_assertions::assert_eq;
use superba_core::{DataDir, RequestQuery};
use superba_router::config::wsl_listen_allowed;
use superba_router::paths::RouterPaths;
use superba_router::store::Reader;
use superba_router::token::{source_token, valid_source, verify_source_token};
use superba_router::{MemberKind, RouterConfig, RouterServer, StatsConfig};

#[test]
fn source_tokens_name_their_source_and_only_verify_with_the_router_token() {
    let token = source_token(ROUTER_TOKEN.as_bytes(), "wsl:Ubuntu-24.04").unwrap();
    assert!(token.starts_with("sbs_"));
    assert!(
        token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    );
    assert_eq!(
        verify_source_token(ROUTER_TOKEN.as_bytes(), token.as_bytes()).as_deref(),
        Some("wsl:Ubuntu-24.04")
    );
    // Deterministic, different per source and per router token.
    assert_eq!(
        source_token(ROUTER_TOKEN.as_bytes(), "wsl:Ubuntu-24.04").unwrap(),
        token
    );
    assert_ne!(
        source_token(ROUTER_TOKEN.as_bytes(), "wsl:Debian").unwrap(),
        token
    );
    assert_eq!(
        verify_source_token(b"another router token", token.as_bytes()),
        None
    );
    // A changed source or MAC fails.
    let other = token.replacen(
        &hex::encode("wsl:Ubuntu-24.04"),
        &hex::encode("wsl:Ubuntu-24.05"),
        1,
    );
    assert_eq!(
        verify_source_token(ROUTER_TOKEN.as_bytes(), other.as_bytes()),
        None
    );
    let mut flipped = token.clone().into_bytes();
    let last = flipped.len() - 1;
    flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
    assert_eq!(verify_source_token(ROUTER_TOKEN.as_bytes(), &flipped), None);
    assert_eq!(
        verify_source_token(ROUTER_TOKEN.as_bytes(), ROUTER_TOKEN.as_bytes()),
        None
    );
    assert!(source_token(ROUTER_TOKEN.as_bytes(), "wsl:a b").is_none());
    assert!(valid_source("wsl:kali-linux") && !valid_source("") && !valid_source("x/y"));
}

#[test]
fn wsl_listen_accepts_private_adapter_addresses_only() {
    for (ip, ok) in [
        ("172.25.80.1", true),
        ("192.168.32.1", true),
        ("10.0.0.1", true),
        ("100.64.0.1", true),
        ("127.0.0.1", false),
        ("8.8.8.8", false),
        ("169.254.1.1", false),
        ("172.25.80.255", false),
        ("0.0.0.0", false),
    ] {
        assert_eq!(wsl_listen_allowed(ip.parse().unwrap()), ok, "{ip}");
    }
    let mut cfg = test_config(vec![member("krill", MemberKind::Krill, false, vec![])]);
    cfg.wsl_listen = vec!["8.8.8.8".parse().unwrap()];
    assert!(cfg.validate().is_err());
    // Round trip: the field is kept, and absent from files that never had it.
    let json = serde_json::to_string(&RouterConfig::new(1, vec![])).unwrap();
    assert!(!json.contains("wsl_listen"));
}

/// A private IPv4 address of this machine (the source address of a route to a
/// private network), when it has one.
fn local_private_ipv4() -> Option<Ipv4Addr> {
    for target in ["172.31.255.254:9", "10.255.255.254:9", "192.168.255.254:9"] {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        if s.connect(target).is_err() {
            continue;
        }
        if let Ok(SocketAddr::V4(a)) = s.local_addr()
            && wsl_listen_allowed(*a.ip())
        {
            return Some(*a.ip());
        }
    }
    None
}

struct Started {
    addr: SocketAddr,
    admin_token: String,
    handle: superba_router::ShutdownHandle,
    paths: RouterPaths,
    _dir: tempfile::TempDir,
}

async fn start_recording(mut cfg: RouterConfig) -> Started {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    cfg.stats = Some(StatsConfig::default());
    secrets_for(&cfg).save(&paths).unwrap();
    cfg.save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let started = Started {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        paths,
        _dir: dir,
    };
    tokio::spawn(server.serve());
    started
}

async fn post(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    host_header: Option<&str>,
) -> reqwest::Response {
    let mut rb = http
        .post(format!("{base}/backend-api/codex/responses"))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .header("session-id", "019a7c00-0000-7000-8000-0000000000e1")
        .body(r#"{"model":"gpt-5","input":"hi","stream":false}"#);
    if let Some(h) = host_header {
        rb = rb.header("x-superba-codex-host", h);
    }
    rb.send().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_codex_host_comes_from_the_token_and_is_recorded_never_forwarded() {
    let up = upstream(Mode::Json(200, r#"{"id":"resp_1","status":"completed"}"#)).await;
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    let r = start_recording(cfg).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{}", r.addr);
    let wsl = source_token(ROUTER_TOKEN.as_bytes(), "wsl:Ubuntu").unwrap();
    assert_eq!(post(&http, &base, &wsl, None).await.status(), 200);
    // This computer's token; a header claiming WSL is dropped.
    assert_eq!(
        post(&http, &base, ROUTER_TOKEN, Some("wsl:Spoofed"))
            .await
            .status(),
        200
    );
    assert_eq!(post(&http, &base, "sbs_00_00", None).await.status(), 401);
    for seen in up.seen() {
        assert!(
            !seen.headers.contains_key("x-superba-codex-host"),
            "{seen:?}"
        );
    }
    let reader = Reader::open(&r.paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut hosts: Vec<Option<String>> = Vec::new();
    while Instant::now() < deadline {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if page.rows.len() >= 2 {
            hosts = page.rows.iter().map(|row| row.codex_host.clone()).collect();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    hosts.sort();
    assert_eq!(hosts, vec![None, Some("wsl:Ubuntu".to_owned())]);
    r.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wsl_listener_takes_wsl_source_tokens_and_hides_the_admin_api() {
    let Some(ip) = local_private_ipv4() else {
        eprintln!("no private IPv4 address on this machine; skipped");
        return;
    };
    let up = upstream(Mode::Json(200, r#"{"id":"resp_1","status":"completed"}"#)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    cfg.wsl_listen = vec![ip];
    let r = start_recording(cfg).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let wsl_base = format!("http://{ip}:{}", r.addr.port());
    // Bound on the first reconcile tick.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        let s: superba_router::RouterStatus = http
            .get(format!("http://{}/_superba/v1/status", r.addr))
            .bearer_auth(&r.admin_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if !s.wsl_listen.is_empty() || Instant::now() > deadline {
            break s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(status.wsl_listen, vec![format!("{ip}:{}", r.addr.port())]);
    let wsl = source_token(ROUTER_TOKEN.as_bytes(), "wsl:Ubuntu").unwrap();
    assert_eq!(post(&http, &wsl_base, &wsl, None).await.status(), 200);
    // This computer's own token and other sources are not accepted there.
    assert_eq!(
        post(&http, &wsl_base, ROUTER_TOKEN, None).await.status(),
        401
    );
    let other = source_token(ROUTER_TOKEN.as_bytes(), "ide:x").unwrap();
    assert_eq!(post(&http, &wsl_base, &other, None).await.status(), 401);
    // The admin API: health only.
    let health = http
        .get(format!("{wsl_base}/_superba/v1/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let admin = http
        .get(format!("{wsl_base}/_superba/v1/status"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin.status(), 403);
    r.handle.shutdown();
}
