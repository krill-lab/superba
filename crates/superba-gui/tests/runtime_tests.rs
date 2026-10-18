#![allow(missing_docs)]

mod support;

use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use superba_core::{
    AuthMode, Decision, DecisionRequest, Error, ExecuteOptions, LocalizedText, ModelDefaults,
    OutcomeStatus, Profile, Secret, Site, SwitchEvent, SwitchIntent, SwitchOutcome, SwitchPhase,
    SwitchPlan, SwitchRequest,
};
use superba_gui::runtime::{Backend, Command, PendingDecision, Reply, after_switches};
use support::fake::{FakeOptions, FakeService};

const TIMEOUT: Duration = Duration::from_secs(5);

fn compatible_pair(id: &str) -> (Site, Profile) {
    let site = superba_switch::new_compatible_site(
        id.into(),
        "Example API",
        &"https://api.example.com/tenant/v1".parse().unwrap(),
    )
    .unwrap();
    let profile = Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        id: id.into(),
        name: "Example API".into(),
        builtin: None,
        site_id: site.id.clone(),
        line_id: None,
        auth_mode: AuthMode::ApiKey,
        key_ref: None,
        base_url_override: None,
        model: ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: Some("example-model".into()),
            reasoning_effort: None,
        },
        auth: superba_core::AuthChoice::from_mode(AuthMode::ApiKey),
        backup_lines: Vec::new(),
        codex_settings: Default::default(),
    };
    (site, profile)
}

fn add_compatible(backend: &Backend, site: Site, profile: Profile, key: &str) -> Vec<Reply> {
    backend.send(Command::AddCompatibleSite {
        site: Box::new(site),
        profile: Box::new(profile),
        key: Secret::new(key),
    });
    let replies = collect_until(backend, "site add and refresh", |reply| {
        matches!(reply, Reply::Profiles(_))
    });
    assert!(matches!(
        replies.as_slice(),
        [Reply::SiteAdded(_), Reply::Sites(_), Reply::Profiles(_)]
    ));
    replies
}

fn remove_compatible(backend: &Backend, id: &str) -> Vec<Reply> {
    backend.send(Command::RemoveCompatibleSite(id.into()));
    let replies = collect_until(backend, "site removal and refresh", |reply| {
        matches!(reply, Reply::Profiles(_))
    });
    assert!(matches!(
        replies.as_slice(),
        [Reply::SiteRemoved(_), Reply::Sites(_), Reply::Profiles(_)]
    ));
    replies
}

#[test]
fn add_compatible_site_stores_profile_key_and_refreshes_both_lists() {
    let backend = backend(false);
    let (site, profile) = compatible_pair("example");
    let replies = add_compatible(&backend, site.clone(), profile.clone(), "test-key");
    let [
        Reply::SiteAdded(Ok(saved)),
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
    ] = replies.as_slice()
    else {
        panic!("unexpected add replies: {replies:?}");
    };
    let mut expected = profile;
    assert!(saved.key_ref.is_some());
    expected.key_ref = saved.key_ref.clone();
    assert_eq!(saved, &expected);
    assert_eq!(sites.iter().find(|s| s.id == site.id), Some(&site));
    assert_eq!(profiles.profile(&saved.id), Some(saved));
}

#[test]
fn invalid_compatible_site_is_not_saved() {
    let backend = backend(false);
    let (mut site, profile) = compatible_pair("invalid");
    site.name = LocalizedText::new("", "");
    let replies = add_compatible(&backend, site, profile, "test-key");
    let [
        Reply::SiteAdded(Err(_)),
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
    ] = replies.as_slice()
    else {
        panic!("unexpected invalid-site replies: {replies:?}");
    };
    assert!(!sites.iter().any(|s| s.id == "invalid"));
    assert!(profiles.profile(&"invalid".into()).is_none());
}

#[test]
fn failed_profile_save_rolls_back_the_new_site() {
    let backend = backend(false);
    let (site, profile) = compatible_pair("rollback");
    // The site is valid; the fake service rejects the empty key in save_profile.
    let replies = add_compatible(&backend, site, profile, "");
    let [
        Reply::SiteAdded(Err(_)),
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
    ] = replies.as_slice()
    else {
        panic!("unexpected rollback replies: {replies:?}");
    };
    assert!(!sites.iter().any(|s| s.id == "rollback"));
    assert!(profiles.profile(&"rollback".into()).is_none());
}

