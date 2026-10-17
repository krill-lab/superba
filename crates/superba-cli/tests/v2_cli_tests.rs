//! `spb settings`, `spb router`, `spb profile add/edit` (v2 profiles) and the
//! settings diff in switch previews.

mod common;

use clap::Parser;
use common::*;
use superba_core::{AuthChoice, LineId, ProfileId, SettingValue, SiteId, SwitchIntent};

#[tokio::test]
async fn settings_list_and_get_show_catalog_rows() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(service.clone(), &["settings", "list"]).await;
    assert_eq!(code, 0, "{stderr}");
    for expected in [
        "Profile: work",
        "Catalog for Codex: 0.162.0",
        "features.fast_mode | true | false | false | Stable",
        "settings list --advanced",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in\n{stdout}"
        );
    }
    let (code, stdout, _) = invoke(
        service.clone(),
        &[
            "settings",
            "list",
            "--advanced",
            "--profile",
            "work",
            "features",
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert!(!stdout.contains("--advanced"));
    let views = service.calls().settings_views;
    assert!(views[1].advanced);
    assert_eq!(views[1].profile_id, Some(ProfileId::new("work")));
    assert_eq!(views[1].paths, ["features"]);

    let (code, stdout, _) =
        invoke(service.clone(), &["settings", "get", "features.fast_mode"]).await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Description: Use the fast speed tier."),
        "{stdout}"
    );
    assert!(!stdout.contains("Chinese"));
    let (_, stdout, _) = invoke(
        service.clone(),
        &["--locale", "zh-CN", "settings", "get", "features.fast_mode"],
    )
    .await;
    assert!(stdout.contains("暂无中文说明"), "{stdout}");
    assert!(
        stdout.contains("Use the fast speed tier."),
        "the official English text is shown as is"
    );
    let (code, _, _) = invoke(service.clone(), &["settings", "get", "no.such"]).await;
    assert_eq!(code, 1);
}

#[tokio::test]
async fn settings_edits_parse_toml_literals_and_map_to_the_service() {
    let service = FakeService::new();
    for (raw, expected) in [
        ("true", SettingValue::Bool(true)),
        ("3", SettingValue::from(3i64)),
        ("\"live\"", SettingValue::from("live")),
        ("plain text", SettingValue::from("plain text")),
    ] {
        let (code, stdout, stderr) =
            invoke(service.clone(), &["settings", "set", "x.y", raw]).await;
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.contains("profile use work"), "{stdout}");
        let edit = service.calls().settings_edits.pop().unwrap();
        assert_eq!(edit.set, vec![("x.y".to_owned(), expected)], "{raw}");
        assert!(!edit.force);
    }
    invoke(
        service.clone(),
        &[
            "settings",
            "set",
            "a.b",
            "-1",
            "--force",
            "--profile",
            "work",
        ],
    )
    .await;
    let edit = service.calls().settings_edits.pop().unwrap();
    assert!(edit.force);
    assert_eq!(edit.profile_id, Some(ProfileId::new("work")));
    assert_eq!(edit.set[0].1, SettingValue::from(-1i64));
    invoke(service.clone(), &["settings", "reset", "a", "b"]).await;
    invoke(service.clone(), &["settings", "forget", "c"]).await;
    invoke(service.clone(), &["settings", "migrate", "features.old"]).await;
    let edits = service.calls().settings_edits;
    assert_eq!(edits[edits.len() - 3].reset, ["a", "b"]);
    assert_eq!(edits[edits.len() - 2].forget, ["c"]);
    assert_eq!(edits[edits.len() - 1].accept_migrations, ["features.old"]);
}

