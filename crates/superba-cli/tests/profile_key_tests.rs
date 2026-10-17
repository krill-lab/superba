mod common;

use common::*;
use std::ffi::OsString;
use std::sync::Arc;
use superba_core::*;

#[derive(Debug)]
struct NoKeyReads;
impl EnvSource for NoKeyReads {
    fn var_os(&self, name: &str) -> Option<OsString> {
        assert_eq!(
            name, "SUPERBA_LOCALE",
            "storing a reference must not read the key"
        );
        Some("en".into())
    }
}

#[tokio::test]
async fn add_environment_reference_does_not_read_or_store_a_secret() {
    let service = FakeService::new();
    let mut rt = runtime(service.clone());
    rt.env = Arc::new(NoKeyReads);
    rt.read_stdin_line = Arc::new(|| panic!("stdin must not be read"));
    let (code, stdout, stderr) =
        invoke_runtime(&rt, &["profile", "add", "new", "--key-env", "TEST_KEY"]).await;
    assert_eq!(code, 0);
    let saved = &service.calls().saves[0];
    assert_eq!(
        saved.profile.key_ref,
        Some(KeyRef::Env {
            var: "TEST_KEY".into()
        })
    );
    assert!(!saved.secret_present);
    assert_eq!(saved.last4, None);
    assert!(stdout.contains("env:TEST_KEY"));
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn add_stdin_key_uses_injected_reader_and_only_displays_mask() {
    let service = FakeService::new();
    let mut rt = runtime(service.clone());
    rt.read_stdin_line = Arc::new(|| Ok(format!("  {FAKE_KEY}\n")));
    let (code, stdout, stderr) =
        invoke_runtime(&rt, &["profile", "add", "new", "--key-stdin"]).await;
    assert_eq!(code, 0);
    let saved = &service.calls().saves[0];
    assert!(saved.secret_present);
    assert_eq!(saved.last4.as_deref(), Some("1111"));
    assert!(stdout.contains("••••1111"));
    assert!(stdout.contains("keyring"));
    assert!(!stdout.contains(FAKE_KEY) && !stderr.contains(FAKE_KEY));
}

#[tokio::test]
async fn add_builds_defaults_and_all_overrides() {
    let service = FakeService::new();
    assert_eq!(
        invoke(service.clone(), &["profile", "add", "minimal"])
            .await
            .0,
        0
    );
    let p = service.calls().saves[0].profile.clone();
    assert_eq!(p.name, "minimal");
    assert_eq!(p.site_id, SiteId::new("krill"));
    assert_eq!(p.line_id, None);
    // v2: auth is Auto unless chosen under Advanced; Krill's recommended keys
    // are the profile's own Codex settings.
    assert_eq!(p.auth_choice(), superba_core::AuthChoice::Auto);
    assert!(!p.codex_settings.is_empty() && p.builtin.is_none());
    let base = url::Url::parse("https://example.invalid/v1").unwrap();
    service.sites.lock().unwrap().push(
        superba_switch::new_compatible_site(SiteId::new("compatible"), "Compatible", &base)
            .unwrap(),
    );
    let (code, stdout, _) = invoke(
        service.clone(),
        &[
            "profile",
            "add",
            "custom",
            "--site",
            "compatible",
            "--name",
            "My site",
            "--line",
            "one",
            "--base-url",
            "https://example.invalid/v1",
            "--model",
            "test-model",
            "--effort",
            "high",
            "--no-recommended-keys",
            "--json",
        ],
    )
    .await;
    assert_eq!(code, 0);
    let p = service.calls().saves[1].profile.clone();
    assert_eq!(p.auth_choice(), superba_core::AuthChoice::Auto);
    assert!(p.codex_settings.is_empty());
    assert_eq!(p.name, "My site");
    assert_eq!(p.line_id, Some(LineId::new("one")));
    assert_eq!(
        p.base_url_override.as_ref().map(|u| u.as_str()),
        Some("https://example.invalid/v1")
    );
    assert_eq!(p.model.model.as_deref(), Some("test-model"));
    assert_eq!(p.model.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(one_json(&stdout)["id"], "custom");
    assert_eq!(
        invoke(
            service.clone(),
            &["profile", "add", "explicit", "--auth", "apikey"]
        )
        .await
        .0,
        0
    );
    assert_eq!(service.calls().saves[2].profile.auth_mode, AuthMode::ApiKey);
}

#[tokio::test]
async fn invalid_and_reserved_ids_are_usage_errors_without_service_calls() {
    for id in [
        "",
        "UPPER",
        "_start",
        "a.b",
        "a/b",
        "a b",
        "配置",
        "original",
        "official",
        &"a".repeat(65),
    ] {
        let service = FakeService::new();
        let (code, _, _) = invoke(service.clone(), &["profile", "add", id]).await;
        assert_eq!(code, 2, "{id}");
        assert!(service.calls().names.is_empty());
    }
    for id in ["a", "0", "a-z_9", &"a".repeat(64)] {
        assert_eq!(
            invoke(FakeService::new(), &["profile", "add", id]).await.0,
            0
        );
    }
}

#[tokio::test]
async fn replacing_requires_the_flag() {
    let service = FakeService::new();
    let (code, _, stderr) = invoke(service.clone(), &["profile", "add", "work"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("--replace"));
    assert!(service.calls().saves.is_empty());
    assert_eq!(
        invoke(
            service.clone(),
            &["profile", "add", "work", "--replace", "--name", "Replaced"]
        )
        .await
        .0,
        0
    );
    assert_eq!(service.calls().saves[0].profile.name, "Replaced");
}

#[tokio::test]
async fn invalid_url_and_empty_stdin_are_usage_errors() {
    let service = FakeService::new();
    assert_eq!(
        invoke(
            service.clone(),
            &["profile", "add", "new", "--base-url", "relative/path"]
        )
        .await
        .0,
        2
    );
    let mut rt = runtime(service.clone());
    rt.read_stdin_line = Arc::new(|| Ok(" \r\n".into()));
    for args in [
        vec!["profile", "add", "new", "--key-stdin"],
        vec!["key", "set", "--profile", "work", "--stdin"],
        vec!["key", "check", "--stdin"],
    ] {
        assert_eq!(invoke_runtime(&rt, &args).await.0, 2);
    }
    assert!(service.calls().saves.is_empty());
    assert!(service.calls().checks.is_empty());
}

#[tokio::test]
async fn stdin_io_failure_does_not_leak_reader_details() {
    let service = FakeService::new();
    let mut rt = runtime(service);
    rt.read_stdin_line = Arc::new(|| Err(std::io::Error::other(FAKE_KEY)));
    let (code, stdout, stderr) = invoke_runtime(&rt, &["key", "check", "--stdin", "--json"]).await;
    assert_eq!(code, 1);
    assert_eq!(one_json(&stdout)["error"]["code"], "other");
    assert!(!stdout.contains(FAKE_KEY) && stderr.is_empty());
}

#[tokio::test]
async fn key_set_updates_only_the_key_reference_or_supplies_a_secret() {
    let service = FakeService::new();
    let before = service
        .profiles
        .lock()
        .unwrap()
        .profile(&ProfileId::new("work"))
        .unwrap()
        .clone();
    let mut rt = runtime(service.clone());
    rt.env = Arc::new(NoKeyReads);
    assert_eq!(
        invoke_runtime(
            &rt,
            &["key", "set", "--profile", "work", "--key-env", "TEST_KEY"]
        )
        .await
        .0,
        0
    );
    let mut expected = before;
    expected.key_ref = Some(KeyRef::Env {
        var: "TEST_KEY".into(),
    });
    assert_eq!(service.calls().saves[0].profile, expected);
    assert!(!service.calls().saves[0].secret_present);
    let (code, stdout, stderr) = invoke_runtime(
        &rt,
        &["key", "set", "--profile", "work", "--stdin", "--json"],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(one_json(&stdout)["key_ref"]["kind"], "keyring");
    assert!(service.calls().saves[1].secret_present);
    assert!(!stdout.contains(FAKE_KEY) && stderr.is_empty());
    assert_eq!(
        invoke_runtime(&rt, &["key", "set", "--profile", "missing", "--stdin"])
            .await
            .0,
        1
    );
}

#[tokio::test]
async fn stdin_aliases_use_applied_profile_and_fail_before_reading_without_one() {
    for flag in ["--stdin", "--key-stdin"] {
        let service = FakeService::new();
        let (code, stdout, stderr) = invoke(service.clone(), &["key", "set", flag]).await;
        assert_eq!(code, 0);
        let saves = service.calls().saves;
        assert_eq!(saves[0].profile.id, ProfileId::new("work"));
        assert!(saves[0].secret_present);
        assert!(!stdout.contains(FAKE_KEY) && !stderr.contains(FAKE_KEY));
        assert_eq!(
            invoke(service.clone(), &["profile", "add", "new", flag])
                .await
                .0,
            0
        );
        assert!(service.calls().saves[1].secret_present);

        service.overview.lock().unwrap().active = None;
        let mut rt = runtime(service.clone());
        rt.read_stdin_line = Arc::new(|| panic!("missing profile must not consume stdin"));
        let (code, stdout, stderr) = invoke_runtime(&rt, &["key", "set", flag]).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("--profile <PROFILE>") && stderr.contains("spb profile list"));
        assert_eq!(service.calls().saves.len(), 2);
    }
}

#[tokio::test]
async fn key_check_uses_applied_profile_and_profile_defaults() {
    let service = FakeService::new();
    service
        .profiles
        .lock()
        .unwrap()
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap()
        .line_id = Some(LineId::new("cdn"));
    let (code, stdout, stderr) = invoke(service.clone(), &["key", "check"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        service.calls().checks[0],
        (
            SiteId::new("krill"),
            LineId::new("cdn"),
            AuthMode::Pat,
            RecordedKey::Profile(ProfileId::new("work"))
        )
    );
    for value in [
        "Available",
        "healthy",
        "tester@example.invalid",
        "Test user",
        "/backend-api/codex",
        "1111",
    ] {
        assert!(stdout.contains(value), "missing {value}");
    }
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn key_check_explicit_profile_and_overrides_win() {
    let service = FakeService::new();
    service.overview.lock().unwrap().active = None;
    let (code, _, _) = invoke(
        service.clone(),
        &[
            "key",
            "check",
            "--profile",
            "work",
            "--site",
            "krill",
            "--line",
            "cn-fast",
            "--auth",
            "apikey",
        ],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(
        service.calls().checks[0],
        (
            SiteId::new("krill"),
            LineId::new("cn-fast"),
            AuthMode::ApiKey,
            RecordedKey::Profile(ProfileId::new("work"))
        )
    );
    assert!(!service.calls().names.contains(&"overview"));
}

#[tokio::test]
async fn key_check_reads_injected_environment_and_uses_default_site_line_and_mode() {
    let service = FakeService::new();
    let mut rt = runtime(service.clone());
    rt.env = Arc::new(
        MapEnv::new()
            .with("SUPERBA_LOCALE", "en")
            .with("TEST_KEY", format!(" {FAKE_KEY} ")),
    );
    let (code, stdout, stderr) =
        invoke_runtime(&rt, &["key", "check", "--key-env", "TEST_KEY", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        service.calls().checks[0],
        (
            SiteId::new("krill"),
            LineId::new("overseas"),
            AuthMode::ApiKey,
            RecordedKey::Secret {
                last4: "1111".into()
            }
        )
    );
    assert_eq!(one_json(&stdout)["access"]["kind"], "ok");
    assert!(!stdout.contains(FAKE_KEY) && stderr.is_empty());
    assert!(!service.calls().names.contains(&"overview"));
    assert!(!service.calls().names.contains(&"profile_set"));
    assert_eq!(invoke_runtime(&rt, &["key", "check", "--stdin"]).await.0, 0);
    assert!(matches!(
        service.calls().checks[1].3,
        RecordedKey::Secret { .. }
    ));
}

#[tokio::test]
async fn key_check_missing_input_is_usage_and_missing_profile_is_error() {
    let service = FakeService::new();
    service.overview.lock().unwrap().active = None;
    assert_eq!(invoke(service.clone(), &["key", "check"]).await.0, 2);
    assert_eq!(
        invoke(service.clone(), &["key", "check", "--profile", "missing"])
            .await
            .0,
        1
    );
    for value in [None, Some(""), Some("   ")] {
        let mut rt = runtime(service.clone());
        let env = MapEnv::new().with("SUPERBA_LOCALE", "en");
        rt.env = Arc::new(if let Some(v) = value {
            env.with("TEST_KEY", v)
        } else {
            env
        });
        assert_eq!(
            invoke_runtime(&rt, &["key", "check", "--key-env", "TEST_KEY"])
                .await
                .0,
            2
        );
    }
    assert!(service.calls().checks.is_empty());
}

#[tokio::test]
async fn key_access_failures_exit_one() {
    for access in [
        KeyAccess::InvalidKey,
        KeyAccess::AccessPointDenied,
        KeyAccess::QuotaExhausted,
        KeyAccess::ChannelDenied,
        KeyAccess::EdgeBlocked,
        KeyAccess::Unreachable,
        KeyAccess::Unknown {
            status: Some(500),
            message: "test failure".into(),
        },
    ] {
        let service = FakeService::new();
        service.key_check.lock().unwrap().access = access;
        let (code, stdout, _) = invoke(service, &["key", "check", "--json"]).await;
        assert_eq!(code, 1);
        assert!(one_json(&stdout).get("access").is_some());
    }
}

#[tokio::test]
async fn profile_list_preserves_builtin_names_and_selection_and_remove_delegates() {
    let service = FakeService::new();
    {
        let mut set = service.profiles.lock().unwrap();
        set.last_selected = Some(ProfileId::new("work"));
        set.profiles[0].name = "Stored builtin name".into();
        set.profiles[2].key_ref = Some(KeyRef::File {
            path: "/fake/key".into(),
        });
    }
    let (code, stdout, _) = invoke(service.clone(), &["profile", "list"]).await;
    assert_eq!(code, 0);
    for value in ["Stored builtin name", "* | work", "auto", "file"] {
        assert!(stdout.contains(value));
    }
    let (_, stdout, stderr) = invoke(service.clone(), &["profile", "list", "--json"]).await;
    let value = one_json(&stdout);
    assert_eq!(value["last_selected"], "work");
    assert!(value.get("settings").is_some() && stderr.is_empty());
    assert_eq!(
        invoke(service.clone(), &["profile", "remove", "work"])
            .await
            .0,
        0
    );
    assert_eq!(service.calls().deletes, [ProfileId::new("work")]);
}
