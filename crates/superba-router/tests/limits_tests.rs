//! The router's local budget (user bug 35): admissions bound concurrent
//! requests and WebSocket sessions, requests over the budget get a local 503
//! `local_router_busy`, and a local "too many open files" is never counted
//! against a line (no breaker, no failover, no "line down").
//!
//! The tests that lower `RLIMIT_NOFILE` run the router in a child process (this
//! test binary started again for one test); the fake upstreams and the clients
//! stay in the parent, so only the router's own descriptors count.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use superba_core::fdlimit::FileLimitHit;
use superba_router::limits::{Budget, LOCAL_BUSY_CODE, file_limit_in, is_file_limit_error};
use superba_router::{LineState, MemberKind, RouteEventKind, RouterStatus, Tuning};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const OK: &str = r#"{"ok":true}"#;
const BODY: &str = r#"{"model":"gpt-x","input":"hello"}"#;

#[test]
fn the_budget_follows_the_open_file_limit() {
    let t = Tuning::default();
    let n = |b: Budget| {
        (
            b.max_concurrent,
            b.max_connections,
            b.pool_max_idle_per_host,
            b.busy_wait_ms,
        )
    };
    // macOS's 256 for launchd agents: a quarter for requests (two sockets
    // each), half for inbound connections.
    assert_eq!(n(Budget::new(Some(256), &t)), (64, 128, 4, 2_000));
    assert_eq!(n(Budget::new(Some(1024), &t)), (256, 512, 16, 2_000));
    assert_eq!(n(Budget::new(Some(8192), &t)), (512, 2048, 16, 2_000));
    // No limit (Windows, RLIM_INFINITY).
    assert_eq!(n(Budget::new(None, &t)), (512, 2048, 16, 2_000));
    // Very low limits keep a minimum.
    assert_eq!(n(Budget::new(Some(8), &t)), (4, 8, 2, 2_000));
    // Configured values win.
    let mut custom = t.clone();
    custom.max_concurrent = Some(10);
    custom.max_connections = Some(15);
    custom.pool_max_idle_per_host = Some(0);
    custom.busy_wait_ms = 50;
    assert_eq!(n(Budget::new(Some(256), &custom)), (10, 15, 0, 50));
    custom.max_connections = None;
    assert_eq!(Budget::new(Some(256), &custom).max_connections, 40);
    // Every inbound connection, an upstream socket per admission and the idle
    // pools of four hosts leave room for the rest of the process.
    for limit in [256u64, 512, 1024, 4096, 8192, 65_536] {
        let b = Budget::new(Some(limit), &t);
        let worst = b.max_connections + b.max_concurrent + 4 * b.pool_max_idle_per_host;
        assert!(
            worst as u64 + 16 <= limit,
            "limit {limit}: {worst} descriptors at worst"
        );
    }
}

#[derive(Debug)]
struct Wrapped(Box<dyn std::error::Error + Send + Sync>);

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("connect error")
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

/// "Too many open files" of this platform: `EMFILE` (Unix), Winsock's
/// `WSAEMFILE` (Windows sockets).
fn emfile() -> std::io::Error {
    #[cfg(unix)]
    let code = libc::EMFILE;
    #[cfg(windows)]
    let code = 10_024;
    std::io::Error::from_raw_os_error(code)
}

