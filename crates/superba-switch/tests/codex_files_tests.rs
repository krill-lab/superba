//! The file editor through the service (GitHub issue 9), with the real config
//! engine: open, save with an encrypted backup, the compare-and-swap refusal,
//! syntax errors never written, backups listed with switch pre-images and
//! originals, restore, ownership (a saved change to Superba's keys is the
//! user's: named as the editor's, never reverted, the next switch asks), "in
//! effect" after a save, and no key in any log line.

#![allow(missing_docs)]

mod fakes;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use fakes::*;
use superba_core::{
    BackupSource, ChangeSource, CodexFileBackupQuery, CodexFileKind, CodexFileSave, ExecuteOptions,
    NoEvents, OutcomeStatus, PlanChoice, ProblemKind, ProfileId, SafeDefaults, SaveReason,
    SecretBytes, SwitchPlan, SwitchRequest, SwitchService, TargetId,
};
use superba_switch::{Switcher, Tuning};

// ------------------------------------------------------------- log capture

static LOG: Mutex<String> = Mutex::new(String::new());
static INSTALL: Once = Once::new();

/// Records every event and span field of every level, in all threads.
struct Capture;

struct Fields<'a>(&'a mut String);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={value:?}", field.name());
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut log = LOG.lock().unwrap();
        attrs.record(&mut Fields(&mut log));
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut log = LOG.lock().unwrap();
        values.record(&mut Fields(&mut log));
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut log = LOG.lock().unwrap();
        log.push('\n');
        event.record(&mut Fields(&mut log));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn capture_logs() {
    INSTALL.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Capture);
    });
}

// ------------------------------------------------------------------ setup

