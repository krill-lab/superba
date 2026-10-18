//! v2 planning over in-memory fakes: auth Auto, line priority, Codex settings
//! (plans, preview, settings page, edits), router mode and its watchdog.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AuthChoice, AuthMode, BlockReason, CatalogFeature, CatalogKey, CatalogSnapshot, ChangeReason,
    CodexVersion, Error, ExecuteOptions, FeatureStage, KeyAccess, NoEvents, ProfileId,
    ResolvedMode, RestartKind, RouterInstallRequest, RouterWatchState, SafeDefaults, SettingEntry,
    SettingEnumValue, SettingKind, SettingValue, SettingsEdit, SettingsViewRequest, StaticCatalog,
    SwitchIntent, SwitchPlan, SwitchRequest, SwitchService, TargetId,
};
use superba_switch::{Switcher, Tuning};

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn tuning() -> Tuning {
    Tuning {
        poll: Duration::from_millis(10),
        verify_timeout: Duration::from_millis(200),
        launch_verify_timeout: Duration::from_millis(200),
        stable: Duration::ZERO,
        reask_after: Duration::from_secs(60),
        plan_cache: 16,
    }
}

fn catalog() -> StaticCatalog {
    let snapshot = |v: &str| {
        let mut verbosity = CatalogKey::new("model_verbosity", SettingKind::Enum);
        verbosity.enum_values = ["low", "medium", "high"]
            .into_iter()
            .map(|v| SettingEnumValue {
                value: v.into(),
                description: None,
            })
            .collect();
        verbosity.description = Some("Verbosity of the model's answers.".into());
        CatalogSnapshot::new(CodexVersion::parse(v).unwrap())
            .with_key(verbosity)
            .with_feature(CatalogFeature {
                key: "fast_mode".into(),
                config_path: "features.fast_mode".into(),
                stage: FeatureStage::Stable,
                default_enabled: false,
                name: None,
                menu_description: None,
            })
            .with_feature(CatalogFeature {
                key: "lab".into(),
                config_path: "features.lab".into(),
                stage: FeatureStage::UnderDevelopment,
                default_enabled: false,
                name: None,
                menu_description: None,
            })
    };
    StaticCatalog::new([snapshot("0.162.0-alpha.17.2"), snapshot("0.162.0")])
}

fn switcher_with_catalog(w: &World) -> Switcher {
    let mut deps = w.deps();
    deps.catalog = Arc::new(catalog());
    deps.explain_zh = Arc::new(|path: &str| (path == "model_verbosity").then(|| "人工撰写".into()));
    Switcher::with_tuning(deps, tuning())
}

/// Edit the `work` profile as a v2 profile (only its own Codex settings).
fn set_profile(w: &World, f: impl FnOnce(&mut superba_core::Profile)) {
    let mut set = w.profiles.set.lock().unwrap();
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    f(p);
}

fn desired(plan: &SwitchPlan) -> &superba_core::ResolvedProfile {
    plan.config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan")
}

fn token(plan: &SwitchPlan) -> (AuthMode, String) {
    match &desired(plan).mode {
        ResolvedMode::Token {
            auth, endpoints, ..
        } => (*auth, endpoints.openai_base_url.to_string()),
        other => panic!("unexpected mode {other:?}"),
    }
}

fn warning_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings.iter().map(|n| n.text.key.clone()).collect()
}

fn unusable(line: &str) -> superba_core::LineCaps {
    superba_core::LineCaps {
        reachable: false,
        codex_path: None,
        ..good_caps("krill", line)
    }
}

// --- auth Auto and line priority -----------------------------------------------------

#[tokio::test]
async fn auto_uses_pat_where_the_line_supports_it_and_falls_back_with_a_reason() {
    let w = World::new();
    set_profile(&w, |p| p.set_auth(AuthChoice::Auto));
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert_eq!(token(&plan).0, AuthMode::Pat);

    w.prober.caps.lock().unwrap().insert(
        "overseas".into(),
        superba_core::LineCaps {
            pat_ok: false,
            ..good_caps("krill", "overseas")
        },
    );
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert_eq!(token(&plan).0, AuthMode::ApiKey);
    assert!(warning_keys(&plan).contains(&"switch.warn.pat_fallback".to_owned()));
}

