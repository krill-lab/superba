//! User bug 63 (follow-up of bug 58): Codex inside a WSL distribution that
//! follows this computer chooses its model by the same rules. Without a model in the distribution's
//! `config.toml` turning the router on moved its Codex from the provider's
//! default to a default of its own, and nothing in the plan said so.
//!
//! Now each distribution gets its own decision (its own `config.toml`, auth,
//! line, Codex version and kept model), named in the plan for that
//! distribution; this computer's kept model is never copied to it. With the
//! app's "Run Codex in WSL" mode the app's Codex inside the default
//! distribution reads this computer's `config.toml`, and the plan says it keeps
//! the model there too.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AuthMode, AuthShape, ChangeReason, CodexVersion, ExecuteOptions, Locale, ModelList,
    ModelListSource, ModelSpec, ModelVisibility, NoEvents, OutcomeStatus, ProfileId,
    RouterInstallRequest, SafeDefaults, SettingChange, SwitchIntent, SwitchPlan, SwitchRequest,
    SwitchService, TargetId, Timestamp, WslNetworking,
};
use superba_switch::model_keep::{
    ScopeFacts, commit_kept_host, host_on_site, kept_host_model, scope_of,
};
use superba_switch::{Switcher, Tuning, messages};

const UBUNTU: &str = "WSL: Ubuntu";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn spec(slug: &str, priority: i64, visibility: ModelVisibility) -> ModelSpec {
    ModelSpec {
        slug: slug.into(),
        priority: Some(priority),
        visibility,
        ..ModelSpec::default()
    }
}

fn krill_list(default: &str) -> ModelList {
    ModelList {
        source: ModelListSource::Upstream {
            path: "/backend-api/codex/models?client_version=0.162.0".into(),
            fetched_at: Timestamp(T0),
            etag: None,
        },
        models: vec![
            spec("gpt-5.5", 13, ModelVisibility::List),
            spec(default, 1, ModelVisibility::List),
            spec("gpt-6-luna", 4, ModelVisibility::List),
        ],
        fallback_reason: None,
        sources: Vec::new(),
    }
}

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

fn router(enable: bool) -> SwitchRequest {
    let mut r = SwitchRequest::profile(ProfileId::new(""));
    r.intent = SwitchIntent::Router { enable };
    r
}

async fn execute(sw: &Switcher, plan: SwitchPlan) {
    let outcome = sw
        .execute(
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
        .unwrap();
    assert!(
        matches!(
            outcome.status,
            OutcomeStatus::Switched | OutcomeStatus::WrittenOnly
        ),
        "{outcome:?}"
    );
}

fn target() -> TargetId {
    superba_core::wsl::target_id("Ubuntu")
}

/// The distribution's `model` now (the fake host mirrors what was applied).
fn wsl_model(w: &World) -> Option<String> {
    toml_model(w.remote.config_toml.lock().unwrap().as_deref())
}

/// The `model` the plan writes to the distribution.
fn planned_wsl_model(plan: &SwitchPlan) -> Option<String> {
    plan.remotes
        .iter()
        .find(|p| p.target_id == target())
        .expect("a plan for the distribution")
        .desired
        .as_ref()
        .and_then(|d| d.model.model.clone())
}

fn wsl_base_url(plan: &SwitchPlan) -> String {
    match &plan
        .remotes
        .iter()
        .find(|p| p.target_id == target())
        .unwrap()
        .desired
        .as_ref()
        .unwrap()
        .mode
    {
        superba_core::ResolvedMode::Token { endpoints, .. } => {
            endpoints.openai_base_url.to_string()
        }
        other => panic!("{other:?}"),
    }
}

/// The preview row of `model` for `target`.
async fn model_row(sw: &Switcher, plan: &SwitchPlan, target: &TargetId) -> Option<SettingChange> {
    sw.plan_settings(&plan.id)
        .await
        .unwrap()
        .changes
        .into_iter()
        .find(|c| c.path == "model" && &c.target == target)
}

/// Notices about the distribution's model: (inner key, rendered text).
fn wsl_notes(plan: &SwitchPlan, locale: Locale) -> Vec<(String, String)> {
    let catalog = messages::catalog(locale);
    plan.warnings
        .iter()
        .filter(|n| n.text.key == "switch.warn.on_host")
        .filter_map(|n| {
            let inner = n.text.nested.iter().find(|(k, _)| k == "text")?;
            inner
                .1
                .key
                .starts_with("switch.models.")
                .then(|| (inner.1.key.clone(), catalog.text(&n.text)))
        })
        .collect()
}

fn local_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings
        .iter()
        .map(|n| n.text.key.clone())
        .filter(|k| k.starts_with("switch.models."))
        .collect()
}

