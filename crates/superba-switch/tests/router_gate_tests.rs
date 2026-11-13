//! User bug 39 (0.1.1-alpha.1 on Windows): Krill with its API key was already
//! the router's member and the router was answering, `spb router doctor` said
//! "成员 · 可以作为成员（有 API key）", yet the Router page kept 开启 disabled.
//! The service side of the one rule: the gate, the member router mode applies
//! and the doctor agree, and turning router mode on with Auto auth on a
//! PAT-capable Krill profile writes a local PAT form without changing the profile's auth choice.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use superba_core::{
    AuthMode, ExecuteOptions, Locale, NoEvents, OutcomeStatus, ProfileId, ResolvedMode,
    RouterInstallRequest, SafeDefaults, SwitchIntent, SwitchPlan, SwitchRequest, SwitchService,
    catalog,
};
use superba_switch::router_doctor::{
    eligible_members, has_eligible_member, report, router_candidates, router_mode_profile,
};
use superba_switch::{Switcher, Tuning, messages};

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn router_on_request() -> SwitchRequest {
    let mut r = SwitchRequest::profile(ProfileId::new(""));
    r.intent = SwitchIntent::Router { enable: true };
    r
}

/// The world's fakes with the real config engine writing `config.toml` and
/// `auth.json` under the temporary home.
fn switcher(w: &World) -> Switcher {
    let mut deps = w.deps();
    deps.config = Arc::new(superba_config::TomlConfigEngine::new(w.ctx.clone()));
    Switcher::with_tuning(
        deps,
        Tuning {
            poll: Duration::from_millis(10),
            verify_timeout: Duration::from_millis(200),
            launch_verify_timeout: Duration::from_millis(200),
            stable: Duration::ZERO,
            reask_after: Duration::from_secs(60),
            plan_cache: 16,
            windows_app: false,
        },
    )
}

async fn execute(sw: &Switcher, plan: SwitchPlan) -> superba_core::SwitchOutcome {
    sw.execute(
        plan,
        ExecuteOptions {
            allow_full_restart: true,
            turn_quiet_ms: 0,
            ..ExecuteOptions::default()
        },
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap()
}

fn read(w: &World, file: &str) -> Vec<u8> {
    std::fs::read(w.ctx.codex_home().root().join(file)).unwrap_or_default()
}

fn token_mode(plan: &SwitchPlan) -> (AuthMode, String) {
    let desired = plan.config.as_ref().and_then(|c| c.desired.as_ref());
    match &desired.expect("a desired profile").mode {
        ResolvedMode::Token {
            auth, endpoints, ..
        } => (*auth, endpoints.openai_base_url.to_string()),
        other => panic!("unexpected mode {other:?}"),
    }
}

/// The user's state: Krill ("work": Auto auth, key stored) applied directly
/// with a PAT, then made the only router member with the router answering.
async fn users_state() -> (World, Switcher) {
    let w = World::new();
    let sw = switcher(&w);
    let work = sw
        .profile_set()
        .await
        .unwrap()
        .profile(&ProfileId::new("work"))
        .cloned()
        .unwrap();
    assert_eq!(work.auth_mode, AuthMode::ApiKey);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        token_mode(&plan).0,
        AuthMode::ApiKey,
        "Auto uses the PAT direct"
    );
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
    let (mode, _) = superba_config::authjson::token_credentials(&read(&w, "auth.json")).unwrap();
    assert_eq!(mode, AuthMode::ApiKey);
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    assert!(sw.router_status().await.unwrap().running);
    (w, sw)
}

