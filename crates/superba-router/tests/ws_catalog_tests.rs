//! Codex WebSocket sessions under the merged catalog, configured as Superba
//! installs the router (every member in the catalog, Codex mode): the first
//! frame's model chooses the member before it is dialed, a later frame of
//! another member's model reconnects there, a model WebSocket cannot carry
//! sends the session to HTTP with 426, and the request log records each turn
//! as a WebSocket request on the line that served it.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::responses::*;
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use superba_core::{DataDir, RequestKind, RequestQuery, RequestRow, RequestTransport};
use superba_router::merged_catalog::{CatalogConfig, MemberCatalog};
use superba_router::openai::{OpenAiConfig, OpenAiEndpoint, OpenAiMember};
use superba_router::paths::RouterPaths;
use superba_router::{MemberKind, Reader, RouterConfig, RouterServer, StatsConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const SESSION: &str = "019a7c00-0000-7000-8000-0000000000c1";

fn entry(slug: &str) -> Value {
    json!({"slug":slug,"display_name":slug,"priority":1,"shell_type":"unified_exec","visibility":"list",
        "supported_reasoning_levels":[],"experimental_supported_tools":[],"supported_in_api":true,
        "support_verbosity":false,"truncation_policy":{"mode":"bytes","limit":10000},
        "base_instructions":"Use tools.","context_window":128000})
}

/// A Krill-like member: its Codex catalog lists `models`, its WebSocket
/// answers every `response.create` (prewarm: created + completed only).
async fn krill(models: &'static [&'static str]) -> Upstream {
    let list = Arc::new(
        move |path: &str, _: &http::HeaderMap, _: &[u8]| match path {
            "/backend-api/codex/models" => (
                200,
                json!({"models": models.iter().map(|m| entry(m)).collect::<Vec<_>>()}),
            ),
            "/v1/models" => (200, json!({"data": []})),
            _ => (404, json!({"error": {"message": "not here"}})),
        },
    );
    let script = Arc::new(|frame: &str| {
        let v: Value = serde_json::from_str(frame).unwrap_or_default();
        if v["type"] != "response.create" {
            return Vec::new();
        }
        if v["generate"] == json!(false) {
            return vec![
                json!({"type": "response.created", "response": {"id": "resp_w", "status": "in_progress"}})
                    .to_string(),
                json!({"type": "response.completed", "response": {"id": "resp_w", "status": "completed", "output": [], "usage": null}})
                    .to_string(),
            ];
        }
        text_answer()
            .into_iter()
            .map(|(_, e)| e.to_string())
            .collect()
    });
    upstream(Mode::WsOrPath(list, script)).await
}

