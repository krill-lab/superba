//! Mixed login and the official ChatGPT member through a running router,
//! against fake upstreams (no real ChatGPT account): the official JWT is
//! accepted only for the configured account, never reaches a relay, a relay's
//! 401 never reaches Codex as a 401, and the official member uses the account's
//! own token, fails over on its plan limit and serves Codex only.

mod common;

use base64::Engine;
use common::*;
use superba_router::MemberKind;
use superba_router::official::{OfficialClientConfig, OfficialTokens};

fn jwt(account: &str) -> String {
    let e = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    };
    format!(
        "{}.{}.sig",
        e(serde_json::json!({"alg":"none"})),
        e(serde_json::json!({
            "exp": 4_102_444_800u64,
            "https://api.openai.com/auth": {"chatgpt_account_id": account, "chatgpt_plan_type": "pro"}
        }))
    )
}

const OK: &str = r#"{"id":"resp_1","object":"response","status":"completed","output":[]}"#;
const BODY: &str = r#"{"model":"gpt-5","input":"hi","stream":false}"#;

fn mixed(cfg: &mut superba_router::RouterConfig) {
    cfg.official_client = Some(OfficialClientConfig {
        account_id: "acc-1".into(),
    });
}

async fn post_as(r: &Router, token: &str) -> reqwest::Response {
    r.http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(token)
        .header("chatgpt-account-id", "acc-1")
        .header("content-type", "application/json")
        .body(BODY)
        .send()
        .await
        .unwrap()
}

fn auth_of(seen: &Seen) -> String {
    seen.headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn the_official_login_is_accepted_and_never_reaches_a_relay() {
    let krill = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", krill.base("/backend-api/codex"))],
    )]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    let token = jwt("acc-1");
    let resp = post_as(&r, &token).await;
    assert_eq!(resp.status(), 200);
    let seen = krill.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(auth_of(&seen[0]), format!("Bearer {}", key_of("krill")));
    assert!(seen[0].headers.get("chatgpt-account-id").is_none());
    assert!(
        !seen[0]
            .headers
            .iter()
            .any(|(_, v)| v.to_str().is_ok_and(|v| v.contains(&token)))
    );
}

#[tokio::test]
async fn another_account_is_refused_with_403_not_401() {
    let krill = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", krill.base("/backend-api/codex"))],
    )]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    let resp = post_as(&r, &jwt("acc-2")).await;
    assert_eq!(resp.status(), 403);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "official_account_changed");
    assert_eq!(krill.hits(), 0);
}

#[tokio::test]
async fn without_mixed_login_a_chatgpt_token_is_not_a_router_token() {
    let krill = upstream(Mode::Json(200, OK)).await;
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", krill.base("/backend-api/codex"))],
    )]);
    let r = start(cfg).await;
    assert_eq!(post_as(&r, &jwt("acc-1")).await.status(), 401);
    assert_eq!(krill.hits(), 0);
}

#[tokio::test]
async fn a_relay_401_reaches_the_official_client_as_502() {
    let krill = upstream(Mode::Json(401, r#"{"error":{"message":"bad key"}}"#)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", krill.base("/backend-api/codex"))],
    )]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    assert_eq!(post_as(&r, &jwt("acc-1")).await.status(), 502);
    // A router-token client still sees the member's own answer.
    assert_eq!(r.post(None, BODY).await.status(), 401);
}

fn official_member(up: &Upstream) -> superba_router::MemberConfig {
    let mut m = member(
        "official",
        MemberKind::Official,
        true,
        vec![line("chatgpt", up.base("/backend-api/codex"))],
    );
    m.compaction = superba_core::CompactionSupport::Native;
    m
}

#[tokio::test]
async fn the_official_member_uses_the_clients_own_token() {
    let chatgpt = upstream(Mode::Json(200, OK)).await;
    let krill = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![
        official_member(&chatgpt),
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", krill.base("/backend-api/codex"))],
        ),
    ]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    let token = jwt("acc-1");
    let resp = post_as(&r, &token).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "official/chatgpt");
    let seen = chatgpt.seen();
    assert_eq!(auth_of(&seen[0]), format!("Bearer {token}"));
    assert_eq!(seen[0].headers["chatgpt-account-id"], "acc-1");
    assert_eq!(krill.hits(), 0);
}