#[tokio::test]
async fn router_catalog_edits_are_shared_and_reloaded_without_replacing_direct_overrides() {
    let (w, sw) = users_state().await;
    sw.router_set_enabled(true).await.unwrap();
    let value = superba_core::ModelOverride {
        context_window: Some(64000),
        auto_compact: Some(48000),
        hidden: true,
        ..Default::default()
    };
    sw.save_models(superba_core::ModelsEdit {
        profile_id: Some(ProfileId::new("work")),
        set: [("krill/gpt-6.1-sol".into(), value.clone())].into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let set = sw.profile_set().await.unwrap();
    assert_eq!(
        set.settings.router.catalog.overrides["krill/gpt-6.1-sol"],
        value
    );
    assert!(
        set.profile(&ProfileId::new("work"))
            .unwrap()
            .model_overrides
            .is_empty()
    );
    {
        let written = w.router.written.lock().unwrap();
        let catalog = written.last().unwrap().catalog.as_ref().unwrap();
        assert_eq!(catalog.settings.overrides["krill/gpt-6.1-sol"], value);
        assert!(!catalog.settings.visibility["krill/gpt-6.1-sol"]);
    }
    sw.save_models(superba_core::ModelsEdit {
        profile_id: Some(ProfileId::new("work")),
        reset: vec!["krill/gpt-6.1-sol".into()],
        ..Default::default()
    })
    .await
    .unwrap();
    let set = sw.profile_set().await.unwrap();
    assert!(set.settings.router.catalog.overrides.is_empty());
    assert!(set.settings.router.catalog.visibility.is_empty());
}

#[tokio::test]
async fn router_globals_are_folded_into_live_entries_not_global_codex_keys() {
    let (w, sw) = users_state().await;
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        p.model.context_window = Some(64000);
        p.model.auto_compact = Some(48000);
    }
    let plan = sw.plan(router_on_request()).await.unwrap();
    assert!(!plan.is_blocked());
    let desired = plan.config.as_ref().unwrap().desired.as_ref().unwrap();
    assert!(desired.model.context_window.is_none());
    assert!(desired.model.auto_compact.is_none());
    assert!(desired.model_catalog.is_none());
}

#[tokio::test]
async fn with_the_speed_menu_a_routed_codex_gets_the_members_models_in_a_catalog_file() {
    // Codex signs in to the router with an API key and never fetches the
    // router's catalog: the app would list only its bundled models.
    let (w, sw) = users_state().await;
    w.profiles.set.lock().unwrap().settings.app_speed_menu = true;
    let plan = sw.plan(router_on_request()).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let desired = plan.config.as_ref().unwrap().desired.as_ref().unwrap();
    assert!(
        desired.model_catalog.is_some(),
        "a catalog file from the members' lists"
    );
    assert!(desired.model.context_window.is_none());
    // A file that cannot be made does not block router mode.
    *w.app.preflight_error.lock().unwrap() = Some(superba_core::Error::parse(
        "model_catalog_json",
        "missing field `base_instructions`",
    ));
    let plan = sw.plan(router_on_request()).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let desired = plan.config.as_ref().unwrap().desired.as_ref().unwrap();
    assert!(desired.model_catalog.is_none());
    assert!(
        plan.warnings
            .iter()
            .any(|n| n.text.key == "switch.models.menu_catalog_later")
    );
}

#[tokio::test]
async fn a_krill_member_with_its_key_turns_router_mode_on_with_a_local_pat() {
    let (w, sw) = users_state().await;
    let set = sw.profile_set().await.unwrap();
    let sites = sw.sites().await.unwrap();

    // The gate, the doctor and the planner see the same thing.
    let candidates = router_candidates(&set, &sites);
    assert!(has_eligible_member(&candidates));
    assert_eq!(
        eligible_members(&candidates)
            .map(ProfileId::as_str)
            .collect::<Vec<_>>(),
        ["work"]
    );
    let doctor = sw.router_doctor().await.unwrap();
    assert!(
        !doctor
            .findings
            .iter()
            .any(|f| f.text.key == "switch.router_eligibility.none_eligible"),
        "{:?}",
        doctor.findings
    );
    let catalog = messages::catalog(Locale::ZhCn);
    let text = doctor.render_text(&|t| catalog.text(t));
    assert!(text.contains("成员 · 可以作为成员（有 API key）"), "{text}");
    sw.router_set_enabled(false).await.unwrap();

    // Turning router mode on: the routed configuration uses a local PAT
    // with the router's endpoint and token, whatever Auto picked direct.
    let plan = sw.plan(router_on_request()).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        plan.to.profile_id.as_ref().map(ProfileId::as_str),
        Some("work")
    );
    assert_eq!(
        token_mode(&plan),
        (AuthMode::ApiKey, FakeRouter::base_url(18_431))
    );
    assert_eq!(plan.to.auth_mode, Some(AuthMode::ApiKey));
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");

    let config = String::from_utf8(read(&w, "config.toml")).unwrap();
    assert!(
        config.contains(&FakeRouter::base_url(18_431)),
        "config.toml points at the router:\n{config}"
    );
    let auth = read(&w, "auth.json");
    let (mode, token) = superba_config::authjson::token_credentials(&auth).unwrap();
    assert_eq!(mode, AuthMode::ApiKey, "auth.json uses the local PAT");
    assert_eq!(token.expose(), ROUTER_TOKEN, "Codex holds the router token");
    assert!(
        !String::from_utf8_lossy(&auth).contains(KEY),
        "the Krill key stays with the router"
    );

    // Router mode is stored; the profile keeps its choice (Auto) and lines.
    let set = sw.profile_set().await.unwrap();
    assert!(set.settings.router.enabled);
    let work = set.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(work.auth_mode, AuthMode::ApiKey);
    assert!(sw.router_status().await.unwrap().codex_routed);
}