#[tokio::test]
async fn auto_uses_an_api_key_when_a_local_codex_is_too_old_for_pat() {
    let w = World::new();
    set_profile(&w, |p| p.set_auth(AuthChoice::Auto));
    // The app's bundled Codex reads top-level openai_base_url but has no PAT.
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 150, 0));
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(token(&plan).0, AuthMode::ApiKey);
    assert!(warning_keys(&plan).contains(&"switch.warn.pat_fallback".to_owned()));
    assert!(plan.env.is_none(), "API key mode needs no auth address");

    // Same for the CLI first on the PATH: PAT would break it.
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 162, 0));
    w.app.binaries.lock().unwrap()[0].version = Some(CodexVersion::new(0, 155, 0));
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(token(&plan).0, AuthMode::ApiKey);

    // Unknown versions keep PAT; the version gate asks about them.
    w.app.binaries.lock().unwrap()[0].version = None;
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert_eq!(token(&plan).0, AuthMode::Pat);

    // Remote-only switches do not look at the local Codex versions.
    w.app.binaries.lock().unwrap()[0].version = Some(CodexVersion::new(0, 155, 0));
    let mut r = req("work");
    r.targets.local = false;
    let plan = w.switcher().plan(r).await.unwrap();
    assert_eq!(plan.to.auth_mode, Some(AuthMode::Pat));
}

#[tokio::test]
async fn manual_pat_never_falls_back() {
    let w = World::new();
    set_profile(&w, |p| p.set_auth(AuthChoice::Pat));
    w.prober.caps.lock().unwrap().insert(
        "overseas".into(),
        superba_core::LineCaps {
            pat_ok: false,
            ..good_caps("krill", "overseas")
        },
    );
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(matches!(
        plan.primary,
        RestartKind::Blocked { ref reasons }
            if reasons.iter().any(|r| matches!(r, BlockReason::AuthModeUnavailable { auth_mode: AuthMode::Pat, .. }))
    ));
}

#[tokio::test]
async fn an_unusable_primary_line_falls_back_to_the_next_line_in_priority() {
    let w = World::new();
    set_profile(&w, |p| {
        p.set_line_priority(["cn-fast".into(), "cdn".into(), "overseas".into()])
    });
    w.prober
        .caps
        .lock()
        .unwrap()
        .insert("cn-fast".into(), unusable("cn-fast"));
    w.prober.caps.lock().unwrap().insert(
        "cdn".into(),
        superba_core::LineCaps {
            key_access: Some(KeyAccess::AccessPointDenied),
            ..good_caps("krill", "cdn")
        },
    );
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(!plan.primary.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        plan.to.line_id.as_ref().map(|l| l.as_str()),
        Some("overseas")
    );
    assert!(token(&plan).1.starts_with(OVERSEAS));
    let fallback = plan
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.warn.line_fallback")
        .expect("fallback note");
    assert_eq!(
        fallback
            .text
            .nested
            .iter()
            .find(|(k, _)| k == "line")
            .map(|(_, v)| v.fallback.as_str()),
        Some("Overseas")
    );
    let probes: Vec<_> = w
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("prober.probe_with_key"))
        .collect();
    assert_eq!(
        probes,
        [
            "prober.probe_with_key cn-fast",
            "prober.probe_with_key cdn",
            "prober.probe_with_key overseas"
        ]
    );
}

#[tokio::test]
async fn an_explicit_line_never_falls_back_and_the_last_line_reports_its_blocks() {
    let w = World::new();
    set_profile(&w, |p| {
        p.set_line_priority(["cn-fast".into(), "overseas".into()])
    });
    w.prober
        .caps
        .lock()
        .unwrap()
        .insert("cn-fast".into(), unusable("cn-fast"));
    let mut r = req("work");
    r.intent = SwitchIntent::Profile {
        profile_id: ProfileId::new("work"),
        line_id: Some("cn-fast".into()),
        auth_mode: None,
    };
    let plan = w.switcher().plan(r).await.unwrap();
    assert!(plan.primary.is_blocked());

    w.prober
        .caps
        .lock()
        .unwrap()
        .insert("overseas".into(), unusable("overseas"));
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert!(matches!(
        plan.primary,
        RestartKind::Blocked { ref reasons }
            if reasons == &vec![BlockReason::LineUnavailable { line_id: "overseas".into() }]
    ));
}