fn switcher(w: &World) -> Switcher {
    let mut deps = w.deps();
    deps.config = Arc::new(superba_config::TomlConfigEngine::with_clock(
        w.ctx.clone(),
        w.clock.clone(),
    ));
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

fn read(w: &World, file: &str) -> String {
    std::fs::read_to_string(path(w, file)).unwrap_or_default()
}

/// The user's Codex with comments, then Superba's Krill switch ("work", PAT).
async fn on_krill() -> (World, Switcher) {
    let w = World::new();
    std::fs::create_dir_all(w.ctx.codex_home().root()).unwrap();
    std::fs::write(
        path(&w, "config.toml"),
        "# my settings\nmodel = \"gpt-5.5\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\n",
    )
    .unwrap();
    let sw = switcher(&w);
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(plan.choices.is_empty() && !plan.is_blocked(), "{plan:?}");
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    (w, sw)
}

fn save(doc: &superba_core::CodexFileDoc, text: &str) -> CodexFileSave {
    CodexFileSave {
        target: doc.target.clone(),
        kind: doc.kind,
        contents: SecretBytes::new(text.as_bytes().to_vec()),
        base_sha256: doc.sha256.clone(),
        reason: SaveReason::Edit,
        force: false,
    }
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn an_edit_is_saved_after_a_backup_and_stays_the_users() {
    let (w, sw) = on_krill().await;
    let doc = sw
        .codex_file(TargetId::local(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    assert!(doc.read_only.is_none());
    assert!(
        doc.owned.iter().any(|k| k.path == "openai_base_url"),
        "{doc:?}"
    );
    assert_eq!(doc.profile, Some(ProfileId::new("work")));
    assert!(
        !doc.schema.consumers.is_empty(),
        "the app's and the CLI's Codex"
    );
    let before = doc.text().unwrap();
    assert!(before.contains("# my settings"));

    // A minute later the user points Codex elsewhere by hand.
    w.clock.advance(Duration::from_secs(60));
    let edited = before.replace(OVERSEAS, "https://relay.example");
    assert_ne!(edited, before);
    let saved = sw.save_codex_file(save(&doc, &edited)).await.unwrap();
    assert_eq!(read(&w, "config.toml"), edited);
    let backup = saved.backup.clone().expect("backed up");
    assert_eq!(backup.source, BackupSource::BeforeEdit);
    assert_eq!(saved.owned_changed.len(), 2, "{saved:?}");
    assert!(
        saved
            .owned_changed
            .iter()
            .any(|k| k.path == "openai_base_url")
    );
    assert!(
        saved
            .notes
            .iter()
            .any(|n| n.text.key == "codex_files.owned_promise")
    );
    // "In effect": the app's Codex core started before the save.
    let runtime = saved.runtime.as_ref().expect("local runtime view");
    assert_eq!(
        runtime.edited.map(|e| e.kind),
        Some(CodexFileKind::ConfigToml)
    );
    assert!(runtime.previous_count() > 0, "{runtime:?}");

    // The background check names the editor and offers no "other tool".
    let x = sw.overview().await.unwrap().external.expect("flagged");
    assert!(
        matches!(x.source, ChangeSource::SuperbaEditor { .. }),
        "{x:?}"
    );
    // The next switch asks instead of silently reverting it.
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let [PlanChoice::OwnedKeyConflict { source, .. }] = plan.choices.as_slice() else {
        panic!("{:?}", plan.choices);
    };
    assert!(matches!(source, ChangeSource::SuperbaEditor { .. }));
    assert_eq!(read(&w, "config.toml"), edited);

    // The backup is listed with the switch's pre-image and the original, and
    // restoring it puts the file back (after a backup of its own).
    let list = sw
        .codex_file_backups(CodexFileBackupQuery {
            target: Some(TargetId::local()),
            kind: Some(CodexFileKind::ConfigToml),
        })
        .await
        .unwrap();
    let ids: Vec<&str> = list.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids[0], backup.id, "newest first: {ids:?}");
    assert!(ids.iter().any(|i| i.starts_with("switch-g")), "{ids:?}");
    assert!(ids.contains(&"original-config"), "{ids:?}");
    let b = sw.codex_file_backup(&backup.id).await.unwrap();
    assert_eq!(b.contents.expose(), before.as_bytes());
    let now = sw
        .codex_file(TargetId::local(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    let mut restore = save(&now, &before);
    restore.reason = SaveReason::Restore {
        backup: backup.id.clone(),
    };
    let restored = sw.save_codex_file(restore).await.unwrap();
    assert_eq!(read(&w, "config.toml"), before);
    assert_eq!(
        restored.backup.unwrap().source,
        BackupSource::BeforeRestore {
            restored: backup.id.clone()
        }
    );
    // Back to what Superba wrote: nothing is flagged any more.
    assert!(sw.overview().await.unwrap().external.is_none());
}

#[tokio::test]
async fn stale_broken_and_unchanged_saves_write_nothing() {
    let (w, sw) = on_krill().await;
    let doc = sw
        .codex_file(TargetId::local(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    let before = read(&w, "config.toml");

    // Another program wrote the file after it was opened.
    std::fs::write(path(&w, "config.toml"), format!("{before}# theirs\n")).unwrap();
    let err = sw
        .save_codex_file(save(&doc, &format!("{before}# mine\n")))
        .await
        .unwrap_err();
    assert!(matches!(err, superba_core::Error::Conflict { .. }), "{err}");
    assert!(read(&w, "config.toml").ends_with("# theirs\n"));

    // A syntax error is refused with its position; nothing is written or backed up.
    let doc = sw
        .codex_file(TargetId::local(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    let err = sw
        .save_codex_file(save(&doc, "model = \"x\"\nbroken = \n"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    let backups = sw
        .codex_file_backups(CodexFileBackupQuery::default())
        .await
        .unwrap();
    assert!(backups.iter().all(|b| b.source != BackupSource::BeforeEdit));

    // Unchanged: no write, no backup.
    let same = sw
        .save_codex_file(save(&doc, &doc.text().unwrap()))
        .await
        .unwrap();
    assert!(same.backup.is_none());
}

#[tokio::test]
async fn wrong_types_need_save_anyway() {
    let (w, sw) = on_krill().await;
    let catalog = Arc::new(superba_core::StaticCatalog::new([
        superba_core::CatalogSnapshot::new(superba_core::CodexVersion::new(0, 162, 0)).with_key(
            superba_core::CatalogKey::new("model", superba_core::SettingKind::String),
        ),
    ]));
    let mut deps = w.deps();
    deps.config = Arc::new(superba_config::TomlConfigEngine::with_clock(
        w.ctx.clone(),
        w.clock.clone(),
    ));
    deps.catalog = catalog;
    let sw2 = Switcher::new(deps);
    drop(sw);
    let doc = sw2
        .codex_file(TargetId::local(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    let text = doc
        .text()
        .unwrap()
        .replace("model = \"gpt-5.5\"", "model = 5");
    let problems = superba_switch::codex_files::validate(
        CodexFileKind::ConfigToml,
        &text,
        &superba_switch::codex_files::CheckContext {
            schema: Some(&doc.schema),
            ..Default::default()
        },
    );
    assert!(
        problems
            .iter()
            .any(|p| p.kind == ProblemKind::Schema && p.needs_force()),
        "{problems:?}"
    );
    let err = sw2.save_codex_file(save(&doc, &text)).await.unwrap_err();
    assert!(err.to_string().contains("save anyway"), "{err}");
    let mut forced = save(&doc, &text);
    forced.force = true;
    sw2.save_codex_file(forced).await.unwrap();
    assert!(read(&w, "config.toml").contains("model = 5"));
}

#[tokio::test]
async fn a_new_credential_is_saved_flagged_and_never_logged() {
    capture_logs();
    let (w, sw) = on_krill().await;
    let doc = sw
        .codex_file(TargetId::local(), CodexFileKind::AuthJson)
        .await
        .unwrap();
    assert!(doc.owns_credential);
    assert!(!format!("{doc:?}").contains(KEY));
    let new_key = "sk-hand-typed-00000000000000zz";
    let edited = doc.text().unwrap().replace(KEY, new_key);
    let mut s = save(&doc, &edited);
    assert!(!format!("{s:?}").contains(new_key));
    s.reason = SaveReason::Edit;
    let saved = sw.save_codex_file(s).await.unwrap();
    assert!(saved.credential.is_some(), "{saved:?}");
    assert_eq!(read(&w, "auth.json"), edited);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path(&w, "auth.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let backup = sw
        .codex_file_backup(&saved.backup.as_ref().unwrap().id)
        .await
        .unwrap();
    assert!(!format!("{backup:?}").contains(KEY));
    // The overview flags it as the user's own edit.
    let x = sw.overview().await.unwrap().external.expect("flagged");
    assert!(x.auth.is_some());
    assert!(matches!(x.source, ChangeSource::SuperbaEditor { .. }));
    // No key reached a log line, a serialized result or the backup index.
    let log = LOG.lock().unwrap().clone();
    assert!(log.contains("saved an edited Codex file"), "{log}");
    for secret in [KEY, new_key] {
        assert!(!log.contains(secret), "a key was logged");
        assert!(!serde_json::to_string(&saved).unwrap().contains(secret));
    }
    let index =
        std::fs::read_to_string(w.ctx.data_dir.editor_backups_dir().join("index.json")).unwrap();
    assert!(!index.contains(KEY) && !index.contains(new_key));
}

#[tokio::test]
async fn a_remote_host_is_edited_through_its_own_codex_and_backed_up_here() {
    let (w, sw) = on_krill().await;
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("devbox", Some("remote-ssh-discovered:devbox")));
    *w.remote.config_toml.lock().unwrap() = Some("# host\nmodel = \"gpt-5\"\n".into());
    let target = TargetId::remote("devbox");
    let doc = sw
        .codex_file(target.clone(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    assert_eq!(doc.path, "/home/u/.codex/config.toml");
    assert_eq!(doc.target_label, "devbox");
    assert!(matches!(
        doc.schema.consumers[0].consumer,
        superba_core::ConsumerKind::Remote { .. }
    ));
    let saved = sw
        .save_codex_file(save(&doc, "# host\nmodel = \"gpt-6\"\n"))
        .await
        .unwrap();
    assert_eq!(
        w.remote.config_toml.lock().unwrap().as_deref(),
        Some("# host\nmodel = \"gpt-6\"\n")
    );
    assert!(saved.runtime.is_none());
    assert!(
        saved
            .notes
            .iter()
            .any(|n| n.text.key == "switch.codex_files.remote_restart")
    );
    // The replaced file is backed up on this computer, listed for that host.
    let list = sw
        .codex_file_backups(CodexFileBackupQuery {
            target: Some(target.clone()),
            kind: None,
        })
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].target_label, "devbox");
    let b = sw.codex_file_backup(&list[0].id).await.unwrap();
    assert_eq!(b.contents.expose(), b"# host\nmodel = \"gpt-5\"\n");
    // A stale editor view is refused on the host too.
    let err = sw
        .save_codex_file(save(&doc, "model = \"x\"\n"))
        .await
        .unwrap_err();
    assert!(matches!(err, superba_core::Error::Conflict { .. }), "{err}");
}