fn save_site_and_profile(backend: &Backend, site: Site, profile: Profile, key: &str) -> Vec<Reply> {
    backend.send(Command::SaveSiteAndProfile {
        site: Box::new(site),
        profile: Box::new(profile),
        key: Some(Secret::new(key)),
    });
    let replies = collect_until(backend, "site and profile saved", |reply| {
        matches!(reply, Reply::ProfileSaved(_))
    });
    assert!(matches!(
        replies.as_slice(),
        [Reply::Sites(_), Reply::Profiles(_), Reply::ProfileSaved(_)]
    ));
    replies
}

#[test]
fn the_wizard_save_stores_site_profile_and_key_and_removes_a_new_site_on_failure() {
    let backend = backend(false);
    let (site, profile) = compatible_pair("wizard");
    let replies = save_site_and_profile(&backend, site.clone(), profile.clone(), "test-key");
    let [
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
        Reply::ProfileSaved(Ok(saved)),
    ] = replies.as_slice()
    else {
        panic!("unexpected save replies: {replies:?}");
    };
    assert!(saved.key_ref.is_some());
    assert!(sites.iter().any(|s| s.id == site.id));
    assert!(profiles.profile(&saved.id).is_some());
    // A re-run edits the stored site; a failing profile save keeps it.
    let mut edited = site;
    edited.name = LocalizedText::new("", "Renamed API");
    let replies = save_site_and_profile(&backend, edited, profile, "");
    let [Reply::Sites(Ok(sites)), _, Reply::ProfileSaved(Err(_))] = replies.as_slice() else {
        panic!("unexpected edit replies: {replies:?}");
    };
    assert!(
        sites.iter().any(|s| s.id == "wizard"),
        "an existing site stays"
    );
    // A new site goes again when its profile cannot be saved.
    let (site, profile) = compatible_pair("wizard-new");
    let replies = save_site_and_profile(&backend, site, profile, "");
    let [Reply::Sites(Ok(sites)), _, Reply::ProfileSaved(Err(_))] = replies.as_slice() else {
        panic!("unexpected rollback replies: {replies:?}");
    };
    assert!(!sites.iter().any(|s| s.id == "wizard-new"));
}

fn save_second_profile(backend: &Backend, mut profile: Profile) -> Profile {
    profile.id = "second".into();
    backend.send(Command::SaveProfile {
        profile: Box::new(profile),
        key: Some(Secret::new("test-second")),
    });
    let replies = collect_until(backend, "second profile saved", |r| {
        matches!(r, Reply::ProfileSaved(_))
    });
    let Some(Reply::ProfileSaved(Ok(saved))) = replies.last() else {
        panic!("{replies:?}");
    };
    saved.clone()
}

#[test]
fn remove_compatible_site_removes_all_its_profiles_and_keeps_other_sites() {
    let backend = backend(false);
    let (site, profile) = compatible_pair("example");
    assert!(matches!(
        &add_compatible(&backend, site, profile.clone(), "test-key")[0],
        Reply::SiteAdded(Ok(_))
    ));
    save_second_profile(&backend, profile);
    let replies = remove_compatible(&backend, "example");
    let [
        Reply::SiteRemoved(Ok(())),
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
    ] = replies.as_slice()
    else {
        panic!("unexpected removal replies: {replies:?}");
    };
    assert!(!sites.iter().any(|s| s.id == "example"));
    assert!(!profiles.profiles.iter().any(|p| p.site_id == "example"));
    assert!(profiles.profile(&"krill".into()).is_some());
    assert!(sites.iter().any(|s| s.id == "krill"));
}

