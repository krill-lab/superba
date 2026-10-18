//! Remote compaction check (GitHub issue 3): the request is Codex's V2
//! compaction (a `compaction_trigger` item on `…/responses`), and the answer
//! shapes of native, unsupported and inconclusive providers are told apart.

mod support;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_core::{CompactionSupport, Error, LineProber};

use support::*;

fn sse(events: &[Value]) -> MockResponse {
    let mut body = String::new();
    for e in events {
        body.push_str(&format!(
            "event: {}\ndata: {}\n\n",
            e["type"].as_str().unwrap(),
            e
        ));
    }
    MockResponse::sse(body)
}

fn compaction_item() -> Value {
    json!({"type": "compaction", "id": "cmp_1", "encrypted_content": "gAAAAB-opaque"})
}

fn completed(output: Value) -> Value {
    json!({"type": "response.completed", "response": {"id": "resp_1", "output": output}})
}

async fn check(
    handler: impl Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static,
) -> (superba_core::CompactionCheck, MockServer) {
    let server = MockServer::start(move |r| {
        if r.path == RESPONSES {
            handler(r)
        } else {
            MockResponse::empty(404)
        }
    })
    .await;
    let c = prober()
        .probe_compaction(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    (c, server)
}

#[tokio::test]
async fn the_check_sends_codex_s_compaction_request_to_responses() {
    let (c, server) = check(|_| {
        sse(&[
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": compaction_item()}),
            completed(json!([compaction_item()])),
        ])
    })
    .await;
    assert_eq!(c.support, CompactionSupport::Native);
    assert_eq!(c.status, Some(200));
    let requests = server.recorded();
    assert_eq!(requests.len(), 1, "never the legacy /responses/compact");
    let r = &requests[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, RESPONSES);
    assert_eq!(r.header("accept"), Some("text/event-stream"));
    let body: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(body["model"], "gpt-x");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[1]["role"], "assistant");
    assert_eq!(input[2], json!({"type": "compaction_trigger"}));
    assert_authenticated(&server);
}

#[tokio::test]
async fn an_item_only_in_the_completed_output_or_named_compaction_summary_counts() {
    let (c, _) = check(|_| sse(&[completed(json!([compaction_item()]))])).await;
    assert_eq!(c.support, CompactionSupport::Native);
    let (c, _) = check(|_| {
        sse(&[
            json!({"type": "response.output_item.done", "item": {"type": "compaction_summary", "encrypted_content": "x"}}),
            completed(json!([])),
        ])
    })
    .await;
    assert_eq!(c.support, CompactionSupport::Native);
    // A JSON (non-streamed) answer is judged by its output.
    let (c, _) =
        check(|_| MockResponse::json(200, json!({"id": "r", "output": [compaction_item()]}))).await;
    assert_eq!(c.support, CompactionSupport::Native);
}

#[tokio::test]
async fn a_provider_that_ignores_the_trigger_is_unsupported() {
    // A normal reply: the trigger was dropped and the model just answered.
    let (c, _) = check(|_| {
        sse(&[
            json!({"type": "response.output_item.done", "item": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "pong"}]}}),
            completed(json!([{"type": "message"}])),
        ])
    })
    .await;
    assert_eq!(c.support, CompactionSupport::Unsupported);
    assert!(c.detail.unwrap().contains("without a compaction item"));
    // Two items: Codex expects exactly one.
    let (c, _) = check(|_| sse(&[completed(json!([compaction_item(), compaction_item()]))])).await;
    assert_eq!(c.support, CompactionSupport::Unsupported);
    let (c, _) = check(|_| MockResponse::json(200, json!({"output": []}))).await;
    assert_eq!(c.support, CompactionSupport::Unsupported);
}

