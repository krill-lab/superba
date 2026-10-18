#![allow(missing_docs)]

use serde_json::json;
use superba_core::*;

macro_rules! json_round_trip {
    ($ty:ty, $value:expr) => {{
        let original: $ty = $value;
        let encoded = serde_json::to_value(&original).unwrap();
        let decoded = serde_json::from_value::<$ty>(encoded.clone()).unwrap();
        assert_eq!(decoded, original);
        encoded
    }};
}

#[test]
fn profile_round_trip_preserves_optional_model_url_and_key_reference() {
    let profile = Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        id: "work".into(),
        name: "Work profile".into(),
        builtin: None,
        site_id: "compatible".into(),
        line_id: Some("primary".into()),
        backup_lines: vec!["backup".into()],
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::ApiKey,
        key_ref: Some(KeyRef::Env {
            var: "TEST_API_KEY".into(),
        }),
        base_url_override: Some("https://compatible.example/v1".parse().unwrap()),
        model: ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: Some("test-model".into()),
            reasoning_effort: Some("high".into()),
        },
        codex_settings: CodexSettings::krill_recommended(),
    };
    let encoded = json_round_trip!(Profile, profile);
    assert_eq!(encoded["auth"], "apikey");
    assert_eq!(encoded["backup_lines"], json!(["backup"]));
    assert_eq!(encoded["codex_settings"]["web_search"], "live");
    assert_eq!(encoded["id"], "work");
    assert_eq!(encoded["auth_mode"], "apikey");
    assert_eq!(
        encoded["key_ref"],
        json!({"kind":"env", "var":"TEST_API_KEY"})
    );
    assert_eq!(
        encoded["base_url_override"],
        "https://compatible.example/v1"
    );
    assert_eq!(
        encoded["model"],
        json!({"model":"test-model", "reasoning_effort":"high"})
    );
}

#[test]
fn minimal_profile_deserialization_supplies_documented_defaults() {
    let profile: Profile = serde_json::from_value(json!({
        "id":"minimal", "name":"Minimal", "site_id":"krill", "auth_mode":"pat"
    }))
    .unwrap();
    assert!(!profile.is_builtin());
    assert_eq!(profile.line_id, None);
    assert_eq!(profile.key_ref, None);
    assert_eq!(profile.base_url_override, None);
    assert_eq!(profile.model, ModelDefaults::default());
    assert_eq!(profile.auth, AuthChoice::Auto);
    assert!(profile.backup_lines.is_empty());
    assert!(profile.codex_settings.is_empty());
}

#[test]
fn default_profile_set_round_trips_with_builtins_and_settings() {
    let set = ProfileSet::default();
    assert_eq!(set.schema, PROFILE_SET_SCHEMA);
    assert_eq!(set.profiles, catalog::builtin_profiles());
    assert!(set.custom_sites.is_empty());
    assert_eq!(set.last_selected, None);
    assert_eq!(set.settings, Settings::default());
    assert_eq!(
        set.profile(&"original".into()).unwrap().builtin,
        Some(BuiltinProfile::Original)
    );
    assert_eq!(
        set.profile(&"official".into()).unwrap().builtin,
        Some(BuiltinProfile::Official)
    );
    assert!(set.profile(&"absent".into()).is_none());
    json_round_trip!(ProfileSet, set);
}

#[test]
fn key_references_round_trip_with_kind_tags_and_no_key_material() {
    for (reference, expected) in [
        (
            KeyRef::Keyring {
                service: "superba".into(),
                account: "profile:work".into(),
            },
            json!({"kind":"keyring", "service":"superba", "account":"profile:work"}),
        ),
        (
            KeyRef::File {
                path: "/fixtures/secrets/work".into(),
            },
            json!({"kind":"file", "path":"/fixtures/secrets/work"}),
        ),
        (
            KeyRef::Env {
                var: "API_KEY".into(),
            },
            json!({"kind":"env", "var":"API_KEY"}),
        ),
    ] {
        assert_eq!(json_round_trip!(KeyRef, reference), expected);
    }
    assert!(serde_json::from_value::<KeyRef>(json!({"kind":"unknown"})).is_err());
}

#[test]
fn auth_mode_wire_names_are_pat_apikey_and_chatgpt() {
    for (mode, name) in [
        (AuthMode::Pat, "pat"),
        (AuthMode::ApiKey, "apikey"),
        (AuthMode::OfficialChatGpt, "chatgpt"),
    ] {
        assert_eq!(json_round_trip!(AuthMode, mode), json!(name));
    }
    for name in ["api_key", "official_chat_gpt", "PAT"] {
        assert!(serde_json::from_value::<AuthMode>(json!(name)).is_err());
    }
}

