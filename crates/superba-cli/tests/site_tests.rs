mod common;

use clap::Parser;
use common::*;
use superba_core::{ProfileId, SiteId};

#[tokio::test]
async fn compatible_sites_are_added_listed_and_removed() {
    let service = FakeService::new();
    let (code, stdout, _) = invoke(
        service.clone(),
        &[
            "site",
            "add",
            "acme",
            "--name",
            "Acme",
            "--base-url",
            "https://api.acme.example/v1",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("acme"), "{stdout}");
    assert!(stdout.contains("spb profile add"), "{stdout}");
    let (code, stdout, _) = invoke(service.clone(), &["site", "list", "--json"]).await;
    assert_eq!(code, 0);
    let sites = one_json(&stdout);
    let acme = sites
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "acme")
        .expect("listed");
    assert_eq!(acme["kind"], "compatible");
    assert_eq!(acme["lines"][0]["origin"], "https://api.acme.example");
    assert_eq!(acme["endpoints"]["models_unauth"], "/v1/models");

    let (code, _, _) = invoke(service.clone(), &["site", "remove", "acme"]).await;
    assert_eq!(code, 0);
    assert!(service.calls().names.contains(&"delete_site"));
}

#[tokio::test]
async fn site_add_refuses_unusable_base_urls() {
    for url in [
        "not a url",
        "http://api.acme.example/v1",
        "https://user:pw@api.acme.example/v1",
    ] {
        let service = FakeService::new();
        let (code, _, _) =
            invoke(service.clone(), &["site", "add", "acme", "--base-url", url]).await;
        assert_ne!(code, 0, "{url}");
        assert!(!service.calls().names.contains(&"save_site"), "{url}");
    }
}

fn compatible(service: &FakeService) {
    let base = url::Url::parse("https://api.acme.example/v1").unwrap();
    let site = superba_switch::new_compatible_site(SiteId::new("acme"), "Acme", &base).unwrap();
    service.sites.lock().unwrap().push(site);
}

#[tokio::test]
async fn site_add_takes_model_mappings() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &[
            "site",
            "add",
            "acme",
            "--base-url",
            "https://api.acme.example/v1",
            "--map",
            "gpt-5.5=vendor/gpt-5.5",
            "--map",
            " gpt-x = vendor/gpt-x ",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("gpt-5.5 → vendor/gpt-5.5"), "{stdout}");
    let sites = service.sites.lock().unwrap().clone();
    let acme = sites.iter().find(|s| s.id.as_str() == "acme").unwrap();
    assert_eq!(
        acme.model_map.get("gpt-x").map(String::as_str),
        Some("vendor/gpt-x")
    );
    for bad in ["gpt-5.5", "=x", "x="] {
        assert!(
            superba_cli::Cli::try_parse_from([
                "spb",
                "site",
                "add",
                "a",
                "--base-url",
                "https://a.example",
                "--map",
                bad
            ])
            .is_err(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn site_edit_changes_name_url_and_mappings_and_keeps_the_rest() {
    let service = FakeService::new();
    compatible(&service);
    let (code, _, stderr) = invoke(
        service.clone(),
        &["site", "edit", "acme", "--map", "a=b", "--map", "c=d"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &[
            "site",
            "edit",
            "acme",
            "--name",
            "Acme 2",
            "--base-url",
            "https://eu.acme.example/api/v2",
            "--unmap",
            "a",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("next switch"), "{stdout}");
    let site = service
        .sites
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.id.as_str() == "acme")
        .cloned()
        .unwrap();
    assert_eq!(site.name.fallback, "Acme 2");
    assert_eq!(site.lines[0].origin.as_str(), "https://eu.acme.example");
    assert_eq!(site.endpoints.models_unauth, "/api/v2/models");
    assert_eq!(
        site.model_map.into_iter().collect::<Vec<_>>(),
        [("c".to_owned(), "d".to_owned())]
    );
    let (code, _, _) = invoke(service.clone(), &["site", "edit", "acme", "--clear-map"]).await;
    assert_eq!(code, 0);
    assert!(
        service
            .sites
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.model_map.is_empty())
    );
}

#[tokio::test]
async fn site_edit_refuses_builtins_and_empty_edits() {
    let service = FakeService::new();
    compatible(&service);
    let (code, _, stderr) =
        invoke(service.clone(), &["site", "edit", "krill", "--name", "x"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("built in"), "{stderr}");
    let (code, _, stderr) = invoke(service.clone(), &["site", "edit", "acme"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(!service.calls().names.contains(&"save_site"));
}

#[tokio::test]
async fn site_probe_without_a_key_checks_reachability_only() {
    let service = FakeService::new();
    compatible(&service);
    let (code, stdout, stderr) = invoke(service.clone(), &["site", "probe", "acme"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Latency (ms): 123"), "{stdout}");
    assert!(stdout.contains("only latency"), "{stdout}");
    assert_eq!(
        service.calls().site_probes,
        [SiteProbeCall {
            site: SiteId::new("acme"),
            line: None,
            key: None,
            model: None,
            responses: false,
        }]
    );
}

#[tokio::test]
async fn site_probe_with_a_key_reports_access_models_mappings_and_responses() {
    let service = FakeService::new();
    compatible(&service);
    service
        .sites
        .lock()
        .unwrap()
        .iter_mut()
        .find(|s| s.id.as_str() == "acme")
        .unwrap()
        .model_map
        .insert("gpt-5.5".into(), "vendor/missing".into());
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &[
            "site",
            "probe",
            "acme",
            "--stdin",
            "--model",
            "gpt-5.5",
            "--responses",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Access: Available"), "{stdout}");
    assert!(stdout.contains("Models: 1 model"), "{stdout}");
    assert!(
        stdout.contains("vendor/missing (for gpt-5.5) is not in"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Responses: OK with vendor/missing (321 ms)"),
        "{stdout}"
    );
    let call = service.calls().site_probes.remove(0);
    assert_eq!(
        call.key,
        Some(RecordedKey::Secret {
            last4: "1111".into()
        })
    );
    assert!(call.responses);
    assert!(!stdout.contains(FAKE_KEY) && !stderr.contains(FAKE_KEY));

    let (code, stdout, _) = invoke(
        service.clone(),
        &["--json", "site", "probe", "acme", "--profile", "work"],
    )
    .await;
    assert_eq!(code, 0);
    let report = one_json(&stdout);
    assert_eq!(report["site_id"], "acme");
    assert_eq!(report["mapped"][0]["available"], false);
    assert_eq!(
        service.calls().site_probes[1].key,
        Some(RecordedKey::Profile(ProfileId::new("work")))
    );
    let (code, _, _) = invoke(
        service.clone(),
        &["site", "probe", "acme", "--profile", "nobody"],
    )
    .await;
    assert_eq!(code, 1);
}