#[tokio::test]
async fn overrides_are_stored_as_primary_line_and_manual_auth() {
    let w = World::new();
    set_profile(&w, |p| {
        p.set_line_priority(["overseas".into(), "cdn".into()])
    });
    let sw = w.switcher();
    let mut r = req("work");
    r.intent = SwitchIntent::Profile {
        profile_id: ProfileId::new("work"),
        line_id: Some("cdn".into()),
        auth_mode: Some(AuthMode::ApiKey),
    };
    let plan = sw.plan(r).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    let set = w.profiles.set.lock().unwrap().clone();
    let p = set.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(
        p.line_priority(),
        vec!["cdn".into(), "overseas".into()] as Vec<superba_core::LineId>
    );
    assert_eq!(p.auth_choice(), AuthChoice::ApiKey);
}

// --- Codex settings ------------------------------------------------------------------

#[tokio::test]
async fn profile_settings_are_planned_per_home_and_previewed() {
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("features.fast_mode", true).unwrap();
        p.codex_settings.set("model_verbosity", "high").unwrap();
        p.codex_settings.unset("features.lab").unwrap();
    });
    let sw = switcher_with_catalog(&w);
    let plan = sw.plan(req("work")).await.unwrap();
    let settings = &desired(&plan).settings;
    assert_eq!(
        settings.get("features.fast_mode"),
        Some(&SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert_eq!(
        settings.get("model_verbosity"),
        Some(&SettingEntry::Set(SettingValue::from("high")))
    );
    let ps = sw.plan_settings(&plan.id).await.unwrap();
    assert_eq!(ps.targets.len(), 1);
    assert_eq!(ps.targets[0].home, Some(TargetId::local()));
    assert_eq!(ps.targets[0].consumers.len(), 2, "app and CLI");
    let fast = ps
        .changes
        .iter()
        .find(|c| c.path == "features.fast_mode")
        .expect("previewed");
    assert_eq!(fast.to.as_deref(), Some("true"));
    assert_eq!(fast.reason, ChangeReason::Setting);
    assert_eq!(fast.target, TargetId::local());
    assert!(sw.plan_settings("nope").await.is_err());
}

#[tokio::test]
async fn remote_settings_go_only_through_a_sync_that_reads_the_hosts_config_and_owned_keys() {
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "high").unwrap();
    });
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    // The user keeps a table where the setting wants a plain value.
    *w.remote.config_toml.lock().unwrap() = Some("[model_verbosity]\nx = 1\n".into());
    let sw = w.switcher();
    let mut r = req("work");
    r.targets.remotes = vec![TargetId::remote("dev")];
    let plan = sw.plan(r.clone()).await.unwrap();
    let ps = sw.plan_settings(&plan.id).await.unwrap();
    // v0.3: a switch changes only the host's connection, never its settings.
    assert!(
        !ps.targets
            .iter()
            .any(|t| t.home == Some(TargetId::remote("dev")))
    );
    let local = ps
        .targets
        .iter()
        .find(|t| t.home == Some(TargetId::local()))
        .unwrap();
    assert!(local.apply.get("model_verbosity").is_some());

    // The sync preview filters for the host and keeps the user's table.
    let sync = sw.host_sync_plan(&TargetId::remote("dev")).await.unwrap();
    let row = sync
        .rows
        .iter()
        .find(|r| r.path == "model_verbosity")
        .expect("a row for the setting");
    assert!(
        matches!(
            &row.action,
            superba_core::HostSyncAction::Skip {
                reason: superba_core::SkipReason::ShapeConflict { .. }
            }
        ),
        "{row:?}"
    );
    assert!(!row.selected);

    // A value Superba wrote there itself is not the user's.
    *w.remote.owned_paths.lock().unwrap() = vec!["model_verbosity".into()];
    let sync = sw.host_sync_plan(&TargetId::remote("dev")).await.unwrap();
    let row = sync
        .rows
        .iter()
        .find(|r| r.path == "model_verbosity")
        .unwrap();
    assert_eq!(row.action, superba_core::HostSyncAction::Write);
    assert!(row.selected);
}

#[tokio::test]
async fn invalid_settings_are_skipped_with_a_note() {
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "loud").unwrap();
    });
    let sw = switcher_with_catalog(&w);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(desired(&plan).settings.get("model_verbosity").is_none());
    assert!(
        warning_keys(&plan).contains(&"switch.warn.setting_skipped".to_owned()),
        "{:?}",
        warning_keys(&plan)
    );
}