#[test]
fn switch_profile_request_round_trips_with_local_defaults() {
    let request = SwitchRequest::profile("work".into());
    assert_eq!(request.targets, TargetSelection::default());
    assert!(request.targets.local);
    assert!(request.targets.remotes.is_empty());
    let encoded = json_round_trip!(SwitchRequest, request);
    assert_eq!(
        encoded,
        json!({
            "intent": {"kind":"profile", "profile_id":"work", "line_id":null, "auth_mode":null},
            "targets": {"local":true, "remotes":[]},
            "model_override":null, "legacy_sessions":null, "take_over_foreign_env":null,
            "accept_unknown_versions":false, "key_resolutions":{}, "accept_shared_home":false,
            "migration_exclude":[]
        })
    );
    let minimal: SwitchRequest =
        serde_json::from_value(json!({"intent":{"kind":"profile", "profile_id":"work"}})).unwrap();
    assert_eq!(minimal, SwitchRequest::profile("work".into()));
}

#[test]
fn every_restart_kind_round_trips_with_its_discriminator_and_payload() {
    for (restart, expected) in [
        (
            RestartKind::Blocked {
                reasons: vec![BlockReason::MissingKey],
            },
            json!({"kind":"blocked", "reasons":[{"kind":"missing_key"}]}),
        ),
        (RestartKind::None, json!({"kind":"none"})),
        (
            RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            },
            json!({"kind":"app_server_hot", "host_id":"local", "kill_remote":false}),
        ),
        (
            RestartKind::AppServerHot {
                host_id: "host-1".into(),
                kill_remote: true,
            },
            json!({"kind":"app_server_hot", "host_id":"host-1", "kill_remote":true}),
        ),
        (
            RestartKind::FullAppRestart {
                reason: FullRestartReason::NoCdp,
            },
            json!({"kind":"full_app_restart", "reason":"no_cdp"}),
        ),
        (RestartKind::NextLaunch, json!({"kind":"next_launch"})),
        (RestartKind::DaemonRestart, json!({"kind":"daemon_restart"})),
    ] {
        assert_eq!(json_round_trip!(RestartKind, restart), expected);
    }
}

#[test]
fn line_capabilities_round_trip_with_checks_and_localized_details() {
    let caps = LineCaps {
        site_id: "krill".into(),
        line_id: "overseas".into(),
        checked_at: Timestamp(1_792_139_400_123),
        reachable: true,
        latency_ms: Some(42),
        pat_ok: false,
        apikey_ok: true,
        codex_path: Some(CodexPath::CodexV1),
        blocked_by_edge: true,
        key_access: Some(KeyAccess::Unknown {
            status: Some(502),
            message: "gateway".into(),
        }),
        checks: vec![EndpointCheck {
            endpoint: ProbeEndpoint::WhamAccountsCheck,
            status: Some(200),
            latency_ms: Some(41),
            ok: false,
            edge_blocked: true,
            detail: Some("empty response".into()),
        }],
        detail: Some(
            LocalizedText::new("test.probe", "Probe took {latency} ms").arg("latency", 42),
        ),
    };
    let encoded = json_round_trip!(LineCaps, caps);
    assert_eq!(encoded["checked_at"], 1_792_139_400_123_i64);
    assert_eq!(encoded["codex_path"], "codex_v1");
    assert_eq!(encoded["key_access"]["kind"], "unknown");
    assert_eq!(encoded["checks"][0]["endpoint"], "wham_accounts_check");
    assert_eq!(encoded["detail"]["args"], json!([["latency", "42"]]));
}

