//! User bug 58 (Windows test matrix, real test relay, 0.1.0): a Krill profile
//! without a model ran the account's default `gpt-6.1-sol` with the personal
//! access token; turning the local router on put Codex in API-key mode, where
//! Codex and the desktop app choose a default of their own (`gpt-5.5`), and
//! nothing in the plan said so. The relay had no capacity for that model and
//! the app kept "Reconnecting 5/5".
//!
//! Now a switch that changes how Codex chooses its model writes the model it
//! runs with now and names it in the plan; the reverse switch drops it again
//! when the personal access token gives the same model, and keeps it (named)
//! when it would not.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ActiveState, AuthMode, ChangeReason, ConfigState, Error, ExecuteOptions, Locale, ModelList,
    ModelListSource, ModelSpec, ModelVisibility, NoEvents, OutcomeStatus, ProfileId,
    RouterInstallRequest, SafeDefaults, SettingChange, SiteId, SwitchIntent, SwitchPlan,
    SwitchRequest, SwitchService, Timestamp,
};
use superba_switch::model_keep::{
    KeepDecision, KeepFacts, KeepScope, ModelForm, ScopeInput, commit_kept, decide, keep_scope,
    kept_model, provider_default,
};
use superba_switch::{Switcher, Tuning, messages};

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

