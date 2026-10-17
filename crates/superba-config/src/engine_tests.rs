//! Crash recovery and concurrent-change tests. They call the `*_sync` methods on
//! the test thread so the thread-local fault-injection hooks apply.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use superba_core::{
    AuthMode, CodexPath, DataDir, LineId, ManualClock, MapEnv, ModelDefaults, Origin, ProfileId,
    ResolvedEndpoints, Secret, SiteId,
};

use super::hooks::{BEFORE_PUBLISH, CRASH, Crash};
use super::*;
use crate::keystore::ENV_SUPERBA_SECRET_BACKEND;

const CONFIG: &str = "model = \"gpt-5.5\"\n\n[mcp_servers.fs]\ncommand = \"mcp-fs\"\n";
const AUTH: &str = "{\"OPENAI_API_KEY\": \"someone-elses-key-0042\"}";

struct T {
    _tmp: tempfile::TempDir,
    engine: TomlConfigEngine,
    root: PathBuf,
}

impl T {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = LocalContext::new(
            tmp.path().join("home"),
            DataDir::new(tmp.path().join("data")),
            Arc::new(MapEnv::new().with(ENV_SUPERBA_SECRET_BACKEND, "file")),
        );
        let root = ctx.codex_home().root().to_path_buf();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.toml"), CONFIG).unwrap();
        std::fs::write(root.join("auth.json"), AUTH).unwrap();
        let clock = Arc::new(ManualClock::new(Timestamp::from_millis(1_790_000_000_000)));
        Self {
            engine: TomlConfigEngine::with_clock(ctx, clock),
            _tmp: tmp,
            root,
        }
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.root.join("config.toml")).unwrap()
    }

    fn auth(&self) -> String {
        std::fs::read_to_string(self.root.join("auth.json")).unwrap()
    }

    fn plan(&self) -> ApplyPlan {
        let origin = Origin::parse("https://api.krill-code.net").unwrap();
        let desired = ResolvedProfile {
            model_catalog: None,
            profile_id: Some(ProfileId::new("p")),
            site_id: Some(SiteId::new("krill")),
            line_id: Some(LineId::new("overseas")),
            mode: ResolvedMode::Token {
                site_kind: SiteKind::Krill,
                auth: AuthMode::Pat,
                endpoints: ResolvedEndpoints::for_line(
                    &origin,
                    CodexPath::BackendApiCodex,
                    AuthMode::Pat,
                )
                .unwrap(),
                key: Secret::new("krill-unit-test-pat-0007"),
            },
            model: ModelDefaults::default(),
            key_resolutions: BTreeMap::new(),
            settings: superba_core::CodexSettings::krill_recommended(),
        };
        self.engine.plan_sync(&self.root, &desired).unwrap()
    }

    fn crash_apply(&self, at: Crash) -> ApplyPlan {
        let plan = self.plan();
        CRASH.with(|c| *c.borrow_mut() = Some(at));
        let res = self.engine.apply_sync(&plan);
        CRASH.with(|c| *c.borrow_mut() = None);
        assert!(res.is_err(), "the injected crash must abort apply");
        assert!(self.engine.pending(&self.root).unwrap());
        // Planning is blocked while a journal entry is pending.
        assert!(matches!(
            self.engine
                .plan_sync(&self.root, plan.desired.as_ref().unwrap()),
            Err(Error::Blocked { .. })
        ));
        plan
    }

    fn temps(&self) -> Vec<String> {
        std::fs::read_dir(&self.root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    fn generation(&self) -> Option<u64> {
        state::read_home(self.engine.data(), &self.root)
            .unwrap()
            .active()
            .map(|a| a.generation)
    }
}

#[test]
fn crash_before_staging_is_discarded() {
    let t = T::new();
    t.crash_apply(Crash::Pending);
    let r = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(r.entries.len(), 1);
    assert_eq!(r.entries[0].action, JournalAction::Discarded);
    assert_eq!(t.config(), CONFIG);
    assert_eq!(t.auth(), AUTH);
    assert!(t.temps().is_empty());
    assert!(!t.engine.pending(&t.root).unwrap());
    assert_eq!(t.generation(), None);
}

#[test]
fn crash_after_staging_is_discarded_and_temps_removed() {
    let t = T::new();
    t.crash_apply(Crash::Staged);
    assert_eq!(t.temps().len(), 2);
    let r = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(r.entries[0].action, JournalAction::Discarded);
    assert!(t.temps().is_empty());
    assert_eq!(t.config(), CONFIG);
    assert_eq!(t.auth(), AUTH);
}

#[test]
fn crash_mid_publish_rolls_forward() {
    let t = T::new();
    t.crash_apply(Crash::FirstPublish);
    // auth.json was published, config.toml not yet.
    assert!(t.auth().contains("personal_access_token"));
    assert_eq!(t.config(), CONFIG);
    let r = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(r.entries[0].action, JournalAction::Completed);
    assert!(t.config().contains("chatgpt_base_url"));
    assert!(t.temps().is_empty());
    assert_eq!(t.generation(), Some(1));
    // Ready for the next plan.
    assert!(
        t.engine
            .plan_sync(&t.root, t.plan().desired.as_ref().unwrap())
            .is_ok()
    );
}

#[test]
fn crash_mid_publish_without_temp_rolls_back() {
    let t = T::new();
    t.crash_apply(Crash::FirstPublish);
    for name in t.temps() {
        std::fs::remove_file(t.root.join(name)).unwrap();
    }
    let r = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(r.entries[0].action, JournalAction::RolledBack);
    assert_eq!(t.auth(), AUTH, "the pre-image is restored byte for byte");
    assert_eq!(t.config(), CONFIG);
    assert_eq!(t.generation(), None);
}

#[test]
fn crash_then_external_edit_is_drift_and_left_alone() {
    let t = T::new();
    t.crash_apply(Crash::FirstPublish);
    std::fs::write(t.root.join("config.toml"), "model = \"edited\"\n").unwrap();
    let r = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(r.entries[0].action, JournalAction::Drift);
    assert_eq!(t.config(), "model = \"edited\"\n");
    assert_eq!(
        t.auth(),
        AUTH,
        "undo published auth before abandoning the apply"
    );
    assert_eq!(t.generation(), None);
    assert!(t.temps().is_empty());
    assert!(!t.engine.pending(&t.root).unwrap());
}

#[test]
fn recovery_keeps_partial_publish_journal_until_undo_succeeds() {
    let t = T::new();
    t.crash_apply(Crash::FirstPublish);
    let external = format!("{CONFIG}\n[desktop]\ntheme = \"light\"\n");
    std::fs::write(t.root.join("config.toml"), &external).unwrap();
    let entries = journal::list(t.engine.data(), Some(&t.root)).unwrap();
    let backup = entries[0]
        .files
        .iter()
        .find(|file| file.kind == CodexFileKind::AuthJson)
        .unwrap()
        .backup
        .as_ref()
        .unwrap();
    let sealed = std::fs::read(backup).unwrap();
    std::fs::remove_file(backup).unwrap();
    assert!(matches!(
        t.engine.recover_sync(&t.root),
        Err(Error::NotFound { .. })
    ));
    assert!(t.engine.pending(&t.root).unwrap());
    assert_eq!(t.generation(), None);
    std::fs::write(backup, sealed).unwrap();
    let result = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(result.entries[0].action, JournalAction::Drift);
    assert_eq!(t.auth(), AUTH);
    assert_eq!(t.config(), external);
    assert!(!t.engine.pending(&t.root).unwrap());
    assert!(t.temps().is_empty());
}

#[test]
fn recovery_of_partial_rollback_with_external_config_edit_undoes_published_auth() {
    let t = T::new();
    let mut applied = t.engine.apply_sync(&t.plan()).unwrap();
    let managed_auth = t.auth();
    let managed_config = t.config();
    applied.files.reverse(); // Publish auth first during rollback.
    CRASH.with(|c| *c.borrow_mut() = Some(Crash::FirstPublish));
    let result = t.engine.rollback_sync(&applied);
    CRASH.with(|c| *c.borrow_mut() = None);
    assert!(result.is_err());
    assert_eq!(t.auth(), AUTH);
    let external = format!("{managed_config}\n[desktop]\ntheme = \"light\"\n");
    std::fs::write(t.root.join("config.toml"), &external).unwrap();
    let recovered = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(recovered.entries[0].action, JournalAction::Drift);
    assert_eq!(t.auth(), managed_auth);
    assert_eq!(t.config(), external);
    assert_eq!(t.generation(), Some(1));
    assert!(!t.engine.pending(&t.root).unwrap());
}

#[test]
fn recovery_does_not_undo_a_generation_committed_before_journal_cleanup() {
    let t = T::new();
    t.crash_apply(Crash::FirstPublish);
    let entry = journal::list(t.engine.data(), Some(&t.root))
        .unwrap()
        .remove(0);
    for file in &entry.files {
        if fsx::file_sha256(&file.path).unwrap() != file.after_sha256 {
            let bytes = t.engine.forward_contents(file).unwrap().unwrap().unwrap();
            fsx::write_atomic(&file.path, bytes.expose(), mode_for(file.kind)).unwrap();
        }
    }
    t.engine.commit(&entry).unwrap();
    let auth = t.auth();
    let external = format!("{}\n[desktop]\ntheme = \"light\"\n", t.config());
    std::fs::write(t.root.join("config.toml"), &external).unwrap();
    let recovered = t.engine.recover_sync(&t.root).unwrap();
    assert_eq!(recovered.entries[0].action, JournalAction::Drift);
    assert_eq!(t.auth(), auth);
    assert_eq!(t.config(), external);
    assert_eq!(t.generation(), Some(1));
    assert!(!t.engine.pending(&t.root).unwrap());
}

#[test]
fn concurrent_change_before_publish_is_rebased() {
    let t = T::new();
    let plan = t.plan();
    let cfg = t.root.join("config.toml");
    let mut fired = false;
    let target = cfg.clone();
    BEFORE_PUBLISH.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |p: &std::path::Path| {
            if !fired && p == target {
                fired = true;
                // The app writes its own section while Superba is switching.
                let mut s = std::fs::read_to_string(&target).unwrap();
                s.push_str("\n[desktop]\ntheme = \"light\"\n");
                std::fs::write(&target, s).unwrap();
            }
        }))
    });
    let res = t.engine.apply_sync(&plan);
    BEFORE_PUBLISH.with(|h| *h.borrow_mut() = None);
    let report = res.unwrap();
    assert_eq!(report.generation, 1);
    let out = t.config();
    assert!(out.contains("[desktop]\ntheme = \"light\""), "{out}");
    assert!(out.contains("chatgpt_base_url"), "{out}");
    assert!(t.auth().contains("personal_access_token"));
    assert!(t.temps().is_empty());
    assert!(!t.engine.pending(&t.root).unwrap());
}

