//! The read side and housekeeping of `Switcher`: profiles, settings, sites, key
//! checks, opening the app, overview, doctor and start-up recovery.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ActiveState, ApplyReport, AuthMode, BlockReason, CodexFileKind, CodexProcessKind, EnvApplied,
    EnvObservation, FileWrite, HotSwitchStatus, JournalAction, KeyInput, KeyRef, Line, LineId,
    LineSource, LocalizedText, MapEnv, Origin, Profile, ProfileId, Secret, Settings, Site, SiteId,
    SiteKind, SwitchService, Timestamp, catalog,
};
use superba_switch::journal::{JournalEntry, JournalStatus, StateLock, SwitchJournal};

fn compatible_site(id: &str, origin: &str) -> Site {
    Site {
        id: SiteId::new(id),
        kind: SiteKind::Compatible,
        name: LocalizedText::new("raw", id),
        homepage: None,
        lines: vec![Line {
            id: LineId::new("main"),
            name: LocalizedText::new("raw", "Main"),
            origin: Origin::parse(origin).unwrap(),
            source: LineSource::Builtin,
        }],
        capabilities: catalog::compatible_capabilities(),
        endpoints: Default::default(),
        default_line: Some(LineId::new("main")),
        model_map: Default::default(),
    }
}

// --- profiles ------------------------------------------------------------------

#[tokio::test]
async fn saving_a_profile_with_a_key_stores_the_key_and_a_reference() {
    let w = World::new();
    let sw = w.switcher();
    let mut p = krill_profile("home", None, AuthMode::ApiKey);
    p.key_ref = None;
    let saved = sw
        .save_profile(p, Some(Secret::new("sk-test-2222bbbb3333")))
        .await
        .unwrap();
    assert_eq!(
        saved.key_ref,
        Some(KeyRef::Keyring {
            service: "superba".into(),
            account: "profile:home".into()
        })
    );
    assert_eq!(
        w.secrets
            .map
            .lock()
            .unwrap()
            .get("profile:home")
            .map(String::as_str),
        Some("sk-test-2222bbbb3333")
    );
    let set = sw.profile_set().await.unwrap();
    assert!(set.profile(&ProfileId::new("home")).is_some());
    // Saving again replaces in place.
    let mut p2 = saved.clone();
    p2.name = "Home".into();
    sw.save_profile(p2, None).await.unwrap();
    let set = sw.profile_set().await.unwrap();
    assert_eq!(set.profiles.iter().filter(|p| p.id == "home").count(), 1);
    assert_eq!(set.profile(&ProfileId::new("home")).unwrap().name, "Home");
}

#[tokio::test]
async fn env_key_references_are_stored_without_a_secret() {
    let w = World::new();
    let mut p = krill_profile("ci", Some("cdn"), AuthMode::Pat);
    p.key_ref = Some(KeyRef::Env {
        var: "KRILL_KEY".into(),
    });
    let saved = w.switcher().save_profile(p, None).await.unwrap();
    assert!(matches!(saved.key_ref, Some(KeyRef::Env { .. })));
    assert!(!w.secrets.map.lock().unwrap().contains_key("profile:ci"));
}

#[tokio::test]
async fn invalid_profiles_are_refused() {
    let w = World::new();
    let sw = w.switcher();
    for (p, code) in [
        (krill_profile("Bad Id", None, AuthMode::Pat), "invalid"),
        (krill_profile("original", None, AuthMode::Pat), "invalid"),
        (krill_profile("x", Some("mars"), AuthMode::Pat), "not_found"),
        (
            Profile {
                site_id: SiteId::new("nowhere"),
                ..krill_profile("y", None, AuthMode::Pat)
            },
            "not_found",
        ),
        (
            Profile {
                builtin: Some(superba_core::BuiltinProfile::Official),
                ..krill_profile("z", None, AuthMode::Pat)
            },
            "invalid",
        ),
    ] {
        let id = p.id.clone();
        let err = sw.save_profile(p, None).await.unwrap_err();
        assert_eq!(err.code(), code, "{id}: {err}");
    }
    // A compatible site offers API keys only.
    sw.save_site(compatible_site("acme", "https://api.acme.example"))
        .await
        .unwrap();
    let mut p = Profile {
        site_id: SiteId::new("acme"),
        ..krill_profile("acme-pat", None, AuthMode::Pat)
    };
    // A manual PAT choice is refused; Auto would resolve to an API key there.
    p.set_auth(superba_core::AuthChoice::Pat);
    assert_eq!(
        sw.save_profile(p.clone(), None).await.unwrap_err().code(),
        "invalid"
    );
    p.set_auth(superba_core::AuthChoice::Auto);
    sw.save_profile(p, None).await.unwrap();
    assert_eq!(
        sw.save_profile(
            krill_profile("e", None, AuthMode::Pat),
            Some(Secret::new("  "))
        )
        .await
        .unwrap_err()
        .code(),
        "invalid"
    );
}