/// Krill's list as `/backend-api/codex/models` sends it (not in priority order).
fn krill_list(default: &str) -> ModelList {
    ModelList {
        source: ModelListSource::Upstream {
            path: "/backend-api/codex/models?client_version=0.162.0".into(),
            fetched_at: Timestamp(T0),
            etag: None,
        },
        models: vec![
            spec("gpt-5.5", 13, ModelVisibility::List),
            spec("codex-auto-review", 0, ModelVisibility::Hide),
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

fn with_auth(profile: &str, auth: AuthMode) -> SwitchRequest {
    let mut r = SwitchRequest::profile(ProfileId::new(profile));
    r.intent = SwitchIntent::Profile {
        profile_id: ProfileId::new(profile),
        line_id: None,
        auth_mode: Some(auth),
    };
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
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
}

fn config_toml(w: &World) -> String {
    std::fs::read_to_string(w.ctx.codex_home().config_toml()).unwrap_or_default()
}

/// The `model` line of config.toml, if any.
fn model_line(w: &World) -> Option<String> {
    config_toml(w)
        .lines()
        .find(|l| l.trim_start().starts_with("model ="))
        .map(|l| l.trim().to_owned())
}

/// The desired `model` of a plan's local write.
fn planned_model(plan: &SwitchPlan) -> Option<String> {
    plan.config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .and_then(|d| d.model.model.clone())
}

/// The preview row of `model` of a plan.
async fn model_row(sw: &Switcher, plan: &SwitchPlan) -> Option<SettingChange> {
    sw.plan_settings(&plan.id)
        .await
        .unwrap()
        .changes
        .into_iter()
        .find(|c| c.path == "model")
}

fn notice_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings
        .iter()
        .map(|n| n.text.key.clone())
        .filter(|k| k.starts_with("switch.models."))
        .collect()
}

fn notice(plan: &SwitchPlan, key: &str, locale: Locale) -> String {
    let n = plan
        .warnings
        .iter()
        .find(|n| n.text.key == key)
        .unwrap_or_else(|| panic!("no {key} in {:?}", plan.warnings));
    messages::catalog(locale).text(&n.text)
}

/// Krill ("work", Automatic auth, no model) applied directly with the PAT,
/// the router installed with it as the only member, Krill's list answering.
async fn pat_applied(default: &str) -> (World, Switcher) {
    let w = World::new();
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
    assert_eq!(planned_model(&plan), None, "a profile without a model");
    assert!(notice_keys(&plan).is_empty(), "{:?}", plan.warnings);
    execute(&sw, plan).await;
    assert_eq!(model_line(&w), None);
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    sw.router_set_enabled(false).await.unwrap();
    w.prober.model_list_calls.lock().unwrap().clear();
    (w, sw)
}

// ---------------------------------------------------------------------------
// The switches of the report
// ---------------------------------------------------------------------------

#[tokio::test]
async fn turning_the_router_on_keeps_the_model_the_pat_ran_and_says_so() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;

    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(plan.to.auth_mode, Some(AuthMode::Pat));
    assert_eq!(planned_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    let row = model_row(&sw, &plan)
        .await
        .expect("a model row in the preview");
    assert_eq!(row.from, None);
    assert_eq!(row.to.as_deref(), Some("\"gpt-6.1-sol\""));
    assert_eq!(row.reason, ChangeReason::KeptModel);
    assert_eq!(notice_keys(&plan), ["switch.models.keep_router"]);
    assert_eq!(
        notice(&plan, "switch.models.keep_router", Locale::En),
        "Codex keeps gpt-6.1-sol, the model it runs with now: router members may have different \
         default models. To use another model, set one on the profile (Models page, or \
         spb profile edit --model)."
    );
    assert!(
        notice(&plan, "switch.models.keep_router", Locale::ZhCn)
            .starts_with("Codex 继续使用当前的模型 gpt-6.1-sol："),
    );
    // The PAT's list was read at the line config.toml names, with the key.
    assert_eq!(
        *w.prober.model_list_calls.lock().unwrap(),
        [format!("{OVERSEAS}/backend-api/codex")]
    );
    execute(&sw, plan).await;
    assert_eq!(model_line(&w).as_deref(), Some("model = \"gpt-6.1-sol\""));
    let config = config_toml(&w);
    assert!(config.contains("127.0.0.1"), "{config}");

    // A later switch in router mode (the same profile) keeps it without
    // reading any list, and changes nothing about the model.
    w.prober.model_list_calls.lock().unwrap().clear();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(model_row(&sw, &plan).await, None);
    assert!(w.prober.model_list_calls.lock().unwrap().is_empty());
    execute(&sw, plan).await;
    assert_eq!(model_line(&w).as_deref(), Some("model = \"gpt-6.1-sol\""));

    // Router off: the PAT's own default is the same model, so the key goes
    // back to the user's (absent) value.
    let plan = sw.plan(router(false)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(plan.to.auth_mode, Some(AuthMode::Pat));
    assert_eq!(planned_model(&plan), None);
    let row = model_row(&sw, &plan)
        .await
        .expect("the kept model is released");
    assert_eq!(row.from.as_deref(), Some("\"gpt-6.1-sol\""));
    assert_eq!(row.to, None);
    assert_eq!(row.reason, ChangeReason::Released);
    assert!(notice_keys(&plan).is_empty(), "{:?}", plan.warnings);
    execute(&sw, plan).await;
    assert_eq!(model_line(&w), None);

    // Nothing is remembered as kept any more.
    let home = w.ctx.codex_home();
    let state = superba_config::TomlConfigEngine::new(w.ctx.clone());
    let state = superba_core::ConfigEngine::read_state(&state, &home)
        .await
        .unwrap();
    assert!(!state.model_owned);
    assert_eq!(kept_model(&w.ctx.data_dir, &home, &state), None);
}

#[tokio::test]
async fn router_off_keeps_the_model_when_the_pat_default_moved_on() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    execute(&sw, sw.plan(router(true)).await.unwrap()).await;
    assert_eq!(model_line(&w).as_deref(), Some("model = \"gpt-6.1-sol\""));

    // The provider's default changed while the router was on.
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_list("gpt-6.2-sol")));
    let plan = sw.plan(router(false)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_model(&plan).as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(model_row(&sw, &plan).await, None, "the value stays");
    assert_eq!(
        notice(&plan, "switch.models.keep_pat_default", Locale::En),
        "Codex keeps gpt-6.1-sol, the model it runs with now; with the personal access token it \
         would change to gpt-6.2-sol, the default model of “Krill”. To use another model, set \
         one on the profile (Models page, or spb profile edit --model)."
    );
    execute(&sw, plan).await;
    assert_eq!(model_line(&w).as_deref(), Some("model = \"gpt-6.1-sol\""));
}

#[tokio::test]
async fn a_list_that_does_not_load_is_named_instead_of_guessing() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    *w.prober.model_list.lock().unwrap() = Some(Err(Error::Http {
        url: format!("{OVERSEAS}/backend-api/codex/models"),
        status: 503,
        message: "unavailable".into(),
    }));
    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_model(&plan), None);
    let n = plan
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.models.keep_unknown")
        .expect("the uncertainty is named");
    assert_eq!(n.severity, superba_core::Severity::Warning);
    let text = notice(&plan, "switch.models.keep_unknown", Locale::En);
    assert!(text.contains("did not load"), "{text}");
}