#[tokio::test]
async fn turning_the_router_on_applies_a_member_never_another_profile() {
    let (w, sw) = users_state().await;
    // Another Krill profile with a key, applied, but not a member.
    let second = krill_profile("second", Some("overseas"), AuthMode::ApiKey);
    sw.save_profile(
        second,
        Some(superba_core::Secret::new("nb-second-key-2222")),
    )
    .await
    .unwrap();
    let plan = sw.plan(req("second")).await.unwrap();
    assert_eq!(
        execute(&sw, plan).await.status,
        OutcomeStatus::Switched,
        "second applied"
    );
    let plan = sw.plan(router_on_request()).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        plan.to.profile_id.as_ref().map(ProfileId::as_str),
        Some("work"),
        "the member, not the applied non-member"
    );
    execute(&sw, plan).await;
    assert_eq!(
        sw.profile_set().await.unwrap().settings.router.members,
        vec![ProfileId::new("work")],
        "turning the router on adds no member"
    );
    assert_eq!(
        w.router.configs.lock().unwrap().last().unwrap(),
        &vec!["work".to_owned()]
    );
}

#[test]
fn the_routed_member_is_the_applied_eligible_member_else_the_first_one() {
    let w = World::new();
    let mut set = w.profiles.set.lock().unwrap().clone();
    let mut keyless = krill_profile("keyless", None, AuthMode::ApiKey);
    keyless.key_ref = None;
    let second = krill_profile("second", None, AuthMode::ApiKey);
    // PAT for direct switches: still routed with its key (user bug 78).
    let pat = krill_profile("pat", None, AuthMode::ApiKey);
    set.profiles.extend([keyless, second, pat]);
    let sites = catalog::builtin_sites();
    let id = ProfileId::new;

    set.settings.router.members = vec![id("keyless"), id("work"), id("second"), id("pat")];
    let c = router_candidates(&set, &sites);
    assert!(has_eligible_member(&c));
    // The applied profile when it is an eligible member.
    assert_eq!(
        router_mode_profile(&c, Some(&id("second"))),
        Some(id("second"))
    );
    assert_eq!(router_mode_profile(&c, Some(&id("pat"))), Some(id("pat")));
    // A member without a key is skipped for the first eligible one.
    assert_eq!(
        router_mode_profile(&c, Some(&id("keyless"))),
        Some(id("work"))
    );
    assert_eq!(router_mode_profile(&c, None), Some(id("work")));

    // An eligible profile that is not a member is never routed by itself.
    set.settings.router.members = vec![id("keyless")];
    let c = router_candidates(&set, &sites);
    assert!(!has_eligible_member(&c));
    assert_eq!(router_mode_profile(&c, Some(&id("work"))), None);
    let report = report(Err(&superba_core::Error::not_implemented("facts")), &c);
    assert_eq!(
        report.findings[0].text.key,
        "switch.router_eligibility.none_eligible"
    );
}