#[tokio::test]
async fn the_settings_view_lists_catalog_rows_with_values_and_hides_advanced_ones() {
    let w = World::new();
    std::fs::create_dir_all(w.ctx.codex_home().root()).unwrap();
    std::fs::write(
        w.ctx.codex_home().config_toml(),
        "model_verbosity = \"low\"\n[features]\nfast_mode = false\n",
    )
    .unwrap();
    set_profile(&w, |p| {
        p.codex_settings.set("features.fast_mode", true).unwrap();
        p.codex_settings.set("custom.thing", 1i64).unwrap();
    });
    let sw = switcher_with_catalog(&w);
    let view = sw
        .settings_view(SettingsViewRequest::default())
        .await
        .unwrap();
    assert_eq!(view.profile_id.as_str(), "work");
    let paths: Vec<_> = view.rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        ["custom.thing", "features.fast_mode", "model_verbosity"]
    );
    let verbosity = view
        .rows
        .iter()
        .find(|r| r.path == "model_verbosity")
        .unwrap();
    assert_eq!(verbosity.current.as_deref(), Some("\"low\""));
    assert_eq!(verbosity.zh.as_deref(), Some("人工撰写"));
    assert_eq!(verbosity.support.len(), 2);
    let fast = view
        .rows
        .iter()
        .find(|r| r.path == "features.fast_mode")
        .unwrap();
    assert_eq!(
        fast.managed,
        Some(SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert_eq!(
        fast.feature.as_ref().map(|f| f.stage),
        Some(FeatureStage::Stable)
    );
    assert!(
        view.rows
            .iter()
            .all(|r| r.zh.is_none() || r.path == "model_verbosity")
    );

    let advanced = sw
        .settings_view(SettingsViewRequest {
            advanced: true,
            paths: vec!["features".into()],
            ..SettingsViewRequest::default()
        })
        .await
        .unwrap();
    let paths: Vec<_> = advanced.rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["features.fast_mode", "features.lab"]);
    assert!(advanced.rows[1].advanced);
}

#[tokio::test]
async fn edits_are_validated_against_the_catalog() {
    let w = World::new();
    let sw = switcher_with_catalog(&w);
    let report = sw
        .edit_settings(SettingsEdit {
            set: vec![
                ("features.fast_mode".into(), SettingValue::Bool(true)),
                ("model_verbosity".into(), "medium".into()),
            ],
            reset: vec!["features.lab".into()],
            ..SettingsEdit::default()
        })
        .await
        .unwrap();
    let s = &report.profile.codex_settings;
    assert_eq!(
        s.get("features.fast_mode"),
        Some(&SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert_eq!(s.get("features.lab"), Some(&SettingEntry::Unset));
    let stored = w.profiles.set.lock().unwrap().clone();
    assert_eq!(
        &stored
            .profile(&ProfileId::new("work"))
            .unwrap()
            .codex_settings,
        s
    );

    for (path, value) in [
        ("model_verbosity", SettingValue::from("loud")),
        ("no.such.key", SettingValue::Bool(true)),
        ("features.fast_mode", SettingValue::from("yes")),
    ] {
        let e = sw
            .edit_settings(SettingsEdit {
                set: vec![(path.into(), value)],
                ..SettingsEdit::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Invalid { .. }), "{path}: {e}");
    }
    // Reserved paths belong to the connection.
    let e = sw
        .edit_settings(SettingsEdit {
            set: vec![("openai_base_url".into(), "https://x.example".into())],
            force: true,
            ..SettingsEdit::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e}");
    let forced = sw
        .edit_settings(SettingsEdit {
            set: vec![("no.such.key".into(), SettingValue::Bool(true))],
            forget: vec!["features.lab".into()],
            force: true,
            ..SettingsEdit::default()
        })
        .await
        .unwrap();
    assert!(forced.profile.codex_settings.contains("no.such.key"));
    assert!(!forced.profile.codex_settings.contains("features.lab"));
    assert!(!forced.notes.is_empty());
}

// --- router mode ---------------------------------------------------------------------

async fn router_on(sw: &Switcher) {
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
}

#[tokio::test]
async fn router_install_builds_members_with_keys_and_refuses_the_official_login() {
    let w = World::new();
    let sw = w.switcher();
    let e = sw
        .router_install(RouterInstallRequest {
            members: vec![ProfileId::new("official")],
        })
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "{e}");
    let e = sw.router_set_enabled(true).await.unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "no members yet: {e}");
    router_on(&sw).await;
    assert_eq!(
        *w.router.configs.lock().unwrap(),
        vec![vec!["work".to_owned()]]
    );
    let view = sw.router_status().await.unwrap();
    assert!(view.settings.enabled && view.running && view.installed);
    assert!(
        !view.codex_routed,
        "Codex is pointed at it only by a switch"
    );
    assert_eq!(view.members[0].profile_id.as_str(), "work");
}

#[tokio::test]
async fn router_mode_points_the_local_home_at_the_router_in_api_key_mode() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.primary.is_blocked(), "{:?}", plan.primary);
    let (auth, base) = token(&plan);
    assert_eq!(auth, AuthMode::ApiKey);
    assert_eq!(base, FakeRouter::base_url(18_431));
    match &desired(&plan).mode {
        ResolvedMode::Token { key, .. } => assert_eq!(key.expose(), ROUTER_TOKEN),
        _ => unreachable!(),
    }
    assert!(warning_keys(&plan).contains(&"switch.warn.router_mode".to_owned()));
    assert_eq!(plan.to.label.key, "switch.destination.router");
    assert!(plan.env.is_none(), "no auth API env in router mode");
    assert!(
        !w.calls()
            .iter()
            .any(|c| c.starts_with("prober.probe_with_key")),
        "lines are the router's business"
    );
    // Execute: Codex now goes through the router.
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert!(sw.router_status().await.unwrap().codex_routed);
}