#[tokio::test]
async fn a_profile_model_or_the_users_own_model_is_left_alone() {
    // The profile names a model: it is written as before.
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
    assert_eq!(planned_model(&plan).as_deref(), Some("gpt-6-luna"));
    assert_eq!(
        model_row(&sw, &plan).await.map(|r| r.reason),
        Some(ChangeReason::Model)
    );
    assert!(notice_keys(&plan).is_empty(), "{:?}", plan.warnings);
    assert!(w.prober.model_list_calls.lock().unwrap().is_empty());

    // The user's own `model` (written by the user or the app) stays and is
    // what Codex runs in both modes.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    let path = w.ctx.codex_home().config_toml();
    let mut config = config_toml(&w);
    config.insert_str(0, "model = \"gpt-5.6-sol\"\n");
    std::fs::write(&path, config).unwrap();
    let plan = sw.plan(router(true)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(planned_model(&plan), None);
    assert_eq!(model_row(&sw, &plan).await, None);
    assert!(notice_keys(&plan).is_empty(), "{:?}", plan.warnings);
    execute(&sw, plan).await;
    assert_eq!(model_line(&w).as_deref(), Some("model = \"gpt-5.6-sol\""));
}

#[tokio::test]
async fn an_api_key_turned_into_a_pat_names_the_model_codex_will_run() {
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    execute(
        &sw,
        sw.plan(with_auth("work", AuthMode::ApiKey)).await.unwrap(),
    )
    .await;
    // PAT -> API key direct keeps the model too.
    assert_eq!(model_line(&w).as_deref(), Some("model = \"gpt-6.1-sol\""));

    // Without a kept model (an API-key profile from the start), the PAT's
    // default is named.
    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    let state = w
        .ctx
        .data_dir
        .root()
        .join(superba_switch::model_keep::KEPT_MODEL_FILE);
    execute(
        &sw,
        sw.plan(with_auth("work", AuthMode::ApiKey)).await.unwrap(),
    )
    .await;
    std::fs::remove_file(&state).unwrap();
    let plan = sw.plan(with_auth("work", AuthMode::Pat)).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    // config.toml holds a model Superba wrote but no longer knows as kept:
    // nothing is guessed about it.
    assert!(notice_keys(&plan).is_empty(), "{:?}", plan.warnings);

    let (w, sw) = pat_applied("gpt-6.1-sol").await;
    *w.prober.model_list.lock().unwrap() = Some(Err(Error::not_implemented("offline")));
    execute(
        &sw,
        sw.plan(with_auth("work", AuthMode::ApiKey)).await.unwrap(),
    )
    .await;
    assert_eq!(model_line(&w), None, "nothing known, nothing written");
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_list("gpt-6.1-sol")));
    let plan = sw.plan(with_auth("work", AuthMode::Pat)).await.unwrap();
    assert_eq!(planned_model(&plan), None);
    assert_eq!(
        notice(&plan, "switch.models.pat_default", Locale::En),
        "With the personal access token Codex runs gpt-6.1-sol, the default model of “Krill”. \
         To use another model, set one on the profile (Models page, or spb profile edit --model)."
    );
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