/// Krill ("work", Automatic auth, no model) applied with the PAT to this
/// computer and to `Ubuntu` (WSL 1, its own Codex 0.161.0, reaching the
/// router on 127.0.0.1), the router installed with Krill, off.
async fn pat_applied(default: &str) -> (World, Switcher) {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(wsl_host("Ubuntu", 1, WslNetworking::Nat));
    *w.remote.mirror.lock().unwrap() = true;
    *w.remote.version.lock().unwrap() = Some(CodexVersion::new(0, 161, 0));
    w.remote
        .http
        .lock()
        .unwrap()
        .insert("127.0.0.1:18431".into(), router_health());
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        p.set_auth(superba_core::AuthChoice::Auto);
    }
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_list(default)));
    let sw = switcher(&w);
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(plan.to.auth_mode, Some(AuthMode::Pat));
    assert_eq!(planned_wsl_model(&plan), None);
    assert!(
        wsl_notes(&plan, Locale::En).is_empty(),
        "{:?}",
        plan.warnings
    );
    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w), None);
    assert!(matches!(
        w.remote.auth.lock().unwrap().clone(),
        Some(AuthShape::Pat { .. })
    ));
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(false).await.unwrap();
    w.prober.model_list_calls.lock().unwrap().clear();
    w.prober.model_list_versions.lock().unwrap().clear();
    (w, sw)
}

// ---------------------------------------------------------------------------
// Router on and off
// ---------------------------------------------------------------------------

#[tokio::test]
async fn router_on_keeps_the_model_of_a_following_distribution_and_names_it() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;

    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(wsl_base_url(&plan), FakeRouter::base_url(18_431));
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    let row = model_row(&sw, &plan, &target())
        .await
        .expect("a model row for the distribution");
    assert_eq!(row.from, None);
    assert_eq!(row.to.as_deref(), Some("\"gpt-6.1-sol\""));
    assert_eq!(row.reason, ChangeReason::KeptModel);
    assert_eq!(row.target_label, "WSL: Ubuntu");
    let notes = wsl_notes(&plan, Locale::En);
    assert_eq!(
        notes,
        [(
            "switch.models.keep_router".to_owned(),
            "WSL: Ubuntu: Codex keeps gpt-6.1-sol, the model it runs with now: router members may have different default models. To use another model, set one on the profile (Models page, or spb profile edit --model)."
                .to_owned()
        )]
    );
    assert!(
        wsl_notes(&plan, Locale::ZhCn)[0]
            .1
            .starts_with("WSL: Ubuntu：Codex 继续使用当前的模型 gpt-6.1-sol："),
        "{:?}",
        wsl_notes(&plan, Locale::ZhCn)
    );
    // This computer keeps its model too (bug 58), named once for it.
    assert_eq!(local_keys(&plan), ["switch.models.keep_router"]);
    // The distribution's list was read at its own line with its own Codex.
    let calls = w.prober.model_list_calls.lock().unwrap().clone();
    let versions = w.prober.model_list_versions.lock().unwrap().clone();
    let at = calls
        .iter()
        .zip(&versions)
        .map(|(c, v)| (c.clone(), v.clone()))
        .collect::<Vec<_>>();
    assert!(
        at.contains(&(
            format!("{OVERSEAS}/backend-api/codex"),
            Some("0.161.0".into())
        )),
        "{at:?}"
    );

    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w).as_deref(), Some("gpt-6.1-sol"));
    let state =
        superba_core::RemoteHosts::probe(&*w.remote, &wsl_host("Ubuntu", 1, WslNetworking::Nat))
            .await
            .unwrap();
    assert_eq!(
        kept_host_model(&w.ctx.data_dir, &target(), &state, Some("gpt-6.1-sol")).as_deref(),
        Some("gpt-6.1-sol")
    );

    // Another switch in router mode keeps it, reading no list and changing
    // nothing about the model.
    w.prober.model_list_calls.lock().unwrap().clear();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(model_row(&sw, &plan, &target()).await, None);
    assert!(w.prober.model_list_calls.lock().unwrap().is_empty());
    execute(&sw, plan).await;

    // Router off: the PAT's own default is the same model, so the key goes
    // back to the distribution's (absent) value, nothing named.
    let plan = sw.plan(router(false)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert!(wsl_base_url(&plan).starts_with(OVERSEAS));
    assert_eq!(planned_wsl_model(&plan), None);
    let row = model_row(&sw, &plan, &target())
        .await
        .expect("the kept model is released");
    assert_eq!(row.from.as_deref(), Some("\"gpt-6.1-sol\""));
    assert_eq!(row.to, None);
    assert_eq!(row.reason, ChangeReason::Released);
    assert!(
        wsl_notes(&plan, Locale::En).is_empty(),
        "{:?}",
        plan.warnings
    );
    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w), None);
    let state =
        superba_core::RemoteHosts::probe(&*w.remote, &wsl_host("Ubuntu", 1, WslNetworking::Nat))
            .await
            .unwrap();
    assert_eq!(
        kept_host_model(&w.ctx.data_dir, &target(), &state, None),
        None
    );
    let file = std::fs::read_to_string(
        w.ctx
            .data_dir
            .root()
            .join(superba_switch::model_keep::KEPT_MODEL_FILE),
    )
    .unwrap();
    assert!(!file.contains("remote-wsl"), "{file}");
}