#[tokio::test]
async fn switching_profiles_in_router_mode_reorders_members_without_a_restart() {
    let w = World::new();
    w.profiles.set.lock().unwrap().profiles.push(krill_profile(
        "second",
        Some("cdn"),
        AuthMode::ApiKey,
    ));
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:second".into(), "sk-second-key-2222".into());
    let sw = w.switcher();
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work"), ProfileId::new("second")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
    let plan = sw.plan(req("work")).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    *w.config.noop_plan.lock().unwrap() = true;
    let plan = sw.plan(req("second")).await.unwrap();
    assert_eq!(plan.primary, RestartKind::None, "{:?}", plan.primary);
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert_eq!(
        w.router.configs.lock().unwrap().last().unwrap(),
        &vec!["second".to_owned(), "work".to_owned()]
    );
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.router.members,
        vec![ProfileId::new("second"), ProfileId::new("work")]
    );
}

#[tokio::test]
async fn a_dead_router_blocks_router_mode_switches() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    *w.router.up.lock().unwrap() = false;
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(matches!(
        plan.primary,
        RestartKind::Blocked { ref reasons }
            if reasons.iter().any(|r| matches!(r, BlockReason::RouterUnavailable { .. }))
    ));
}

#[tokio::test]
async fn remote_hosts_stay_direct_in_router_mode() {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("ssh:dev")));
    let sw = w.switcher();
    router_on(&sw).await;
    let mut r = req("work");
    r.targets.remotes = vec![TargetId::new("ssh:dev")];
    let plan = sw.plan(r).await.unwrap();
    assert_eq!(token(&plan).1, FakeRouter::base_url(18_431));
    let remote = plan.remotes.first().expect("remote plan");
    match &remote.desired.as_ref().unwrap().mode {
        ResolvedMode::Token { endpoints, key, .. } => {
            assert!(endpoints.openai_base_url.as_str().starts_with(OVERSEAS));
            assert_eq!(key.expose(), KEY);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn disabling_router_mode_switches_back_to_direct_and_uninstall_needs_it() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    let e = sw.router_uninstall().await.unwrap_err();
    assert!(matches!(e, Error::Conflict { .. }), "{e}");
    sw.router_set_enabled(false).await.unwrap();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(token(&plan).1.starts_with(OVERSEAS));
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    let view = sw.router_uninstall().await.unwrap();
    assert!(!view.installed && !view.running && !view.codex_routed);
    assert!(w.calls().contains(&"router.uninstall".to_owned()));
}

#[tokio::test]
async fn the_watchdog_falls_back_to_direct_mode_after_the_grace_period() {
    let w = World::new();
    let sw = w.switcher();
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Inactive
    );
    router_on(&sw).await;
    let plan = sw.plan(req("work")).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Healthy
    );
    *w.router.up.lock().unwrap() = false;
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::Suspect { .. }),
        "{tick:?}"
    );
    w.clock.advance(Duration::from_secs(10));
    assert!(matches!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Suspect { .. }
    ));
    w.clock.advance(Duration::from_secs(15));
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::FellBack { .. }),
        "{tick:?}"
    );
    assert!(tick.outcome.is_some(), "{tick:?}");
    let view = sw.router_status().await.unwrap();
    assert!(!view.settings.enabled);
    assert!(!view.codex_routed, "Codex is back on a direct line");
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Inactive
    );
}