#[tokio::test]
async fn deleting_a_profile_removes_its_key_and_selection() {
    let w = World::new();
    let sw = w.switcher();
    w.profiles.set.lock().unwrap().last_selected = Some(ProfileId::new("work"));
    sw.delete_profile(&ProfileId::new("work")).await.unwrap();
    let set = sw.profile_set().await.unwrap();
    assert!(set.profile(&ProfileId::new("work")).is_none());
    assert_eq!(set.last_selected, None);
    assert_eq!(w.secrets.deleted.lock().unwrap().len(), 1);
    assert_eq!(
        sw.delete_profile(&ProfileId::new("official"))
            .await
            .unwrap_err()
            .code(),
        "invalid"
    );
    assert_eq!(
        sw.delete_profile(&ProfileId::new("ghost"))
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test]
async fn settings_clamp_the_countdown() {
    let w = World::new();
    let sw = w.switcher();
    sw.save_settings(Settings {
        countdown_secs: 99,
        ..Settings::default()
    })
    .await
    .unwrap();
    assert_eq!(sw.profile_set().await.unwrap().settings.countdown_secs, 30);
}

// --- sites --------------------------------------------------------------------------

#[tokio::test]
async fn custom_sites_are_validated_listed_and_protected_while_used() {
    let w = World::new();
    let sw = w.switcher();
    let saved = sw
        .save_site(compatible_site("acme", "https://api.acme.example"))
        .await
        .unwrap();
    assert_eq!(saved.lines[0].source, LineSource::User);
    let ids: Vec<String> = sw
        .sites()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id.0)
        .collect();
    assert_eq!(ids, ["krill", "official", "acme"]);
    // Built-in ids, Krill hosts and non-compatible kinds are refused.
    let mut krillish = compatible_site("fake-krill", "https://api.krill-code.net");
    assert_eq!(
        sw.save_site(krillish.clone()).await.unwrap_err().code(),
        "invalid"
    );
    krillish.id = SiteId::new("krill");
    assert_eq!(sw.save_site(krillish).await.unwrap_err().code(), "invalid");
    let mut wrong = compatible_site("other", "https://x.example");
    wrong.kind = SiteKind::Krill;
    assert_eq!(sw.save_site(wrong).await.unwrap_err().code(), "invalid");

    let mut p = krill_profile("acme-key", None, AuthMode::ApiKey);
    p.site_id = SiteId::new("acme");
    sw.save_profile(p, None).await.unwrap();
    assert_eq!(
        sw.delete_site(&SiteId::new("acme"))
            .await
            .unwrap_err()
            .code(),
        "conflict"
    );
    sw.delete_profile(&ProfileId::new("acme-key"))
        .await
        .unwrap();
    sw.delete_site(&SiteId::new("acme")).await.unwrap();
}

// --- key check ------------------------------------------------------------------------

#[tokio::test]
async fn key_checks_resolve_profiles_and_report_missing_keys() {
    let w = World::new();
    let sw = w.switcher();
    let check = sw
        .check_key(
            &SiteId::new("krill"),
            &LineId::new("overseas"),
            AuthMode::Pat,
            KeyInput::Profile(ProfileId::new("work")),
        )
        .await
        .unwrap();
    assert_eq!(check.key_last4, "1111");
    w.secrets.map.lock().unwrap().clear();
    let err = sw
        .check_key(
            &SiteId::new("krill"),
            &LineId::new("overseas"),
            AuthMode::Pat,
            KeyInput::Profile(ProfileId::new("work")),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "secret");
    let err = sw
        .check_key(
            &SiteId::new("krill"),
            &LineId::new("moon"),
            AuthMode::Pat,
            KeyInput::Secret(Secret::new("sk-test-1")),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_found");
    let err = sw
        .check_key(
            &SiteId::new("krill"),
            &LineId::new("overseas"),
            AuthMode::ApiKey,
            KeyInput::Secret(Secret::new("")),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, superba_core::Error::Blocked { ref reasons } if reasons == &vec![BlockReason::MissingKey])
    );
}

