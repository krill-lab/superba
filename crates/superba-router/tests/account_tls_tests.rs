mod common;

use common::*;
use superba_router::{MemberKind, RouterPaths, RouterServer};

#[tokio::test]
async fn local_pat_uses_a_stable_identity_and_a_scoped_tls_certificate() {
    let up = upstream(Mode::Json(200, "{}")).await;
    let cfg = test_config(vec![member(
        "member",
        MemberKind::Krill,
        false,
        vec![line("line", up.base("/backend-api/codex"))],
    )]);
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::from_dir(dir.path().join("router"));
    cfg.save(&paths).unwrap();
    secrets_for(&cfg).save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let addr = server.local_addr();
    let stop = server.shutdown_handle();
    let task = tokio::spawn(server.serve());
    let pem = std::fs::read(paths.certificate_pem()).unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let tls = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&pem).unwrap())
        .build()
        .unwrap();
    let whoami = format!("http://{addr}/v1/user-auth-credential/whoami");
    assert_eq!(http.get(&whoami).send().await.unwrap().status(), 401);
    let identity: serde_json::Value = http
        .get(&whoami)
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let account = format!("https://{addr}/backend-api/wham/accounts/check");
    assert!(
        http.get(&account)
            .bearer_auth(ROUTER_TOKEN)
            .send()
            .await
            .is_err()
    );
    let reply: serde_json::Value = tls
        .get(&account)
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["accounts"][0]["id"], identity["chatgpt_account_id"]);
    assert_eq!(
        reply["accounts"][0]["workspace_backend_origin"],
        "NO_CONSTRAINT"
    );
    assert_eq!(
        reply["accounts"][0]["account_routing_override"],
        "NO_CONSTRAINT"
    );
    let settings: serde_json::Value = tls
        .get(format!("https://{addr}/backend-api/wham/settings/user"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["commit_attribution_enabled"], false);
    assert_eq!(
        up.hits(),
        0,
        "local identity must not depend on member availability"
    );
    stop.shutdown();
    task.await.unwrap().unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    assert_eq!(std::fs::read(paths.certificate_pem()).unwrap(), pem);
    drop(server);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(paths.dir().join("tls-identity.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn codex_trust_bundle_preserves_custom_roots_without_growing_on_repeated_launches() {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::from_dir(dir.path().join("router"));
    let extra = dir.path().join("company.pem");
    let certificate = rcgen::generate_simple_self_signed(vec!["company.test".into()])
        .unwrap()
        .cert
        .pem();
    std::fs::write(&extra, &certificate).unwrap();
    superba_router::tls::prepare_client_bundle(&paths, std::slice::from_ref(&extra), false)
        .unwrap();
    let first = std::fs::read(paths.client_certificate_pem()).unwrap();
    assert!(String::from_utf8_lossy(&first).contains(&certificate));
    assert_eq!(
        String::from_utf8_lossy(&first)
            .matches("BEGIN CERTIFICATE")
            .count(),
        2
    );
    superba_router::tls::prepare_client_bundle(&paths, &[extra], true).unwrap();
    assert_eq!(
        std::fs::read(paths.client_certificate_pem()).unwrap(),
        first
    );
    // A new activation without that user setting does not keep obsolete roots.
    superba_router::tls::prepare_client_bundle(&paths, &[], false).unwrap();
    assert_eq!(
        std::fs::read(paths.client_certificate_pem()).unwrap(),
        std::fs::read(paths.certificate_pem()).unwrap()
    );
}
