//! User bug 39 (0.1.1-alpha.1 on Windows): Krill with its API key was already
//! the router's member and the router was answering, `spb router doctor` said
//! "成员 · 可以作为成员（使用 API key）", yet the Router page kept 开启 disabled.
//! The service side of the one rule: the gate, the member router mode applies
//! and the doctor agree, and turning router mode on with Auto auth on a
//! PAT-capable Krill profile writes the API-key form (router mode is API key
//! only) without changing the profile's auth choice.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use superba_core::{
    AuthChoice, AuthMode, ExecuteOptions, Locale, NoEvents, OutcomeStatus, ProfileId, ResolvedMode,
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
            stable: Duration::ZERO,
            reask_after: Duration::from_secs(60),
            plan_cache: 16,
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
    assert_eq!(work.auth_choice(), AuthChoice::Auto);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        token_mode(&plan).0,
        AuthMode::Pat,
        "Auto uses the PAT direct"
    );
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
    let (mode, _) = superba_config::authjson::token_credentials(&read(&w, "auth.json")).unwrap();
    assert_eq!(mode, AuthMode::Pat);
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    assert!(sw.router_status().await.unwrap().running);
    (w, sw)
}

#[tokio::test]
async fn a_krill_member_with_its_key_turns_router_mode_on_with_the_api_key_form() {
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
    assert!(
        text.contains("成员 · 可以作为成员（使用 API key）"),
        "{text}"
    );
    sw.router_set_enabled(false).await.unwrap();

    // Turning router mode on: the routed configuration uses the API-key form
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
    assert_eq!(mode, AuthMode::ApiKey, "auth.json is the API-key form");
    assert_eq!(token.expose(), ROUTER_TOKEN, "Codex holds the router token");
    assert!(
        !String::from_utf8_lossy(&auth).contains(KEY),
        "the Krill key stays with the router"
    );

    // Router mode is stored; the profile keeps its choice (Auto) and lines.
    let set = sw.profile_set().await.unwrap();
    assert!(set.settings.router.enabled);
    let work = set.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(work.auth_choice(), AuthChoice::Auto);
    assert!(sw.router_status().await.unwrap().codex_routed);
}

#[tokio::test]
async fn turning_the_router_on_applies_a_member_never_another_profile() {
    let (w, sw) = users_state().await;
    // Another Krill profile with a key, applied, but not a member.
    let second = krill_profile("second", Some("overseas"), AuthMode::Pat);
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
    let mut pat = krill_profile("pat", None, AuthMode::Pat);
    pat.set_auth(AuthChoice::Pat);
    let second = krill_profile("second", None, AuthMode::ApiKey);
    set.profiles.extend([pat, second]);
    let sites = catalog::builtin_sites();
    let id = ProfileId::new;

    set.settings.router.members = vec![id("pat"), id("work"), id("second")];
    let c = router_candidates(&set, &sites);
    assert!(has_eligible_member(&c));
    // The applied profile when it is an eligible member.
    assert_eq!(
        router_mode_profile(&c, Some(&id("second"))),
        Some(id("second"))
    );
    // A PAT-only member is skipped for the first eligible one.
    assert_eq!(router_mode_profile(&c, Some(&id("pat"))), Some(id("work")));
    assert_eq!(router_mode_profile(&c, None), Some(id("work")));

    // An eligible profile that is not a member is never routed by itself.
    set.settings.router.members = vec![id("pat")];
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