#[test]
fn too_many_open_files_is_found_anywhere_in_an_error_chain() {
    use std::io;
    // reqwest → hyper-util → io::Error, as for a failed socket().
    let chain = Wrapped(Box::new(Wrapped(Box::new(emfile()))));
    assert_eq!(file_limit_in(&chain), Some(FileLimitHit::Process));
    // A custom error inside an io::Error (its `source` skips the custom one).
    let nested = io::Error::other(Wrapped(Box::new(emfile())));
    assert_eq!(file_limit_in(&nested), Some(FileLimitHit::Process));
    #[cfg(unix)]
    {
        let enfile = io::Error::from_raw_os_error(libc::ENFILE);
        assert_eq!(
            file_limit_in(&Wrapped(Box::new(enfile))),
            Some(FileLimitHit::System)
        );
    }
    let core = superba_core::Error::io("/data/router/usage.sqlite3", &emfile());
    assert_eq!(file_limit_in(&core), Some(FileLimitHit::Process));
    assert!(is_file_limit_error(&core));
    // Everything else is a line's business.
    let refused = Wrapped(Box::new(io::Error::from(io::ErrorKind::ConnectionRefused)));
    assert_eq!(file_limit_in(&refused), None);
    let text = Wrapped(Box::new(io::Error::other("too many open files")));
    assert_eq!(file_limit_in(&text), None);
    let denied = superba_core::Error::io("/x", &io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(!is_file_limit_error(&denied));
}

/// What a test needs of a router, in this process or in a child.
#[derive(Clone)]
struct Endpoint {
    addr: std::net::SocketAddr,
    admin_token: String,
    http: reqwest::Client,
}

impl Endpoint {
    fn of(r: &Router) -> Self {
        Self {
            addr: r.addr,
            admin_token: r.admin_token.clone(),
            http: r.http.clone(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn post(&self, session: Option<&str>, body: &'static str) -> reqwest::Response {
        let mut rb = self
            .http
            .post(self.url("/backend-api/codex/responses"))
            .bearer_auth(ROUTER_TOKEN)
            .header("content-type", "application/json")
            .body(body);
        if let Some(s) = session {
            rb = rb.header("session-id", s);
        }
        rb.send().await.unwrap()
    }

    async fn status(&self) -> RouterStatus {
        self.http
            .get(self.url("/_superba/v1/status"))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn healthy_and_unblamed(st: &RouterStatus) {
    for m in &st.members {
        for l in &m.lines {
            assert_eq!(l.state, LineState::Healthy, "{}/{} was blamed", m.id, l.id);
            assert_eq!(l.consecutive_failures, 0, "{}/{}", m.id, l.id);
            assert_eq!(l.failovers_away, 0, "{}/{}", m.id, l.id);
            assert_eq!(l.last_error, None, "{}/{}", m.id, l.id);
        }
    }
    assert_eq!(st.totals.failovers, 0);
    assert_eq!(st.totals.unavailable, 0);
    for e in &st.recent {
        assert!(
            !matches!(
                e.kind,
                RouteEventKind::LineDown
                    | RouteEventKind::Failover
                    | RouteEventKind::Unavailable
                    | RouteEventKind::StreamBroken
            ),
            "unexpected event {e:?}"
        );
    }
}

async fn busy_answer(resp: reqwest::Response) -> String {
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()["connection"], "close");
    assert_eq!(resp.headers()["retry-after"], "1");
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], LOCAL_BUSY_CODE);
    assert_eq!(v["error"]["type"], LOCAL_BUSY_CODE);
    v["error"]["message"].as_str().unwrap().to_owned()
}

/// Release gated SSE answers until the bodies are read to the end.
async fn finish(gate: &Notify, bodies: Vec<reqwest::Response>) {
    let reads: Vec<_> = bodies
        .into_iter()
        .map(|r| tokio::spawn(async move { r.text().await.unwrap() }))
        .collect();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !reads.iter().all(|r| r.is_finished()) {
            gate.notify_waiters();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for r in reads {
        assert!(r.await.unwrap().ends_with("data: [DONE]\n\n"));
    }
}

async fn wait_idle(r: &Endpoint) -> RouterStatus {
    for _ in 0..200 {
        let st = r.status().await;
        if st.local.in_flight == 0 {
            return st;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("admissions still in use: {:?}", r.status().await.local);
}

#[tokio::test]
async fn requests_over_the_admission_limit_get_a_local_503_and_no_line_is_blamed() {
    let gate = Arc::new(Notify::new());
    let up = upstream(Mode::Sse {
        first: "data: 1\n\n",
        rest: "data: [DONE]\n\n",
        gate: Some(gate.clone()),
        break_mid: false,
    })
    .await;
    let other = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", up.base("/backend-api/codex"))],
        ),
        member(
            "site",
            MemberKind::Compatible,
            false,
            vec![line("b", other.base("/v1"))],
        ),
    ]);
    cfg.tuning.max_concurrent = Some(2);
    cfg.tuning.busy_wait_ms = 200;
    let r = Endpoint::of(&start(cfg).await);
    // Two streams in flight hold both admissions until their bodies end.
    let a = r.post(Some("s1"), BODY).await;
    let b = r.post(Some("s2"), BODY).await;
    assert_eq!((a.status().as_u16(), b.status().as_u16()), (200, 200));
    let started = std::time::Instant::now();
    let message = busy_answer(r.post(Some("s3"), BODY).await).await;
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "it waited for an admission first"
    );
    assert!(
        message.starts_with("Superba local router: local router busy: 2 requests"),
        "{message}"
    );
    assert_eq!(up.hits(), 2);
    assert_eq!(other.hits(), 0, "a local shortage is never a failover");
    // The admin API is not counted: status answers while every admission is in use.
    let st = r.status().await;
    assert_eq!(st.local.max_concurrent, 2);
    assert_eq!(st.local.in_flight, 2);
    assert_eq!(st.local.busy, 1);
    assert!(st.local.last_busy_ms.is_some());
    assert_eq!(st.local.file_limit_errors, 0);
    assert!(
        st.recent
            .iter()
            .any(|e| e.kind == RouteEventKind::LocalLimit)
    );
    healthy_and_unblamed(&st);
    // A second busy answer within the minute is counted but not shown again.
    busy_answer(r.post(Some("s4"), BODY).await).await;
    let st = r.status().await;
    assert_eq!(st.local.busy, 2);
    assert_eq!(
        st.recent
            .iter()
            .filter(|e| e.kind == RouteEventKind::LocalLimit)
            .count(),
        1
    );
    finish(&gate, vec![a, b]).await;
    let st = wait_idle(&r).await;
    healthy_and_unblamed(&st);
    // Places are free again.
    up.set(Mode::Json(200, OK));
    let resp = r.post(Some("s5"), BODY).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/a");
}

fn ws_request(r: &Endpoint) -> http::Request<()> {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    req
}

#[tokio::test]
async fn a_websocket_session_holds_an_admission_until_it_closes() {
    let up = upstream(Mode::WsEcho).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    cfg.tuning.max_concurrent = Some(1);
    cfg.tuning.busy_wait_ms = 100;
    let r = Endpoint::of(&start(cfg).await);
    let (mut ws, resp) = tokio_tungstenite::connect_async(ws_request(&r))
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);
    let st = r.status().await;
    assert_eq!(st.local.in_flight, 1);
    assert!(
        st.local.connections >= 1,
        "the upgraded socket keeps its place"
    );
    // The session's admission is the only one.
    busy_answer(r.post(None, BODY).await).await;
    let err = tokio_tungstenite::connect_async(ws_request(&r))
        .await
        .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(refused) = err else {
        panic!("expected an HTTP answer, got {err:?}");
    };
    assert_eq!(refused.status(), 503);
    ws.close(None).await.unwrap();
    drop(ws);
    let st = wait_idle(&r).await;
    healthy_and_unblamed(&st);
    assert_eq!(st.local.busy, 2);
    up.set(Mode::Json(200, OK));
    assert_eq!(r.post(None, BODY).await.status(), 200);
}

#[cfg(unix)]
mod child {
    //! The router in a child process under a low open-file limit.

    use std::io::{Read, Write};
    use std::net::SocketAddr;
    use std::process::Stdio;
    use std::time::Duration;

    use superba_router::RouterConfig;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use url::Url;

    use super::common;

    /// Scenario the child runs (unset: this is the parent).
    pub const SCENARIO: &str = "SUPERBA_ROUTER_FD_CHILD";
    /// Space-separated upstream base URLs the parent serves.
    pub const UPSTREAMS: &str = "SUPERBA_ROUTER_FD_UPSTREAMS";
    /// launchd's soft limit for agents (and GUI apps) on macOS.
    pub const MACOS_AGENT_LIMIT: libc::rlim_t = 256;

    pub fn scenario() -> Option<String> {
        std::env::var(SCENARIO).ok()
    }

    pub fn upstreams() -> Vec<Url> {
        std::env::var(UPSTREAMS)
            .unwrap()
            .split_whitespace()
            .map(|u| Url::parse(u).unwrap())
            .collect()
    }

    /// Lower this process's soft open-file limit (the hard one is kept).
    pub fn set_soft_limit(soft: libc::rlim_t) {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit / setrlimit with a valid pointer to a local struct.
        let ok = unsafe {
            libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 && {
                rl.rlim_cur = soft;
                libc::setrlimit(libc::RLIMIT_NOFILE, &rl) == 0
            }
        };
        assert!(ok, "setrlimit: {}", std::io::Error::last_os_error());
    }

    /// Child: serve `cfg` in this process, run `prepare` (with the router
    /// started), tell the parent where it listens, and stop when the parent
    /// closes stdin.
    pub async fn serve<T>(cfg: RouterConfig, prepare: impl FnOnce() -> T) {
        let r = common::start(cfg).await;
        // Started before `prepare` may use up the descriptors.
        let parent_gone = tokio::task::spawn_blocking(|| {
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink);
        });
        let kept = prepare();
        let mut out = std::io::stdout();
        writeln!(out, "ROUTER-READY {} {}", r.addr, r.admin_token).unwrap();
        out.flush().unwrap();
        parent_gone.await.unwrap();
        drop(kept);
        r.handle.shutdown();
    }

    /// The router of a child process, seen from the parent.
    pub struct ChildRouter {
        pub router: super::Endpoint,
        process: tokio::process::Child,
    }

    impl ChildRouter {
        /// Close the child's stdin and check that it ended well.
        pub async fn stop(mut self) {
            drop(self.process.stdin.take());
            let status = tokio::time::timeout(Duration::from_secs(30), self.process.wait())
                .await
                .expect("the child router stops")
                .unwrap();
            assert!(status.success(), "child router failed: {status}");
        }
    }

    /// Parent: start test `name` of this binary as the child router.
    pub async fn spawn(
        name: &str,
        scenario: &str,
        upstreams: &[Url],
        http: reqwest::Client,
    ) -> ChildRouter {
        let urls: Vec<String> = upstreams.iter().map(Url::to_string).collect();
        let mut process = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(SCENARIO, scenario)
            .env(UPSTREAMS, urls.join(" "))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(process.stdout.take().unwrap()).lines();
        let ready = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                // libtest's "test name ... " precedes it on the same line.
                if let Some((_, rest)) = line.split_once("ROUTER-READY ") {
                    return rest.to_owned();
                }
            }
            panic!("the child router exited before it was ready");
        })
        .await
        .expect("the child router starts");
        // Keep the pipe drained (libtest's own lines follow).
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let (addr, token) = ready.split_once(' ').unwrap();
        let addr: SocketAddr = addr.parse().unwrap();
        ChildRouter {
            router: super::Endpoint {
                addr,
                admin_token: token.to_owned(),
                http,
            },
            process,
        }
    }
}