async fn route_codex(sw: &Switcher) {
    let plan = sw.plan(req("work")).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert!(sw.router_status().await.unwrap().codex_routed);
}

#[tokio::test]
async fn codex_on_an_earlier_router_port_still_counts_as_routed() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&sw).await;
    // The router's configuration moved to another port; Codex did not follow.
    *w.router.port.lock().unwrap() = Some(19_000);
    let view = sw.router_status().await.unwrap();
    assert!(view.codex_routed, "the watchdog and guards still see it");
    let e = sw.router_uninstall().await.unwrap_err();
    assert!(matches!(e, Error::Conflict { .. }), "{e}");
    // An unrelated loopback base URL with another key is not the router.
    w.config.state.lock().unwrap().auth = superba_core::AuthShape::ApiKey {
        key_last4: "9999".into(),
    };
    assert!(!sw.router_status().await.unwrap().codex_routed);
}

#[tokio::test]
async fn a_failed_fallback_keeps_router_mode_on_and_is_retried() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&sw).await;
    *w.router.up.lock().unwrap() = false;
    *w.config.fail_apply.lock().unwrap() = Some(Error::other("disk full"));
    assert!(matches!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Suspect { .. }
    ));
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert!(tick.error.is_some(), "{tick:?}");
    assert!(
        matches!(tick.state, RouterWatchState::Suspect { .. }),
        "{tick:?}"
    );
    assert!(
        w.profiles.set.lock().unwrap().settings.router.enabled,
        "router mode stays on until the direct switch succeeded"
    );
    assert!(sw.router_status().await.unwrap().codex_routed);

    // The next ticks keep watching and fall back once the switch works.
    *w.config.fail_apply.lock().unwrap() = None;
    assert!(matches!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Suspect { .. }
    ));
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::FellBack { .. }),
        "{tick:?}"
    );
    let view = sw.router_status().await.unwrap();
    assert!(!view.settings.enabled && !view.codex_routed);
}

#[tokio::test]
async fn the_fallback_keeps_owned_keys_the_app_changed() {
    let w = World::new();
    let sw = w.switcher();
    router_on(&sw).await;
    route_codex(&sw).await;
    w.config.state.lock().unwrap().drift = vec![superba_core::KeyChange {
        path: "model".into(),
        before: Some("\"gpt-5.5\"".into()),
        after: Some("\"gpt-6\"".into()),
    }];
    *w.router.up.lock().unwrap() = false;
    sw.router_watch().await.unwrap();
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::FellBack { .. }),
        "{tick:?}"
    );
    assert!(!sw.router_status().await.unwrap().codex_routed);
}

#[tokio::test]
async fn deleting_or_rekeying_a_router_member_updates_the_routers_keys() {
    let w = World::new();
    w.profiles.set.lock().unwrap().profiles.push(krill_profile(
        "second",
        Some("cdn"),
        AuthMode::ApiKey,
    ));
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:second".into(), "sk-second-key-2222".into());
    let sw = w.switcher();
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work"), ProfileId::new("second")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
    route_codex(&sw).await;

    // A new key for a member reaches the router's key file.
    let second = w
        .profiles
        .set
        .lock()
        .unwrap()
        .profile(&ProfileId::new("second"))
        .cloned()
        .unwrap();
    sw.save_profile(second, Some(superba_core::Secret::new("sk-rotated-3333")))
        .await
        .unwrap();
    assert!(w.calls().contains(&"router.rewrite work,second".to_owned()));

    // Deleting a member drops it (and its key) from the router.
    sw.delete_profile(&ProfileId::new("second")).await.unwrap();
    assert_eq!(
        w.router.configs.lock().unwrap().last().unwrap(),
        &vec!["work".to_owned()]
    );
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.router.members,
        vec![ProfileId::new("work")]
    );
    assert!(
        sw.router_install(RouterInstallRequest::default())
            .await
            .is_ok()
    );

    // The last member cannot go while Codex goes through the router.
    let e = sw
        .delete_profile(&ProfileId::new("work"))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Conflict { .. }), "{e}");
    assert!(
        w.profiles
            .set
            .lock()
            .unwrap()
            .profile(&ProfileId::new("work"))
            .is_some()
    );

    // Once Codex is direct, deleting it removes the router and its key file.
    let mut off = req("work");
    off.intent = SwitchIntent::Router { enable: false };
    let plan = sw.plan(off).await.unwrap();
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    sw.delete_profile(&ProfileId::new("work")).await.unwrap();
    assert!(w.calls().contains(&"router.uninstall".to_owned()));
    let set = w.profiles.set.lock().unwrap().clone();
    assert!(set.settings.router.members.is_empty() && !set.settings.router.enabled);
}

