#![allow(missing_docs)]
//! Transaction, authentication and fake-upstream tests for L2 compaction.
mod common;

use bytes::Bytes;
use common::*;
use serde_json::{Value, json};
use std::sync::Arc;
use superba_config::compaction_keys::{CompactionKeyStore, CompactionKeys};
use superba_core::{CompactionSupport, LocalContext, MapEnv};
use superba_router::compaction_proxy::{
    COMPACT_PROMPT, PREFIX, Scope, SyntheticModel, Transaction, expand,
};
use superba_router::{MemberKind, RouterConfig};

fn scope() -> Scope {
    Scope::new("a", b"account-a")
}
fn request() -> Value {
    json!({"model":"deepseek-test","stream":true,"input":[
        {"type":"message","role":"developer","content":[{"type":"input_text","text":"Keep policy"}]},
        {"type":"message","role":"user","internal_chat_message_metadata_passthrough":{"caller":"private"},"content":[{"type":"input_text","text":"Remember ORCHID-731"}]},
        {"type":"function_call","name":"read_file","call_id":"tool-1","arguments":"{}"},
        {"type":"function_call_output","call_id":"tool-1","output":"picture is blue"},
        {"type":"compaction_trigger"}],"previous_response_id":"must-not-forward","tools":[{"type":"function","name":"read_file"}],"metadata":{"private":"x"}})
}
fn capsule(keys: &CompactionKeys) -> Value {
    let mut req = request();
    let tx = Transaction::begin(&mut req, keys, &[scope()]).unwrap();
    assert_eq!(
        req["input"].as_array().unwrap().last().unwrap()["content"][0]["text"],
        COMPACT_PROMPT
    );
    assert_eq!(req["tools"], json!([]));
    assert!(req.get("previous_response_id").is_none());
    assert!(req.get("metadata").is_none());
    assert!(
        !req.to_string()
            .contains("internal_chat_message_metadata_passthrough")
    );
    assert_eq!(req["input"][2]["call_id"], req["input"][3]["call_id"]);
    tx.complete(
        keys,
        &scope(),
        "The user's reference is ORCHID-731; the picture is blue.".into(),
    )
    .unwrap()
}