#[tokio::test]
async fn router_off_keeps_the_distribution_model_when_the_pat_default_moved_on() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    execute(&sw, sw.plan(router(true)).await.unwrap()).await;
    assert_eq!(wsl_model(&w).as_deref(), Some("gpt-6.1-sol"));

    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_list("gpt-6.2-sol")));
    let plan = sw.plan(router(false)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(
        model_row(&sw, &plan, &target()).await,
        None,
        "the value stays"
    );
    let notes = wsl_notes(&plan, Locale::En);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, "switch.models.keep_pat_default");
    assert!(
        notes[0].1.starts_with(
            "WSL: Ubuntu: Codex keeps gpt-6.1-sol, the model it runs with now; with the personal \
             access token it would change to gpt-6.2-sol"
        ),
        "{notes:?}"
    );
    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w).as_deref(), Some("gpt-6.1-sol"));
}

#[tokio::test]
async fn the_distributions_own_model_is_never_replaced_by_this_computers() {
    // The distribution names a model of its own: its Codex runs it in both
    // modes, and this computer's kept model is not written over it.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    *w.remote.config_toml.lock().unwrap() = Some("model = \"gpt-5.6-sol\"\n".into());
    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(wsl_base_url(&plan), FakeRouter::base_url(18_431));
    assert_eq!(planned_wsl_model(&plan), None);
    assert_eq!(model_row(&sw, &plan, &target()).await, None);
    assert!(
        wsl_notes(&plan, Locale::En).is_empty(),
        "{:?}",
        plan.warnings
    );
    // This computer keeps its own.
    assert_eq!(local_keys(&plan), ["switch.models.keep_router"]);
    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w).as_deref(), Some("gpt-5.6-sol"));

    // The profile names a model: it is written there as before.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        p.model.model = Some("gpt-6-luna".into());
    }
    let plan = sw.plan(router(true)).await.unwrap();
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6-luna"));
    assert_eq!(
        model_row(&sw, &plan, &target()).await.map(|r| r.reason),
        Some(ChangeReason::Model)
    );
    assert!(
        wsl_notes(&plan, Locale::En).is_empty(),
        "{:?}",
        plan.warnings
    );
}

#[tokio::test]
async fn a_distribution_that_stays_direct_keeps_nothing() {
    // The router is not reachable from the distribution: it stays on its PAT
    // line, so its Codex keeps choosing from the provider's list.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    w.remote.http.lock().unwrap().clear();
    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert!(wsl_base_url(&plan).starts_with(OVERSEAS));
    assert_eq!(planned_wsl_model(&plan), None);
    assert!(
        wsl_notes(&plan, Locale::En).is_empty(),
        "{:?}",
        plan.warnings
    );
    // This computer is routed and keeps its model.
    assert_eq!(local_keys(&plan), ["switch.models.keep_router"]);
}

#[tokio::test]
async fn a_list_that_does_not_load_is_named_for_the_distribution() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    *w.prober.model_list.lock().unwrap() = Some(Err(superba_core::Error::Http {
        url: format!("{OVERSEAS}/backend-api/codex/models"),
        status: 503,
        message: "unavailable".into(),
    }));
    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_wsl_model(&plan), None);
    let n = plan
        .warnings
        .iter()
        .find(|n| {
            n.text.key == "switch.warn.on_host"
                && n.text
                    .nested
                    .iter()
                    .any(|(_, t)| t.key == "switch.models.keep_unknown")
        })
        .expect("the uncertainty is named for the distribution");
    assert_eq!(n.severity, superba_core::Severity::Warning);
}