#[tokio::test]
async fn router_mode_keeps_the_codex_model_name_for_mapped_sites() {
    let w = World::new();
    let sw = w.switcher();
    let base = url::Url::parse("https://api.acme.example/v1").unwrap();
    let mut site =
        superba_switch::new_compatible_site(superba_core::SiteId::new("acme"), "Acme", &base)
            .unwrap();
    site.model_map.insert("gpt-5.5".into(), "glm-4.6".into());
    sw.save_site(site).await.unwrap();
    let p = superba_core::Profile {
        site_id: superba_core::SiteId::new("acme"),
        line_id: None,
        model: superba_core::ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: Some("gpt-5.5".into()),
            reasoning_effort: None,
        },
        ..krill_profile("acme", None, AuthMode::ApiKey)
    };
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:acme".into(), KEY.into());
    sw.save_profile(p, None).await.unwrap();
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("acme"), ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(true).await.unwrap();
    *w.prober.models.lock().unwrap() = Ok(vec!["glm-4.6".into()]);
    let plan = sw.plan(req("acme")).await.unwrap();
    assert_eq!(token(&plan).1, FakeRouter::base_url(18_431));
    assert_eq!(
        desired(&plan).model.model.as_deref(),
        Some("gpt-5.5"),
        "the router maps names per member"
    );
    assert!(!warning_keys(&plan).contains(&"switch.warn.model_mapped".to_owned()));
}

#[tokio::test]
async fn the_router_intent_stores_router_mode_only_after_the_switch() {
    let w = World::new();
    let sw = w.switcher();
    let mut intent = req("work");
    intent.intent = SwitchIntent::Router { enable: true };
    let e = sw.plan(intent.clone()).await.unwrap_err();
    assert!(matches!(e, Error::Invalid { .. }), "no members yet: {e}");
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    let plan = sw.plan(intent.clone()).await.unwrap();
    assert_eq!(token(&plan).1, FakeRouter::base_url(18_431));
    assert_eq!(
        plan.to.profile_id.as_ref().map(|p| p.as_str()),
        Some("work")
    );
    assert!(
        !w.profiles.set.lock().unwrap().settings.router.enabled,
        "planning stores nothing"
    );
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert!(w.profiles.set.lock().unwrap().settings.router.enabled);
    assert!(sw.router_status().await.unwrap().active());

    let mut off = req("work");
    off.intent = SwitchIntent::Router { enable: false };
    let plan = sw.plan(off).await.unwrap();
    assert!(token(&plan).1.starts_with(OVERSEAS));
    sw.execute(
        plan,
        ExecuteOptions::default(),
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    let view = sw.router_status().await.unwrap();
    assert!(!view.settings.enabled && !view.codex_routed);
    let stored = w.profiles.set.lock().unwrap().clone();
    assert_eq!(
        stored
            .profile(&ProfileId::new("work"))
            .unwrap()
            .line_priority(),
        vec!["overseas".into()] as Vec<superba_core::LineId>,
        "router mode never changes the profile's lines"
    );
}

#[tokio::test]
async fn uninstalling_superba_removes_the_local_router_too() {
    let w = World::new();
    w.set_session(None);
    let sw = w.switcher();
    router_on(&sw).await;
    let mut r = req("work");
    r.intent = SwitchIntent::Uninstall {
        delete_backups: false,
        provider: Some(superba_core::ProviderRestore::KeepOpenai),
    };
    let plan = sw.plan(r).await.unwrap();
    let outcome = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            Arc::new(SafeDefaults),
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        superba_core::OutcomeStatus::Switched,
        "{:?}",
        outcome.notices
    );
    assert!(w.calls().contains(&"router.uninstall".to_owned()));
    assert!(!w.profiles.set.lock().unwrap().settings.router.enabled);
}