/// The router program on a Parallels shared folder (user bug 39): the doctor
/// warns in both languages and suggests the MSI; the program section says
/// where it is.
#[tokio::test]
async fn the_doctor_report_warns_about_a_router_program_on_a_network_share() {
    let (_w, sw) = users_state().await;
    let set = sw.profile_set().await.unwrap();
    let candidates = router_candidates(&set, &sw.sites().await.unwrap());
    let exe = r"\\Mac\Home\Downloads\windows\superba-router.exe";
    let mut facts: superba_router::doctor::RouterFacts =
        serde_json::from_value(serde_json::json!({
            "superba_version": "0.1.1-alpha.1",
            "platform": "windows x86_64",
            "os_version": null,
            "data_dir": r"C:\Users\jane\AppData\Roaming\Superba",
            "exe": {"path": exe, "exists": true, "transient": false, "network": true,
                    "protected_folder": false, "candidates": []},
            "service": {
                "kind": "windows_run_key",
                "status": {"Ok": {"kind": "windows_run_key", "installed": true, "running": true}},
                "diagnosis": {"program": exe, "fields": [], "crash_looping": false},
            },
            "config": {"config_exists": true, "port": 50947, "secrets_exists": true,
                       "members": [{"id": "work", "name": "Krill", "lines": 3, "has_key": true}]},
            "runtime": null,
            "port": null,
            "live": {"up": true, "version": "0.1.1-alpha.1", "members": 1, "uptime_ms": 34000},
            "log": {"path": r"C:\Users\jane\router.log", "exists": false, "tail": []},
            "foreground": [exe, "run"],
            "findings": [],
        }))
        .unwrap();
    facts.findings = superba_router::doctor::findings(&facts, false, None);
    let report = report(Ok(&facts), &candidates);
    let finding = report
        .findings
        .iter()
        .find(|f| f.text.key == "switch.router_doctor.finding.exe_on_network")
        .expect("the network share is reported");
    assert_eq!(finding.severity, superba_core::Severity::Warning);
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.text.key == "switch.router_eligibility.none_eligible"),
        "Krill is an eligible member"
    );
    for (locale, needles) in [
        (
            Locale::ZhCn,
            [
                r"路由程序 \\Mac\Home\Downloads\windows\superba-router.exe 在网络共享上",
                "请用 MSI 安装 Superba",
                "网络共享（UNC 路径或映射的网络驱动器）",
            ],
        ),
        (
            Locale::En,
            [
                r"The router program \\Mac\Home\Downloads\windows\superba-router.exe is on a network share",
                "Install Superba with the MSI",
                "network share (UNC path or mapped network drive)",
            ],
        ),
    ] {
        let catalog = messages::catalog(locale);
        let text = report.render_text(&|t| catalog.text(t));
        for needle in needles {
            assert!(text.contains(needle), "{locale:?}: {needle:?} in\n{text}");
        }
    }
}

/// A Windows user environment where Superba set `CODEX_AUTHAPI_BASE_URL`
/// for a personal access token before user bug 139, over the user's own
/// `original` value. The real config and env engines, an isolated store.
async fn windows_pat_env_world(
    original: Option<&str>,
) -> (
    World,
    Switcher,
    Arc<superba_config::MemoryNativeEnv>,
    Arc<superba_config::ShellEnvPersister>,
) {
    use superba_config::NativeEnv;
    use superba_core::{EnvLocation, EnvPersister, codex::ENV_AUTHAPI_BASE_URL};

    let w = World::new();
    let native = Arc::new(superba_config::MemoryNativeEnv::new());
    let loc = EnvLocation::WindowsUserEnv;
    if let Some(value) = original {
        native.set(&loc, ENV_AUTHAPI_BASE_URL, value).unwrap();
    }
    native.set(&loc, "USER_SETTING", "keep-me").unwrap();
    let env = Arc::new(superba_config::ShellEnvPersister::with_platform(
        w.ctx.clone(),
        superba_config::EnvPlatform::Windows,
        native.clone(),
    ));
    let old = env
        .plan(
            ENV_AUTHAPI_BASE_URL,
            superba_core::EnvOp::Set {
                value: OVERSEAS.into(),
                take_over_foreign: true,
            },
        )
        .await
        .unwrap();
    env.apply(&old).await.unwrap();
    assert_eq!(
        native.get(&loc, ENV_AUTHAPI_BASE_URL).unwrap().as_deref(),
        Some(OVERSEAS)
    );
    let mut deps = w.deps();
    deps.config = Arc::new(superba_config::TomlConfigEngine::new(w.ctx.clone()));
    deps.env = env.clone();
    let sw = Switcher::new(deps);
    (w, sw, native, env)
}

