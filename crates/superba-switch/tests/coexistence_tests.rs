//! GH#5 through the service, with the real config engine: cc-switch and AI
//! Toolbox (their stores from superba-import's fixtures, their writes from
//! `superba-config/tests/common/tools.rs`) change what Superba wrote; the
//! overview names who and what, a switch asks (take over or cancel), a
//! restore and an uninstall keep the other tool's changes, a hand-off releases
//! everything and a later switch takes over again from a fresh baseline, and
//! the router's fallback writes nothing once another tool took Codex over.

#![allow(missing_docs)]

mod fakes;
#[path = "../../superba-config/tests/common/tools.rs"]
mod tools;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use superba_config::toml_keys::ConfigDoc;
use superba_core::codex::config_keys as k;
use superba_core::{
    AUTH_JSON_PATH, AuthShape, ChangeSource, ExecuteOptions, NoEvents, OutcomeStatus, PlanChoice,
    ProfileId, ProviderRestore, RestartKind, RestoreTarget, RouterInstallRequest, SafeDefaults,
    SwitchIntent, SwitchPlan, SwitchRequest, SwitchService,
};
use superba_switch::{Switcher, Tuning};
use tools::{AITB_RELAY, CCS_KIMI};

const IMPORT_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../superba-import/tests/fixtures"
);

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

fn path(w: &World, file: &str) -> std::path::PathBuf {
    w.ctx.codex_home().root().join(file)
}

fn read(w: &World, file: &str) -> Option<String> {
    std::fs::read_to_string(path(w, file)).ok()
}

fn write(w: &World, file: &str, text: Option<&str>) {
    match text {
        Some(t) => std::fs::write(path(w, file), t).unwrap(),
        None => {
            let _ = std::fs::remove_file(path(w, file));
        }
    }
}

fn get(w: &World, key: &str) -> Option<String> {
    ConfigDoc::parse(read(w, "config.toml").unwrap_or_default().as_bytes())
        .unwrap()
        .get_str(key)
}

fn fixture(rel: &str) -> String {
    std::fs::read_to_string(format!("{IMPORT_FIXTURES}/{rel}")).unwrap()
}

fn sqlite(target: &std::path::Path, sql: &str, wal: bool) {
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(target).unwrap();
    conn.execute_batch(sql).unwrap();
    if wal {
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
    }
}

/// cc-switch (current Codex provider "Kimi") and AI Toolbox ("Example Relay"
/// applied) installed in the test home.
fn install_tools(w: &World) {
    let home = &w.ctx.home_dir;
    sqlite(
        &home.join(".cc-switch/cc-switch.db"),
        &fixture("ccs/schema-v20.sql"),
        false,
    );
    std::fs::write(
        home.join(".cc-switch/settings.json"),
        fixture("ccs/settings.json"),
    )
    .unwrap();
    let aitb = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/com.ai-toolbox/ai-toolbox.db")
    } else if cfg!(windows) {
        home.join("AppData/Roaming/com.ai-toolbox/ai-toolbox.db")
    } else {
        home.join(".local/share/com.ai-toolbox/ai-toolbox.db")
    };
    sqlite(&aitb, &fixture("ai-toolbox/schema-v29.sql"), true);
}

/// The user's Codex, then Superba's Krill switch ("work", PAT).
async fn on_krill() -> (World, Switcher) {
    let w = World::new();
    install_tools(&w);
    std::fs::create_dir_all(w.ctx.codex_home().root()).unwrap();
    write(
        &w,
        "config.toml",
        Some("# my settings\nmodel = \"gpt-5.5\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\n"),
    );
    let sw = switcher(&w);
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(plan.choices.is_empty() && !plan.is_blocked(), "{plan:?}");
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert!(read(&w, "auth.json").is_some());
    (w, sw)
}

fn cc_switch_switches(w: &World) {
    let (config, auth) = tools::cc_switch(read(w, "config.toml").as_deref(), &CCS_KIMI);
    write(w, "config.toml", Some(&config));
    write(w, "auth.json", auth.as_deref());
}