#[test]
fn persistent_concurrent_change_gives_up_without_changes() {
    let t = T::new();
    let plan = t.plan();
    let auth = t.root.join("auth.json");
    let mut n = 0;
    let target = auth.clone();
    BEFORE_PUBLISH.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |p: &std::path::Path| {
            if p == target {
                n += 1;
                std::fs::write(
                    &target,
                    format!("{{\"OPENAI_API_KEY\": \"rotating-key-{n:04}\"}}"),
                )
                .unwrap();
            }
        }))
    });
    let res = t.engine.apply_sync(&plan);
    BEFORE_PUBLISH.with(|h| *h.borrow_mut() = None);
    assert!(matches!(res, Err(Error::Conflict { .. })), "{res:?}");
    assert_eq!(t.config(), CONFIG, "nothing was published");
    assert!(t.auth().contains("rotating-key-0003"));
    assert!(t.temps().is_empty());
    assert!(!t.engine.pending(&t.root).unwrap());
    assert_eq!(t.generation(), None);
}

#[test]
fn change_after_first_publish_undoes_and_retries() {
    let t = T::new();
    let plan = t.plan();
    let cfg = t.root.join("config.toml");
    let mut fired = false;
    let target = cfg.clone();
    BEFORE_PUBLISH.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |p: &std::path::Path| {
            if !fired && p == target {
                fired = true;
                std::fs::write(&target, format!("{CONFIG}# touched\n")).unwrap();
            }
        }))
    });
    let res = t.engine.apply_sync(&plan);
    BEFORE_PUBLISH.with(|h| *h.borrow_mut() = None);
    res.unwrap();
    assert!(t.config().contains("# touched"));
    assert!(t.config().contains("openai_base_url"));
    assert!(t.auth().contains("personal_access_token"));
    // The other tool's auth.json was stashed nowhere (not a ChatGPT login) but its
    // first-write pre-image exists for the original restore.
    let hs = state::read_home(t.engine.data(), &t.root).unwrap();
    assert!(hs.first_write.contains_key(&CodexFileKind::AuthJson));
}