#[tokio::test]
async fn the_official_plan_limit_fails_over_to_the_next_member() {
    let chatgpt = upstream(Mode::Json(
        429,
        r#"{"error":{"type":"usage_limit_reached","message":"limit","resets_in_seconds":60}}"#,
    ))
    .await;
    let krill = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![
        official_member(&chatgpt),
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", krill.base("/backend-api/codex"))],
        ),
    ]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    let resp = post_as(&r, &jwt("acc-1")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/a");
    assert_eq!(chatgpt.hits(), 1);
}

#[tokio::test]
async fn the_last_members_plan_limit_reaches_codex_as_429() {
    let chatgpt = upstream(Mode::Json(
        429,
        r#"{"error":{"type":"usage_limit_reached","message":"limit"}}"#,
    ))
    .await;
    let mut cfg = test_config(vec![official_member(&chatgpt)]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    let resp = post_as(&r, &jwt("acc-1")).await;
    assert_eq!(resp.status(), 429);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["type"], "usage_limit_reached");
}

#[tokio::test]
async fn without_a_login_the_official_member_is_skipped() {
    let chatgpt = upstream(Mode::Json(200, OK)).await;
    let krill = upstream(Mode::Json(200, OK)).await;
    let cfg = test_config(vec![
        official_member(&chatgpt),
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", krill.base("/backend-api/codex"))],
        ),
    ]);
    // Router token, no stored login: nothing to authenticate with.
    let r = start(cfg).await;
    let resp = r.post(None, BODY).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/a");
    assert_eq!(chatgpt.hits(), 0);
}

#[tokio::test]
async fn a_stored_login_serves_router_token_clients() {
    let chatgpt = upstream(Mode::Json(200, OK)).await;
    let cfg = test_config(vec![official_member(&chatgpt)]);
    let mut secrets = secrets_for(&cfg);
    let stored = jwt("acc-9");
    secrets.official_tokens.insert(
        "official".into(),
        OfficialTokens::new(&stored, "rt-fixture", None).unwrap(),
    );
    let server = superba_router::RouterServer::bind(cfg, secrets)
        .await
        .unwrap();
    let r = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    assert_eq!(r.post(None, BODY).await.status(), 200);
    let seen = chatgpt.seen();
    assert_eq!(auth_of(&seen[0]), format!("Bearer {stored}"));
    assert_eq!(seen[0].headers["chatgpt-account-id"], "acc-9");
}

#[test]
fn the_official_member_serves_codex_only() {
    let mut cfg = test_config(vec![member(
        "official",
        MemberKind::Official,
        true,
        vec![line(
            "chatgpt",
            url::Url::parse("https://chatgpt.com/backend-api/codex").unwrap(),
        )],
    )]);
    cfg.validate().unwrap();
    use superba_router::translate::ir::Protocol;
    assert!(
        cfg.upstream_protocol("official", Protocol::Responses)
            .is_some()
    );
    assert!(
        cfg.upstream_protocol("official", Protocol::Messages)
            .is_none()
    );
    assert!(cfg.upstream_protocol("official", Protocol::Chat).is_none());
    assert!(
        cfg.openai_eligibility(
            "official",
            superba_router::openai::OpenAiProtocol::Responses
        )
        .is_err()
    );
    // Another host is refused for an official member, and chatgpt.com for
    // any other member.
    cfg.members[0].lines[0].base_url =
        url::Url::parse("https://relay.example/backend-api/codex").unwrap();
    assert!(cfg.validate().is_err());
    cfg.members[0].kind = MemberKind::Krill;
    cfg.members[0].lines[0].base_url =
        url::Url::parse("https://chatgpt.com/backend-api/codex").unwrap();
    assert!(cfg.validate().is_err());
}

#[tokio::test]
async fn the_router_never_answers_account_calls_for_the_official_login() {
    let krill = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", krill.base("/backend-api/codex"))],
    )]);
    mixed(&mut cfg);
    let r = start(cfg).await;
    for path in [
        "/backend-api/wham/accounts/check",
        "/v1/user-auth-credential/whoami",
        "/backend-api/wham/usage",
        "/backend-api/wham/remote/control/server/enroll",
        "/backend-api/wham/tasks/list",
    ] {
        let resp = r
            .http
            .get(r.url(path))
            .bearer_auth(jwt("acc-1"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404, "{path}");
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v["error"]["code"], "mixed_login_chatgpt_call", "{path}");
    }
    // The router token still gets the local identity (PAT router mode).
    let resp = r
        .http
        .get(r.url("/backend-api/wham/accounts/check"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(krill.hits(), 0);
}
