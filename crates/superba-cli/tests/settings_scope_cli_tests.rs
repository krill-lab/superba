//! `spb settings` without a profile is this computer's config.toml, like the
//! settings page; `--profile` shows or edits one profile's own settings, and
//! built-in profiles say why they have none (user bug 72).

mod common;

use std::sync::Arc;

use clap::CommandFactory;
use common::*;
use superba_cli::Cli;
use superba_core::{
    BuiltinProfile, LocalizedText, Notice, ProfileId, SettingsScope, catalog::builtin_profiles,
};

fn with_builtins(service: &Arc<FakeService>) {
    service
        .profiles
        .lock()
        .unwrap()
        .profiles
        .extend(builtin_profiles());
}

fn apply(service: &Arc<FakeService>, id: Option<&str>) {
    let mut o = service.overview.lock().unwrap();
    match (id, o.active.as_mut()) {
        (Some(id), Some(a)) => a.profile_id = Some(ProfileId::new(id)),
        (None, _) => o.active = None,
        (Some(_), None) => panic!("the fake overview has an active state"),
    }
}

fn note(key: &str, english: &str) -> Notice {
    Notice::info(LocalizedText::new(key, english))
}

#[tokio::test]
async fn list_without_a_profile_is_this_computers_config_toml() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(service.clone(), &["settings", "list"]).await;
    assert_eq!(code, 0, "{stderr}");
    for expected in [
        "Codex settings on this computer: /home/u/.codex/config.toml",
        "applied profile work",
        // Setting | Current | Profile | Default | Stage
        "features.fast_mode | false | true | false | Stable",
        "settings list --advanced",
    ] {
        assert!(stdout.contains(expected), "{expected:?} in\n{stdout}");
    }
    assert_eq!(service.calls().settings_views[0].profile_id, None);

    // The official login applied: no profile column, the reason as a note.
    {
        let mut v2 = service.v2.lock().unwrap();
        v2.view.profile_id = None;
        v2.view.rows[0].managed = None;
        v2.view.notes = vec![note(
            "switch.settings.computer_official",
            "The official login is applied on this computer: Superba writes no Codex settings for it, so these are config.toml's own values.",
        )];
    }
    let (code, stdout, stderr) = invoke(service.clone(), &["settings", "list"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("features.fast_mode | false | false | Stable"),
        "{stdout}"
    );
    assert!(!stdout.contains("applied profile"), "{stdout}");
    assert!(
        format!("{stdout}{stderr}").contains("official login is applied"),
        "{stdout}{stderr}"
    );
}

#[tokio::test]
async fn list_with_a_profile_shows_its_own_settings_and_built_ins_explain_themselves() {
    let service = FakeService::new();
    {
        let mut v2 = service.v2.lock().unwrap();
        v2.view.scope = SettingsScope::Profile;
    }
    let (code, stdout, _) =
        invoke(service.clone(), &["settings", "list", "--profile", "work"]).await;
    assert_eq!(code, 0);
    for expected in [
        "Profile: work",
        "Only the settings profile work keeps are listed",
        // Setting | Profile | Current | Default | Stage
        "features.fast_mode | true | false | false | Stable",
    ] {
        assert!(stdout.contains(expected), "{expected:?} in\n{stdout}");
    }
    assert!(!stdout.contains("--advanced"), "{stdout}");
    assert_eq!(
        service.calls().settings_views[0].profile_id,
        Some(ProfileId::new("work"))
    );

    // A profile without settings of its own says how to add one.
    service.v2.lock().unwrap().view.rows.clear();
    let (_, stdout, _) = invoke(service.clone(), &["settings", "list", "--profile", "work"]).await;
    assert!(
        stdout.contains("keeps no Codex settings of its own") && stdout.contains("--profile work"),
        "{stdout}"
    );

    // Built in: exit 0 with the explanation, not an error.
    {
        let mut v2 = service.v2.lock().unwrap();
        v2.view.profile_id = Some(ProfileId::new("official"));
        v2.view.builtin = Some(BuiltinProfile::Official);
        v2.view.notes = vec![
            note(
                "switch.settings.builtin_official",
                "{profile} is built into Superba: the official ChatGPT login.",
            )
            .clone(),
        ];
    }
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["settings", "list", "--profile", "official"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let all = format!("{stdout}{stderr}");
    assert!(all.contains("built into Superba"), "{all}");
    assert!(!all.contains("keeps no Codex settings"), "{all}");
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["settings", "get", "model", "--profile", "official"],
    )
    .await;
    assert_ne!(code, 0, "an unknown key is still an error");
    assert!(
        format!("{stdout}{stderr}").contains("built into Superba"),
        "the reason comes first: {stdout}{stderr}"
    );
}

#[tokio::test]
async fn edits_without_a_profile_go_to_the_applied_one_or_explain_why_they_cannot() {
    let service = FakeService::new();
    with_builtins(&service);
    // `work` is applied: stored there, without asking which one.
    let (code, _, stderr) = invoke(service.clone(), &["settings", "set", "x.y", "1"]).await;
    assert_eq!(code, 0, "{stderr}");
    let edits = service.calls().settings_edits;
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].profile_id, Some(ProfileId::new("work")));

    // The official login applied: nothing is stored, the way out is named.
    apply(&service, Some("official"));
    let (code, stdout, stderr) = invoke(service.clone(), &["settings", "set", "x.y", "1"]).await;
    assert_eq!(code, 2, "{stdout}{stderr}");
    for expected in [
        "official login is applied on this computer",
        "--profile <ID> (profiles: work)",
    ] {
        assert!(stderr.contains(expected), "{expected:?} in\n{stderr}");
    }
    assert_eq!(service.calls().settings_edits.len(), 1, "nothing stored");
    // Named explicitly, the key-based profile takes it.
    let (code, _, stderr) = invoke(
        service.clone(),
        &["settings", "reset", "x.y", "--profile", "work"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    // A built-in profile is refused with what it is.
    let (code, _, stderr) = invoke(
        service.clone(),
        &["settings", "set", "x.y", "1", "--profile", "official"],
    )
    .await;
    assert_eq!(code, 2);
    assert!(stderr.contains("official ChatGPT login"), "{stderr}");
    // Nothing applied and no key-based profile: add one first.
    apply(&service, None);
    service
        .profiles
        .lock()
        .unwrap()
        .profiles
        .retain(|p| p.is_builtin());
    let (code, _, stderr) = invoke(service.clone(), &["settings", "forget", "x.y"]).await;
    assert_eq!(code, 2);
    assert!(
        stderr.contains("No provider is applied") && stderr.contains("spb profile add <ID>"),
        "{stderr}"
    );
    // In Chinese too.
    let (_, _, stderr) = invoke(
        service.clone(),
        &["--locale", "zh-CN", "settings", "set", "x.y", "1"],
    )
    .await;
    assert!(stderr.contains("没有应用任何供应商"), "{stderr}");
}

#[test]
fn the_help_says_what_the_commands_work_on() {
    let mut cmd = Cli::command();
    let settings = cmd.find_subcommand_mut("settings").unwrap();
    let about = settings.get_about().unwrap().to_string();
    assert!(about.contains("this computer's config.toml"), "{about}");
    let list = settings.find_subcommand_mut("list").unwrap();
    let profile = list
        .get_arguments()
        .find(|a| a.get_id() == "profile")
        .unwrap()
        .get_help()
        .unwrap()
        .to_string();
    assert!(profile.contains("built-in"), "{profile}");
    let set = settings
        .find_subcommand("set")
        .unwrap()
        .get_about()
        .unwrap()
        .to_string();
    assert!(set.contains("applied on this computer"), "{set}");
}