// --- open app -------------------------------------------------------------------------------

#[tokio::test]
async fn open_app_launches_with_cdp_and_the_auth_address() {
    let w = World::new();
    let sw = w.switcher();
    // Already ours: returned as is, and brought to the front (GH#7).
    assert_eq!(sw.open_app().await.unwrap().main_pid, 100);
    assert!(
        w.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "app.activate 100")
    );
    assert!(w.app.launches.lock().unwrap().is_empty());
    // Running without Superba: refused (a restart through Superba is needed).
    w.set_session(Some(session(None, false)));
    assert_eq!(sw.open_app().await.unwrap_err().code(), "conflict");
    // Not running: launched with the default Krill line's auth address.
    w.set_session(None);
    let s = sw.open_app().await.unwrap();
    assert!(s.cdp.is_some());
    let launch = w.app.launches.lock().unwrap().last().cloned().unwrap();
    assert_eq!(launch.cdp, superba_core::CdpMode::Auto);
    assert_eq!(
        launch.env.get("CODEX_AUTHAPI_BASE_URL").map(String::as_str),
        Some(OVERSEAS)
    );
    // Applied Krill line wins; safe mode disables CDP.
    w.set_session(None);
    w.config.state.lock().unwrap().active = Some(ActiveState {
        profile_id: Some("work".into()),
        site_id: Some("krill".into()),
        line_id: Some("cn-fast".into()),
        auth_mode: Some(AuthMode::Pat),
        generation: 1,
        applied_at: Timestamp(T0),
    });
    w.profiles.set.lock().unwrap().settings.safe_mode = true;
    sw.open_app().await.unwrap();
    let launch = w.app.launches.lock().unwrap().last().cloned().unwrap();
    assert_eq!(launch.cdp, superba_core::CdpMode::Disabled);
    assert_eq!(
        launch.env.get("CODEX_AUTHAPI_BASE_URL").map(String::as_str),
        Some(CN_FAST)
    );
}

// --- overview -------------------------------------------------------------------------------

#[tokio::test]
async fn overview_reports_hot_switch_status_and_the_auth_line() {
    let w = World::new();
    let sw = w.switcher();
    let o = sw.overview().await.unwrap();
    assert_eq!(o.app.hot_switch, HotSwitchStatus::Ready);
    assert_eq!(o.app.authapi_line, Some(LineId::new("overseas")));
    assert_eq!(o.codex_home, w.ctx.codex_home().root());
    assert!(o.baseline_captured);

    *w.app.bridge_ready.lock().unwrap() = false;
    assert!(matches!(
        sw.overview().await.unwrap().app.hot_switch,
        HotSwitchStatus::Unavailable { .. }
    ));
    w.set_session(Some(session(Some(CN_FAST), false)));
    assert_eq!(
        sw.overview().await.unwrap().app.hot_switch,
        HotSwitchStatus::Unavailable {
            reason: superba_core::FullRestartReason::NoCdp
        }
    );
    w.set_session(None);
    assert_eq!(
        sw.overview().await.unwrap().app.hot_switch,
        HotSwitchStatus::NotRunning
    );
    // Installs are a slow fact of the environment monitor: the overview sees
    // the uninstall after the monitor detects again (here: an explicit refresh).
    w.app.installs.lock().unwrap().clear();
    assert_eq!(
        sw.overview().await.unwrap().app.hot_switch,
        HotSwitchStatus::NotRunning
    );
    sw.environment_event(superba_core::EnvironmentEvent::Refresh);
    assert_eq!(
        sw.overview().await.unwrap().app.hot_switch,
        HotSwitchStatus::NotInstalled
    );
}