#[test]
fn capsule_is_confidential_and_fork_resume_model_provider_switch_are_stateless() {
    let keys = CompactionKeys::ephemeral();
    let item = capsule(&keys);
    assert!(
        item["encrypted_content"]
            .as_str()
            .unwrap()
            .starts_with(PREFIX)
    );
    assert!(!item.to_string().contains("ORCHID"));
    for model in ["deepseek-test", "another-l2", "gpt-native"] {
        let mut next = json!({"model":model,"input":[item.clone(),{"role":"user","content":"Recall reference"}],"previous_response_id":"old"});
        expand(
            &mut next,
            &keys,
            &[scope(), Scope::new("b", b"account-b")],
            model != "gpt-native",
        )
        .unwrap();
        assert_eq!(next["input"][0]["role"], "assistant");
        assert!(
            next["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("ORCHID-731")
        );
        assert_eq!(next["input"][1]["content"], "Recall reference");
        assert!(next.get("previous_response_id").is_none());
        assert!(!next.to_string().contains(PREFIX));
    }
}

#[test]
fn coverage_deduplicates_retained_history_and_nested_summaries_but_keeps_policy_and_tail() {
    let keys = CompactionKeys::ephemeral();
    let item = capsule(&keys);
    let mut req = request();
    let input = req["input"].as_array_mut().unwrap();
    input.pop();
    input.push(item);
    input.push(json!({"role":"user","content":"tail-after-compaction"}));
    expand(&mut req, &keys, &[scope()], true).unwrap();
    assert_eq!(req["input"].as_array().unwrap().len(), 3);
    assert_eq!(req["input"][0]["role"], "developer");
    assert_eq!(req["input"][1]["role"], "assistant");
    assert_eq!(req["input"][2]["content"], "tail-after-compaction");

    let old = capsule(&keys);
    let mut second = json!({"model":"deepseek-test","input":[old.clone(),{"role":"user","content":"new fact"},{"type":"compaction_trigger"}]});
    let tx = Transaction::begin(&mut second, &keys, &[scope()]).unwrap();
    let new = tx
        .complete(&keys, &scope(), "ORCHID-731 plus new fact".into())
        .unwrap();
    let mut next = json!({"input":[old,new]});
    expand(&mut next, &keys, &[scope()], true).unwrap();
    assert_eq!(next["input"].as_array().unwrap().len(), 1);
}

#[test]
fn corruption_cross_tenant_unknown_versions_and_foreign_ciphertext_fail_closed() {
    let keys = CompactionKeys::ephemeral();
    let item = capsule(&keys);
    let original = json!({"input":[item.clone()]});
    for scopes in [
        vec![],
        vec![Scope::new("a", b"different-account")],
        vec![Scope::new("b", b"account-a")],
    ] {
        let mut req = original.clone();
        assert_eq!(
            expand(&mut req, &keys, &scopes, true).unwrap_err().code,
            "compaction_scope_mismatch"
        );
        assert_eq!(req, original, "errors must not partially mutate history");
    }
    assert!(
        expand(
            &mut original.clone(),
            &CompactionKeys::ephemeral(),
            &[scope()],
            true
        )
        .is_err()
    );
    let wire = item["encrypted_content"].as_str().unwrap();
    for bad in [
        format!("{wire}garbage"),
        wire.replacen(":1:", ":7:", 1),
        wire[..wire.len() / 2].to_owned(),
    ] {
        assert!(
            expand(
                &mut json!({"input":[{"type":"compaction","encrypted_content":bad}]}),
                &keys,
                &[scope()],
                true
            )
            .is_err()
        );
    }
    let foreign = json!({"input":[{"type":"compaction","encrypted_content":"native-ciphertext"}]});
    let mut native = foreign.clone();
    assert!(!expand(&mut native, &keys, &[scope()], false).unwrap());
    assert_eq!(native, foreign);
    assert_eq!(
        expand(&mut foreign.clone(), &keys, &[scope()], true)
            .unwrap_err()
            .code,
        "compaction_foreign_ciphertext"
    );
    assert!(
        expand(
            &mut json!({"input":[{"type":"reasoning","encrypted_content":"foreign"}]}),
            &keys,
            &[scope()],
            true
        )
        .is_err()
    );
}

fn store(tmp: &tempfile::TempDir) -> CompactionKeyStore {
    let mut ctx = LocalContext::for_tests(tmp.path());
    ctx.env = Arc::new(MapEnv::new().with("SUPERBA_SECRET_BACKEND", "file"));
    CompactionKeyStore::new(ctx)
}

#[test]
fn vault_restart_rotation_retirement_and_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let first = store.load().unwrap();
    let item = capsule(&first);
    let old_id = first.active().0.to_owned();
    let second = store.rotate().unwrap();
    assert_ne!(first.active().0, second.active().0);
    assert_eq!(first.owner(), second.owner());
    let restored = store.load().unwrap();
    expand(
        &mut json!({"input":[item.clone()]}),
        &restored,
        &[scope()],
        true,
    )
    .unwrap();
    let retired = store.retire(&old_id).unwrap();
    assert_eq!(
        expand(&mut json!({"input":[item]}), &retired, &[scope()], true)
            .unwrap_err()
            .code,
        "compaction_key_unavailable"
    );
    assert!(store.retire(retired.active().0).is_err());
    let file = dir.path().join("data/secrets/compaction-keys.enc");
    let saved = std::fs::read(&file).unwrap();
    assert!(!saved.windows(old_id.len()).any(|w| w == old_id.as_bytes()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&file, b"corrupt").unwrap();
    assert!(store.load().is_err());
}

#[test]
fn concurrent_vault_creation_and_rotation_preserves_all_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let jobs: Vec<_> = (0..6)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || store.rotate().unwrap())
        })
        .collect();
    let rings: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    let final_ring = store.load().unwrap();
    for r in rings {
        assert_eq!(r.owner(), final_ring.owner());
        assert!(final_ring.key(r.active().0).is_some());
    }
}

