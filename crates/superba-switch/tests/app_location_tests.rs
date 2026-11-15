//! The desktop app location of the settings: checked before it is saved,
//! cleared, an older install choice dropped when it changes, its doctor
//! findings, and the model-catalog error that says what to do when no app
//! (or no bundled Codex) is found.

#![allow(missing_docs)]

mod fakes;

use std::path::PathBuf;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AppLocationCheck, AuthMode, BlockReason, Error, ModelOverride, ProfileId, RestartKind,
    Settings, Severity, SwitchRequest, SwitchService,
};

/// The fake's install, moved to an absolute path of this platform.
fn installed(w: &World) -> PathBuf {
    let path = w.tmp.path().join("Apps").join("ChatGPT");
    w.app.installs.lock().unwrap()[0].path = path.clone();
    path
}

/// An absolute path of this platform under the test's directory.
fn abs(w: &World, name: &str) -> PathBuf {
    w.tmp.path().join(name)
}

#[tokio::test]
async fn a_location_is_checked_saved_and_cleared() {
    let w = World::new();
    w.profiles.set.lock().unwrap().settings.preferred_app_id = Some("older".into());
    let sw = w.switcher();
    let path = installed(&w);

    let check = sw.check_app_location(path.clone()).await.unwrap();
    assert!(matches!(check, AppLocationCheck::Install { .. }));
    assert!(
        w.profiles
            .set
            .lock()
            .unwrap()
            .settings
            .app_location
            .is_none(),
        "a check saves nothing"
    );

    let saved = sw.set_app_location(Some(path.clone())).await.unwrap();
    assert_eq!(saved, Some(check));
    let settings = sw.profile_set().await.unwrap().settings;
    assert_eq!(settings.app_location, Some(path));
    // The install found there is the one to use now.
    assert_eq!(settings.preferred_app_id, None);

    assert_eq!(sw.set_app_location(None).await.unwrap(), None);
    assert_eq!(sw.profile_set().await.unwrap().settings.app_location, None);
}

#[tokio::test]
async fn a_location_without_an_app_is_refused_and_nothing_is_saved() {
    let w = World::new();
    let sw = w.switcher();
    let saves = *w.profiles.saves.lock().unwrap();
    for path in [abs(&w, "not-an-app"), abs(&w, "missing")] {
        match sw.set_app_location(Some(path.clone())).await {
            Err(Error::Invalid { message }) => assert!(
                message.contains(&superba_core::paths::display_path(&path)),
                "{message}"
            ),
            other => panic!("{other:?}"),
        }
    }
    assert!(matches!(
        sw.check_app_location(abs(&w, "missing")).await,
        Ok(AppLocationCheck::Missing)
    ));
    // Empty and relative paths are refused before anything is looked at.
    for path in [PathBuf::new(), PathBuf::from("ChatGPT")] {
        assert!(matches!(
            sw.check_app_location(path).await,
            Err(Error::Invalid { .. })
        ));
    }
    assert_eq!(*w.profiles.saves.lock().unwrap(), saves);
    assert_eq!(sw.profile_set().await.unwrap().settings.app_location, None);
}

#[tokio::test]
async fn saving_settings_with_a_new_location_drops_the_older_install_choice() {
    let w = World::new();
    let sw = w.switcher();
    let mut settings = Settings {
        preferred_app_id: Some("older".into()),
        ..Settings::default()
    };
    // Unchanged location: the choice stays.
    sw.save_settings(settings.clone()).await.unwrap();
    assert_eq!(
        sw.profile_set()
            .await
            .unwrap()
            .settings
            .preferred_app_id
            .as_deref(),
        Some("older")
    );
    settings.app_location = Some(installed(&w));
    sw.save_settings(settings).await.unwrap();
    let stored = sw.profile_set().await.unwrap().settings;
    assert_eq!(stored.app_location, Some(installed(&w)));
    assert_eq!(stored.preferred_app_id, None);
}