#[test]
fn remove_active_compatible_site_refuses_before_deleting_any_profiles() {
    let backend = backend(false);
    let (site, profile) = compatible_pair("example");
    assert!(matches!(
        &add_compatible(&backend, site, profile.clone(), "test-key")[0],
        Reply::SiteAdded(Ok(_))
    ));
    let second = save_second_profile(&backend, profile);
    backend.send(Command::Plan {
        seq: 42,
        request: SwitchRequest::profile(second.id.clone()),
    });
    let replies = collect_until(&backend, "compatible plan", |r| {
        matches!(r, Reply::Planned { .. })
    });
    let Some(Reply::Planned {
        result: Ok(plan), ..
    }) = replies.last()
    else {
        panic!("{replies:?}");
    };
    assert_eq!(plan.to.line_id.as_ref().unwrap().as_str(), "default");
    // The plan's settings preview is sent after it by the same command; wait
    // for it so it cannot arrive in the middle of the removal's replies.
    collect_until(&backend, "compatible plan settings", |r| {
        matches!(r, Reply::PlanSettings { .. })
    });
    execute(&backend, plan.clone());
    assert_eq!(take_outcome(&backend).0.status, OutcomeStatus::Switched);
    let replies = collect_until(&backend, "applied compatible site", |r| {
        matches!(r, Reply::Overview(_))
    });
    let Some(Reply::Overview(Ok(overview))) = replies.last() else {
        panic!("{replies:?}");
    };
    assert_eq!(
        overview.active.as_ref().unwrap().profile_id.as_ref(),
        Some(&second.id)
    );
    assert_eq!(
        overview.config.as_ref().unwrap().openai_base_url.as_deref(),
        Some("https://api.example.com/tenant/v1")
    );

    let replies = remove_compatible(&backend, "example");
    let [
        Reply::SiteRemoved(Err(Error::Conflict { .. })),
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
    ] = replies.as_slice()
    else {
        panic!("unexpected active-site replies: {replies:?}");
    };
    assert!(sites.iter().any(|s| s.id == "example"));
    // The first profile is inactive but must survive the failed removal too.
    assert!(profiles.profile(&"example".into()).is_some());
    assert!(profiles.profile(&second.id).is_some());
}

#[test]
fn add_with_a_stale_id_keeps_the_existing_site_and_profile() {
    let backend = backend(false);
    let (site, profile) = compatible_pair("example");
    assert!(matches!(
        &add_compatible(&backend, site.clone(), profile.clone(), "test-key")[0],
        Reply::SiteAdded(Ok(_))
    ));
    let replies = add_compatible(&backend, site.clone(), profile, "");
    let [
        Reply::SiteAdded(Err(Error::Conflict { .. })),
        Reply::Sites(Ok(sites)),
        Reply::Profiles(Ok(profiles)),
    ] = replies.as_slice()
    else {
        panic!("unexpected duplicate-site replies: {replies:?}");
    };
    assert_eq!(sites.iter().find(|s| s.id == site.id), Some(&site));
    assert!(
        profiles
            .profile(&"example".into())
            .unwrap()
            .key_ref
            .is_some()
    );
}

fn backend(busy_turns: bool) -> Backend {
    let svc = FakeService::new(FakeOptions {
        fast: true,
        busy_turns,
        ..FakeOptions::default()
    });
    Backend::start(Arc::new(svc), Arc::new(|| {})).expect("start the test backend")
}

fn line_request() -> SwitchRequest {
    SwitchRequest {
        intent: SwitchIntent::Profile {
            profile_id: "krill".into(),
            line_id: Some("cdn".into()),
            auth_mode: Some(AuthMode::Pat),
        },
        ..SwitchRequest::profile("krill".into())
    }
}

// A deadline bounds the whole collection, even if unrelated replies keep arriving.
fn collect_until(
    backend: &Backend,
    expected: &str,
    mut done: impl FnMut(&Reply) -> bool,
) -> Vec<Reply> {
    let deadline = Instant::now() + TIMEOUT;
    let mut replies = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for {expected}: {replies:?}"
        );
        let reply = backend.recv_timeout(remaining).unwrap_or_else(|| {
            panic!("backend stopped or timed out waiting for {expected}: {replies:?}")
        });
        let complete = done(&reply);
        replies.push(reply);
        if complete {
            return replies;
        }
    }
}

fn plan(backend: &Backend, seq: u64) -> SwitchPlan {
    let request = line_request();
    backend.send(Command::Plan {
        seq,
        request: request.clone(),
    });
    let mut replies = collect_until(backend, "Planned", |reply| {
        matches!(reply, Reply::Planned { .. })
    });
    let Some(Reply::Planned {
        seq: actual,
        result,
    }) = replies.pop()
    else {
        unreachable!("collection ends with Planned");
    };
    assert_eq!(actual, seq);
    let plan = result.expect("line switch can be planned");
    assert_eq!(plan.request, request);
    assert!(plan.primary.is_hot());
    assert!(plan.choices.is_empty());
    // The settings preview of the plan follows the plan itself.
    let preview = collect_until(backend, "PlanSettings", |reply| {
        matches!(reply, Reply::PlanSettings { .. })
    });
    assert!(matches!(
        preview.last(),
        Some(Reply::PlanSettings { plan_id, .. }) if *plan_id == plan.id
    ));
    plan
}

