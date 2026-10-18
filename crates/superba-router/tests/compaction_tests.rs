//! Remote compaction awareness (GitHub issue 3): compaction requests skip
//! members that cannot compact, a compacted conversation stays with the
//! member that made its compaction item (over HTTP and WebSocket, also after
//! a restart), and when no member can serve it the router says so. Bodies
//! pass through untouched.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use superba_core::{CompactionSupport, DataDir};
use superba_router::compaction::{
    self, CompactionInput, MemberFacts, OWNER_UNAVAILABLE_CODE, Owners, Route, UNSUPPORTED_CODE,
    fingerprint,
};
use superba_router::paths::RouterPaths;
use superba_router::{MemberKind, RouteEventKind, RouterConfig, RouterServer, StatsConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

// ---------------------------------------------------------------------------
// Reading and routing
// ---------------------------------------------------------------------------

fn body(items: Value) -> Vec<u8> {
    json!({"model": "gpt-x", "input": items, "stream": true})
        .to_string()
        .into_bytes()
}

#[test]
fn the_input_is_read_for_the_trigger_and_compaction_items() {
    assert_eq!(
        compaction::inspect(br#"{"model":"m","input":"hello"}"#),
        CompactionInput::default()
    );
    assert_eq!(compaction::inspect(b"not json"), CompactionInput::default());
    let c = compaction::inspect(&body(json!([
        {"type": "compaction", "encrypted_content": "blob-1"},
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "about compaction"}]},
        {"type": "compaction_summary", "encrypted_content": "blob-2"},
        {"type": "compaction", "encrypted_content": ""},
        {"type": "compaction_trigger"},
    ])));
    assert!(c.trigger);
    assert_eq!(c.items, vec![fingerprint("blob-1"), fingerprint("blob-2")]);
    // The word in a message alone is nothing.
    let c = compaction::inspect(&body(json!([
        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "compaction_trigger"}]}
    ])));
    assert!(c.is_empty());
    assert_eq!(fingerprint("blob-1").len(), 32);
    assert_ne!(fingerprint("blob-1"), fingerprint("blob-2"));
}

#[test]
fn answers_are_read_for_the_compaction_item() {
    let done = json!({"type": "response.output_item.done", "item": {"type": "compaction", "encrypted_content": "blob"}});
    assert_eq!(
        compaction::output_item(done.to_string().as_bytes()),
        Some(fingerprint("blob"))
    );
    let msg = json!({"type": "response.output_item.done", "item": {"type": "message", "content": "compaction"}});
    assert_eq!(compaction::output_item(msg.to_string().as_bytes()), None);
    let completed = json!({"type": "response.completed", "response": {"output": [
        {"type": "reasoning"}, {"type": "compaction", "encrypted_content": "blob"}
    ]}});
    assert_eq!(
        compaction::completed_items(completed.to_string().as_bytes()),
        vec![fingerprint("blob")]
    );
}