#[tokio::test]
async fn overview_marks_stale_processes_and_a_diverging_auth_line() {
    let w = World::new();
    w.config.state.lock().unwrap().active = Some(ActiveState {
        profile_id: Some("work".into()),
        site_id: Some("krill".into()),
        line_id: Some("cdn".into()),
        auth_mode: Some(AuthMode::Pat),
        generation: 2,
        applied_at: Timestamp(T0),
    });
    w.app.processes.lock().unwrap().extend([
        // Clearly older than the switch (beyond the start-time slack)...
        cli_process(300, CodexProcessKind::CliTui, T0 - 60_000),
        cli_process(301, CodexProcessKind::CliTui, T0 + 10),
        // ...but one started just around it proves nothing (coarse OS times).
        cli_process(302, CodexProcessKind::CliTui, T0 - 10),
    ]);
    let o = w.switcher().overview().await.unwrap();
    // The TUI and the app-server (started before the apply: not restarted yet).
    let stale: Vec<u32> = o
        .cli
        .processes
        .iter()
        .filter(|p| p.stale)
        .map(|p| p.pid)
        .collect();
    assert_eq!(stale, vec![150, 300]);
    assert_eq!(o.cli.stale_count(), 2);
    let keys: Vec<&str> = o.notices.iter().map(|n| n.text.key.as_str()).collect();
    assert!(keys.contains(&"switch.overview.stale_processes"));
    assert!(keys.contains(&"switch.overview.authapi_line"));
}

// --- doctor -----------------------------------------------------------------------------------

#[tokio::test]
async fn doctor_lists_actionable_findings() {
    let mut w = World::new();
    w.ctx.env = Arc::new(MapEnv::new().with("OPENAI_BASE_URL", "https://elsewhere.example"));
    {
        let mut st = w.config.state.lock().unwrap();
        st.model_provider = Some("krill".into());
        st.provider_table_present = true;
        st.profile_override = Some("fast".into());
        st.active = Some(ActiveState {
            profile_id: Some("work".into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::Pat),
            generation: 1,
            applied_at: Timestamp(T0),
        });
    }
    let mut tui = cli_process(300, CodexProcessKind::CliTui, T0 + 1);
    tui.authapi_env = EnvObservation::Absent;
    w.app.processes.lock().unwrap().push(tui);
    *w.sessions.report.lock().unwrap() =
        session_report(w.ctx.codex_home().root(), &[("krill", 4), ("openai", 9)]);
    let ids: Vec<String> = w
        .switcher()
        .doctor()
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.id)
        .collect();
    for want in [
        "legacy_provider",
        "provider_table",
        "profile_override",
        "missing_env",
        "hidden_sessions",
        "openai_env_conflict",
    ] {
        assert!(ids.iter().any(|i| i == want), "{want} missing from {ids:?}");
    }
    // A healthy setup reports nothing.
    let w = World::new();
    assert!(w.switcher().doctor().await.unwrap().is_empty());
}

#[tokio::test]
async fn doctor_names_the_line_the_app_authenticates_through() {
    // e2e: after a hot switch the app keeps its launch-time auth origin; the
    // finding names that line (looked up on the applied site, not by line id
    // across every site).
    let w = World::new();
    w.config.state.lock().unwrap().active = Some(ActiveState {
        profile_id: Some("work".into()),
        site_id: Some("krill".into()),
        line_id: Some("cdn".into()),
        auth_mode: Some(AuthMode::Pat),
        generation: 2,
        applied_at: Timestamp(T0),
    });
    let find = |findings: Vec<superba_core::DoctorFinding>| {
        findings.into_iter().find(|f| f.id == "authapi_other_line")
    };
    w.set_session(Some(session(Some(catalog::KRILL_ORIGIN_OVERSEAS), true)));
    let f = find(w.switcher().doctor().await.unwrap()).expect("finding");
    assert_eq!(f.title.key, "switch.doctor.authapi_other_line_named");
    assert!(
        f.title
            .nested
            .iter()
            .any(|(k, v)| k == "line" && v.key == "line.krill.overseas"),
        "{:?}",
        f.title
    );
    // An origin that is not a line of the applied site keeps the plain text.
    w.set_session(Some(session(Some("https://elsewhere.example"), true)));
    let f = find(w.switcher().doctor().await.unwrap()).expect("finding");
    assert_eq!(f.title.key, "switch.doctor.authapi_other_line");
    // The current line's own origin: nothing to report.
    w.set_session(Some(session(Some(catalog::KRILL_ORIGIN_CDN), true)));
    assert!(find(w.switcher().doctor().await.unwrap()).is_none());
}

// --- recovery -----------------------------------------------------------------------------------

