//! The router never logs keys, tokens, bodies or session ids, at any level.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::*;
use superba_router::MemberKind;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

const SECRET_BODY: &str = r#"{"model":"gpt-x","input":"body-marker-7f3a"}"#;
const SESSION: &str = "session-marker-91c2";

// `#[tokio::test]` runs on one thread, so the thread-local subscriber sees every
// task the router spawns.
#[tokio::test]
async fn logs_never_contain_secrets_or_bodies() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let good = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let flaky = upstream(Mode::Sse {
        first: "data: x\n\n",
        rest: "",
        gate: None,
        break_mid: true,
    })
    .await;
    let mut site = member(
        "site",
        MemberKind::Compatible,
        false,
        vec![line("c", flaky.base("/v1")), line("d", good.base("/v1"))],
    );
    site.model_map.insert("gpt-x".into(), "vendor-x".into());
    let r = start(test_config(vec![
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", dead_base()), line("b", resetter().await)],
        ),
        site,
    ]))
    .await;
    let mut resp = r.post(Some(SESSION), SECRET_BODY).await;
    while let Ok(Some(_)) = resp.chunk().await {}
    r.post(Some(SESSION), SECRET_BODY)
        .await
        .text()
        .await
        .unwrap();
    r.http
        .post(r.url("/backend-api/codex/responses?q=query-marker-55"))
        .bearer_auth("wrong-token-marker")
        .body(SECRET_BODY)
        .send()
        .await
        .unwrap();
    r.status().await;

    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("route event"), "nothing captured:\n{text}");
    for needle in [
        key_of("krill"),
        key_of("site"),
        ROUTER_TOKEN.to_owned(),
        r.admin_token.clone(),
        "body-marker-7f3a".to_owned(),
        SESSION.to_owned(),
        "query-marker-55".to_owned(),
        "wrong-token-marker".to_owned(),
    ] {
        assert!(!text.contains(&needle), "log contains {needle}:\n{text}");
    }
}

// With request recording and the stats line on, logs still
// carry no key, token, thread or turn id, cache key, prompt text or body bytes.
#[tokio::test]
async fn logs_stay_clean_while_recording() {
    use common::responses::{sse, text_answer};
    use superba_router::paths::RouterPaths;
    use superba_router::{RouterServer, StatsConfig, StatsLineConfig};

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let answer: &'static str = Box::leak(sse(&text_answer(), false).into_boxed_str());
    let good = upstream(Mode::Sse {
        first: answer,
        rest: "",
        gate: None,
        break_mid: false,
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&superba_core::DataDir::new(dir.path()));
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![
            line("a", dead_base()),
            line("b", good.base("/backend-api/codex")),
        ],
    )]);
    cfg.stats = Some(StatsConfig {
        line: Some(StatsLineConfig {
            template: "⚡ {tps} tok/s".into(),
            style: Default::default(),
            originators: vec!["Codex Desktop".into()],
        }),
        ..StatsConfig::default()
    });
    secrets_for(&cfg).save(&paths).unwrap();
    cfg.save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let addr = server.local_addr();
    let admin = server.admin_token().expose().to_owned();
    let handle = server.shutdown_handle();
    tokio::spawn(server.serve());

    let body = serde_json::json!({
        "model": "gpt-x",
        "prompt_cache_key": "cachekey-marker-5e1d",
        "input": [
            {"role": "user", "content": [{"type": "input_text", "text": "prompt-marker-31bd"}]},
            {"role": "assistant", "content": [{"type": "output_text", "text": "old-answer-marker\n\n> ⚡\u{2060} 1 tok/s"}]}
        ]
    });
    let turn = serde_json::json!({
        "thread_id": "thread-marker-77aa",
        "turn_id": "turn-marker-90fe",
        "request_kind": "turn",
        "thread_source": "user"
    });
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = http
        .post(format!("http://{addr}/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("originator", "Codex Desktop")
        .header("session-id", SESSION)
        .header("x-codex-turn-metadata", turn.to_string())
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .unwrap();
    let text = resp.text().await.unwrap();
    assert!(text.contains("⚡\u{2060} "), "the line was added");
    let _ = http
        .get(format!("http://{addr}/_superba/v1/requests"))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    handle.shutdown();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while paths.runtime_json().exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("route event"), "nothing captured:\n{logs}");
    for needle in [
        key_of("krill"),
        ROUTER_TOKEN.to_owned(),
        admin.clone(),
        SESSION.to_owned(),
        "prompt-marker-31bd".to_owned(),
        "old-answer-marker".to_owned(),
        "thread-marker-77aa".to_owned(),
        "turn-marker-90fe".to_owned(),
        "cachekey-marker-5e1d".to_owned(),
        "Hello world".to_owned(),
    ] {
        assert!(!logs.contains(&needle), "log contains {needle}:\n{logs}");
    }
}