#[test]
fn the_provider_default_is_the_first_listed_model_by_priority() {
    let list = krill_list("gpt-6.1-sol");
    assert_eq!(
        provider_default(&list.models).as_deref(),
        Some("gpt-6.1-sol")
    );
    // Hidden entries never are the default, entries without a priority last.
    let models = vec![
        ModelSpec {
            slug: "unranked".into(),
            ..ModelSpec::default()
        },
        spec("hidden", 0, ModelVisibility::Hide),
        spec("b", 5, ModelVisibility::List),
        spec("a", 5, ModelVisibility::List),
    ];
    assert_eq!(provider_default(&models).as_deref(), Some("b"));
    assert_eq!(
        provider_default(&[spec("hidden", 0, ModelVisibility::Hide)]),
        None
    );
    assert_eq!(provider_default(&[]), None);
}

#[test]
fn the_form_follows_the_auth_mode() {
    assert_eq!(ModelForm::of(AuthMode::Pat), Some(ModelForm::ProviderList));
    assert_eq!(ModelForm::of(AuthMode::ApiKey), Some(ModelForm::ApiKey));
    assert_eq!(ModelForm::of(AuthMode::OfficialChatGpt), None);
}

fn state(auth: AuthMode, site: &str, model: Option<&str>, owned: bool) -> ConfigState {
    let mut s = base_state(std::path::Path::new("/home/u/.codex"));
    s.model = model.map(str::to_owned);
    s.model_owned = owned;
    s.active = Some(ActiveState {
        profile_id: Some(ProfileId::new("work")),
        site_id: Some(SiteId::new(site)),
        line_id: None,
        auth_mode: Some(auth),
        generation: 3,
        applied_at: Timestamp(T0),
    });
    s
}

#[test]
fn the_scope_is_a_change_of_form_on_the_same_site_with_no_model() {
    static KRILL: std::sync::LazyLock<SiteId> = std::sync::LazyLock::new(|| SiteId::new("krill"));
    fn input(s: &ConfigState) -> ScopeInput<'_> {
        ScopeInput {
            state: s,
            current_routed: false,
            kept: None,
            dest_site: &KRILL,
            dest_auth: AuthMode::ApiKey,
            dest_routed: false,
            dest_sets_model: false,
            left: None,
        }
    }
    let pat = state(AuthMode::Pat, "krill", None, false);
    assert_eq!(
        keep_scope(input(&pat)),
        Some(KeepScope {
            from: ModelForm::ProviderList,
            to: ModelForm::ApiKey,
            kept: None,
        })
    );
    // The destination names a model, config.toml keeps one, another site,
    // the same form, the official login, a hand-off: out of scope.
    assert_eq!(
        keep_scope(ScopeInput {
            dest_sets_model: true,
            ..input(&pat)
        }),
        None
    );
    assert_eq!(
        keep_scope(ScopeInput {
            left: Some("gpt-5.6-sol"),
            ..input(&pat)
        }),
        None
    );
    let other = SiteId::new("custom");
    assert_eq!(
        keep_scope(ScopeInput {
            dest_site: &other,
            ..input(&pat)
        }),
        None
    );
    let api = state(AuthMode::ApiKey, "krill", None, false);
    assert_eq!(keep_scope(input(&api)), None);
    let official = state(AuthMode::OfficialChatGpt, "krill", None, false);
    assert_eq!(keep_scope(input(&official)), None);
    // A Superba-written model that was not kept (a profile's model the user
    // removed since) is released as asked.
    let removed = state(AuthMode::Pat, "krill", Some("gpt-6-luna"), true);
    assert_eq!(keep_scope(input(&removed)), None);
    // A kept model is in scope also without a change of form.
    let kept = state(AuthMode::ApiKey, "krill", Some("gpt-6.1-sol"), true);
    assert_eq!(
        keep_scope(ScopeInput {
            kept: Some("gpt-6.1-sol"),
            ..input(&kept)
        }),
        Some(KeepScope {
            from: ModelForm::ApiKey,
            to: ModelForm::ApiKey,
            kept: Some("gpt-6.1-sol".into()),
        })
    );
}