#[test]
fn overview_round_trips_nested_app_cli_remote_account_and_notice_values() {
    let overview = Overview {
        generated_at: Timestamp(1_234),
        codex_home: "/fixtures/codex".into(),
        active: Some(ActiveState {
            profile_id: Some("work".into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::Pat),
            generation: 7,
            applied_at: Timestamp(1_000),
        }),
        config: None,
        app: AppOverview {
            installs: vec![AppInstall {
                id: "codex-app".into(),
                variant: AppVariant::Codex,
                path: "/fixtures/app".into(),
                executable: "/fixtures/app/bin/codex".into(),
                app_version: Some("26.1013".into()),
                bundled_codex: Some("/fixtures/app/bin/codex-cli".into()),
                bundled_codex_version: Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap()),
            }],
            session: Some(AppSession {
                app_server_exe: None,
                install_id: Some("codex-app".into()),
                main_pid: 100,
                started_at: Some(Timestamp(900)),
                executable: Some("/fixtures/app/bin/codex".into()),
                cdp: Some(CdpEndpoint {
                    port: 8123,
                    owner_verified: true,
                }),
                launched_by_us: true,
                authapi_env: EnvObservation::Present("https://api.krill-code.net".into()),
                home_override: None,
                codex_home: None,
            }),
            hot_switch: HotSwitchStatus::Ready,
            authapi_line: Some("overseas".into()),
        },
        cli: CliOverview {
            binaries: vec![CodexBinary {
                path: "/fixtures/bin/codex".into(),
                version: Some(CodexVersion::new(0, 162, 0)),
                source: InstallSource::Npm,
                path_rank: Some(0),
            }],
            processes: vec![ProcessInfo {
                pid: 200,
                ppid: Some(1),
                kind: CodexProcessKind::CliTui,
                exe: Some("/fixtures/bin/codex".into()),
                cmdline: vec!["codex".into()],
                started_at: Some(Timestamp(500)),
                authapi_env: EnvObservation::Absent,
                codex_home: None,
                stale: true,
            }],
        },
        remotes: vec![RemoteOverview {
            host: RemoteHost {
                target_id: TargetId::remote("build"),
                label: "Build host".into(),
                host_id: Some("build".into()),
                kind: superba_core::HostKind::Ssh,
                ssh: SshEndpoint {
                    alias: Some("builder".into()),
                    ..SshEndpoint::default()
                },
                sources: vec![HostSource::GlobalState, HostSource::SshConfig],
                connected: Some(false),
                wsl: None,
            },
            state: None,
            error: Some(Error::Ssh {
                host: "builder".into(),
                message: "offline".into(),
            }),
        }],
        account: Some(AccountSummary {
            display_name: Some("Test user".into()),
            key_last4: Some("abcd".into()),
            balance: Some("12.34".into()),
            currency: Some("USD".into()),
            usage: vec![UsageWindow {
                label: "daily".into(),
                used_percent: Some(12.5),
                resets_at: Some(Timestamp(2_000)),
            }],
            ..AccountSummary::default()
        }),
        baseline_captured: true,
        baseline_at: Some(Timestamp::from_millis(1_760_000_000_000)),
        login_stash: Some(superba_core::LoginStashInfo {
            stashed_at: Timestamp::from_millis(1_760_000_000_000),
            last_refresh: None,
            id_token_exp: None,
            email: Some("someone@example.com".into()),
        }),
        notices: vec![Notice::warning(
            LocalizedText::new("test.stale", "{count} old process").arg("count", 1),
        )],
    };
    let encoded = json_round_trip!(Overview, overview);
    assert_eq!(encoded["active"]["auth_mode"], "pat");
    assert_eq!(
        encoded["app"]["installs"][0]["bundled_codex_version"],
        "0.162.0-alpha.17.2"
    );
    assert_eq!(encoded["app"]["session"]["authapi_env"]["kind"], "present");
    assert_eq!(encoded["remotes"][0]["host"]["target_id"], "ssh:build");
    assert_eq!(encoded["remotes"][0]["error"]["code"], "ssh");
    assert_eq!(encoded["account"]["balance"], "12.34");
}

#[test]
fn file_write_serialization_excludes_secret_contents() {
    // FileChange is serialize-only: a deserialized Write could not carry its contents.
    let change = FileChange::Write {
        sha256: "test-hash".into(),
        contents: SecretBytes::new(b"private-auth-token".to_vec()),
    };
    let encoded = serde_json::to_value(&change).unwrap();
    assert_eq!(encoded, json!({"kind":"write", "sha256":"test-hash"}));
    let serialized = serde_json::to_string(&change).unwrap();
    assert!(!serialized.contains("contents"));
    assert!(!serialized.contains("private-auth-token"));
    assert_eq!(
        serde_json::to_value(FileChange::Unchanged).unwrap(),
        json!({"kind":"unchanged"})
    );
    assert_eq!(
        serde_json::to_value(FileChange::Remove).unwrap(),
        json!({"kind":"remove"})
    );
}

#[test]
fn empty_settings_deserialize_to_defaults_and_partial_settings_preserve_other_defaults() {
    let settings: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.countdown_secs, 5);
    assert_eq!(settings.default_auth_mode, AuthMode::Pat);
    assert_eq!(settings.theme, Theme::System);
    assert_eq!(settings.locale, None);
    assert!(settings.auto_probe);
    assert!(!settings.safe_mode);
    assert!(!settings.takeover_app);
    assert!(!settings.tray);
    let partial: Settings =
        serde_json::from_value(json!({"locale":"zh-CN", "safe_mode":true, "countdown_secs":0}))
            .unwrap();
    assert_eq!(
        partial,
        Settings {
            locale: Some(Locale::ZhCn),
            safe_mode: true,
            countdown_secs: 0,
            ..Settings::default()
        }
    );
    json_round_trip!(Settings, partial);
}

#[test]
fn timestamps_serialize_as_signed_millisecond_integers() {
    for millis in [0, -1, 1_792_139_400_123] {
        assert_eq!(
            json_round_trip!(Timestamp, Timestamp(millis)),
            json!(millis)
        );
    }
}