/// Requests of a burst.
#[cfg(unix)]
const BURST: usize = 300;

/// Child side of a burst: one member line, a soft limit of 256, and with
/// `unbounded` admissions and connections far above what the limit allows
/// (as the router had before its budget).
#[cfg(unix)]
async fn burst_child(unbounded: bool) {
    child::set_soft_limit(child::MACOS_AGENT_LIMIT);
    let up = child::upstreams();
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up[0].clone())],
    )]);
    if unbounded {
        cfg.tuning.max_concurrent = Some(10_000);
        cfg.tuning.max_connections = Some(10_000);
    }
    child::serve(cfg, || {
        assert_eq!(superba_core::fdlimit::open_files_limit(), Some(256));
    })
    .await;
}

/// Parent side of a burst: [`BURST`] concurrent slow requests (2.5 s each)
/// against the child router `name`. Every one must be answered 200 or 503
/// `local_router_busy` and no line may be blamed. Returns the answered and
/// busy counts and the final status.
#[cfg(unix)]
async fn burst(name: &str) -> (u64, u64, RouterStatus) {
    // The parent holds a client and an upstream socket per request.
    superba_core::fdlimit::raise_open_files_limit(superba_core::fdlimit::OPEN_FILES_TARGET);
    let up = upstream(Mode::Slow(Duration::from_millis(2_500), OK)).await;
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    let child = child::spawn(name, "burst", &[up.base("/backend-api/codex")], http).await;
    let r = child.router.clone();
    assert_eq!(r.status().await.local.open_files_limit, Some(256));
    let started = std::time::Instant::now();
    let jobs: Vec<_> = (0..BURST)
        .map(|i| {
            let http = r.http.clone();
            let url = r.url("/backend-api/codex/responses");
            tokio::spawn(async move {
                let resp = http
                    .post(url)
                    .bearer_auth(ROUTER_TOKEN)
                    .header("session-id", format!("s{i}"))
                    .header("content-type", "application/json")
                    .body(BODY)
                    .send()
                    .await
                    .unwrap_or_else(|e| panic!("request {i}: {e:?}"));
                match resp.status().as_u16() {
                    200 => {
                        assert_eq!(resp.text().await.unwrap(), OK);
                        true
                    }
                    503 => {
                        let v: serde_json::Value = resp.json().await.unwrap();
                        assert_eq!(v["error"]["code"], LOCAL_BUSY_CODE, "request {i}: {v}");
                        false
                    }
                    other => panic!("request {i}: HTTP {other}"),
                }
            })
        })
        .collect();
    let (mut ok, mut busy) = (0u64, 0u64);
    for job in jobs {
        if job.await.unwrap() {
            ok += 1;
        } else {
            busy += 1;
        }
    }
    let st = r.status().await;
    println!(
        "{name}: {BURST} requests under a 256 limit: {ok} answered, {busy} local 503s \
         ({} for want of an admission, {} too many open files), {:.1} s",
        st.local.busy,
        st.local.file_limit_errors,
        started.elapsed().as_secs_f64()
    );
    assert_eq!(ok as usize, up.hits(), "nothing was sent twice");
    healthy_and_unblamed(&st);
    child.stop().await;
    (ok, busy, st)
}