const APP_CONFIG: &str =
    "\n[desktop]\nfollowUpQueueMode = \"steer\"\n\n[mcp_servers.node_repl]\ncommand = \"node\"\n";

#[test]
fn rollback_crash_mid_publish_recovers_from_encrypted_sources() {
    for auth_first in [false, true] {
        let t = T::new();
        let mut report = t.engine.apply_sync(&t.plan()).unwrap();
        let live_config = format!("{}{APP_CONFIG}", t.config());
        std::fs::write(t.root.join("config.toml"), &live_config).unwrap();
        // Exercise recovery of either file, including the computed TOML bytes
        // when config.toml has not yet been published and its temp is lost.
        if auth_first {
            report.files.reverse();
        }
        CRASH.with(|c| *c.borrow_mut() = Some(Crash::FirstPublish));
        let result = t.engine.rollback_sync(&report);
        CRASH.with(|c| *c.borrow_mut() = None);
        assert!(result.is_err());
        assert_eq!(t.generation(), Some(1));

        let entries = journal::list(t.engine.data(), Some(&t.root)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].phase, Phase::Publishing);
        assert_eq!(entries[0].commit, Commit::Rollback { generation: 1 });
        let config = entries[0]
            .files
            .iter()
            .find(|f| f.kind == CodexFileKind::ConfigToml)
            .unwrap();
        assert_eq!(
            config.before_sha256,
            Some(fsx::sha256_hex(live_config.as_bytes()))
        );
        let source = config.source.as_ref().unwrap();
        let sealed = std::fs::read(source).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("followUpQueueMode"));
        let restored = t.engine.vault.read_sealed(source).unwrap().unwrap();
        assert_eq!(
            config.after_sha256,
            Some(fsx::sha256_hex(restored.expose()))
        );
        if auth_first {
            assert_eq!(t.config(), live_config);
            assert_eq!(t.auth(), AUTH);
        } else {
            assert_eq!(t.config().as_bytes(), restored.expose());
            assert!(t.auth().contains("personal_access_token"));
        }
        for temp in t.temps() {
            std::fs::remove_file(t.root.join(temp)).unwrap();
        }

        let recovery = t.engine.recover_sync(&t.root).unwrap();
        assert_eq!(recovery.entries.len(), 1);
        assert_eq!(recovery.entries[0].action, JournalAction::Completed);
        assert_eq!(t.config().as_bytes(), restored.expose());
        assert!(t.config().contains(APP_CONFIG));
        let doc = ConfigDoc::parse(restored.expose()).unwrap();
        let before = ConfigDoc::parse(CONFIG.as_bytes()).unwrap();
        for path in render::all_owned_paths() {
            assert_eq!(doc.get_value(path), before.get_value(path), "{path}");
        }
        assert_eq!(t.auth(), AUTH);
        assert_eq!(t.generation(), None);
        assert!(t.temps().is_empty());
        assert!(!t.engine.pending(&t.root).unwrap());
        assert!(t.engine.recover_sync(&t.root).unwrap().entries.is_empty());
    }
}