#[test]
fn malformed_triggers_empty_summaries_and_retry_do_not_commit_empty_history() {
    let keys = CompactionKeys::ephemeral();
    for input in [
        json!([]),
        json!([{"type":"compaction_trigger"}]),
        json!([{"type":"compaction_trigger"},{"role":"user","content":"late"}]),
        json!([{"type":"compaction_trigger"},{"type":"compaction_trigger"}]),
    ] {
        assert!(
            Transaction::begin(&mut json!({"model":"m","input":input}), &keys, &[scope()]).is_err()
        );
    }
    let tx = Transaction::begin(&mut request(), &keys, &[scope()]).unwrap();
    assert!(tx.complete(&keys, &scope(), "  \n".into()).is_err());
    for _ in 0..2 {
        assert!(
            tx.complete(&keys, &scope(), "same retry summary".into())
                .is_ok()
        );
    }
    assert!(!format!("{tx:?}").contains("ORCHID"));
}

const SUMMARY: &str = r#"{"id":"summary","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Reference ORCHID-731. The picture is blue."}]}],"usage":{"input_tokens":200,"output_tokens":20,"total_tokens":220}}"#;
fn config(up: &Upstream) -> RouterConfig {
    let mut cfg = test_config(vec![member(
        "a",
        MemberKind::Krill,
        true,
        vec![line("one", up.base("/native"))],
    )]);
    cfg.members[0].compaction = CompactionSupport::Unsupported;
    cfg.openai = Some(superba_router::OpenAiConfig {
        members: [(
            "a".into(),
            superba_router::openai::OpenAiMember {
                responses: Some(superba_router::openai::OpenAiEndpoint::at("/v1")),
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    });
    cfg.synthetic_models.push(SyntheticModel {
        slug: "deepseek-test".into(),
        member: "a".into(),
        model: "deepseek-upstream".into(),
    });
    cfg
}
async fn post(router: &Router, req: Value) -> reqwest::Response {
    router
        .http
        .post(router.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .json(&req)
        .send()
        .await
        .unwrap()
}
fn item(sse: &str) -> Value {
    sse.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|e| e["type"] == "response.output_item.done")
        .unwrap()["item"]
        .clone()
}

#[tokio::test]
async fn fake_upstream_roundtrip_uses_standard_endpoint_and_does_not_leak_internal_items() {
    let up = upstream(Mode::Json(200, SUMMARY)).await;
    let router = start(config(&up)).await;
    let resp = post(&router, request()).await;
    assert_eq!(resp.status(), 200);
    let sse = resp.text().await.unwrap();
    let compact = item(&sse);
    assert_eq!(compact["type"], "compaction");
    assert!(sse.contains("response.completed"));
    let sent = &up.seen()[0];
    assert_eq!(sent.path, "/v1/responses");
    let req: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(req["model"], "deepseek-upstream");
    assert!(!req.to_string().contains("compaction_trigger"));
    assert!(req.to_string().contains("CONTEXT CHECKPOINT COMPACTION"));
    let next = json!({"model":"deepseek-test","input":[compact,{"role":"user","content":"What was my reference?"}]});
    assert_eq!(post(&router, next).await.status(), 200);
    let req: Value = serde_json::from_slice(&up.seen()[1].body).unwrap();
    assert_eq!(req["input"][0]["role"], "assistant");
    assert!(req.to_string().contains("ORCHID-731"));
    assert!(!req.to_string().contains(PREFIX));
    router.handle.shutdown();
}

#[tokio::test]
async fn empty_failed_truncated_refused_or_tool_summary_is_never_a_successful_compaction() {
    let up = upstream(Mode::Json(200, SUMMARY)).await;
    let router = start(config(&up)).await;
    for reply in [
        r#"{"status":"completed","output":[]}"#,
        r#"{"status":"incomplete","output":[]}"#,
        r#"{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]}"#,
        r#"{"status":"completed","output":[{"type":"function_call","name":"exec"}]}"#,
    ] {
        up.set(Mode::Json(200, reply));
        let resp = post(&router, request()).await;
        assert_eq!(resp.status(), 502);
        assert_eq!(
            resp.json::<Value>().await.unwrap()["error"]["code"],
            "compaction_failed"
        );
    }
    up.set(Mode::Sse { first:"event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{}}\n\n",rest:"",gate:None,break_mid:false });
    assert_eq!(post(&router, request()).await.status(), 502);
    up.set(Mode::Json(200, SUMMARY));
    assert_eq!(
        post(&router, request()).await.status(),
        200,
        "retry remains possible"
    );
    router.handle.shutdown();
}

#[tokio::test]
async fn compressed_foreign_capsules_are_rejected_and_native_gpt_trigger_is_unchanged() {
    let up = upstream(Mode::Json(200, SUMMARY)).await;
    let mut cfg = config(&up);
    cfg.members[0].compaction = CompactionSupport::Unknown;
    let router = start(cfg).await;
    let foreign = json!({"model":"deepseek-test","input":[{"type":"compaction","encrypted_content":"foreign"}]}).to_string();
    let compressed = zstd::stream::encode_all(foreign.as_bytes(), 1).unwrap();
    let resp = router
        .http
        .post(router.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-encoding", "zstd")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(up.hits(), 0);
    let mut native = request();
    native["model"] = json!("gpt-native");
    assert_eq!(post(&router, native.clone()).await.status(), 200);
    assert_eq!(up.seen()[0].path, "/native/responses");
    assert_eq!(
        serde_json::from_slice::<Value>(&up.seen()[0].body).unwrap(),
        native
    );
    router.handle.shutdown();
}

#[tokio::test]
async fn cancelled_compaction_has_no_committed_capsule_and_can_be_retried() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let up = upstream(Mode::Sse {
        first: "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        rest: "",
        gate: Some(gate.clone()),
        break_mid: false,
    })
    .await;
    let router = start(config(&up)).await;
    let req = router
        .http
        .post(router.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .json(&request())
        .send();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), req)
            .await
            .is_err()
    );
    gate.notify_waiters();
    up.set(Mode::Json(200, SUMMARY));
    let resp = post(&router, request()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(item(&resp.text().await.unwrap())["type"], "compaction");
    router.handle.shutdown();
}

#[tokio::test]
async fn sse_terminal_summary_and_usage_roundtrip() {
    let response: Value = serde_json::from_str(SUMMARY).unwrap();
    let event = format!(
        "data: {}\r\n\r\n",
        json!({"type":"response.completed","response":response})
    );
    let up = upstream(Mode::Chunks(Arc::new(vec![Bytes::from(event)]))).await;
    let router = start(config(&up)).await;
    let mut req = request();
    req["stream"] = json!(false);
    let resp = post(&router, req).await;
    assert_eq!(resp.status(), 200);
    let json: Value = resp.json().await.unwrap();
    assert_eq!(json["output"][0]["type"], "compaction");
    assert_eq!(json["usage"]["total_tokens"], 220);
    router.handle.shutdown();
}

#[test]
fn duplicate_capsule_is_idempotent_and_images_keep_their_prefix_boundary() {
    let keys = CompactionKeys::ephemeral();
    let mut req = request();
    let image = json!({"type":"message","role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,fixture"}]});
    req["input"]
        .as_array_mut()
        .unwrap()
        .insert(1, image.clone());
    let tx = Transaction::begin(&mut req, &keys, &[scope()]).unwrap();
    assert_eq!(req["input"][1], image);
    let item = tx
        .complete(
            &keys,
            &scope(),
            "Reference ORCHID-731; image depicts a blue square".into(),
        )
        .unwrap();
    let mut next = json!({"input":[image.clone(),item.clone(),item,image]});
    expand(&mut next, &keys, &[scope()], true).unwrap();
    assert_eq!(
        next["input"].as_array().unwrap().len(),
        2,
        "covered image removed; new copy after the capsule remains"
    );
}