fn entry(w: &World, id: &str, status: JournalStatus) -> JournalEntry {
    JournalEntry {
        id: id.into(),
        started_at: Timestamp(T0),
        updated_at: Timestamp(T0),
        status,
        codex_home: w.ctx.codex_home().root().to_path_buf(),
        summary: "test".into(),
        config: Some(ApplyReport {
            journal_id: format!("apply-{id}"),
            codex_home: w.ctx.codex_home().root().to_path_buf(),
            generation: 1,
            applied_at: Timestamp(T0),
            files: vec![FileWrite {
                kind: CodexFileKind::ConfigToml,
                path: w.ctx.codex_home().config_toml(),
                before_sha256: Some("cfg-0".into()),
                after_sha256: Some("cfg-1".into()),
                backup: None,
            }],
        }),
        env: Some(EnvApplied {
            id: "env-1".into(),
            var: "CODEX_AUTHAPI_BASE_URL".into(),
            edits: Vec::new(),
            backups: Vec::new(),
            applied_at: Timestamp(T0),
        }),
        certificate_env: None,
        migration: None,
        remotes: Vec::new(),
        remote_failures: Vec::new(),
        rollback: Default::default(),
    }
}

#[tokio::test]
async fn recovery_rolls_back_interrupted_writes_and_completes_committed_ones() {
    let w = World::new();
    let host = remote_host("devbox", Some("h-dev"));
    w.remote.hosts.lock().unwrap().push(host.clone());
    let journal = SwitchJournal::new(&w.ctx.data_dir);
    for (id, status) in [
        ("a", JournalStatus::Applying),
        ("b", JournalStatus::Committed),
        ("c", JournalStatus::Done),
        ("d", JournalStatus::Partial),
    ] {
        let mut e = entry(&w, id, status);
        e.remotes.push(superba_switch::journal::RemoteReceipt {
            target_id: host.target_id.clone(),
            report: superba_core::RemoteApplyReport {
                target_id: host.target_id.clone(),
                applied_at: Timestamp(T0),
                files: Vec::new(),
                env: None,
                certificate: None,
                login_fallback: None,
            },
            rolled_back: false,
        });
        journal.write(&e).unwrap();
    }

    // An unfinished entry blocks new switches until recovery ran.
    let p = w
        .switcher()
        .plan(superba_core::SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(
        matches!(&p.primary, superba_core::RestartKind::Blocked { reasons } if reasons.contains(&BlockReason::JournalPending))
    );

    let report = w.switcher().recover().await.unwrap();
    let actions: Vec<(String, JournalAction)> = report
        .entries
        .iter()
        .map(|e| (e.journal_id.clone(), e.action))
        .collect();
    assert_eq!(
        actions,
        vec![
            ("a".to_owned(), JournalAction::RolledBack),
            ("b".to_owned(), JournalAction::Completed)
        ]
    );
    let calls = w.calls();
    assert!(calls.contains(&"config.recover".to_owned()));
    assert_eq!(calls.iter().filter(|c| *c == "config.rollback").count(), 1);
    assert_eq!(calls.iter().filter(|c| *c == "env.revert").count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "remote.rollback devbox")
            .count(),
        1
    );
    for e in journal.entries().unwrap() {
        assert_eq!(e.remotes[0].rolled_back, e.id == "a");
    }
    assert!(journal.unfinished().unwrap().is_empty());
    // Running it again is a no-op.
    assert!(w.switcher().recover().await.unwrap().entries.is_empty());
}

#[tokio::test]
async fn recovery_reports_drift_when_a_rollback_fails_and_respects_the_lock() {
    let w = World::new();
    SwitchJournal::new(&w.ctx.data_dir)
        .write(&entry(&w, "a", JournalStatus::Applying))
        .unwrap();
    *w.config.fail_rollback.lock().unwrap() = Some(superba_core::Error::Conflict {
        message: "edited".into(),
    });
    {
        let _held = StateLock::acquire(&w.ctx.data_dir).unwrap();
        assert_eq!(w.switcher().recover().await.unwrap_err().code(), "locked");
    }
    let report = w.switcher().recover().await.unwrap();
    assert_eq!(report.entries[0].action, JournalAction::Drift);
    let journal = SwitchJournal::new(&w.ctx.data_dir);
    assert_eq!(journal.unfinished().unwrap().len(), 1);
    *w.config.fail_rollback.lock().unwrap() = None;
    let report = w.switcher().recover().await.unwrap();
    assert_eq!(report.entries[0].action, JournalAction::RolledBack);
    assert!(journal.unfinished().unwrap().is_empty());
    assert_eq!(
        w.calls().iter().filter(|c| *c == "env.revert").count(),
        1,
        "a completed rollback step is not retried"
    );
}