#[test]
fn the_decision_keeps_what_runs_now_unless_the_destination_runs_it_too() {
    let site = superba_core::LocalizedText::new("", "Krill");
    let scope = |from, to, kept: Option<&str>| KeepScope {
        from,
        to,
        kept: kept.map(str::to_owned),
    };
    let read = |m: &str| KeepFacts {
        provider_default: Some(m.into()),
        list_error: None,
    };
    let keep = |d: KeepDecision| match d {
        KeepDecision::Keep { model, notice } => (model, notice.text.key),
        other => panic!("{other:?}"),
    };
    use ModelForm::{ApiKey, ProviderList};
    assert_eq!(
        keep(decide(
            &scope(ProviderList, ApiKey, None),
            &read("sol"),
            &site
        )),
        ("sol".into(), "switch.models.keep_api_key".into())
    );
    assert_eq!(
        decide(
            &scope(ApiKey, ProviderList, Some("sol")),
            &read("sol"),
            &site
        ),
        KeepDecision::Nothing
    );
    assert_eq!(
        keep(decide(
            &scope(ApiKey, ProviderList, Some("sol")),
            &read("astra"),
            &site
        )),
        ("sol".into(), "switch.models.keep_pat_default".into())
    );
    assert_eq!(
        keep(decide(
            &scope(ApiKey, ApiKey, Some("sol")),
            &KeepFacts::default(),
            &site
        )),
        ("sol".into(), "switch.models.keep_api_key".into())
    );
    let unread = KeepFacts {
        provider_default: None,
        list_error: Some("HTTP 503".into()),
    };
    assert_eq!(
        keep(decide(
            &scope(ApiKey, ProviderList, Some("sol")),
            &unread,
            &site
        )),
        ("sol".into(), "switch.models.keep_unread".into())
    );
    match decide(&scope(ApiKey, ProviderList, None), &unread, &site) {
        KeepDecision::Nothing => {}
        other => panic!("{other:?}"),
    }
    match decide(&scope(ApiKey, ProviderList, None), &read("sol"), &site) {
        KeepDecision::Note(n) => assert_eq!(n.text.key, "switch.models.pat_default"),
        other => panic!("{other:?}"),
    }
    match decide(
        &scope(ProviderList, ApiKey, None),
        &KeepFacts {
            provider_default: None,
            list_error: Some("HTTP 503".into()),
        },
        &site,
    ) {
        KeepDecision::Note(n) => assert_eq!(n.text.key, "switch.models.keep_unknown"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_kept_model_counts_only_while_its_generation_holds_it() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = superba_core::LocalContext::for_tests(tmp.path());
    let home = ctx.codex_home();
    let s = state(AuthMode::ApiKey, "krill", Some("gpt-6.1-sol"), true);
    assert_eq!(kept_model(&ctx.data_dir, &home, &s), None);
    commit_kept(&ctx.data_dir, &home, Some("gpt-6.1-sol"), 3).unwrap();
    assert_eq!(
        kept_model(&ctx.data_dir, &home, &s).as_deref(),
        Some("gpt-6.1-sol")
    );
    // Another generation, another value, a value Superba does not own.
    let mut later = s.clone();
    later.active.as_mut().unwrap().generation = 4;
    assert_eq!(kept_model(&ctx.data_dir, &home, &later), None);
    let mut edited = s.clone();
    edited.model = Some("gpt-5.5".into());
    assert_eq!(kept_model(&ctx.data_dir, &home, &edited), None);
    let mut foreign = s.clone();
    foreign.model_owned = false;
    assert_eq!(kept_model(&ctx.data_dir, &home, &foreign), None);
    commit_kept(&ctx.data_dir, &home, None, 4).unwrap();
    assert_eq!(kept_model(&ctx.data_dir, &home, &s), None);
}