#[tokio::test]
async fn doctor_reports_an_unusable_location_and_a_missing_app() {
    let w = World::new();
    let sw = w.switcher();
    let ids = |findings: &[superba_core::DoctorFinding]| -> Vec<String> {
        findings.iter().map(|f| f.id.clone()).collect()
    };
    let findings = sw.doctor().await.unwrap();
    assert!(!ids(&findings).contains(&"app_location".to_owned()));
    assert!(!ids(&findings).contains(&"no_app_install".to_owned()));

    let bad = abs(&w, "not-an-app");
    w.profiles.set.lock().unwrap().settings.app_location = Some(bad.clone());
    w.app.installs.lock().unwrap().clear();
    let findings = sw.doctor().await.unwrap();
    let location = findings.iter().find(|f| f.id == "app_location").unwrap();
    assert_eq!(location.severity, Severity::Warning);
    assert_eq!(location.title.key, "switch.doctor.app_location");
    assert!(
        location
            .title
            .render_fallback()
            .contains(&superba_core::paths::display_path(&bad))
    );
    assert!(ids(&findings).contains(&"no_app_install".to_owned()));
}

/// `SUPERBA_APP_PATH` set to one version's folder of the Store app's package
/// (support told a user to): doctor says it is not needed and how to remove it.
#[tokio::test]
async fn doctor_explains_a_store_package_folder_in_superba_app_path() {
    let mut w = World::new();
    let given = "C:\\Program Files\\WindowsApps\\OpenAI.Codex_26.1103.4239.0_x64__2p2nqsd0c76g0\\app\\ChatGPT.exe";
    w.ctx.env = std::sync::Arc::new(
        superba_core::MapEnv::new()
            .with("SUPERBA_SECRET_BACKEND", "file")
            .with(superba_core::codex::ENV_SUPERBA_APP_PATH, given),
    );
    let sw = w.switcher();
    let findings = sw.doctor().await.unwrap();
    let finding = findings
        .iter()
        .find(|f| f.id == "app_path_store_package")
        .expect("a finding for the variable");
    assert_eq!(finding.severity, Severity::Warning);
    assert_eq!(finding.title.key, "switch.doctor.app_path_store_package");
    let text = finding.title.render_fallback();
    assert!(text.contains("SUPERBA_APP_PATH"), "{text}");
    assert!(text.contains("OpenAI.Codex_2p2nqsd0c76g0"), "{text}");
    assert!(text.contains("SetEnvironmentVariable"), "{text}");

    // A custom folder in the variable is no such case.
    let mut w = World::new();
    w.ctx.env = std::sync::Arc::new(
        superba_core::MapEnv::new()
            .with("SUPERBA_SECRET_BACKEND", "file")
            .with(
                superba_core::codex::ENV_SUPERBA_APP_PATH,
                "D:\\Apps\\ChatGPT",
            ),
    );
    let findings = w.switcher().doctor().await.unwrap();
    assert!(!findings.iter().any(|f| f.id == "app_path_store_package"));
}

fn catalog_block(plan: &superba_core::SwitchPlan) -> superba_core::LocalizedText {
    match &plan.primary {
        RestartKind::Blocked { reasons } => reasons
            .iter()
            .find_map(|r| match r {
                BlockReason::ModelCatalog { reason } => Some(reason.clone()),
                _ => None,
            })
            .expect("a model catalog block"),
        other => panic!("{other:?}"),
    }
}

/// A profile whose per-model settings need a catalog file (not optional).
fn needs_catalog(w: &World) {
    let mut set = w.profiles.set.lock().unwrap();
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    p.auth_mode = AuthMode::ApiKey;
    p.model_overrides.insert(
        "gpt-6-astra".into(),
        ModelOverride {
            context_window: Some(400_000),
            ..ModelOverride::default()
        },
    );
}

#[tokio::test]
async fn without_an_app_the_catalog_error_says_how_to_set_its_location() {
    let w = World::new();
    needs_catalog(&w);
    w.app.installs.lock().unwrap().clear();
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let reason = catalog_block(&plan);
    assert_eq!(reason.key, "switch.models.no_app");
    assert!(
        reason.render_fallback().contains("ChatGPT app location"),
        "{}",
        reason.render_fallback()
    );
}

#[tokio::test]
async fn an_app_without_its_codex_is_named_in_the_catalog_error() {
    let w = World::new();
    needs_catalog(&w);
    let path = superba_core::paths::display_path(&installed(&w));
    {
        let mut installs = w.app.installs.lock().unwrap();
        installs[0].bundled_codex = None;
        installs[0].bundled_codex_version = None;
    }
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let reason = catalog_block(&plan);
    assert_eq!(reason.key, "switch.models.no_consumer");
    assert!(
        reason.args.iter().any(|(k, v)| k == "path" && *v == path),
        "{reason:?}"
    );
}