#[tokio::test]
async fn a_distribution_updated_later_keeps_its_model_too() {
    // Stopped while the router was turned on (user bug 54): the next switch
    // that finds it running routes it, with the model its PAT gave.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        h.wsl.as_mut().unwrap().set_running(false);
    }
    w.remote.written.lock().unwrap().push(UBUNTU.into());
    execute(&sw, sw.plan(router(true)).await.unwrap()).await;
    assert_eq!(wsl_model(&w), None, "a stopped distribution is not written");
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        h.wsl.as_mut().unwrap().set_running(true);
    }
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(wsl_base_url(&plan), FakeRouter::base_url(18_431));
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(
        model_row(&sw, &plan, &target()).await.map(|r| r.reason),
        Some(ChangeReason::KeptModel)
    );
    assert_eq!(
        wsl_notes(&plan, Locale::En)
            .into_iter()
            .map(|(k, _)| k)
            .collect::<Vec<_>>(),
        ["switch.models.keep_router"]
    );
    // This computer still runs the model it kept.
    assert_eq!(local_keys(&plan), ["switch.models.keep_router"]);
    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w).as_deref(), Some("gpt-6.1-sol"));
}

// ---------------------------------------------------------------------------
// "Run Codex in WSL"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_codex_in_wsl_names_the_model_kept_for_its_distribution() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        h.wsl.as_mut().unwrap().default = true;
    }
    let path = w.ctx.codex_home().config_toml();
    let mut config = std::fs::read_to_string(&path).unwrap();
    config.push_str("\n[desktop]\nrunCodexInWindowsSubsystemForLinux = true\n");
    std::fs::write(&path, config).unwrap();

    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        local_keys(&plan),
        ["switch.models.keep_router", "switch.models.keep_legacy_wsl"]
    );
    let n = plan
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.models.keep_legacy_wsl")
        .unwrap();
    assert_eq!(
        messages::catalog(Locale::En).text(&n.text),
        "The ChatGPT app runs this Codex inside WSL (Ubuntu, \"Run Codex in WSL\") with the \
         configuration written here, so it keeps gpt-6.1-sol there too."
    );
    assert!(
        messages::catalog(Locale::ZhCn)
            .text(&n.text)
            .contains("那里也继续使用 gpt-6.1-sol"),
    );
    // The distribution's own Codex (its CLI) gets its own decision.
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    execute(&sw, plan).await;
    let config = std::fs::read_to_string(&path).unwrap();
    assert!(config.contains("model = \"gpt-6.1-sol\""), "{config}");

    // Without a model kept, nothing is said about the distribution.
    let plan = sw.plan(router(false)).await.unwrap();
    assert!(
        !local_keys(&plan).contains(&"switch.models.keep_legacy_wsl".to_owned()),
        "{:?}",
        plan.warnings
    );
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

#[test]
fn a_distribution_is_on_the_site_of_its_line_or_of_the_router() {
    let endpoints: Vec<url::Url> = [
        "http://127.0.0.1:18431/backend-api/codex",
        "http://172.24.16.1:18431/backend-api/codex",
    ]
    .iter()
    .map(|u| url::Url::parse(u).unwrap())
    .collect();
    let site = superba_core::catalog::krill_site();
    let krill = site.id.clone();
    let other = superba_core::SiteId::new("custom");
    let line = format!("{OVERSEAS}/backend-api/codex");
    assert!(host_on_site(Some(&line), &site, None, &endpoints));
    assert!(host_on_site(Some(&line), &site, Some(&other), &endpoints));
    // The router (loopback or the WSL listener) serves the site applied here.
    for router in [
        "http://127.0.0.1:18431/backend-api/codex",
        "http://172.24.16.1:18431/backend-api/codex",
    ] {
        assert!(
            host_on_site(Some(router), &site, Some(&krill), &endpoints),
            "{router}"
        );
        assert!(
            !host_on_site(Some(router), &site, Some(&other), &endpoints),
            "{router}"
        );
        assert!(
            !host_on_site(Some(router), &site, None, &endpoints),
            "{router}"
        );
    }
    for elsewhere in [
        None,
        Some("https://api.openai.com/v1"),
        Some("http://203.0.113.9:18431/backend-api/codex"),
        Some("not a url"),
        Some("http://172.24.16.2:18431/backend-api/codex"),
        Some("http://172.24.16.1:11434/backend-api/codex"),
        Some("http://172.24.16.1:18431/openai/v1"),
    ] {
        assert!(
            !host_on_site(elsewhere, &site, Some(&krill), &endpoints),
            "{elsewhere:?}"
        );
    }
}

