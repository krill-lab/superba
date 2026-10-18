//! Notifications through the switch service (GH#12): channels and their
//! secrets in the key store, saving preferences never touching channels,
//! hooks that report nothing while notifications are off, and the Codex hook
//! written into config.toml (with the editor's backup).

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ChannelKind, HookRequest, HookState, KeyRef, NotifyChannel, NotifySecrets, NotifySkip, Secret,
    SwitchService,
};

fn telegram(id: &str) -> NotifyChannel {
    let mut c = NotifyChannel::new(id, ChannelKind::Telegram, "phone");
    c.params.insert("chat_id".into(), "4242".into());
    c
}

fn secrets(pairs: &[(&str, &str)]) -> NotifySecrets {
    let mut s = NotifySecrets::default();
    for (k, v) in pairs {
        s.set(*k, Secret::new(*v));
    }
    s
}

#[tokio::test]
async fn channel_secrets_live_in_the_key_store_and_edits_keep_them() {
    let w = World::new();
    let sw = w.switcher();
    // Missing token: refused before anything is stored.
    let err = sw
        .notify_save_channel(telegram(""), NotifySecrets::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Bot token"), "{err}");
    let saved = sw
        .notify_save_channel(telegram(""), secrets(&[("bot_token", "123:SECRET-TOKEN")]))
        .await
        .unwrap();
    assert!(saved.id.starts_with("telegram-"), "{}", saved.id);
    assert_eq!(saved.secret_hints["bot_token"], "••••OKEN");
    let Some(KeyRef::Keyring { account, .. }) = &saved.key_ref else {
        panic!("{:?}", saved.key_ref);
    };
    assert_eq!(account, &format!("profile:notify-{}", saved.id));
    let stored = w.secrets.map.lock().unwrap().get(account).cloned().unwrap();
    assert_eq!(stored, r#"{"bot_token":"123:SECRET-TOKEN"}"#);
    // Nothing secret in the preferences.
    let set = sw.profile_set().await.unwrap();
    assert!(
        !serde_json::to_string(&set)
            .unwrap()
            .contains("SECRET-TOKEN")
    );

    // An edit without the token keeps it; a non-field param is dropped.
    let mut edit = saved.clone();
    edit.name = "my phone".into();
    edit.params.insert("bogus".into(), "x".into());
    let edited = sw
        .notify_save_channel(edit, NotifySecrets::default())
        .await
        .unwrap();
    assert_eq!(edited.name, "my phone");
    assert!(!edited.params.contains_key("bogus"));
    let stored = w.secrets.map.lock().unwrap().get(account).cloned().unwrap();
    assert!(stored.contains("SECRET-TOKEN"));
    // The type of a channel is fixed.
    let mut other = edited.clone();
    other.kind = ChannelKind::Bark;
    assert!(
        sw.notify_save_channel(other, NotifySecrets::default())
            .await
            .is_err()
    );

    // Saving a stale copy of the preferences keeps the channel.
    let mut stale = sw.profile_set().await.unwrap().settings;
    stale.notify.channels.clear();
    stale.notify.enabled = true;
    sw.save_settings(stale).await.unwrap();
    let n = sw.profile_set().await.unwrap().settings.notify;
    assert!(n.enabled);
    assert_eq!(n.channels.len(), 1);

    // Deleting removes the secret too.
    sw.notify_delete_channel(&edited.id).await.unwrap();
    assert!(w.secrets.map.lock().unwrap().get(account).is_none());
    assert!(
        sw.profile_set()
            .await
            .unwrap()
            .settings
            .notify
            .channels
            .is_empty()
    );
    assert!(sw.notify_delete_channel(&edited.id).await.is_err());
}

#[tokio::test]
async fn hooks_report_nothing_while_notifications_are_off() {
    let w = World::new();
    let sw = w.switcher();
    let stop = r#"{"session_id":"s","hook_event_name":"Stop","cwd":"/w"}"#;
    let r = sw.notify_hook("claude-code", stop).await.unwrap();
    assert_eq!(r.skipped, Some(NotifySkip::Disabled));
    // Even broken data is no error while off (the hook path must stay cheap).
    assert_eq!(
        sw.notify_hook("codex", "not json").await.unwrap().skipped,
        Some(NotifySkip::Disabled)
    );
}

#[tokio::test]
async fn the_codex_hook_is_written_into_config_toml_with_a_backup() {
    let mut w = World::new();
    // The program hooks run (default: `spb` next to the running binary).
    w.ctx.env = std::sync::Arc::new(
        superba_core::MapEnv::new().with("SUPERBA_NOTIFY_PROGRAM", "/opt/superba/spb"),
    );
    let home = w.ctx.codex_home();
    std::fs::create_dir_all(home.root()).unwrap();
    std::fs::write(home.config_toml(), "model = \"gpt-6\"\n").unwrap();
    let sw = w.switcher();
    let hooks = sw
        .notify_set_hook(HookRequest {
            harness: "codex".into(),
            mechanism: "notify".into(),
            install: true,
        })
        .await
        .unwrap();
    let notify = hooks
        .hooks
        .iter()
        .find(|h| h.mechanism == "notify")
        .unwrap();
    assert_eq!(notify.state, HookState::Installed);
    let text = std::fs::read_to_string(home.config_toml()).unwrap();
    assert!(
        text.contains("notify = [\"/opt/superba/spb\", \"notify\", \"codex\"]"),
        "{text}"
    );
    assert!(text.starts_with("model = \"gpt-6\"\n"), "{text}");
    sw.notify_set_hook(HookRequest {
        harness: "codex".into(),
        mechanism: "notify".into(),
        install: false,
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(home.config_toml()).unwrap(),
        "model = \"gpt-6\"\n"
    );
    // Each save kept the replaced file in the editor's backups.
    let backups = sw
        .codex_file_backups(superba_core::CodexFileBackupQuery {
            target: Some(superba_core::TargetId::local()),
            kind: Some(superba_core::CodexFileKind::ConfigToml),
        })
        .await
        .unwrap();
    assert!(backups.len() >= 2, "{backups:?}");
    // Claude Code's hooks belong to its own settings page.
    assert!(
        sw.notify_set_hook(HookRequest {
            harness: "claude-code".into(),
            mechanism: "settings.json hooks".into(),
            install: true,
        })
        .await
        .is_err()
    );
}