/// ~300 concurrent slow requests against a router whose soft open-file limit
/// is 256 (each holds two sockets, so the router without a budget ran out at
/// about 110 and marked every line down): every request is answered 200 or
/// 503 `local_router_busy`, no line is blamed, and the budget keeps the router
/// from running out of descriptors at all.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_hundred_slow_requests_under_a_256_open_file_limit() {
    const NAME: &str = "three_hundred_slow_requests_under_a_256_open_file_limit";
    if child::scenario().is_some() {
        return burst_child(false).await;
    }
    let (ok, busy, st) = burst(NAME).await;
    assert_eq!(
        (st.local.max_concurrent, st.local.max_connections),
        (64, 128)
    );
    assert!(ok >= 64, "every admission served at least once ({ok})");
    assert_eq!(st.totals.requests, ok);
    assert_eq!(st.local.busy, busy);
    assert_eq!(
        st.local.file_limit_errors, 0,
        "the budget kept the router below its limit: {:?}",
        st.local.last_file_limit
    );
}

/// The same burst with the budget configured far above the limit: the router
/// now does run out of descriptors (accepting and connecting upstream), and
/// still every request is answered 200 or 503 and no line is blamed.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_out_of_descriptors_never_marks_a_line_down() {
    const NAME: &str = "running_out_of_descriptors_never_marks_a_line_down";
    if child::scenario().is_some() {
        return burst_child(true).await;
    }
    let (ok, busy, st) = burst(NAME).await;
    assert_eq!(st.local.max_concurrent, 10_000);
    assert!(ok > 0, "some requests got through");
    assert!(
        st.local.file_limit_errors > 0,
        "the limit was reached: {:?}",
        st.local
    );
    assert_eq!(st.local.busy, 0, "admissions were never the limit");
    assert!(busy > 0, "requests without a descriptor got the local 503");
    assert!(
        st.recent
            .iter()
            .any(|e| e.kind == RouteEventKind::LocalLimit)
    );
}