fn execute(backend: &Backend, plan: SwitchPlan) {
    backend.send(Command::Execute {
        plan: Box::new(plan),
        options: ExecuteOptions::default(),
    });
}

fn take_wait_question(backend: &Backend) -> PendingDecision {
    let mut replies = collect_until(backend, "Decide", |reply| matches!(reply, Reply::Decide(_)));
    let Some(Reply::Decide(question)) = replies.pop() else {
        unreachable!("collection ends with Decide");
    };
    assert!(
        matches!(&question.request, DecisionRequest::WaitForTurns { busy, .. } if !busy.is_empty())
    );
    question
}

fn take_outcome(backend: &Backend) -> (SwitchOutcome, Vec<Reply>) {
    let mut replies = collect_until(backend, "Executed", |reply| {
        matches!(reply, Reply::Executed(_))
    });
    let Some(Reply::Executed(result)) = replies.pop() else {
        unreachable!("collection ends with Executed");
    };
    (result.expect("execution succeeds"), replies)
}

fn refreshed_line(backend: &Backend, line: &str) {
    let mut profiles = false;
    let mut overview = false;
    collect_until(backend, "post-execution Profiles and Overview", |reply| {
        match reply {
            Reply::Profiles(result) => {
                let set = result.as_ref().expect("profiles refresh succeeds");
                assert_eq!(
                    set.profile(&"krill".into())
                        .unwrap()
                        .line_id
                        .as_ref()
                        .unwrap()
                        .as_str(),
                    line
                );
                profiles = true;
            }
            Reply::Overview(result) => {
                let snapshot = result.as_ref().expect("overview refresh succeeds");
                assert_eq!(
                    snapshot
                        .active
                        .as_ref()
                        .unwrap()
                        .line_id
                        .as_ref()
                        .unwrap()
                        .as_str(),
                    line
                );
                overview = true;
            }
            other => panic!("unexpected reply after Executed: {other:?}"),
        }
        profiles && overview
    });
}

#[test]
fn refresh_returns_profiles_sites_and_overview() {
    let backend = backend(false);
    backend.send(Command::Refresh);
    let mut received = BTreeSet::new();
    collect_until(&backend, "all refresh replies", |reply| {
        match reply {
            Reply::Profiles(result) => {
                let profiles = result.as_ref().expect("profiles load succeeds");
                assert!(profiles.profile(&"krill".into()).is_some());
                received.insert("profiles");
            }
            Reply::Sites(result) => {
                let sites = result.as_ref().expect("sites load succeeds");
                assert!(sites.iter().any(|site| site.id == "krill"));
                received.insert("sites");
            }
            Reply::Overview(result) => {
                assert!(
                    result
                        .as_ref()
                        .expect("overview load succeeds")
                        .active
                        .is_some()
                );
                received.insert("overview");
            }
            other => panic!("unexpected refresh reply: {other:?}"),
        }
        received.len() == 3
    });
    assert_eq!(received, BTreeSet::from(["profiles", "sites", "overview"]));
}

#[test]
fn probe_lines_returns_three_krill_capabilities() {
    let backend = backend(false);
    backend.send(Command::ProbeLines("krill".into()));
    let mut replies = collect_until(&backend, "Lines", |reply| {
        matches!(reply, Reply::Lines { .. })
    });
    let Some(Reply::Lines { site, result }) = replies.pop() else {
        unreachable!("collection ends with Lines");
    };
    assert_eq!(site.as_str(), "krill");
    let caps = result.expect("line probes succeed");
    assert_eq!(caps.len(), 3);
    assert!(caps.iter().all(|cap| cap.site_id == site));
    assert_eq!(
        caps.iter()
            .map(|cap| cap.line_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["cdn", "cn-fast", "overseas"])
    );
}

