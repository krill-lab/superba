//! The ChatGPT app's side calls in PAT router mode are answered by the router
//! and never reach a member; model traffic still does.

mod common;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use superba_router::{MemberKind, ROUTE_HEADER, RouterPaths, RouterServer};
use tokio::net::TcpListener;
use url::Url;

use common::*;

/// An upstream that records every request it gets, with or without a key.
#[derive(Clone)]
struct Counting {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Counting {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up = Counting {
            addr: listener.local_addr().unwrap(),
            hits: Arc::default(),
        };
        let state = up.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = state.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let state = state.clone();
                        async move {
                            let line = format!("{} {}", req.method(), req.uri().path());
                            let models = req.uri().path().ends_with("/models");
                            let _ = req.into_body().collect().await;
                            state.hits.lock().unwrap().push(line);
                            let body = if models {
                                r#"{"models":[]}"#
                            } else {
                                r#"{"id":"resp_1","object":"response","output":[]}"#
                            };
                            let mut resp =
                                Response::new(Full::new(Bytes::from_static(body.as_bytes())));
                            resp.headers_mut()
                                .insert("content-type", "application/json".parse().unwrap());
                            Ok::<_, Infallible>(resp)
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        up
    }

    fn base(&self) -> Url {
        Url::parse(&format!("http://{}/backend-api/codex", self.addr)).unwrap()
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }
}

/// One side call: method, path and query, whether Codex sends the router
/// token with it, the JSON body (POST), the status the router must answer.
struct Call {
    method: &'static str,
    path: &'static str,
    token: bool,
    body: Option<Value>,
    status: u16,
}

const fn get(path: &'static str, status: u16) -> Call {
    Call {
        method: "GET",
        path,
        token: true,
        body: None,
        status,
    }
}

fn post(path: &'static str, body: Value, status: u16) -> Call {
    Call {
        method: "POST",
        path,
        token: true,
        body: Some(body),
        status,
    }
}

fn mcp(body: Value, status: u16) -> Call {
    Call {
        token: false,
        ..post("/backend-api/ps/mcp", body, status)
    }
}

/// The side calls Codex Desktop 0.163 made in PAT mode (bolt and a8bolt
/// Windows runs, the router spike), plus the account and usage pages of the
/// app (chatgpt-app-usage-ui API.md).
fn side_calls() -> Vec<Call> {
    vec![
        get("/v1/user-auth-credential/whoami", 200),
        get("/backend-api/wham/accounts/check", 200),
        get("/backend-api/wham/settings/user", 200),
        get("/backend-api/plugins/featured?platform=codex", 200),
        get("/backend-api/ps/plugins/installed?limit=200", 200),
        get(
            "/backend-api/ps/plugins/installed?limit=200&includeDownloadUrls=true",
            200,
        ),
        get("/backend-api/ps/plugins/suggested/codex?scope=GLOBAL", 200),
        get("/backend-api/ps/plugins/list?scope=GLOBAL&limit=50", 200),
        get(
            "/backend-api/ps/plugins/plugin_connector_1p_f8509de903288191b14a160c6c5d20b0",
            404,
        ),
        Call {
            method: "GET",
            token: false,
            ..get("/backend-api/ps/mcp", 405)
        },
        mcp(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                   "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                              "clientInfo": {"name": "codex-mcp-client", "version": "0.163.0"}}}),
            200,
        ),
        mcp(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            202,
        ),
        mcp(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            200,
        ),
        get("/backend-api/wham/usage", 200),
        post(
            "/backend-api/codex/analytics-events/events",
            json!({"events": []}),
            200,
        ),
        post(
            "/backend-api/codex/alpha/notes/v2/thread_hint",
            json!({"context": {"session_id": "s"}}),
            200,
        ),
        get("/backend-api/wham/config/bundle", 200),
        get("/backend-api/wham/workspace-messages", 200),
        get(
            "/backend-api/wham/usage/daily-token-usage-breakdown?start_date=2026-10-10&end_date=2026-10-16&group_by=day",
            200,
        ),
        post(
            "/backend-api/wham/usage/thread_usage/query_v2",
            json!({"threads": [{"thread_id": "t", "created_at": "2026-10-16T16:17:41Z",
                                "descendant_thread_ids": []}]}),
            200,
        ),
        get("/backend-api/wham/usage/plan_limit_history?days=7", 404),
        get("/backend-api/wham/usage/credit-usage-events", 200),
        get(
            "/backend-api/wham/analytics/daily-code-review-metrics?group_by=day&workspace_user=true",
            200,
        ),
        post(
            "/backend-api/wham/analytics-events/events",
            json!({"events": []}),
            200,
        ),
        get("/backend-api/wham/rate-limit-reset-credits", 404),
        post("/backend-api/wham/statsig/bootstrap", json!({}), 404),
        get("/backend-api/wham/remote/control/server/refresh", 404),
        get("/backend-api/accounts/check/v4-2023-04-27", 200),
        get("/backend-api/me", 200),
        get("/backend-api/wham/profiles/me", 200),
        get("/backend-api/settings/is_adult", 404),
        get("/backend-api/profiles/me/page", 200),
        get(
            "/backend-api/connectors/directory/list?external_logos=true",
            200,
        ),
        get("/backend-api/sentinel/heartbeat", 404),
        Call {
            token: false,
            ..get("/backend-api/wham/usage", 200)
        },
    ]
}

#[tokio::test]
async fn side_calls_never_reach_a_member_and_model_traffic_still_does() {
    let up = Counting::start().await;
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base())],
    )]);
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::from_dir(dir.path().join("router"));
    cfg.save(&paths).unwrap();
    secrets_for(&cfg).save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let addr = server.local_addr();
    let admin = server.admin_token().expose().to_owned();
    let stop = server.shutdown_handle();
    let task = tokio::spawn(server.serve());
    // Codex reaches chatgpt_base_url over the router's loopback TLS.
    let pem = std::fs::read(paths.certificate_pem()).unwrap();
    let tls = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&pem).unwrap())
        .build()
        .unwrap();

    let calls = side_calls();
    for call in &calls {
        let url = format!("https://{addr}{}", call.path);
        let mut rb = match call.method {
            "GET" => tls.get(&url),
            _ => tls.post(&url),
        };
        if call.token {
            rb = rb.bearer_auth(ROUTER_TOKEN);
        }
        if let Some(body) = &call.body {
            rb = rb.json(body);
        }
        let resp = rb.send().await.unwrap();
        let what = format!("{} {}", call.method, call.path);
        assert_eq!(resp.status().as_u16(), call.status, "{what}");
        assert_eq!(resp.headers()[ROUTE_HEADER], "local", "{what}");
        let bytes = resp.bytes().await.unwrap();
        if ![202, 405].contains(&call.status) {
            let v: Value =
                serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{what}: not JSON ({e})"));
            if call.status == 404 {
                assert_eq!(v, json!({"detail": "Not Found"}), "{what}");
            }
        }
    }
    assert!(
        up.hits().is_empty(),
        "side calls reached the member: {:?}",
        up.hits()
    );

    // The answers Codex parses.
    let installed: Value = tls
        .get(format!("https://{addr}/backend-api/ps/plugins/installed"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        installed,
        json!({"plugins": [], "pagination": {"next_page_token": null}})
    );
    let tools: Value = tls
        .post(format!("https://{addr}/backend-api/ps/mcp"))
        .json(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        tools,
        json!({"jsonrpc": "2.0", "id": 7, "result": {"tools": []}})
    );
    assert!(up.hits().is_empty());

    // Model traffic (and the provider endpoints of a turn) still go upstream.
    let model_calls = [
        ("POST", "/backend-api/codex/responses"),
        ("POST", "/backend-api/codex/responses/compact"),
        ("GET", "/backend-api/codex/models?client_version=0.163.0"),
        ("POST", "/backend-api/codex/memories/trace_summarize"),
        ("POST", "/backend-api/codex/alpha/search"),
    ];
    for (method, path) in model_calls {
        let url = format!("http://{addr}{path}");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let rb = match method {
            "GET" => http.get(&url),
            _ => http
                .post(&url)
                .header("content-type", "application/json")
                .body(r#"{"model":"gpt-x","input":"hi"}"#),
        };
        let resp = rb.bearer_auth(ROUTER_TOKEN).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{method} {path}");
        assert_ne!(
            resp.headers()
                .get(ROUTE_HEADER)
                .map(|v| v.to_str().unwrap()),
            Some("local"),
            "{method} {path}"
        );
    }
    assert_eq!(
        up.hits(),
        vec![
            "POST /backend-api/codex/responses",
            "POST /backend-api/codex/responses/compact",
            "GET /backend-api/codex/models",
            "POST /backend-api/codex/memories/trace_summarize",
            "POST /backend-api/codex/alpha/search",
        ]
    );

    // Unknown paths outside the ChatGPT backend keep the router's 404.
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let elsewhere = http
        .get(format!("http://{addr}/elsewhere"))
        .send()
        .await
        .unwrap();
    assert_eq!(elsewhere.status(), 404);
    let v: Value = elsewhere.json().await.unwrap();
    assert_eq!(v["error"]["type"], "not_found");

    // The admin status counts local answers apart from proxied requests.
    let status: superba_router::RouterStatus = http
        .get(format!("http://{addr}/_superba/v1/status"))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let local = u64::try_from(calls.len() + 2).unwrap();
    assert_eq!(status.totals.local_answers, local);
    assert_eq!(status.local_answers.values().sum::<u64>(), local);
    assert_eq!(status.local_answers["ps/plugins/installed"], 3);
    assert_eq!(status.local_answers["ps/mcp"], 5);
    assert_eq!(status.local_answers["ps/plugins/{id}"], 1);
    assert_eq!(status.local_answers["wham/accounts/check"], 1);
    assert_eq!(status.local_answers["wham/*"], 1);
    assert_eq!(
        status.totals.requests,
        u64::try_from(model_calls.len()).unwrap(),
        "only model traffic counts as proxied requests"
    );
    assert_eq!(status.totals.rejected, 0);
    stop.shutdown();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn identity_endpoints_still_need_the_router_token() {
    let up = Counting::start().await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base())],
    )]))
    .await;
    for path in [
        "/v1/user-auth-credential/whoami",
        "/backend-api/wham/accounts/check",
        "/backend-api/wham/settings/user",
    ] {
        let resp = r.http.get(r.url(path)).send().await.unwrap();
        assert_eq!(resp.status(), 401, "{path}");
    }
    // Side calls hold nothing and need no token.
    let usage = r
        .http
        .get(r.url("/backend-api/wham/usage"))
        .send()
        .await
        .unwrap();
    assert_eq!(usage.status(), 200);
    assert!(up.hits().is_empty());
    let st = r.status().await;
    assert_eq!(st.totals.rejected, 3);
    assert_eq!(st.totals.local_answers, 1);
}