async fn start_files(cfg: &RouterConfig, root: &std::path::Path) -> Router {
    let paths = superba_router::RouterPaths::new(&superba_core::DataDir::new(root));
    cfg.save(&paths).unwrap();
    secrets_for(cfg).save(&paths).unwrap();
    let server = superba_router::RouterServer::bind_from_files(paths)
        .await
        .unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().into(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    router
}

#[tokio::test]
async fn file_backed_resume_rotates_keys_and_switches_provider_without_forwarding_capsules() {
    let dir = tempfile::tempdir().unwrap();
    let keys = store(&dir);
    keys.load().unwrap(); // File-only vault before starting the server.
    let a = upstream(Mode::Json(200, SUMMARY)).await;
    let b = upstream(Mode::Json(200, SUMMARY)).await;
    let mut cfg = config(&a);
    cfg.members.push(member(
        "b",
        MemberKind::Compatible,
        false,
        vec![line("other", b.base("/standard"))],
    ));
    cfg.synthetic_models.push(SyntheticModel {
        slug: "other-model".into(),
        member: "b".into(),
        model: "actual-other-model".into(),
    });
    let first = start_files(&cfg, &dir.path().join("data")).await;
    let capsule = item(&post(&first, request()).await.text().await.unwrap());
    let old_key = keys.load().unwrap().active().0.to_owned();
    first.handle.shutdown();
    keys.rotate().unwrap();
    let second = start_files(&cfg, &dir.path().join("data")).await;
    let response = post(
        &second,
        json!({"model":"other-model","input":[capsule.clone(),{"role":"user","content":"Recall"}]}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let received: Value = serde_json::from_slice(&b.seen()[0].body).unwrap();
    assert_eq!(received["model"], "actual-other-model");
    assert!(received.to_string().contains("ORCHID-731"));
    assert!(!received.to_string().contains(PREFIX));
    keys.retire(&old_key).unwrap();
    let response = post(&second, json!({"model":"other-model","input":[capsule]})).await;
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "compaction_key_unavailable"
    );
    assert_eq!(b.hits(), 1, "retired envelopes never leave the router");
    second.handle.shutdown();
}

#[test]
fn encrypted_function_arguments_cannot_be_silently_summarized_as_plaintext() {
    let keys = CompactionKeys::ephemeral();
    let mut req = json!({"model":"deepseek-test","input":[{"type":"function_call","name":"task","call_id":"a","arguments":"{\"prompt\":\"opaque\"}","encrypted_function_args":["prompt"]},{"type":"compaction_trigger"}]});
    assert_eq!(
        Transaction::begin(&mut req, &keys, &[scope()])
            .unwrap_err()
            .code,
        "compaction_foreign_ciphertext"
    );
}

fn mixed_catalog_reply(native: bool) -> Mode {
    Mode::PathReply(Arc::new(move |path, _, body| {
        if path.ends_with("/models") {
            return if path == "/native/models" {
                (
                    200,
                    json!({"models":if native {vec![json!({"slug":"deepseek-test","display_name":"Native","context_window":128000,"shell_type":"unified_exec","visibility":"list","supported_reasoning_levels":[],"experimental_supported_tools":[],"supported_in_api":true,"support_verbosity":false,"priority":1,"truncation_policy":{"mode":"bytes","limit":10000},"base_instructions":"Use tools when necessary."})]} else {vec![]}}),
                )
            } else {
                (200, json!({"data":[{"id":"deepseek-test"}]}))
            };
        }
        let req: Value = serde_json::from_slice(body).unwrap();
        if req["tool_choice"]["type"] == "function" {
            return (
                200,
                json!({"status":"completed","output":[{"type":"function_call","name":"get_probe_value","call_id":"probe","arguments":"{}"}]}),
            );
        }
        if let Some(output) = req["input"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["type"] == "function_call_output"))
        {
            return (
                200,
                json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":output["output"]}]}]}),
            );
        }
        (200, serde_json::from_str(SUMMARY).unwrap())
    }))
}

fn mixed_config(native: &Upstream, synthetic: &Upstream, native_first: bool) -> RouterConfig {
    let mut members = vec![
        member(
            "native",
            MemberKind::Krill,
            true,
            vec![line("one", native.base("/native"))],
        ),
        member(
            "synthetic",
            MemberKind::Krill,
            true,
            vec![line("one", synthetic.base("/native"))],
        ),
    ];
    members[0].compaction = CompactionSupport::Native;
    members[1].compaction = CompactionSupport::Unsupported;
    if !native_first {
        members.reverse();
    }
    let mut cfg = test_config(members);
    cfg.catalog = Some(superba_router::merged_catalog::CatalogConfig {
        members: ["native", "synthetic"]
            .into_iter()
            .map(|id| {
                (
                    id.into(),
                    superba_router::merged_catalog::MemberCatalog {
                        name: id.into(),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    // Once catalog selection exists it must win over this obsolete seam.
    cfg.synthetic_models.push(SyntheticModel {
        slug: "deepseek-test".into(),
        member: "native".into(),
        model: "must-not-win".into(),
    });
    cfg
}

async fn publish_mixed(router: &Router, native: &Upstream, synthetic: &Upstream) {
    let test: superba_core::RouterModelTest = router
        .http
        .post(router.url("/_superba/v1/catalog/test"))
        .bearer_auth(&router.admin_token)
        .json(&json!({"model":"synthetic/deepseek-test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(test.passed, "{}", test.detail);
    // The L2 side must be independently probed even when the same slug is L1
    // on the other provider. Only its standard endpoint may receive the probe.
    let calls: Vec<_> = synthetic
        .seen()
        .into_iter()
        .filter(|s| s.method == "POST")
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|s| s.path == "/v1/responses"));
    native.seen.lock().unwrap().clear();
    synthetic.seen.lock().unwrap().clear();
}

#[tokio::test]
async fn mixed_slug_native_fails_over_to_synthetic_then_capsule_fails_over_to_native() {
    let dir = tempfile::tempdir().unwrap();
    let keys = store(&dir);
    keys.load().unwrap();
    let native = upstream(mixed_catalog_reply(true)).await;
    let synthetic = upstream(mixed_catalog_reply(false)).await;
    let mut cfg = mixed_config(&native, &synthetic, true);
    cfg.stats = Some(Default::default());
    let router = start_files(&cfg, &dir.path().join("data")).await;
    publish_mixed(&router, &native, &synthetic).await;
    native.set(Mode::Json(503, r#"{"error":"offline"}"#));
    synthetic.set(Mode::Json(200, SUMMARY));
    let resp = router.post(Some("mixed"), &request().to_string()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "synthetic/one");
    let compact = item(&resp.text().await.unwrap());
    let original: Value = serde_json::from_slice(&native.seen()[0].body).unwrap();
    assert_eq!(native.seen()[0].path, "/native/responses");
    assert_eq!(
        original["input"].as_array().unwrap().last().unwrap()["type"],
        "compaction_trigger"
    );
    assert_eq!(original["previous_response_id"], "must-not-forward");
    assert!(!original.to_string().contains(COMPACT_PROMPT));
    let summary: Value = serde_json::from_slice(&synthetic.seen()[0].body).unwrap();
    assert_eq!(synthetic.seen()[0].path, "/v1/responses");
    assert_eq!(summary["model"], "deepseek-test");
    assert_eq!(
        summary["input"].as_array().unwrap().last().unwrap()["content"][0]["text"],
        COMPACT_PROMPT
    );
    assert!(!summary.to_string().contains("compaction_trigger"));
    assert!(summary.get("previous_response_id").is_none());
    let source = Scope::new(
        "synthetic",
        format!("Bearer {}", key_of("synthetic")).as_bytes(),
    );
    expand(
        &mut json!({"input":[compact.clone()]}),
        &keys.load().unwrap(),
        &[source],
        true,
    )
    .unwrap();
    assert_eq!(
        expand(
            &mut json!({"input":[compact.clone()]}),
            &keys.load().unwrap(),
            &[Scope::new(
                "native",
                format!("Bearer {}", key_of("native")).as_bytes()
            )],
            false
        )
        .unwrap_err()
        .code,
        "compaction_scope_mismatch"
    );

    // Even with persistent ownership statistics and a sticky L2 session, an
    // authenticated router capsule can fail over to the recovered native peer.
    native.set(Mode::Json(200, SUMMARY));
    synthetic.set(Mode::Json(503, r#"{"error":"offline"}"#));
    let next = json!({"model":"deepseek-test","stream":true,"input":[compact,{"role":"user","content":"Recall reference"}]});
    let resp = router.post(Some("mixed"), &next.to_string()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "native/one");
    resp.bytes().await.unwrap();
    assert_eq!(native.hits(), 2);
    assert_eq!(synthetic.hits(), 2);
    for up in [&native, &synthetic] {
        let sent: Value = serde_json::from_slice(&up.seen()[1].body).unwrap();
        assert_eq!(sent["input"][0]["role"], "assistant");
        assert!(sent.to_string().contains("ORCHID-731"));
        assert!(!sent.to_string().contains(PREFIX));
    }
    router.handle.shutdown();
}

#[tokio::test]
async fn mixed_slug_synthetic_fails_over_to_native_without_summary_or_header_mutations() {
    let native = upstream(mixed_catalog_reply(true)).await;
    let synthetic = upstream(mixed_catalog_reply(false)).await;
    let router = start(mixed_config(&native, &synthetic, false)).await;
    publish_mixed(&router, &native, &synthetic).await;
    synthetic.set(Mode::Json(503, r#"{"error":"offline"}"#));
    const NATIVE: &str = r#"{"status":"completed","output":[{"type":"compaction","encrypted_content":"native-ciphertext"}]}"#;
    native.set(Mode::Json(200, NATIVE));
    let resp = router
        .http
        .post(router.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("x-codex-turn-state", "native-state")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "native/one");
    assert_eq!(
        resp.json::<Value>().await.unwrap(),
        serde_json::from_str::<Value>(NATIVE).unwrap()
    );
    let sent = &native.seen()[0];
    assert_eq!(sent.path, "/native/responses");
    assert_eq!(sent.headers["x-codex-turn-state"], "native-state");
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body["input"], request()["input"]);
    assert_eq!(body["tools"], request()["tools"]);
    assert_eq!(body["previous_response_id"], "must-not-forward");
    assert_eq!(synthetic.seen()[0].path, "/v1/responses");
    assert!(
        !synthetic.seen()[0]
            .headers
            .contains_key("x-codex-turn-state")
    );
    router.handle.shutdown();
}

#[tokio::test]
async fn mixed_slug_foreign_ciphertext_skips_synthetic_but_remains_intact_for_native() {
    let native = upstream(mixed_catalog_reply(true)).await;
    let synthetic = upstream(mixed_catalog_reply(false)).await;
    let router = start(mixed_config(&native, &synthetic, false)).await;
    publish_mixed(&router, &native, &synthetic).await;
    native.set(Mode::Json(200, SUMMARY));
    let mut req = json!({"model":"deepseek-test","input":[{"type":"compaction","encrypted_content":"native-ciphertext"},{"role":"user","content":"Recall"}]});
    assert_eq!(post(&router, req.clone()).await.status(), 200);
    assert_eq!(synthetic.hits(), 0);
    let sent: Value = serde_json::from_slice(&native.seen()[0].body).unwrap();
    assert_eq!(sent["input"], req["input"]);
    req["model"] = json!("synthetic/deepseek-test");
    let resp = post(&router, req).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "compaction_foreign_ciphertext"
    );
    assert_eq!(synthetic.hits(), 0);
    assert_eq!(native.hits(), 1, "provider namespace must not fall through");
    router.handle.shutdown();
}

#[tokio::test]
async fn mixed_slug_does_not_promote_unsupported_native_compaction() {
    let native = upstream(mixed_catalog_reply(true)).await;
    let synthetic = upstream(mixed_catalog_reply(false)).await;
    let mut cfg = mixed_config(&native, &synthetic, true);
    cfg.members[0].compaction = CompactionSupport::Unsupported;
    let router = start(cfg).await;
    publish_mixed(&router, &native, &synthetic).await;
    synthetic.set(Mode::Json(200, SUMMARY));
    assert_eq!(post(&router, request()).await.status(), 200);
    assert_eq!(native.hits(), 0);
    synthetic.set(Mode::Json(503, r#"{"error":"offline"}"#));
    assert_eq!(post(&router, request()).await.status(), 502);
    assert_eq!(
        native.hits(),
        0,
        "a synthetic alternative must not enable native compaction"
    );
    router.handle.shutdown();
}
