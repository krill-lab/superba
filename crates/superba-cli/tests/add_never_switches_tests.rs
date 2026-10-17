//! `spb profile add/edit` and `spb site add/edit` only save: they never plan or
//! execute a switch (switching is `spb profile use` / `spb switch`), and adding a
//! profile never changes another one.

mod common;

use common::*;
use superba_core::{ProfileId, SiteId};

fn assert_no_switch(service: &FakeService) {
    let calls = service.calls();
    assert!(calls.plans.is_empty(), "{:?}", calls.plans);
    assert!(calls.executions.is_empty(), "{:?}", calls.executions);
    assert!(
        !calls
            .names
            .iter()
            .any(|n| matches!(*n, "plan" | "execute" | "plan_settings")),
        "{:?}",
        calls.names
    );
}

#[tokio::test]
async fn adding_and_editing_profiles_and_sites_never_switches() {
    let service = FakeService::new();
    for args in [
        &["profile", "add", "second", "--key-stdin"][..],
        &[
            "profile",
            "add",
            "third",
            "--line",
            "cdn",
            "--model",
            "gpt-6-astra",
        ][..],
        &[
            "profile", "edit", "second", "--line", "overseas", "--name", "Second",
        ][..],
        // The applied profile too: the edit is saved and applies at the next switch.
        &[
            "profile", "edit", "work", "--line", "cdn", "--effort", "high",
        ][..],
        &[
            "site",
            "add",
            "my-api",
            "--name",
            "My API",
            "--base-url",
            "https://api.example.invalid/v1",
        ][..],
        &["site", "edit", "my-api", "--name", "Mine"][..],
        &["profile", "add", "mine", "--site", "my-api", "--key-stdin"][..],
    ] {
        let (code, stdout, stderr) = invoke(service.clone(), args).await;
        assert_eq!(code, 0, "{args:?}: {stdout}{stderr}");
    }
    assert_no_switch(&service);
    let saved: Vec<String> = service
        .calls()
        .saves
        .iter()
        .map(|s| s.profile.id.to_string())
        .collect();
    assert_eq!(saved, ["second", "third", "second", "work", "mine"]);
}

#[tokio::test]
async fn adding_under_a_preset_id_never_changes_another_profile() {
    let service = FakeService::new();
    // `work` is a profile on the Krill site; `krill` and `custom` are preset IDs
    // the service would treat as a wizard re-run and merge into `work`.
    for id in ["krill", "custom"] {
        let (code, stdout, stderr) =
            invoke(service.clone(), &["profile", "add", id, "--key-stdin"]).await;
        assert_ne!(code, 0, "{stdout}");
        assert!(
            stderr.contains(&format!(
                "{id} is a preset ID and site krill already has profile work"
            )),
            "{stderr}"
        );
    }
    assert!(service.calls().saves.is_empty());
    assert_no_switch(&service);
    // On a site without a profile the preset ID is a plain new profile.
    service.sites.lock().unwrap().push(
        superba_switch::new_compatible_site(
            SiteId::new("compatible"),
            "Compatible",
            &url::Url::parse("https://example.invalid/v1").unwrap(),
        )
        .unwrap(),
    );
    let (code, _, stderr) = invoke(
        service.clone(),
        &["profile", "add", "custom", "--site", "compatible"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        service.calls().saves[0].profile.id,
        ProfileId::new("custom")
    );
}