#[test]
fn rollback_crash_before_publish_discards_journal_and_keeps_generation() {
    for at in [Crash::Pending, Crash::Staged] {
        let t = T::new();
        let report = t.engine.apply_sync(&t.plan()).unwrap();
        let config = format!("{}{APP_CONFIG}", t.config());
        std::fs::write(t.root.join("config.toml"), &config).unwrap();
        let auth = t.auth();
        CRASH.with(|c| *c.borrow_mut() = Some(at));
        let result = t.engine.rollback_sync(&report);
        CRASH.with(|c| *c.borrow_mut() = None);
        assert!(result.is_err());
        assert!(t.engine.pending(&t.root).unwrap());

        let recovery = t.engine.recover_sync(&t.root).unwrap();
        assert_eq!(recovery.entries[0].action, JournalAction::Discarded);
        assert_eq!(t.config(), config);
        assert_eq!(t.auth(), auth);
        assert_eq!(t.generation(), Some(1));
        assert!(t.temps().is_empty());
        assert!(!t.engine.pending(&t.root).unwrap());
    }
}

#[test]
fn rollback_publish_race_stops_and_reports_which_files_were_restored() {
    for kind in [CodexFileKind::ConfigToml, CodexFileKind::AuthJson] {
        let t = T::new();
        let report = t.engine.apply_sync(&t.plan()).unwrap();
        let config = format!("{}{APP_CONFIG}", t.config());
        std::fs::write(t.root.join("config.toml"), &config).unwrap();
        let auth = t.auth();
        let target = t.root.join(file_name(kind));
        let edited = if kind == CodexFileKind::ConfigToml {
            format!("{config}# another app write\n")
        } else {
            "{\"OPENAI_API_KEY\": \"concurrent-auth-edit\"}".to_owned()
        };
        let edited_for_hook = edited.clone();
        BEFORE_PUBLISH.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |path| {
                if path == target {
                    std::fs::write(path, &edited_for_hook).unwrap();
                }
            }));
        });
        let result = t.engine.rollback_sync(&report);
        BEFORE_PUBLISH.with(|h| *h.borrow_mut() = None);
        let Err(Error::Conflict { message }) = result else {
            panic!("expected a rollback conflict, got {result:?}");
        };
        assert!(
            message.contains("changed since the switch, left as is"),
            "{message}"
        );
        assert!(message.contains("state generation unchanged"), "{message}");
        if kind == CodexFileKind::ConfigToml {
            assert!(message.contains("already restored: none"), "{message}");
            assert_eq!(t.config(), edited);
            assert_eq!(t.auth(), auth);
        } else {
            assert!(
                message.contains(&format!(
                    "already restored: {}",
                    t.root.join("config.toml").display()
                )),
                "{message}"
            );
            assert!(!t.config().contains("chatgpt_base_url"));
            assert!(t.config().contains(APP_CONFIG));
            assert_eq!(t.auth(), edited);
        }
        assert_eq!(t.generation(), Some(1));
        assert!(t.temps().is_empty());
        assert!(!t.engine.pending(&t.root).unwrap());
    }
}