#[tokio::test]
async fn refusals_that_name_the_request_are_unsupported() {
    let refusals = [
        (
            400,
            json!({"error": {"message": "Invalid value: 'compaction_trigger'. Supported values are: 'message', 'function_call'.", "type": "invalid_request_error", "param": "input[2].type"}}),
        ),
        (422, json!({"detail": "input.2.type: unknown item type"})),
        (
            400,
            json!({"error": {"message": "remote compaction is not supported by this provider"}}),
        ),
        (
            404,
            json!({"error": {"message": "compaction is not available"}}),
        ),
        (501, json!({"error": "compaction not implemented"})),
    ];
    for (status, body) in refusals {
        let b = body.clone();
        let (c, _) = check(move |_| MockResponse::json(status, b.clone())).await;
        assert_eq!(c.support, CompactionSupport::Unsupported, "{status} {body}");
        assert_eq!(c.status, Some(status));
    }
    // A failed stream that names the item.
    let (c, _) = check(|_| {
        sse(&[json!({"type": "response.failed", "response": {"error": {"message": "unsupported input item type compaction_trigger"}}})])
    })
    .await;
    assert_eq!(c.support, CompactionSupport::Unsupported);
    let (c, _) = check(|_| {
        sse(&[json!({"type": "error", "message": "Unknown input item: compaction_trigger"})])
    })
    .await;
    assert_eq!(c.support, CompactionSupport::Unsupported);
}

#[tokio::test]
async fn answers_that_do_not_tell_are_unknown() {
    let cases: Vec<(u16, MockResponse)> = vec![
        (401, relay_401()),
        (
            403,
            MockResponse::json(403, json!({"error": {"message": "forbidden"}})),
        ),
        (
            429,
            MockResponse::json(429, json!({"error": {"message": "slow down"}})),
        ),
        (
            500,
            MockResponse::json(500, json!({"error": {"message": "boom"}})),
        ),
        (503, MockResponse::empty(503)),
        // No Responses endpoint at all: says nothing about compaction.
        (
            404,
            MockResponse::json(
                404,
                json!({"error": {"message": "Invalid URL (POST /v1/responses)"}}),
            ),
        ),
        (405, MockResponse::empty(405)),
        // The check's model was refused.
        (
            400,
            MockResponse::json(
                400,
                json!({"error": {"message": "The model `gpt-x` does not exist"}}),
            ),
        ),
        // The relay's edge intercepted the request.
        (200, edge_block()),
    ];
    for (status, response) in cases {
        let r = response.clone();
        let (c, _) = check(move |_| r.clone()).await;
        assert_eq!(
            c.support,
            CompactionSupport::Unknown,
            "{status}: {:?}",
            c.detail
        );
        assert_eq!(c.status, Some(status));
        assert!(c.detail.is_some());
    }
    // A stream that stops before response.completed.
    let (c, _) = check(|_| {
        MockResponse::sse("event: response.created\ndata: {\"type\":\"response.created\"}\n\n")
    })
    .await;
    assert_eq!(c.support, CompactionSupport::Unknown);
}

#[tokio::test]
async fn an_unreachable_provider_is_unknown_and_no_key_leaks() {
    let origin = closed_origin().await;
    let endpoints = superba_core::ResolvedEndpoints::for_line(
        &superba_core::Origin::parse(&origin).unwrap(),
        superba_core::CodexPath::BackendApiCodex,
        superba_core::AuthMode::ApiKey,
    )
    .unwrap();
    let c = prober()
        .probe_compaction(&endpoints, &key(), "gpt-x")
        .await
        .unwrap();
    assert_eq!(c.support, CompactionSupport::Unknown);
    assert_eq!(c.status, None);
    assert!(!c.detail.unwrap_or_default().contains(KEY));
    // A key echoed in an error body is redacted.
    let (c, _) = check(|_| {
        MockResponse::json(
            400,
            json!({"error": {"message": format!("unknown input item for key {KEY}")}}),
        )
    })
    .await;
    assert!(!c.detail.unwrap().contains(KEY));
}

#[tokio::test]
async fn an_empty_model_is_refused_without_a_request() {
    let server = MockServer::start(|_| MockResponse::empty(200)).await;
    let e = prober()
        .probe_compaction(&endpoints(&server), &key(), "  ")
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e:?}");
    assert!(server.recorded().is_empty());
}