/// The configuration Superba writes: members in priority order, each in the
/// catalog under its profile name, the installed Codex's models bundled.
fn production(members: Vec<superba_router::MemberConfig>) -> RouterConfig {
    let mut cfg = test_config(members);
    for m in &mut cfg.members {
        m.apply_websocket_hints();
    }
    cfg.catalog = Some(CatalogConfig {
        // Every member serves a bundled model ("gpt-a"); only their Codex
        // catalogs name the others.
        bundled: vec![entry("gpt-a")],
        members: cfg
            .members
            .iter()
            .map(|m| {
                (
                    m.id.clone(),
                    MemberCatalog {
                        name: m.id.clone(),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    cfg.stats = Some(StatsConfig::default());
    cfg
}

fn krill_member(id: &str, up: &Upstream) -> superba_router::MemberConfig {
    member(
        id,
        MemberKind::Krill,
        true,
        vec![line("one", up.base("/backend-api/codex"))],
    )
}

async fn boot(cfg: RouterConfig, paths: &RouterPaths) -> Router {
    let secrets = secrets_for(&cfg);
    cfg.save(paths).unwrap();
    secrets.save(paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let r = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    r
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn handshake(r: &Router) -> http::Request<()> {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    h.insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    h.insert("session-id", SESSION.parse().unwrap());
    h.insert("originator", "Codex Desktop".parse().unwrap());
    h.insert(
        "openai-beta",
        "responses_websockets=2026-02-06".parse().unwrap(),
    );
    req
}

async fn connect(r: &Router) -> Ws {
    let (ws, resp) = tokio_tungstenite::connect_async(handshake(r))
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);
    ws
}

fn create(model: &str, turn: &str) -> String {
    json!({
        "type": "response.create",
        "model": model,
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
        "client_metadata": {"x-codex-turn-metadata": turn_metadata(turn, "turn", "user")},
    })
    .to_string()
}

fn prewarm(model: &str) -> String {
    json!({
        "type": "response.create",
        "model": model,
        "generate": false,
        "input": [],
        "client_metadata": {"x-codex-turn-metadata": turn_metadata("t0", "prewarm", "user")},
    })
    .to_string()
}

/// What Codex reads next: the frames up to a terminal event, or the close
/// code (`Err`).
async fn answer(ws: &mut Ws) -> Result<Vec<Value>, Option<u16>> {
    let mut out = Vec::new();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame in time");
        match msg {
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                let done = matches!(
                    v["type"].as_str(),
                    Some(
                        "response.completed" | "response.failed" | "response.incomplete" | "error"
                    )
                );
                out.push(v);
                if done {
                    return Ok(out);
                }
            }
            Some(Ok(Message::Close(f))) => return Err(f.map(|f| f.code.into())),
            Some(Ok(_)) => {}
            other => panic!("expected a frame, got {other:?}"),
        }
    }
}

/// The frames a member got, as JSON.
fn frames(up: &Upstream) -> Vec<Value> {
    up.frames()
        .iter()
        .map(|f| serde_json::from_str(f).unwrap())
        .collect()
}

/// WebSocket handshakes a member got.
fn dials(up: &Upstream) -> usize {
    up.seen()
        .iter()
        .filter(|s| s.path.ends_with("/responses"))
        .count()
}

/// The request log once it holds `n` rows (oldest first).
async fn rows(paths: &RouterPaths, n: usize) -> Vec<RequestRow> {
    let reader = Reader::open(paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if page.rows.len() >= n || Instant::now() > deadline {
            let mut rows = page.rows;
            rows.sort_by_key(|r| r.id);
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_member_serves_the_catalog_model_over_websocket() {
    let a = krill(&["gpt-a"]).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = boot(production(vec![krill_member("alpha", &a)]), &paths).await;

    let mut ws = connect(&r).await;
    assert_eq!(dials(&a), 0, "the member is dialed for the first frame");
    ws.send(Message::text(prewarm("gpt-a"))).await.unwrap();
    assert_eq!(answer(&mut ws).await.unwrap().len(), 2);
    ws.send(Message::text(create("gpt-a", "t1"))).await.unwrap();
    let events = answer(&mut ws).await.unwrap();
    assert_eq!(events.last().unwrap()["type"], "response.completed");
    assert_eq!(dials(&a), 1);
    let got = frames(&a);
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|f| f["model"] == "gpt-a"));
    ws.close(None).await.unwrap();

    let rows = rows(&paths, 2).await;
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.transport, RequestTransport::WebSocket);
        assert_eq!(row.member_id.as_str(), "alpha");
        assert_eq!(row.line_id.as_deref(), Some("one"));
        assert_eq!(row.model.as_deref(), Some("gpt-a"));
    }
    assert_eq!(rows[0].request_kind, Some(RequestKind::Prewarm));
    assert_eq!(rows[1].request_kind, Some(RequestKind::Turn));
    assert_eq!(rows[1].input_tokens, Some(4002));
    r.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_of_a_later_member_is_dialed_there_with_its_names() {
    let a = krill(&["gpt-a", "gpt-shared"]).await;
    let b = krill(&["gpt-b", "gpt-shared"]).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let mut beta = krill_member("beta", &b);
    beta.model_map = [("gpt-b".to_owned(), "gpt-b-2026".to_owned())].into();
    let r = boot(production(vec![krill_member("alpha", &a), beta]), &paths).await;

    let mut ws = connect(&r).await;
    // A namespace the catalog does not know: answered here, sent nowhere.
    ws.send(Message::text(create("nope/gpt-a", "t0")))
        .await
        .unwrap();
    let refused = answer(&mut ws).await.unwrap();
    assert_eq!(refused[0]["type"], "error");
    assert_eq!(refused[0]["status"], 404);
    assert_eq!(refused[0]["error"]["code"], "unknown_provider");
    assert_eq!(dials(&a) + dials(&b), 0);

    // The model only the second member serves: dialed there, under the
    // member's own name for it.
    ws.send(Message::text(create("gpt-b", "t1"))).await.unwrap();
    answer(&mut ws).await.unwrap();
    assert_eq!(dials(&a), 0, "the first member is never dialed");
    assert_eq!(dials(&b), 1);
    // Its namespace pins a model both serve to it, the prefix stripped.
    ws.send(Message::text(create("beta/gpt-shared", "t2")))
        .await
        .unwrap();
    answer(&mut ws).await.unwrap();
    let got = frames(&b);
    assert_eq!(got[0]["model"], "gpt-b-2026");
    assert_eq!(got[1]["model"], "gpt-shared");
    assert!(frames(&a).is_empty());
    ws.close(None).await.unwrap();

    let rows = rows(&paths, 3).await;
    assert_eq!(rows.len(), 3);
    assert!(matches!(
        rows[0].status,
        superba_core::RequestStatus::Http { status: 404, .. }
    ));
    for row in &rows[1..] {
        assert_eq!(row.transport, RequestTransport::WebSocket);
        assert_eq!(row.member_id.as_str(), "beta");
        assert_eq!(row.line_id.as_deref(), Some("one"));
    }
    assert_eq!(rows[1].model.as_deref(), Some("gpt-b"));
    assert_eq!(rows[1].upstream_model.as_deref(), Some("gpt-b-2026"));
    assert_eq!(rows[2].model.as_deref(), Some("gpt-shared"));
    r.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_for_another_member_reconnects_there() {
    let a = krill(&["gpt-a", "gpt-shared"]).await;
    let b = krill(&["gpt-b"]).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = boot(
        production(vec![krill_member("alpha", &a), krill_member("beta", &b)]),
        &paths,
    )
    .await;

    let mut ws = connect(&r).await;
    ws.send(Message::text(create("gpt-a", "t1"))).await.unwrap();
    answer(&mut ws).await.unwrap();
    // Another model of the same member stays on the socket.
    ws.send(Message::text(create("gpt-shared", "t2")))
        .await
        .unwrap();
    answer(&mut ws).await.unwrap();
    // A model only the other member serves: never sent here; the socket
    // closes with 1012 for Codex to send the turn again on a new one.
    ws.send(Message::text(create("gpt-b", "t3"))).await.unwrap();
    assert_eq!(answer(&mut ws).await.unwrap_err(), Some(1012));
    assert_eq!(frames(&a).len(), 2);
    assert_eq!(dials(&b), 0);

    let mut ws = connect(&r).await;
    ws.send(Message::text(create("gpt-b", "t3"))).await.unwrap();
    answer(&mut ws).await.unwrap();
    assert_eq!(frames(&b).len(), 1);
    assert_eq!(frames(&b)[0]["model"], "gpt-b");
    assert_eq!(dials(&a), 1, "the conversation's member changed");
    ws.close(None).await.unwrap();

    let rows = rows(&paths, 3).await;
    assert_eq!(rows.len(), 3, "the bounced frame has no row");
    let served: Vec<_> = rows
        .iter()
        .map(|r| (r.member_id.as_str(), r.model.as_deref().unwrap()))
        .collect();
    assert_eq!(
        served,
        [
            ("alpha", "gpt-a"),
            ("alpha", "gpt-shared"),
            ("beta", "gpt-b")
        ]
    );
    assert!(
        rows.iter()
            .all(|r| r.transport == RequestTransport::WebSocket)
    );
    r.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_behind_a_protocol_conversion_sends_the_session_to_http() {
    let a = krill(&["gpt-a"]).await;
    let chat = upstream(Mode::Json(404, r#"{"error":{"message":"no list"}}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let conv = member(
        "conv",
        MemberKind::Compatible,
        true,
        vec![line("one", chat.base("/v1"))],
    );
    let mut cfg = production(vec![krill_member("alpha", &a), conv]);
    // Chat Completions only: its Responses are converted (HTTP only).
    cfg.openai = Some(OpenAiConfig {
        members: [(
            "conv".to_owned(),
            OpenAiMember {
                chat: Some(OpenAiEndpoint::line_base()),
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    });
    // The member's WebSocket hint is set: the conversion decides.
    cfg.members[1].websocket = true;
    let r = boot(cfg, &paths).await;

    let mut ws = connect(&r).await;
    ws.send(Message::text(create("gpt-a", "t1"))).await.unwrap();
    answer(&mut ws).await.unwrap();
    ws.send(Message::text(create("conv/some-model", "t2")))
        .await
        .unwrap();
    assert_eq!(answer(&mut ws).await.unwrap_err(), Some(1012));
    assert_eq!(frames(&a).len(), 1, "never sent to the socket's member");

    // Codex's reconnect is refused with 426: it falls back to HTTP.
    let refused = tokio_tungstenite::connect_async(handshake(&r)).await;
    let Err(tokio_tungstenite::tungstenite::Error::Http(resp)) = refused else {
        panic!("expected a refused handshake");
    };
    assert_eq!(resp.status(), 426);
    let body: Value = serde_json::from_slice(resp.body().as_deref().unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "model_routing_http");
    assert_eq!(body["error"]["type"], "model_routing_http");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Superba local router: ")
    );

    // Only that reconnect: a new session's first frame decides again, before
    // anything is dialed.
    let mut ws = connect(&r).await;
    ws.send(Message::text(prewarm("conv/some-model")))
        .await
        .unwrap();
    assert_eq!(answer(&mut ws).await.unwrap_err(), Some(1012));
    assert_eq!(dials(&a), 1);
    assert_eq!(dials(&chat), 0);
    r.handle.shutdown();
}