fn ai_toolbox_applies(w: &World) {
    let (config, auth) = tools::ai_toolbox(
        read(w, "config.toml").as_deref(),
        read(w, "auth.json").as_deref(),
        &AITB_RELAY,
    );
    write(w, "config.toml", Some(&config));
    write(w, "auth.json", Some(&auth));
}

#[tokio::test]
async fn the_overview_names_cc_switch_and_a_switch_asks_before_taking_over() {
    let (w, sw) = on_krill().await;
    cc_switch_switches(&w);
    let before = (read(&w, "config.toml"), read(&w, "auth.json"));

    let x = sw.overview().await.unwrap().external.expect("detected");
    assert_eq!(
        x.source,
        ChangeSource::CcSwitch {
            provider: Some("Kimi".into())
        }
    );
    let keys: Vec<&str> = x.keys.iter().map(|c| c.path.as_str()).collect();
    assert!(keys.contains(&k::MODEL_PROVIDER), "{keys:?}");
    assert_eq!(x.auth.as_ref().map(|a| &a.live), Some(&AuthShape::Missing));
    assert!(!x.fingerprint.is_empty());
    // The diagnostics name it too.
    let doctor = sw.doctor().await.unwrap();
    assert!(doctor.iter().any(|f| f.id == "external_change"));

    // Switching asks, with the credential and the source.
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let [PlanChoice::OwnedKeyConflict { auth, source, .. }] = plan.choices.as_slice() else {
        panic!("{:?}", plan.choices);
    };
    assert!(auth.is_some());
    assert_eq!(source, &x.source);
    // Cancelling (not answering) writes nothing.
    drop(plan);
    assert_eq!((read(&w, "config.toml"), read(&w, "auth.json")), before);

    // Taking over writes Superba's configuration again.
    let mut request = SwitchRequest::profile(ProfileId::new("work"));
    superba_switch::ownership::take_over(&mut request);
    let plan = sw.plan(request).await.unwrap();
    assert!(plan.choices.is_empty(), "{:?}", plan.choices);
    assert_eq!(
        plan.request.key_resolutions.get(AUTH_JSON_PATH),
        Some(&superba_core::KeyResolution::Overwrite)
    );
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert_eq!(get(&w, k::MODEL_PROVIDER).as_deref(), Some("openai"));
    assert!(read(&w, "auth.json").is_some());
    assert!(sw.overview().await.unwrap().external.is_none());
}

#[tokio::test]
async fn restoring_or_uninstalling_keeps_what_cc_switch_changed() {
    for uninstall in [false, true] {
        let (w, sw) = on_krill().await;
        cc_switch_switches(&w);
        let intent = if uninstall {
            SwitchIntent::Uninstall {
                delete_backups: false,
                provider: None,
            }
        } else {
            SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::KeepOpenai,
                },
            }
        };
        let plan = sw
            .plan(SwitchRequest {
                intent,
                ..SwitchRequest::profile(ProfileId::new(""))
            })
            .await
            .unwrap();
        assert!(plan.choices.is_empty(), "{:?}", plan.choices);
        let kept = plan
            .warnings
            .iter()
            .find(|n| n.text.key == "switch.ownership.kept")
            .expect("the plan says what it keeps");
        let items = kept
            .text
            .args
            .iter()
            .find(|(n, _)| n == "items")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(items.contains(k::MODEL_PROVIDER) && items.contains(AUTH_JSON_PATH));
        let outcome = execute(&sw, plan).await;
        assert!(
            matches!(
                outcome.status,
                OutcomeStatus::Switched | OutcomeStatus::WrittenOnly
            ),
            "{outcome:?}"
        );
        // cc-switch's provider, model and missing auth.json stay; Superba's
        // own chatgpt_base_url (which cc-switch does not manage) is gone.
        assert_eq!(get(&w, k::MODEL_PROVIDER).as_deref(), Some("custom"));
        assert_eq!(get(&w, "model").as_deref(), Some("kimi-k3"));
        assert_eq!(
            get(&w, "model_providers.custom.base_url").as_deref(),
            Some(CCS_KIMI.base_url)
        );
        assert_eq!(get(&w, k::CHATGPT_BASE_URL), None);
        assert!(read(&w, "auth.json").is_none());
        assert!(sw.overview().await.unwrap().external.is_none());
    }
}