/// The next API-key switch (direct or through the router) hands the variable
/// back: the user's own value, or none.
async fn windows_api_key_switch_returns_the_pat_env(original: Option<&str>, routed: bool) {
    use superba_config::NativeEnv;
    use superba_core::{EnvLocation, EnvPersister, codex::ENV_AUTHAPI_BASE_URL};

    let (w, sw, native, env) = windows_pat_env_world(original).await;
    let loc = EnvLocation::WindowsUserEnv;
    let write_only = ExecuteOptions {
        write_only: true,
        ..ExecuteOptions::default()
    };
    let plan = if routed {
        sw.router_install(RouterInstallRequest {
            members: vec![ProfileId::new("work")],
        })
        .await
        .unwrap();
        sw.plan(router_on_request()).await.unwrap()
    } else {
        sw.plan(req("work")).await.unwrap()
    };
    assert_eq!(token_mode(&plan).0, AuthMode::ApiKey);
    assert!(
        matches!(
            plan.env.as_ref().map(|e| &e.op),
            Some(superba_core::EnvOp::Clear)
        ),
        "{:?}",
        plan.env
    );
    let outcome = sw
        .execute(plan, write_only, Arc::new(SafeDefaults), Arc::new(NoEvents))
        .await
        .unwrap();
    assert_eq!(outcome.status, OutcomeStatus::WrittenOnly, "{outcome:?}");
    if routed {
        assert!(sw.router_status().await.unwrap().codex_routed);
    }
    let (mode, _) = superba_config::authjson::token_credentials(&read(&w, "auth.json")).unwrap();
    assert_eq!(mode, AuthMode::ApiKey);
    assert_eq!(
        native.get(&loc, ENV_AUTHAPI_BASE_URL).unwrap().as_deref(),
        original,
        "only the value Superba set goes"
    );
    assert_eq!(
        native.get(&loc, "USER_SETTING").unwrap().as_deref(),
        Some("keep-me")
    );
    assert_eq!(
        env.inspect(ENV_AUTHAPI_BASE_URL)
            .await
            .unwrap()
            .superba_value,
        None
    );
    assert!(
        w.app.launches.lock().unwrap().is_empty(),
        "a write-only switch never restarts the app"
    );
}

#[tokio::test]
async fn windows_api_key_switch_clears_the_pat_authapi() {
    windows_api_key_switch_returns_the_pat_env(None, false).await;
}

#[tokio::test]
async fn windows_api_key_switch_restores_the_original_authapi() {
    windows_api_key_switch_returns_the_pat_env(Some("https://original.example.test"), false).await;
}

#[tokio::test]
async fn windows_router_mode_clears_the_pat_authapi() {
    windows_api_key_switch_returns_the_pat_env(None, true).await;
}

fn catalog_row(
    id: &str,
    layers: &[(&str, superba_core::CatalogLayer)],
) -> superba_core::RouterCatalogModel {
    superba_core::RouterCatalogModel {
        entry: serde_json::json!({"slug": id, "display_name": id, "context_window": 64000,
            "visibility": "list", "supported_in_api": true}),
        sources: layers
            .iter()
            .map(|(provider, layer)| superba_core::CatalogProvenance {
                provider: (*provider).into(),
                provider_name: (*provider).into(),
                provider_slug: (*provider).into(),
                key_scope: "scope".into(),
                upstream_id: id.into(),
                endpoint: "https://example.test/v1/models".into(),
                protocol: "responses".into(),
                layer: *layer,
                evidence: "test".into(),
                observed_at: 1,
                readiness: Some(superba_core::CatalogReadiness::Ready),
                alias: Some(format!("{provider}/{id}")),
            })
            .collect(),
        readiness: superba_core::CatalogReadiness::Ready,
        detail: None,
    }
}