#[test]
fn hot_switch_forwards_events_then_outcome_then_refreshed_state() {
    let backend = backend(false);
    let plan = plan(&backend, 17);
    let id = plan.id.clone();
    execute(&backend, plan);
    let (outcome, replies) = take_outcome(&backend);
    assert_eq!(outcome.plan_id, id);
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    assert!(outcome.restart.is_hot());
    let phases: Vec<_> = replies
        .iter()
        .filter_map(|reply| match reply {
            Reply::Event(SwitchEvent::Phase { phase, .. }) => Some(*phase),
            _ => None,
        })
        .collect();
    assert_eq!(phases.first(), Some(&SwitchPhase::TurnCheck));
    assert_eq!(phases.last(), Some(&SwitchPhase::Done));
    assert!(phases.contains(&SwitchPhase::Applying));
    assert!(phases.contains(&SwitchPhase::Restarting));
    assert!(phases.contains(&SwitchPhase::Verifying));
    assert!(replies.iter().all(|reply| matches!(reply, Reply::Event(_))));
    refreshed_line(&backend, "cdn");
}

#[test]
fn forcing_busy_turns_completes_the_switch() {
    let backend = backend(true);
    execute(&backend, plan(&backend, 1));
    take_wait_question(&backend).answer(Decision::Force);
    let (outcome, _) = take_outcome(&backend);
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    refreshed_line(&backend, "cdn");
}

#[test]
fn dropping_the_pending_decision_cancels_without_applying() {
    let backend = backend(true);
    execute(&backend, plan(&backend, 1));
    drop(take_wait_question(&backend));
    let (outcome, replies) = take_outcome(&backend);
    assert_eq!(outcome.status, OutcomeStatus::Cancelled);
    assert!(outcome.generation.is_none());
    assert!(!replies.iter().any(|reply| matches!(
        reply,
        Reply::Event(SwitchEvent::Phase {
            phase: SwitchPhase::Applying,
            ..
        })
    )));
    refreshed_line(&backend, "overseas");
}

#[test]
fn invalid_answer_to_wait_question_is_treated_as_cancel() {
    let backend = backend(true);
    execute(&backend, plan(&backend, 1));
    take_wait_question(&backend).answer(Decision::KeepWritten);
    assert_eq!(take_outcome(&backend).0.status, OutcomeStatus::Cancelled);
    refreshed_line(&backend, "overseas");
}

fn drop_with_timeout(backend: Backend) {
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        drop(backend);
        tx.send(())
            .expect("test is still waiting for backend shutdown");
    });
    rx.recv_timeout(TIMEOUT)
        .expect("Backend::drop must join its thread within five seconds");
    thread.join().expect("shutdown thread must not panic");
}

#[test]
fn dropping_idle_backend_joins_and_releases_the_service() {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..FakeOptions::default()
    }));
    let weak = Arc::downgrade(&service);
    let backend = Backend::start(service, Arc::new(|| {})).unwrap();
    drop_with_timeout(backend);
    assert!(weak.upgrade().is_none(), "runtime must release its service");
}

#[test]
fn dropping_backend_with_an_open_question_cancels_in_flight_work() {
    let backend = backend(true);
    execute(&backend, plan(&backend, 1));
    let question = take_wait_question(&backend);
    drop_with_timeout(backend);
    assert!(
        question.is_stale(),
        "shutdown must close the decision receiver"
    );
}

#[test]
fn results_computed_across_a_finished_switch_are_recomputed() {
    use std::sync::atomic::{AtomicU64, Ordering};
    // e2e: a periodic overview read the old configuration, finished after the
    // switch's refresh and made the GUI show the previous line again.
    let switches = AtomicU64::new(0);
    let runs = AtomicU64::new(0);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let value = rt.block_on(after_switches(&switches, || {
        let n = runs.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            // A switch finishes while the first computation runs.
            switches.fetch_add(1, Ordering::SeqCst);
        }
        async move { n }
    }));
    assert_eq!(value, 1, "the straddling result is replaced by a fresh one");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    // Without a switch in between, one computation is enough.
    let value = rt.block_on(after_switches(&switches, || async { 7 }));
    assert_eq!(value, 7);
}

/// User bug 23: the language the UI shows reaches the service, which writes the
/// default stats line in it (and re-configures the router when it changes).
#[test]
fn the_ui_language_reaches_the_service() {
    use superba_core::Locale;
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..FakeOptions::default()
    }));
    let backend = Backend::start(service.clone(), Arc::new(|| {})).unwrap();
    backend.send(Command::UiLocale(Locale::ZhCn));
    let deadline = Instant::now() + TIMEOUT;
    while service.ui_locales().is_empty() {
        assert!(Instant::now() < deadline, "the UI language never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(service.ui_locales(), [Locale::ZhCn]);
    drop_with_timeout(backend);
}
