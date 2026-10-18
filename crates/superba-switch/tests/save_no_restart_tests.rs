//! Saving providers never touches Codex (user report on 0.3.0-alpha.1: adding a
//! provider prompted a hot restart). The service over the fakes of `fakes/`
//! with the real `config.toml` engine in a temporary home: after a switch,
//! adding a provider, editing a non-active one (key, lines, model, settings,
//! name), duplicating one and adding a custom site write no Codex file, restart
//! nothing and leave the re-apply of the active provider a no-op; editing the
//! active provider's connection is what makes its re-apply write (and the plan
//! names the keys that change, the reason the GUI and `spb` show).

#![allow(missing_docs)]

mod fakes;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use superba_core::{
    AuthChoice, AuthMode, ExecuteOptions, LineId, NoEvents, OutcomeStatus, ProfileId, SafeDefaults,
    Secret, SettingValue, SiteId, SwitchPlan, SwitchRequest, SwitchService,
};
use superba_switch::{Switcher, Tuning};

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
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

/// Every file of the Codex home with its contents.
fn codex_files(w: &World) -> BTreeMap<String, Vec<u8>> {
    let root = w.ctx.codex_home().root().to_path_buf();
    let mut out = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(&root).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    out
}

/// App restarts, quits and launches in the call log.
fn restarts(w: &World) -> Vec<String> {
    w.calls()
        .into_iter()
        .filter(|c| {
            c.starts_with("app.hot_restart")
                || c.starts_with("app.quit")
                || c.starts_with("app.launch")
        })
        .collect()
}

/// `work` applied through a real switch (API key on the overseas line).
async fn applied_world() -> (World, Switcher) {
    let w = World::new();
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "work")
            .unwrap();
        p.auth_mode = AuthMode::ApiKey;
        p.set_auth(AuthChoice::ApiKey);
    }
    let sw = switcher(&w);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
    assert!(codex_files(&w).contains_key("config.toml"));
    (w, sw)
}

#[tokio::test]
async fn saving_providers_other_than_the_applied_one_never_touches_codex() {
    let (w, sw) = applied_world().await;
    let files = codex_files(&w);
    let restarted = restarts(&w);
    let reapply = sw.plan(req("work")).await.unwrap();
    assert!(reapply.config.as_ref().is_some_and(|c| c.is_noop()));

    // Add a provider on the same site with its own key (the Add dialog).
    let second = krill_profile("second", Some("overseas"), AuthMode::ApiKey);
    sw.save_profile(second, Some(Secret::new("nb-second-key-2222")))
        .await
        .unwrap();
    // Edit it: key, lines, auth, model, Codex settings, name.
    let mut second = sw
        .profile_set()
        .await
        .unwrap()
        .profile(&ProfileId::new("second"))
        .cloned()
        .unwrap();
    second.name = "Second".into();
    second.set_line_priority([LineId::new("cn-fast"), LineId::new("overseas")]);
    second.set_auth(AuthChoice::ApiKey);
    second.model.model = Some("gpt-6-astra".into());
    second
        .codex_settings
        .set("model_verbosity", SettingValue::String("low".into()))
        .unwrap();
    sw.save_profile(second.clone(), Some(Secret::new("nb-second-key-3333")))
        .await
        .unwrap();
    // Duplicate it (the detail's menu): a new id, no key.
    let mut copy = second.clone();
    copy.id = ProfileId::new("second-copy");
    copy.name = "Second copy".into();
    copy.key_ref = None;
    sw.save_profile(copy, None).await.unwrap();
    // A custom site and its provider (`spb site add` + `spb profile add`).
    let url = url::Url::parse("https://api.example.invalid/v1").unwrap();
    let site = superba_switch::new_compatible_site(SiteId::new("my-api"), "My API", &url).unwrap();
    sw.save_site(site).await.unwrap();
    let mut custom = krill_profile("my-api", None, AuthMode::ApiKey);
    custom.site_id = SiteId::new("my-api");
    custom.set_auth(AuthChoice::ApiKey);
    sw.save_profile(custom, Some(Secret::new("sk-custom-4444")))
        .await
        .unwrap();

    assert_eq!(codex_files(&w), files, "no Codex file changed");
    assert_eq!(restarts(&w), restarted, "nothing restarted");
    let state = sw.overview().await.unwrap().active.unwrap();
    assert_eq!(state.profile_id, Some(ProfileId::new("work")));
    // What the applied provider needs is unchanged: re-applying it writes nothing.
    let reapply = sw.plan(req("work")).await.unwrap();
    assert!(
        reapply.config.as_ref().is_some_and(|c| c.is_noop()),
        "{:?}",
        reapply.config.as_ref().map(|c| &c.key_changes)
    );
    // Four providers of their own (none merged into another).
    let set = sw.profile_set().await.unwrap();
    let ids: Vec<&str> = set
        .profiles
        .iter()
        .filter(|p| !p.is_builtin())
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(ids, ["work", "second", "second-copy", "my-api"]);
}

#[tokio::test]
async fn editing_the_applied_providers_connection_makes_its_reapply_write_and_says_what() {
    let (w, sw) = applied_world().await;
    let files = codex_files(&w);
    let mut work = sw
        .profile_set()
        .await
        .unwrap()
        .profile(&ProfileId::new("work"))
        .cloned()
        .unwrap();
    work.set_line_priority([LineId::new("cn-fast")]);
    sw.save_profile(work, None).await.unwrap();
    // Saving alone still writes nothing.
    assert_eq!(codex_files(&w), files);
    let plan = sw.plan(req("work")).await.unwrap();
    let from = plan.from.as_ref().and_then(|a| a.profile_id.clone());
    assert_eq!(from, Some(ProfileId::new("work")));
    assert_eq!(plan.to.profile_id, Some(ProfileId::new("work")));
    let config = plan.config.as_ref().expect("a config plan");
    assert!(!config.is_noop());
    let keys: Vec<&str> = config.key_changes.iter().map(|k| k.path.as_str()).collect();
    assert!(
        keys.iter().any(|k| k.contains("base_url")),
        "the reason names the changed address: {keys:?}"
    );
    assert!(plan.primary.is_hot(), "{:?}", plan.primary);
}