#[test]
fn the_rule_is_the_same_for_a_distribution() {
    let facts = ScopeFacts {
        same_site: true,
        auth: Some(AuthMode::Pat),
        dest_auth: AuthMode::ApiKey,
        model: None,
        kept: None,
        dest_sets_model: false,
        left: None,
    };
    let scope = scope_of(facts).expect("in scope");
    assert_eq!(scope.kept, None);
    // Another site, a model of its own, a model the profile sets, no auth Superba
    // knows: out of scope.
    assert_eq!(
        scope_of(ScopeFacts {
            same_site: false,
            ..facts
        }),
        None
    );
    assert_eq!(
        scope_of(ScopeFacts {
            model: Some("gpt-5.6-sol"),
            ..facts
        }),
        None
    );
    assert_eq!(
        scope_of(ScopeFacts {
            dest_sets_model: true,
            ..facts
        }),
        None
    );
    assert_eq!(
        scope_of(ScopeFacts {
            auth: None,
            ..facts
        }),
        None
    );
    // The same form and nothing kept: nothing changes.
    assert_eq!(
        scope_of(ScopeFacts {
            auth: Some(AuthMode::ApiKey),
            ..facts
        }),
        None
    );
    // A kept model is in scope also without a change of form.
    assert!(
        scope_of(ScopeFacts {
            auth: Some(AuthMode::ApiKey),
            model: Some("gpt-6.1-sol"),
            kept: Some("gpt-6.1-sol"),
            ..facts
        })
        .is_some()
    );
}

#[tokio::test]
async fn a_kept_model_counts_only_while_the_distribution_holds_it() {
    let w = World::new();
    let host = wsl_host("Ubuntu", 1, WslNetworking::Nat);
    *w.remote.owned_paths.lock().unwrap() = vec!["model".into()];
    let state = superba_core::RemoteHosts::probe(&*w.remote, &host)
        .await
        .unwrap();
    let dd = &w.ctx.data_dir;
    assert_eq!(
        kept_host_model(dd, &target(), &state, Some("gpt-6.1-sol")),
        None
    );
    commit_kept_host(dd, &target(), &state.codex_home, Some("gpt-6.1-sol")).unwrap();
    assert_eq!(
        kept_host_model(dd, &target(), &state, Some("gpt-6.1-sol")).as_deref(),
        Some("gpt-6.1-sol")
    );
    // Edited, not Superba's any more, another Codex home, another host.
    assert_eq!(
        kept_host_model(dd, &target(), &state, Some("gpt-5.5")),
        None
    );
    let mut released = state.clone();
    released.owned_paths.clear();
    assert_eq!(
        kept_host_model(dd, &target(), &released, Some("gpt-6.1-sol")),
        None
    );
    let mut moved = state.clone();
    moved.codex_home = "/home/u/other".into();
    assert_eq!(
        kept_host_model(dd, &target(), &moved, Some("gpt-6.1-sol")),
        None
    );
    let other = superba_core::wsl::target_id("Debian");
    assert_eq!(
        kept_host_model(dd, &other, &state, Some("gpt-6.1-sol")),
        None
    );
    commit_kept_host(dd, &target(), &state.codex_home, None).unwrap();
    assert_eq!(
        kept_host_model(dd, &target(), &state, Some("gpt-6.1-sol")),
        None
    );
}

#[tokio::test]
async fn a_distribution_whose_write_failed_records_nothing() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    w.remote.fail_apply.lock().unwrap().insert(
        UBUNTU.into(),
        superba_core::Error::Wsl {
            distro: "Ubuntu".into(),
            message: "wsl.exe failed".into(),
        },
    );
    let plan = sw.plan(router(true)).await.unwrap();
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    let outcome = sw
        .execute(
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
        .unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Partial, "{outcome:?}");
    *w.remote.owned_paths.lock().unwrap() = vec!["model".into()];
    let state =
        superba_core::RemoteHosts::probe(&*w.remote, &wsl_host("Ubuntu", 1, WslNetworking::Nat))
            .await
            .unwrap();
    assert_eq!(
        kept_host_model(&w.ctx.data_dir, &target(), &state, Some("gpt-6.1-sol")),
        None
    );
}