#[tokio::test]
async fn settings_diff_and_switch_previews_show_the_changes_with_reasons() {
    let service = FakeService::new();
    service.v2.lock().unwrap().plan_settings.changes = vec![fast_mode_change()];
    let (code, stdout, stderr) = invoke(service.clone(), &["settings", "diff"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("This computer:"), "{stdout}");
    assert!(
        stdout.contains("features.fast_mode: false → true (Profile setting)"),
        "{stdout}"
    );
    assert!(
        !service.calls().names.contains(&"execute"),
        "diff never executes"
    );
    let (_, stdout, _) = invoke(service.clone(), &["--json", "settings", "diff"]).await;
    let value = one_json(&stdout);
    assert_eq!(value["settings"]["changes"][0]["reason"]["kind"], "setting");

    let (code, stdout, _) = invoke(
        service.clone(),
        &["switch", "--profile", "work", "--dry-run"],
    )
    .await;
    assert_eq!(code, 0);
    assert!(stdout.contains("(Profile setting)"), "{stdout}");
    let (_, stdout, _) = invoke(
        service.clone(),
        &["--json", "switch", "--profile", "work", "--dry-run"],
    )
    .await;
    let value = one_json(&stdout);
    assert_eq!(
        value["settings"]["changes"][0]["path"],
        "features.fast_mode"
    );
}

#[tokio::test]
async fn router_install_status_and_uninstall() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["router", "install", "--member", "work", "--member", "other"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Running: yes"), "{stdout}");
    assert_eq!(
        service.calls().router_installs,
        vec![vec![ProfileId::new("work"), ProfileId::new("other")]]
    );
    let (code, stdout, _) = invoke(service.clone(), &["--json", "router", "status"]).await;
    assert_eq!(code, 0);
    assert_eq!(one_json(&stdout)["port"], 18431);
    let (code, stdout, _) = invoke(service.clone(), &["router", "uninstall"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("removed"), "{stdout}");
    let (code, stdout, _) = invoke(service.clone(), &["--json", "router", "watch", "--once"]).await;
    assert_eq!(code, 0);
    assert_eq!(one_json(&stdout)["state"]["kind"], "healthy");
}

#[tokio::test]
async fn router_enable_dry_run_installs_nothing() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["router", "enable", "--member", "work", "--dry-run"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Dry run") && stdout.contains("work"),
        "what would happen is stated: {stdout}"
    );
    assert!(!stdout.contains("Starting the local router"), "{stdout}");
    let calls = service.calls();
    assert!(
        calls.router_installs.is_empty(),
        "a dry run installs no service and copies no key"
    );
    assert!(!calls.names.contains(&"router_set_enabled"));
    assert!(
        calls.plans.is_empty(),
        "nothing to plan before the router runs"
    );

    // With the router running, the dry run plans the router switch only.
    service.v2.lock().unwrap().router.running = true;
    let (code, stdout, stderr) = invoke(service.clone(), &["router", "enable", "--dry-run"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("127.0.0.1"),
        "the local process is stated: {stdout}"
    );
    let calls = service.calls();
    assert!(calls.router_installs.is_empty());
    assert_eq!(
        calls.plans.last().map(|r| r.intent.clone()),
        Some(SwitchIntent::Router { enable: true })
    );
    assert!(calls.plans.last().unwrap().targets.remotes.is_empty());
    let (code, _, stderr) = invoke(
        service.clone(),
        &["router", "enable", "--remote", "dev", "--dry-run"],
    )
    .await;
    assert_eq!(code, 2, "{stderr}");
}

#[tokio::test]
async fn router_enable_installs_then_switches_with_the_router_intent() {
    let service = FakeService::new();
    let (_, _, stderr) = invoke(
        service.clone(),
        &["router", "enable", "--member", "work", "--yes"],
    )
    .await;
    let calls = service.calls();
    assert_eq!(
        calls.router_installs,
        vec![vec![ProfileId::new("work")]],
        "{stderr}"
    );
    assert_eq!(
        calls.plans.last().map(|r| r.intent.clone()),
        Some(SwitchIntent::Router { enable: true })
    );
}

#[tokio::test]
async fn router_disable_without_routing_only_stores_the_choice() {
    let service = FakeService::new();
    let (code, stdout, _) = invoke(service.clone(), &["router", "disable", "--uninstall"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Router mode is off."), "{stdout}");
    let calls = service.calls();
    assert_eq!(calls.router_enabled, [false]);
    assert!(calls.names.contains(&"router_uninstall"));
    assert!(!calls.names.contains(&"plan"));

    service.v2.lock().unwrap().router.codex_routed = true;
    let (code, _, _) = invoke(service.clone(), &["router", "disable", "--dry-run"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        service.calls().plans.last().map(|r| r.intent.clone()),
        Some(SwitchIntent::Router { enable: false })
    );
}

#[tokio::test]
async fn profile_add_creates_v2_profiles() {
    let service = FakeService::new();
    let (code, _, stderr) = invoke(
        service.clone(),
        &[
            "profile",
            "add",
            "fast",
            "--line",
            "cdn",
            "--line",
            "overseas",
            "--key-stdin",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let saved = service.calls().saves.pop().unwrap().profile;
    assert_eq!(saved.auth_choice(), AuthChoice::Auto, "Auto by default");
    assert_eq!(
        saved.line_priority(),
        vec![LineId::new("cdn"), LineId::new("overseas")]
    );
    assert!(
        !saved.codex_settings.is_empty(),
        "Krill profiles start with the recommended settings"
    );
    let (code, _, _) = invoke(
        service.clone(),
        &[
            "profile",
            "add",
            "plain",
            "--auth",
            "apikey",
            "--no-recommended-keys",
        ],
    )
    .await;
    assert_eq!(code, 0);
    let saved = service.calls().saves.pop().unwrap().profile;
    assert_eq!(saved.auth_choice(), AuthChoice::ApiKey);
    assert!(saved.codex_settings.is_empty());

    let base = url::Url::parse("https://api.acme.example/v1").unwrap();
    let site = superba_switch::new_compatible_site(SiteId::new("acme"), "Acme", &base).unwrap();
    service.sites.lock().unwrap().push(site);
    let (code, _, _) = invoke(
        service.clone(),
        &["profile", "add", "acme-p", "--site", "acme"],
    )
    .await;
    assert_eq!(code, 0);
    let saved = service.calls().saves.pop().unwrap().profile;
    assert!(saved.codex_settings.is_empty());
    let (code, _, _) = invoke(service.clone(), &["profile", "add", "x", "--site", "nope"]).await;
    assert_eq!(code, 1);
}

#[tokio::test]
async fn profile_edit_changes_lines_auth_and_model() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &[
            "profile", "edit", "work", "--line", "cdn", "--line", "cn-fast", "--auth", "pat",
            "--model", "gpt-x",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("cdn > cn-fast"), "{stdout}");
    assert!(stdout.contains("next switch"), "{stdout}");
    let saved = service.calls().saves.pop().unwrap().profile;
    assert_eq!(
        saved.line_priority(),
        vec![LineId::new("cdn"), LineId::new("cn-fast")]
    );
    assert_eq!(saved.auth_choice(), AuthChoice::Pat);
    assert_eq!(saved.model.model.as_deref(), Some("gpt-x"));
    let (code, _, _) = invoke(
        service.clone(),
        &["profile", "edit", "work", "--auto-line", "--model", ""],
    )
    .await;
    assert_eq!(code, 0);
    let saved = service.calls().saves.pop().unwrap().profile;
    assert!(saved.line_priority().is_empty());
    assert_eq!(saved.model.model, None);

    let (code, _, _) = invoke(service.clone(), &["profile", "edit", "work"]).await;
    assert_eq!(code, 2);
    let (code, _, _) = invoke(
        service.clone(),
        &["profile", "edit", "official", "--name", "x"],
    )
    .await;
    assert_eq!(code, 2);
    assert!(
        superba_cli::Cli::try_parse_from([
            "spb",
            "profile",
            "edit",
            "work",
            "--line",
            "a",
            "--auto-line"
        ])
        .is_err()
    );
}