#[tokio::test]
async fn routed_models_show_one_row_per_model_and_free_estimated_windows() {
    use superba_core::CatalogLayer::{Codex, StandardKnown, Synthesized};
    let (w, sw) = users_state().await;
    sw.router_set_enabled(true).await.unwrap();
    *w.router.catalog.lock().unwrap() = Some(superba_core::RouterCatalogView {
        models: vec![
            catalog_row("gpt-5.5", &[("krill", Codex), ("office", StandardKnown)]),
            catalog_row("deepseek-x", &[("office", Synthesized)]),
        ],
        aliases: [("krill/gpt-5.5".to_string(), "gpt-5.5".to_string())].into(),
        ..Default::default()
    });
    let view = sw
        .models_view(superba_core::ModelsViewRequest {
            profile_id: Some(ProfileId::new("work")),
            refresh: false,
        })
        .await
        .unwrap();
    let slugs: Vec<_> = view.list.models.iter().map(|m| m.slug.as_str()).collect();
    assert_eq!(slugs, ["gpt-5.5", "deepseek-x"], "aliases are never rows");
    let gpt = view.list.model("gpt-5.5").unwrap();
    assert!(!gpt.estimated_context_window);
    assert_eq!(gpt.max_window(), Some(64000));
    let deepseek = view.list.model("deepseek-x").unwrap();
    assert!(deepseek.estimated_context_window);
    // A family estimate does not cap the user's window.
    assert_eq!(deepseek.max_window(), None);
    assert!(view.router_catalog.is_some());
    // An empty catalog (Codex mode with only converted providers) is a
    // valid answer: the page keeps its router controls and shows no error.
    *w.router.catalog.lock().unwrap() = Some(superba_core::RouterCatalogView::default());
    let view = sw
        .models_view(superba_core::ModelsViewRequest {
            profile_id: Some(ProfileId::new("work")),
            refresh: false,
        })
        .await
        .unwrap();
    assert!(view.router_catalog.is_some());
    assert!(view.list.models.is_empty());
    assert!(
        view.notes
            .iter()
            .all(|n| n.severity == superba_core::Severity::Info),
        "{:?}",
        view.notes
    );
}

#[tokio::test]
async fn router_model_list_mode_is_saved_and_handed_to_the_router() {
    let (w, sw) = users_state().await;
    sw.router_set_enabled(true).await.unwrap();
    let mut settings = sw.profile_set().await.unwrap().settings.router.catalog;
    assert_eq!(settings.mode, superba_core::CatalogMode::Codex);
    settings.mode = superba_core::CatalogMode::Prefixed;
    sw.save_router_catalog(settings).await.unwrap();
    let set = sw.profile_set().await.unwrap();
    assert_eq!(
        set.settings.router.catalog.mode,
        superba_core::CatalogMode::Prefixed
    );
    let written = w.router.written.lock().unwrap();
    let catalog = written.last().unwrap().catalog.as_ref().unwrap();
    assert_eq!(catalog.settings.mode, superba_core::CatalogMode::Prefixed);
}

#[tokio::test]
async fn the_mode_is_a_preference_before_the_router_has_members() {
    let w = World::new();
    let sw = switcher(&w);
    let mut settings = sw.profile_set().await.unwrap().settings.router.catalog;
    settings.mode = superba_core::CatalogMode::Extended;
    sw.save_router_catalog(settings).await.unwrap();
    let set = sw.profile_set().await.unwrap();
    assert_eq!(
        set.settings.router.catalog.mode,
        superba_core::CatalogMode::Extended
    );
    assert!(
        w.router.written.lock().unwrap().is_empty(),
        "the router is untouched"
    );
}