// ---------------------------------------------------------------------------
// A pending update (user bug 75)
// ---------------------------------------------------------------------------

fn set_running(w: &World, running: bool) {
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        if let Some(d) = h.wsl.as_mut() {
            d.set_running(running);
        }
    }
    // Superba wrote it (the fake keeps no baseline by itself).
    let mut written = w.remote.written.lock().unwrap();
    if !written.iter().any(|h| h == UBUNTU) {
        written.push(UBUNTU.into());
    }
}

fn router_enabled(w: &World) -> bool {
    w.profiles.set.lock().unwrap().settings.router.enabled
}

/// The request "Update now" / `spb wsl update` plans for the distribution.
async fn pending_request(sw: &Switcher) -> SwitchRequest {
    sw.hosts()
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.host.target_id == target())
        .and_then(|h| h.pending)
        .expect("an update pending for the distribution")
        .request(&target())
}

#[tokio::test]
async fn an_update_of_a_distribution_stopped_when_the_router_went_on_routes_it_and_keeps_its_model()
{
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    set_running(&w, false);
    let plan = sw.plan(router(true)).await.unwrap();
    assert!(
        plan.remotes.is_empty(),
        "a stopped distribution is left out"
    );
    execute(&sw, plan).await;
    assert!(router_enabled(&w));

    // "Update now": the distribution alone, routed like this computer, its
    // model kept (its own decision, bug 63).
    set_running(&w, true);
    let request = pending_request(&sw).await;
    assert!(!request.targets.local);
    let plan = sw.plan(request).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(wsl_base_url(&plan), FakeRouter::base_url(18_431));
    assert_eq!(planned_wsl_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    assert!(plan.config.is_none(), "this computer is not written");
    let members = w
        .profiles
        .set
        .lock()
        .unwrap()
        .settings
        .router
        .members
        .clone();
    execute(&sw, plan).await;
    assert_eq!(wsl_model(&w).as_deref(), Some("gpt-6.1-sol"));
    assert!(router_enabled(&w));
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.router.members,
        members
    );
}

#[tokio::test]
async fn an_update_applies_the_router_state_of_now() {
    // Pending from "router on", but the router was turned off since (not by a
    // switch): the update writes the direct line and leaves the router off.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    set_running(&w, false);
    execute(&sw, sw.plan(router(true)).await.unwrap()).await;
    sw.router_set_enabled(false).await.unwrap();
    set_running(&w, true);
    let plan = sw.plan(pending_request(&sw).await).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert!(
        wsl_base_url(&plan).starts_with(OVERSEAS),
        "{}",
        wsl_base_url(&plan)
    );
    execute(&sw, plan).await;
    assert!(!router_enabled(&w));
}

/// alpha.8 Windows acceptance (W90): with a model the profile sets itself
/// nothing is "kept", and the plan for the router said nothing about the
/// app's Codex inside WSL. It names the distribution and the model too.
#[tokio::test]
async fn run_codex_in_wsl_names_the_profile_model_when_the_router_turns_on() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        p.model.model = Some("gpt-6.1-sol".into());
    }
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    execute(&sw, plan).await;
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        h.wsl.as_mut().unwrap().default = true;
    }
    let path = w.ctx.codex_home().config_toml();
    let mut config = std::fs::read_to_string(&path).unwrap();
    assert!(config.contains("model = \"gpt-6.1-sol\""), "{config}");
    config.push_str("\n[desktop]\nrunCodexInWindowsSubsystemForLinux = true\n");
    std::fs::write(&path, config).unwrap();

    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let n = plan
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.models.keep_legacy_wsl")
        .unwrap_or_else(|| panic!("{:?}", plan.warnings));
    assert_eq!(
        messages::catalog(Locale::En).text(&n.text),
        "The ChatGPT app runs this Codex inside WSL (Ubuntu, \"Run Codex in WSL\") with the \
         configuration written here, so it keeps gpt-6.1-sol there too."
    );
    execute(&sw, plan).await;

    // A line switch that keeps the route says nothing about it.
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(
        !local_keys(&plan).contains(&"switch.models.keep_legacy_wsl".to_owned()),
        "{:?}",
        plan.warnings
    );
}