/// The router's last descriptor goes to Codex's connection, so connecting to
/// the member line fails with EMFILE: the request gets 503
/// `local_router_busy` naming the limit, no other line or member is tried, and
/// no line is marked down; the same for a WebSocket handshake.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_many_open_files_on_the_upstream_connect_is_local() {
    const NAME: &str = "too_many_open_files_on_the_upstream_connect_is_local";
    if child::scenario().is_some() {
        child::set_soft_limit(child::MACOS_AGENT_LIMIT);
        let up = child::upstreams();
        let cfg = test_config(vec![
            member(
                "krill",
                MemberKind::Krill,
                true,
                vec![line("a", up[0].clone())],
            ),
            member(
                "site",
                MemberKind::Compatible,
                true,
                vec![line("b", up[1].clone())],
            ),
        ]);
        child::serve(cfg, || {
            // Use up every descriptor, then free exactly one.
            let mut files = Vec::new();
            loop {
                match std::fs::File::open("/dev/null") {
                    Ok(f) => files.push(f),
                    Err(e) if e.raw_os_error() == Some(libc::EMFILE) => break,
                    Err(e) => panic!("{e}"),
                }
            }
            files.pop();
            files
        })
        .await;
        return;
    }
    let first = upstream(Mode::Json(200, OK)).await;
    let second = upstream(Mode::Json(200, OK)).await;
    // No pooled connection may keep the child's only free descriptor.
    let http = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let child = child::spawn(NAME, "full", &[up_base(&first), up_base(&second)], http).await;
    let r = &child.router;
    let message = busy_answer(r.post(Some("s1"), BODY).await).await;
    assert!(
        message.contains(
            "too many open files on this computer (limit 256) while connecting to krill/a"
        ),
        "{message}"
    );
    assert!(message.contains("no line was marked down"), "{message}");
    let err = tokio_tungstenite::connect_async(ws_request(r))
        .await
        .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(refused) = err else {
        panic!("expected an HTTP answer, got {err:?}");
    };
    assert_eq!(refused.status(), 503);
    assert_eq!((first.hits(), second.hits()), (0, 0));
    let st = r.status().await;
    healthy_and_unblamed(&st);
    assert!(st.local.file_limit_errors >= 2, "{:?}", st.local);
    let last = st.local.last_file_limit.clone().unwrap();
    // A later connection may also have waited for a descriptor to accept it.
    assert!(
        ["connecting to krill/a", "accepting a connection"].contains(&last.place.as_str()),
        "{last:?}"
    );
    assert!(
        last.detail.to_lowercase().contains("too many open files"),
        "{last:?}"
    );
    assert!(
        st.recent
            .iter()
            .any(|e| e.kind == RouteEventKind::LocalLimit)
    );
    child.stop().await;
}

#[cfg(unix)]
fn up_base(up: &Upstream) -> url::Url {
    up.base("/backend-api/codex")
}