fn facts<'a>(ms: &'a [(&'a str, CompactionSupport)]) -> Vec<MemberFacts<'a>> {
    ms.iter()
        .map(|(id, c)| MemberFacts {
            id,
            name: id,
            compaction: *c,
        })
        .collect()
}

#[test]
fn routing_keeps_compacted_conversations_with_their_maker() {
    use CompactionSupport::*;
    let owners = Owners::load(None);
    let ms = [("a", Unsupported), ("b", Native), ("c", Unknown)];
    let members = facts(&ms);
    // Nothing about compaction: anywhere.
    assert_eq!(
        compaction::route(&members, &CompactionInput::default(), &owners),
        Route::Any
    );
    // A compaction request skips the member that cannot compact.
    let trigger = CompactionInput {
        trigger: true,
        items: Vec::new(),
    };
    let r = compaction::route(&members, &trigger, &owners);
    assert!(!r.allows(0) && r.allows(1) && r.allows(2), "{r:?}");
    // An item of no known maker: likewise.
    let item = CompactionInput {
        trigger: false,
        items: vec![fingerprint("x")],
    };
    let r = compaction::route(&members, &item, &owners);
    assert!(!r.allows(0) && r.allows(1) && r.allows(2));
    // Its maker known: only that member, even when it is marked unsupported
    // (the item proves it made one).
    owners.learn(&[fingerprint("x")], "a", false);
    let r = compaction::route(&members, &item, &owners);
    assert_eq!(
        r,
        Route::Members {
            allowed: vec![true, false, false],
            kept_on: Some(0)
        }
    );
    // The newest item decides.
    owners.learn(&[fingerprint("y")], "c", false);
    let two = CompactionInput {
        trigger: false,
        items: vec![fingerprint("x"), fingerprint("y")],
    };
    assert!(compaction::route(&members, &two, &owners).allows(2));
    // A maker the router no longer has counts as unknown.
    owners.learn(&[fingerprint("z")], "gone", false);
    let gone = CompactionInput {
        trigger: false,
        items: vec![fingerprint("z")],
    };
    assert!(compaction::route(&members, &gone, &owners).allows(1));
    // Nobody can: refused with an explanation.
    let only = [("a", Unsupported)];
    match compaction::route(&facts(&only), &trigger, &owners) {
        Route::Refuse {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 400);
            assert_eq!(code, UNSUPPORTED_CODE);
            assert!(message.contains("new conversation"), "{message}");
        }
        r => panic!("{r:?}"),
    }
    // All members fine: no restriction.
    let fine = [("b", Native), ("c", Unknown)];
    assert_eq!(
        compaction::route(&facts(&fine), &trigger, &owners),
        Route::Any
    );
}

#[test]
fn owners_adopt_only_unknown_items_and_persist_without_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("router").join(compaction::OWNERS_FILE);
    let owners = Owners::load(Some(path.clone()));
    assert!(owners.is_empty());
    owners.learn(&[fingerprint("secret-blob")], "b", false);
    // Adopting never steals a known item.
    owners.learn(
        &[fingerprint("secret-blob"), fingerprint("other")],
        "a",
        true,
    );
    assert_eq!(
        owners.owner(&fingerprint("secret-blob")).as_deref(),
        Some("b")
    );
    assert_eq!(owners.owner(&fingerprint("other")).as_deref(), Some("a"));
    // A new compaction by another member moves it.
    owners.learn(&[fingerprint("other")], "b", false);
    assert_eq!(owners.owner(&fingerprint("other")).as_deref(), Some("b"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("secret-blob"));
    let again = Owners::load(Some(path.clone()));
    assert_eq!(
        again.owner(&fingerprint("secret-blob")).as_deref(),
        Some("b")
    );
    assert_eq!(again.len(), 2);
    std::fs::write(&path, b"{garbage").unwrap();
    assert!(Owners::load(Some(path)).is_empty());
}

// ---------------------------------------------------------------------------
// Through a running router
// ---------------------------------------------------------------------------

const TEXT_SSE: &str = concat!(
    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n",
    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"output\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
);
const COMPACTED_SSE: &str = concat!(
    "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r2\"}}\n\n",
    "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"enc-by-b\"}}\n\n",
    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r2\",\"output\":[{\"type\":\"compaction\",\"id\":\"cmp_1\",\"encrypted_content\":\"enc-by-b\"}],\"usage\":{\"input_tokens\":50,\"output_tokens\":9}}}\n\n",
);
const TRIGGER: &str = r#"{"model":"gpt-x","stream":true,"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]},{"type":"compaction_trigger"}]}"#;
const FOLLOW_UP: &str = r#"{"model":"gpt-x","stream":true,"input":[{"type":"compaction","encrypted_content":"enc-by-b"},{"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}]}"#;
const UNKNOWN_ITEM: &str = r#"{"model":"gpt-x","stream":true,"input":[{"type":"compaction","encrypted_content":"enc-before-router"},{"type":"message","role":"user","content":[{"type":"input_text","text":"next"}]}]}"#;
const PLAIN: &str = r#"{"model":"gpt-x","stream":true,"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}"#;

fn sse(body: &'static str) -> Mode {
    Mode::Sse {
        first: body,
        rest: "",
        gate: None,
        break_mid: false,
    }
}

fn two_members(a: &Upstream, b: &Upstream, a_ws: bool, b_ws: bool) -> RouterConfig {
    let mut ma = member(
        "a",
        MemberKind::Compatible,
        a_ws,
        vec![line("l", a.base("/v1"))],
    );
    ma.compaction = CompactionSupport::Unsupported;
    let mut mb = member(
        "b",
        MemberKind::Krill,
        b_ws,
        vec![line("l", b.base("/backend-api/codex"))],
    );
    mb.compaction = CompactionSupport::Native;
    let mut cfg = test_config(vec![ma, mb]);
    cfg.stats = Some(StatsConfig::default());
    cfg
}

async fn start_from(paths: &RouterPaths, cfg: &RouterConfig) -> Router {
    let secrets = secrets_for(cfg);
    cfg.save(paths).unwrap();
    secrets.save(paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    router
}

async fn stop(r: &Router, paths: &RouterPaths) {
    r.handle.shutdown();
    for _ in 0..500 {
        if !paths.runtime_json().exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_owner(paths: &RouterPaths, member: &str) {
    let file = paths.dir().join(compaction::OWNERS_FILE);
    for _ in 0..300 {
        if std::fs::read_to_string(&file).is_ok_and(|t| t.contains(&format!("\"{member}\""))) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no compaction owner {member} recorded");
}

async fn error_code(resp: reqwest::Response) -> (u16, String, String) {
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap();
    (
        status,
        v["error"]["code"].as_str().unwrap_or_default().to_owned(),
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_compaction_skips_unsupported_members_and_the_conversation_stays_with_its_maker() {
    let a = upstream(sse(TEXT_SSE)).await;
    let b = upstream(sse(COMPACTED_SSE)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let cfg = two_members(&a, &b, false, false);
    let r = start_from(&paths, &cfg).await;

    // An ordinary request goes to the first member.
    let resp = r.post(Some("s1"), PLAIN).await;
    assert_eq!(route(&resp), "a/l");
    resp.text().await.unwrap();
    assert_eq!(a.hits(), 1);

    // The session's compaction skips "a" (no remote compaction) for "b",
    // untouched.
    let resp = r.post(Some("s1"), TRIGGER).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "b/l");
    assert!(resp.text().await.unwrap().contains("enc-by-b"));
    assert_eq!(a.hits(), 1);
    assert_eq!(&b.seen()[0].body[..], TRIGGER.as_bytes());
    wait_for_owner(&paths, "b").await;

    // Its follow-ups go to "b" even from a session the router never saw.
    b.set(sse(TEXT_SSE));
    let resp = r.post(Some("fresh-session"), FOLLOW_UP).await;
    assert_eq!(route(&resp), "b/l");
    resp.text().await.unwrap();
    assert_eq!(a.hits(), 1);
    // A compaction item of unknown origin skips "a" too.
    let resp = r.post(Some("s2"), UNKNOWN_ITEM).await;
    assert_eq!(route(&resp), "b/l");
    resp.text().await.unwrap();
    assert_eq!(a.hits(), 1);

    let status = r.status().await;
    assert!(
        status
            .recent
            .iter()
            .any(|e| e.kind == RouteEventKind::Compaction),
        "{:?}",
        status.recent
    );

    // "b" down: no failover to "a", which cannot read the item.
    stop(&r, &paths).await;
    let mut cfg2 = cfg.clone();
    cfg2.members[1].lines[0].base_url = dead_base();
    let r = start_from(&paths, &cfg2).await;
    let (status, code, message) = error_code(r.post(Some("s3"), FOLLOW_UP).await).await;
    assert_eq!(status, 503);
    assert_eq!(code, OWNER_UNAVAILABLE_CODE);
    assert!(message.contains("“B”"), "{message}");
    assert_eq!(a.hits(), 1, "never sent to a member that cannot read it");
    stop(&r, &paths).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_member_that_compacts_the_router_refuses_with_an_explanation() {
    let a = upstream(sse(TEXT_SSE)).await;
    let mut ma = member(
        "a",
        MemberKind::Compatible,
        false,
        vec![line("l", a.base("/v1"))],
    );
    ma.compaction = CompactionSupport::Unsupported;
    let r = start(test_config(vec![ma])).await;
    let (status, code, message) = error_code(r.post(Some("s"), TRIGGER).await).await;
    assert_eq!(status, 400);
    assert_eq!(code, UNSUPPORTED_CODE);
    assert!(message.contains("remote compaction"), "{message}");
    let (status, code, _) = error_code(r.post(Some("s"), UNKNOWN_ITEM).await).await;
    assert_eq!((status, code.as_str()), (400, UNSUPPORTED_CODE));
    assert_eq!(a.hits(), 0);
    // Ordinary requests still go through.
    let resp = r.post(Some("s"), PLAIN).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(a.hits(), 1);
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(r: &Router, session: &str) -> (Ws, String) {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    h.insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    h.insert("session-id", session.parse().unwrap());
    h.insert("originator", "Codex Desktop".parse().unwrap());
    let (ws, resp) = tokio_tungstenite::connect_async(req).await.unwrap();
    let route = resp
        .headers()
        .get(superba_router::ROUTE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    (ws, route)
}

fn create(body: &str) -> String {
    let mut v: Value = serde_json::from_str(body).unwrap();
    v["type"] = json!("response.create");
    v.as_object_mut().unwrap().remove("stream");
    v.to_string()
}

fn answering() -> WsScript {
    Arc::new(|frame: &str| {
        let v: Value = serde_json::from_str(frame).unwrap_or_default();
        if v["type"] != "response.create" {
            return Vec::new();
        }
        vec![
            json!({"type": "response.created", "response": {"id": "w1"}}).to_string(),
            json!({"type": "response.completed", "response": {"id": "w1", "output": [], "usage": {"input_tokens": 3, "output_tokens": 1}}}).to_string(),
        ]
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_websocket_carrying_a_compacted_conversation_moves_to_its_maker() {
    let a = upstream(Mode::WsScript(answering())).await;
    let b = upstream(sse(COMPACTED_SSE)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let cfg = two_members(&a, &b, true, true);
    let r = start_from(&paths, &cfg).await;
    // "b" compacts the conversation over HTTP (and is remembered).
    let resp = r.post(Some("s-http"), TRIGGER).await;
    assert_eq!(route(&resp), "b/l");
    resp.text().await.unwrap();
    wait_for_owner(&paths, "b").await;
    b.set(Mode::WsScript(answering()));

    // A new socket of another session lands on "a"; its first request
    // carries "b"'s compaction item: not forwarded, the socket is closed for
    // a reconnect, and the reconnect goes to "b".
    let session = "019a7c00-0000-7000-8000-00000000c0de";
    let (mut ws, first) = connect(&r, session).await;
    assert_eq!(first, "a/l");
    ws.send(Message::Text(create(FOLLOW_UP).into()))
        .await
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("a frame in time");
    match msg {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 1012),
        other => panic!("expected a close for the reconnect, got {other:?}"),
    }
    assert!(
        a.frames().is_empty(),
        "the member that cannot read it got nothing"
    );
    let (mut ws, second) = connect(&r, session).await;
    assert_eq!(second, "b/l");
    ws.send(Message::Text(create(FOLLOW_UP).into()))
        .await
        .unwrap();
    let mut done = false;
    for _ in 0..4 {
        let Some(Ok(Message::Text(t))) = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
        else {
            continue;
        };
        if t.as_str().contains("response.completed") {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(b.frames().len(), 1);
    assert!(b.frames()[0].contains("enc-by-b"));
    stop(&r, &paths).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_websocket_compaction_nobody_can_serve_gets_an_error_event() {
    let a = upstream(Mode::WsScript(answering())).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let mut ma = member(
        "a",
        MemberKind::Compatible,
        true,
        vec![line("l", a.base("/v1"))],
    );
    ma.compaction = CompactionSupport::Unsupported;
    let mut cfg = test_config(vec![ma]);
    cfg.stats = Some(StatsConfig::default());
    let r = start_from(&paths, &cfg).await;
    let (mut ws, _) = connect(&r, "s-ws").await;
    ws.send(Message::Text(create(TRIGGER).into()))
        .await
        .unwrap();
    let Some(Ok(Message::Text(t))) = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
    else {
        panic!("expected an error event");
    };
    let v: Value = serde_json::from_str(t.as_str()).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["status"], 400);
    assert_eq!(v["error"]["code"], UNSUPPORTED_CODE);
    assert!(a.frames().is_empty());
    // The socket stays usable for ordinary requests.
    ws.send(Message::Text(create(PLAIN).into())).await.unwrap();
    let Some(Ok(Message::Text(t))) = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
    else {
        panic!("expected the answer");
    };
    assert!(t.as_str().contains("response.created"));
    assert_eq!(a.frames().len(), 1);
    stop(&r, &paths).await;
}