#[tokio::test]
async fn recovery_retries_a_locked_migration_before_reverting_env_or_config() {
    for status in [JournalStatus::Applying, JournalStatus::RollingBack] {
        let w = World::new();
        let journal = SwitchJournal::new(&w.ctx.data_dir);
        let mut e = entry(&w, "interrupted", status);
        e.migration = Some(superba_core::MigrationReceipt {
            id: "migrate-1".into(),
            codex_home: w.ctx.codex_home().root().to_path_buf(),
            migrated_at: Timestamp(T0),
            to_provider: "openai".into(),
            by_source_provider: std::collections::BTreeMap::from([("krill".into(), 3)]),
            threads: 3,
            rollouts: 3,
            backup_dir: w.ctx.data_dir.backups_dir().join("migrate-1"),
            ledger_path: w.tmp.path().join("ledger"),
            rolled_back: false,
        });
        journal.write(&e).unwrap();
        w.config.state.lock().unwrap().config_sha256 = Some("cfg-1".into());
        *w.sessions.fail_rollback.lock().unwrap() = Some(superba_core::Error::Locked {
            what: "Codex is running".into(),
        });

        let report = w.switcher().recover().await.unwrap();
        assert_eq!(report.entries[0].action, JournalAction::Drift);
        assert_eq!(journal.unfinished().unwrap().len(), 1);
        assert!(!w.calls().contains(&"env.revert".into()));
        assert!(!w.calls().contains(&"config.rollback".into()));
        assert_eq!(
            w.config.state.lock().unwrap().config_sha256.as_deref(),
            Some("cfg-1")
        );

        *w.sessions.fail_rollback.lock().unwrap() = None;
        w.log.lock().unwrap().clear();
        let report = w.switcher().recover().await.unwrap();
        assert_eq!(report.entries[0].action, JournalAction::RolledBack);
        assert_eq!(
            w.calls(),
            vec![
                "config.recover",
                "sessions.rollback migrate-1",
                "env.revert",
                "config.rollback"
            ]
        );
        assert_eq!(
            w.config.state.lock().unwrap().config_sha256.as_deref(),
            Some("cfg-0")
        );
        assert!(journal.unfinished().unwrap().is_empty());
        assert!(w.switcher().recover().await.unwrap().entries.is_empty());
    }
}

#[tokio::test]
async fn recovery_stops_at_failed_env_rollback_and_retries_it_before_config() {
    let w = World::new();
    let journal = SwitchJournal::new(&w.ctx.data_dir);
    journal
        .write(&entry(&w, "undo", JournalStatus::RollingBack))
        .unwrap();
    *w.env.fail_revert.lock().unwrap() = Some(superba_core::Error::other("rc file is locked"));
    let report = w.switcher().recover().await.unwrap();
    assert_eq!(report.entries[0].action, JournalAction::Drift);
    assert!(!w.calls().contains(&"config.rollback".into()));
    assert_eq!(journal.unfinished().unwrap().len(), 1);
    *w.env.fail_revert.lock().unwrap() = None;
    let report = w.switcher().recover().await.unwrap();
    assert_eq!(report.entries[0].action, JournalAction::RolledBack);
    assert!(journal.unfinished().unwrap().is_empty());
}

#[test]
fn old_switch_journals_deserialize_without_rollback_progress() {
    let w = World::new();
    for status in [
        JournalStatus::Applying,
        JournalStatus::Committed,
        JournalStatus::Done,
        JournalStatus::RolledBack,
    ] {
        let mut value = serde_json::to_value(entry(&w, "old", status)).unwrap();
        value.as_object_mut().unwrap().remove("rollback");
        let parsed: JournalEntry = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.status, status);
        assert_eq!(parsed.rollback, Default::default());
    }
}

#[tokio::test]
async fn journal_keeps_a_bounded_history() {
    let w = World::new();
    let journal = SwitchJournal::new(&w.ctx.data_dir);
    for i in 0..40 {
        let mut e = entry(&w, &format!("e{i:02}"), JournalStatus::Done);
        e.started_at = Timestamp(T0 + i);
        journal.write(&e).unwrap();
    }
    journal.prune().unwrap();
    let ids: Vec<String> = journal
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(ids.len(), superba_switch::journal::KEEP_FINISHED);
    assert_eq!(ids.first().map(String::as_str), Some("e08"));
}