#[tokio::test]
async fn a_hand_off_releases_superba_and_a_later_switch_takes_over_recoverably() {
    let (w, sw) = on_krill().await;
    ai_toolbox_applies(&w);
    let x = sw.overview().await.unwrap().external.expect("detected");
    assert_eq!(
        x.source,
        ChangeSource::AiToolbox {
            provider: Some("Example Relay".into())
        }
    );
    let mut request = SwitchRequest::profile(ProfileId::new(""));
    request.intent = SwitchIntent::HandOff {
        to: x.source.clone(),
    };
    let plan = sw.plan(request).await.unwrap();
    assert!(plan.choices.is_empty() && !plan.is_blocked(), "{plan:?}");
    assert_eq!(plan.primary, RestartKind::None);
    assert!(plan.migration.is_none());
    assert!(
        plan.warnings
            .iter()
            .any(|n| n.text.key == "switch.ownership.hand_off_note")
    );
    let auth = read(&w, "auth.json");
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    // AI Toolbox's state stays; Superba's leftovers are put back.
    assert_eq!(get(&w, k::MODEL_PROVIDER).as_deref(), Some("custom"));
    assert_eq!(get(&w, k::OPENAI_BASE_URL), None);
    assert_eq!(get(&w, k::CHATGPT_BASE_URL), None);
    assert!(
        read(&w, "config.toml")
            .unwrap()
            .starts_with("#:schema none\n")
    );
    assert_eq!(read(&w, "auth.json"), auth);
    let o = sw.overview().await.unwrap();
    assert!(o.external.is_none());
    assert_eq!(
        o.config.as_ref().unwrap().handed_off.as_ref().unwrap().to,
        x.source
    );
    let handed = (read(&w, "config.toml"), read(&w, "auth.json"));

    // AI Toolbox keeps changing things: not Superba's business any more.
    let picked = tools::codex_model_picker(&read(&w, "config.toml").unwrap(), "gpt-5.4");
    write(&w, "config.toml", Some(&picked));
    assert!(sw.overview().await.unwrap().external.is_none());
    let handed = (Some(picked), handed.1);

    // Switching to a profile takes over again (from a fresh baseline: what
    // AI Toolbox had), and restoring the original completely (its provider
    // `custom` included) gives AI Toolbox's files back byte for byte.
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(plan.choices.is_empty(), "{:?}", plan.choices);
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert_eq!(get(&w, k::MODEL_PROVIDER).as_deref(), Some("openai"));
    let plan = sw
        .plan(SwitchRequest {
            intent: SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::Baseline {
                        reverse_migrate: false,
                    },
                },
            },
            ..SwitchRequest::profile(ProfileId::new(""))
        })
        .await
        .unwrap();
    assert!(plan.choices.is_empty(), "{:?}", plan.choices);
    execute(&sw, plan).await;
    assert_eq!((read(&w, "config.toml"), read(&w, "auth.json")), handed);
}

#[tokio::test]
async fn the_router_fallback_writes_nothing_after_another_tool_took_codex_over() {
    let (w, sw) = on_krill().await;
    sw.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    let mut on = SwitchRequest::profile(ProfileId::new(""));
    on.intent = SwitchIntent::Router { enable: true };
    let plan = sw.plan(on).await.unwrap();
    assert!(plan.choices.is_empty() && !plan.is_blocked(), "{plan:?}");
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert!(sw.profile_set().await.unwrap().settings.router.enabled);

    ai_toolbox_applies(&w);
    let before = (read(&w, "config.toml"), read(&w, "auth.json"));
    let outcome = sw.router_fall_back("router died".into()).await.unwrap();
    assert!(outcome.is_none());
    assert_eq!((read(&w, "config.toml"), read(&w, "auth.json")), before);
    assert!(!sw.profile_set().await.unwrap().settings.router.enabled);
    // Codex no longer goes through the router: the watchdog stands down.
    assert!(!sw.router_status().await.unwrap().codex_routed);
}