#[tokio::test]
async fn e2e_test_sites_are_injected_from_a_file_and_usable_for_pat() {
    let mut w = World::new();
    let mut site = catalog::krill_site();
    site.id = SiteId::new("krill-test");
    site.lines = vec![Line {
        id: LineId::new("a"),
        name: LocalizedText::new("raw", "Line A"),
        origin: Origin::parse("https://127.0.0.1:18001").unwrap(),
        source: LineSource::Builtin,
    }];
    site.default_line = Some(LineId::new("a"));
    let file = w.tmp.path().join("sites.json");
    std::fs::write(&file, serde_json::to_vec(&vec![site]).unwrap()).unwrap();
    w.ctx.env =
        Arc::new(MapEnv::new().with(superba_switch::ENV_SUPERBA_TEST_SITES, file.as_os_str()));
    let sw = w.switcher();
    let sites = sw.sites().await.unwrap();
    let test = sites
        .iter()
        .find(|s| s.id == "krill-test")
        .expect("injected");
    assert_eq!(test.lines[0].source, LineSource::Test);

    let mut p = krill_profile("e2e", Some("a"), AuthMode::Pat);
    p.site_id = SiteId::new("krill-test");
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:e2e".into(), KEY.into());
    sw.save_profile(p, None).await.unwrap();
    w.set_session(Some(session(Some("https://127.0.0.1:18001"), true)));
    let plan = sw
        .plan(superba_core::SwitchRequest::profile(ProfileId::new("e2e")))
        .await
        .unwrap();
    assert!(plan.primary.is_hot(), "{:?}", plan.primary);
    assert_eq!(
        w.env.plans.lock().unwrap().last(),
        Some(&superba_core::EnvOp::Set {
            value: "https://127.0.0.1:18001".into(),
            take_over_foreign: false
        })
    );
    // An unreadable file is ignored.
    std::fs::write(&file, b"not json").unwrap();
    assert!(
        !sw.sites()
            .await
            .unwrap()
            .iter()
            .any(|s| s.id == "krill-test")
    );
}

#[tokio::test]
async fn e2e_test_lines_extend_the_krill_site_and_invalid_values_are_ignored() {
    let mut w = World::new();
    w.ctx.env = Arc::new(MapEnv::new().with(
        superba_net::ENV_SUPERBA_TEST_LINES,
        "a=http://127.0.0.1:18001,b=http://127.0.0.1:18002",
    ));
    let sw = w.switcher();
    let sites = sw.sites().await.unwrap();
    let krill = sites.iter().find(|s| s.id == "krill").unwrap();
    let test: Vec<_> = krill
        .lines
        .iter()
        .filter(|l| l.source == LineSource::Test)
        .map(|l| (l.id.as_str().to_owned(), l.origin.as_str()))
        .collect();
    assert_eq!(
        test,
        vec![
            ("a".to_owned(), "http://127.0.0.1:18001".to_owned()),
            ("b".to_owned(), "http://127.0.0.1:18002".to_owned()),
        ]
    );
    // Non-loopback test lines are refused as a whole (logged, not fatal).
    let mut w = World::new();
    w.ctx.env =
        Arc::new(MapEnv::new().with(superba_net::ENV_SUPERBA_TEST_LINES, "x=https://example.com"));
    let sites = w.switcher().sites().await.unwrap();
    let krill = sites.iter().find(|s| s.id == "krill").unwrap();
    assert!(krill.lines.iter().all(|l| l.source != LineSource::Test));
}

#[tokio::test]
async fn compatible_site_profiles_use_the_base_path_of_their_site() {
    let w = World::new();
    let sw = w.switcher();
    let base = url::Url::parse("https://api.acme.example/v1/").unwrap();
    let site = superba_switch::new_compatible_site(SiteId::new("acme"), "Acme", &base).unwrap();
    sw.save_site(site).await.unwrap();
    let p = Profile {
        site_id: SiteId::new("acme"),
        line_id: None,
        ..krill_profile("acme", None, AuthMode::ApiKey)
    };
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:acme".into(), KEY.into());
    sw.save_profile(p, None).await.unwrap();
    let plan = sw
        .plan(superba_core::SwitchRequest::profile(ProfileId::new("acme")))
        .await
        .unwrap();
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan");
    match &desired.mode {
        superba_core::ResolvedMode::Token { endpoints, .. } => {
            assert_eq!(
                endpoints.openai_base_url.as_str(),
                "https://api.acme.example/v1"
            );
        }
        other => panic!("unexpected mode {other:?}"),
    }
}
