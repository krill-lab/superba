//! Scan, plan, migrate, rollback and recovery against synthetic Codex homes.

#![allow(missing_docs)]

mod common;

use std::fs;

use common::{Fixture, Thread, rollout_provider, row_provider};
use pretty_assertions::assert_eq;
use superba_core::*;
use superba_sessions::{FaultPlan, FaultPoint, takeover_request};

fn provider_request(from: &str, to: &str, include_archived: bool) -> MigrationRequest {
    MigrationRequest {
        to_provider: to.into(),
        selectors: vec![MigrationSelector::Provider {
            provider: from.into(),
        }],
        include_archived,
        exclude: vec![],
    }
}

/// krill: live, live CRLF, compressed live, archived, compressed archived,
/// rollout-only, sqlite-only, catalog; openai: one live; plus one inconsistent.
fn standard() -> Fixture {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill").catalog());
    f.add(Thread::new(2, "krill").crlf().turns(3));
    f.add(Thread::new(3, "krill").zst());
    f.add(Thread::new(4, "krill").archived());
    f.add(Thread::new(5, "krill").archived().zst());
    f.add(Thread::new(6, "krill").no_sqlite());
    f.add(Thread::new(7, "krill").no_rollout());
    f.add(Thread::new(8, "openai"));
    f.add(Thread::new(9, "openai").rollout_provider(Some("krill")));
    f
}

#[tokio::test]
async fn scan_counts_providers_per_location() {
    let f = standard();
    // A rollout without session metadata is ignored with a notice.
    let stray = f
        .home
        .sessions_dir()
        .join("rollout-2026-10-01T00-00-00-stray.jsonl");
    fs::write(&stray, "{\"type\":\"event_msg\"}\n").unwrap();
    let report = f.migrator().scan(&f.home).await.unwrap();
    let krill = &report.by_provider["krill"];
    // sqlite threads rows: 1,2,3,4,5,7.
    assert_eq!(krill.threads, 6);
    // live rollouts with krill heads: 1,2,3,6 and 9 (inconsistent).
    assert_eq!(krill.rollouts, 5);
    assert_eq!(krill.archived, 2);
    assert_eq!(krill.compressed, 2);
    // Distinct sessions per provider (union over every storage form): 1..7 and
    // thread 9, whose rollout still says krill.
    assert_eq!(krill.distinct, 8);
    let openai = &report.by_provider["openai"];
    assert_eq!(openai.threads, 2);
    assert_eq!(openai.distinct, 2);
    assert_eq!(report.inconsistent, 1);
    assert_eq!(report.hidden_under("openai"), 8);
    assert!(report.sqlite_paths.contains(&f.state_db()));
    assert!(report.sqlite_paths.contains(&f.history_db()));
    assert!(report.sqlite_paths.contains(&f.catalog_db()));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.text.key == "sessions.warn.no_session_meta")
    );
    // `external_agent_config_imports.provider_id` is not a session provider.
    assert!(!report.by_provider.contains_key(""));
    let req = takeover_request(&report).unwrap();
    assert_eq!(req.to_provider, "openai");
    assert_eq!(
        req.selectors,
        vec![MigrationSelector::Provider {
            provider: "krill".into()
        }]
    );
}

#[tokio::test]
async fn plan_resolves_selectors_and_writes_nothing() {
    let f = standard();
    let before = f.snapshot();
    let m = f.migrator();

    let all = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    let ids: Vec<&str> = all.items.iter().map(|i| i.thread_id.as_str()).collect();
    assert_eq!(all.items.len(), 8, "{ids:?}");
    assert_eq!(all.by_source_provider["krill"], 8);
    // Rows: 1,2,3,4,5,7 in threads + 1 in the catalog; thread 9's row is openai.
    assert_eq!(all.thread_count, 7);
    // Files with krill heads: 1,2,3,4,5,6,9.
    assert_eq!(all.rollout_count, 7);
    assert!(all.backup_bytes > 0);
    assert!(all.backup_dir.starts_with(f.data.root()));
    assert_eq!(all.codex_home, f.home.root());
    assert!(all.items.iter().any(|i| i.archived));
    assert!(all.items.iter().all(|i| i.created_at.is_some()));
    // Newest first.
    assert!(
        all.items
            .windows(2)
            .all(|w| w[0].created_at >= w[1].created_at)
    );

    let live = m
        .plan(&f.home, &provider_request("krill", "openai", false))
        .await
        .unwrap();
    assert_eq!(live.items.len(), 6);
    assert!(live.items.iter().all(|i| !i.archived));

    let mut req = provider_request("krill", "openai", true);
    req.exclude = vec![f.threads[0].id.clone()];
    let excl = m.plan(&f.home, &req).await.unwrap();
    assert_eq!(excl.items.len(), 7);

    let by_ids = m
        .plan(
            &f.home,
            &MigrationRequest {
                to_provider: "openai".into(),
                selectors: vec![MigrationSelector::ThreadIds {
                    ids: vec![
                        f.threads[1].id.clone(),
                        f.threads[7].id.clone(),
                        "nope".into(),
                    ],
                }],
                include_archived: true,
                exclude: vec![],
            },
        )
        .await
        .unwrap();
    // Thread 8 is already openai.
    assert_eq!(by_ids.items.len(), 1);
    assert_eq!(by_ids.items[0].thread_id, f.threads[1].id);

    let window = m
        .plan(
            &f.home,
            &MigrationRequest {
                to_provider: "krill".into(),
                selectors: vec![MigrationSelector::CreatedBetween {
                    provider: "openai".into(),
                    from: Timestamp(f.threads[7].created_ms),
                    to: Some(Timestamp(f.threads[8].created_ms)),
                }],
                include_archived: true,
                exclude: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(window.items.len(), 1);
    assert_eq!(window.items[0].thread_id, f.threads[7].id);

    let bad = m
        .plan(&f.home, &provider_request("krill", "", true))
        .await
        .unwrap_err();
    assert_eq!(bad.code(), "invalid");

    f.snapshot().assert_same(&before);
    assert!(!all.backup_dir.exists());
}

#[tokio::test]
async fn migrate_changes_only_provider_tokens_and_keeps_projection_valid() {
    let f = standard();
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    assert_eq!(receipt.id, plan.id);
    assert_eq!(receipt.to_provider, "openai");
    assert_eq!(receipt.threads, 7);
    assert_eq!(receipt.rollouts, 7);
    assert!(!receipt.rolled_back);
    assert!(receipt.ledger_path.exists());
    assert!(receipt.backup_dir.join("rollouts").is_dir());
    assert!(receipt.backup_dir.join("sqlite").is_dir());

    let report = m.scan(&f.home).await.unwrap();
    assert!(!report.by_provider.contains_key("krill"), "{report:?}");
    assert_eq!(report.by_provider["openai"].distinct, 9);
    assert_eq!(report.inconsistent, 0);

    let after = f.snapshot();
    for (path, (bytes, mtime)) in &before.files {
        let (new_bytes, new_mtime) = &after.files[path];
        assert_eq!(mtime, new_mtime, "mtime of {}", path.display());
        if !path.to_string_lossy().contains("rollout-") {
            assert!(bytes == new_bytes, "{} changed", path.display());
            continue;
        }
        let compressed = path.to_string_lossy().ends_with(".zst");
        if compressed {
            assert_eq!(&new_bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd], "still zstd");
        }
        let old_plain = String::from_utf8(Fixture::plain_bytes(bytes, compressed)).unwrap();
        let new_plain = String::from_utf8(Fixture::plain_bytes(new_bytes, compressed)).unwrap();
        let expected = old_plain.replacen(
            "\"model_provider\":\"krill\",\"base_instructions\"",
            "\"model_provider\":\"openai\",\"base_instructions\"",
            1,
        );
        assert_eq!(new_plain, expected, "{}", path.display());
        // The decoy inside base_instructions is untouched.
        assert!(new_plain.contains(r#"Never write \"model_provider\":\"krill\""#));
    }
    f.assert_projection_consistent();
    for t in &f.threads {
        if t.sqlite {
            assert_eq!(
                row_provider(&f.state_db(), &t.id).as_deref(),
                Some("openai")
            );
        }
        if t.store != common::Store::None {
            assert_eq!(
                rollout_provider(&f.rollout_path(t)).as_deref(),
                Some("openai")
            );
        }
    }
    let catalog = common::dump_db(&f.catalog_db());
    assert!(catalog["local_thread_catalog"][0].contains("Text(\"openai\")"));
    // Unrelated provider-ish columns are untouched.
    let state = common::dump_db(&f.state_db());
    assert!(state["external_agent_config_imports"][0].contains("Text(\"krill\")"));

    let history = m.history(&f.home).await.unwrap();
    assert_eq!(history, vec![receipt.clone()]);

    // Executing the same plan twice is refused.
    let again = m.migrate(&f.home, &plan).await.unwrap_err();
    assert_eq!(again.code(), "conflict");
}

#[tokio::test]
async fn rollback_restores_everything_exactly() {
    let f = standard();
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
    let report = m.rollback(&f.home, &plan.id).await.unwrap();
    assert_eq!(report.restored_rollouts, 7);
    assert_eq!(report.restored_threads, 7);
    assert_eq!(report.skipped_changed, 0);
    f.snapshot().assert_same(&before);
    f.assert_projection_consistent();
    let history = m.history(&f.home).await.unwrap();
    assert!(history[0].rolled_back);
    // Idempotent.
    assert_eq!(
        m.rollback(&f.home, &plan.id).await.unwrap(),
        RollbackReport::default()
    );
    assert_eq!(
        m.rollback(&f.home, "missing-id").await.unwrap_err().code(),
        "not_found"
    );
    assert_eq!(
        m.rollback(&f.home, "../etc").await.unwrap_err().code(),
        "invalid"
    );
}

#[tokio::test]
async fn all_other_providers_migrate_together_including_archived_and_inconsistent() {
    let mut f = standard();
    f.add(Thread::new(10, "custom-provider").archived().zst());
    f.add(Thread::new(11, "third-party").no_rollout());
    let m = f.migrator();
    let plan = m
        .plan(
            &f.home,
            &MigrationRequest {
                to_provider: "openai".into(),
                selectors: vec![MigrationSelector::OtherProviders {
                    except: "openai".into(),
                }],
                include_archived: true,
                exclude: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 10);
    assert_eq!(plan.by_source_provider["krill"], 8);
    assert_eq!(plan.by_source_provider["custom-provider"], 1);
    assert_eq!(plan.by_source_provider["third-party"], 1);
    assert!(!plan.by_source_provider.contains_key("openai"));
    // A request created while Codex was closing must be part of "all" too.
    f.add(Thread::new(12, "provider-added-after-preview"));
    m.migrate(&f.home, &plan).await.unwrap();
    let report = m.scan(&f.home).await.unwrap();
    assert_eq!(report.hidden_under("openai"), 0);
    assert_eq!(report.by_provider["openai"].distinct, 12);
    assert_eq!(report.inconsistent, 0);
}

#[tokio::test]
async fn rollback_batch_unwinds_overlapping_migrations_and_is_idempotent() {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "first-provider").archived().zst());
    let before = f.snapshot();
    let m = f
        .migrator()
        .with_clock(std::sync::Arc::new(ManualClock::new(Timestamp::now())));
    let mut first = m
        .plan(
            &f.home,
            &provider_request("first-provider", "second-provider", true),
        )
        .await
        .unwrap();
    first.id = "z-first".into();
    first.backup_dir = f.data.migrate_backup_dir(&first.id);
    m.migrate(&f.home, &first).await.unwrap();
    let mut second = m
        .plan(
            &f.home,
            &provider_request("second-provider", "openai", true),
        )
        .await
        .unwrap();
    second.id = "a-second".into();
    second.backup_dir = f.data.migrate_backup_dir(&second.id);
    m.migrate(&f.home, &second).await.unwrap();
    let history = m.history(&f.home).await.unwrap();
    assert_eq!(history[0].migrated_at, history[1].migrated_at);
    assert_eq!(history[0].id, second.id);
    assert_eq!(history[1].id, first.id);
    let invalid = m
        .rollback_many_stopped_with_progress(
            &f.home,
            &[second.id.clone(), "missing-id".into()],
            &ConsumersStopped::new(&f.home, Timestamp::now()),
            std::sync::Arc::new(NoSessionProgress),
        )
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), "not_found");
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[0].id).as_deref(),
        Some("openai")
    );
    let ids = vec![second.id, first.id];
    let report = m
        .rollback_many_stopped_with_progress(
            &f.home,
            &ids,
            &ConsumersStopped::new(&f.home, Timestamp::now()),
            std::sync::Arc::new(NoSessionProgress),
        )
        .await
        .unwrap();
    assert_eq!(report.restored_rollouts, 2);
    assert_eq!(report.skipped_changed, 0);
    assert_eq!(f.snapshot().dbs, before.dbs);
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[0].id).as_deref(),
        Some("first-provider")
    );
    assert!(
        m.history(&f.home)
            .await
            .unwrap()
            .iter()
            .all(|r| r.rolled_back)
    );
    let again = m
        .rollback_many_stopped_with_progress(
            &f.home,
            &ids,
            &ConsumersStopped::new(&f.home, Timestamp::now()),
            std::sync::Arc::new(NoSessionProgress),
        )
        .await
        .unwrap();
    assert_eq!(again, RollbackReport::default());
}

#[tokio::test]
async fn rollback_batch_keeps_changed_content_across_overlapping_ledgers() {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "first-provider"));
    f.add(Thread::new(2, "first-provider"));
    let m = f.migrator();
    let first = m
        .plan(
            &f.home,
            &provider_request("first-provider", "second-provider", true),
        )
        .await
        .unwrap();
    m.migrate(&f.home, &first).await.unwrap();
    let second = m
        .plan(
            &f.home,
            &provider_request("second-provider", "openai", true),
        )
        .await
        .unwrap();
    m.migrate(&f.home, &second).await.unwrap();
    let changed = f.rollout_path(&f.threads[0]);
    let mut contents = fs::read(&changed).unwrap();
    contents.extend_from_slice(b"{\"ordinal\":99,\"type\":\"event_msg\",\"payload\":{}}\n");
    fs::write(&changed, &contents).unwrap();
    let report = m
        .rollback_many_stopped_with_progress(
            &f.home,
            &[second.id, first.id],
            &ConsumersStopped::new(&f.home, Timestamp::now()),
            std::sync::Arc::new(NoSessionProgress),
        )
        .await
        .unwrap();
    assert_eq!(report.skipped_changed, 2);
    assert_eq!(fs::read(&changed).unwrap(), contents);
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[0].id).as_deref(),
        Some("openai")
    );
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[1].id).as_deref(),
        Some("first-provider")
    );
}

#[tokio::test]
async fn rollback_skips_sessions_changed_after_migration() {
    let f = standard();
    let m = f.migrator();
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
    // The user continued thread 1 under openai.
    let t1 = &f.threads[0];
    let path = f.rollout_path(t1);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"ordinal\":99,\"type\":\"event_msg\",\"payload\":{}}\n");
    fs::write(&path, bytes).unwrap();
    let report = m.rollback(&f.home, &plan.id).await.unwrap();
    assert_eq!(report.skipped_changed, 1);
    assert_eq!(report.restored_rollouts, 6);
    // Thread 1 keeps openai everywhere (sqlite row, catalog, rollout).
    assert_eq!(
        row_provider(&f.state_db(), &t1.id).as_deref(),
        Some("openai")
    );
    assert_eq!(rollout_provider(&path).as_deref(), Some("openai"));
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[1].id).as_deref(),
        Some("krill")
    );
}

#[tokio::test]
async fn rollback_without_pre_images_uses_recorded_edits() {
    let f = standard();
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    fs::remove_dir_all(receipt.backup_dir.join("rollouts")).unwrap();
    let report = m.rollback(&f.home, &plan.id).await.unwrap();
    assert_eq!(report.restored_rollouts, 7);
    let after = f.snapshot();
    for (path, (bytes, mtime)) in &before.files {
        let (b2, m2) = &after.files[path];
        assert_eq!(mtime, m2);
        let compressed = path.to_string_lossy().ends_with(".zst");
        // Plain files are byte-identical; compressed ones decompress identically.
        assert_eq!(
            Fixture::plain_bytes(bytes, compressed),
            Fixture::plain_bytes(b2, compressed),
            "{}",
            path.display()
        );
    }
    assert_eq!(before.dbs, after.dbs);
}

#[tokio::test]
async fn reverse_migration_by_ledger_round_trips() {
    let f = standard();
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
    let back = m
        .plan(
            &f.home,
            &MigrationRequest {
                to_provider: "krill".into(),
                selectors: vec![MigrationSelector::Ledger {
                    migration_id: plan.id.clone(),
                }],
                include_archived: true,
                exclude: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(back.items.len(), 8);
    assert!(back.items.iter().all(|i| i.source_provider == "openai"));
    m.migrate(&f.home, &back).await.unwrap();
    // Every rollout is back to its original content. Thread 9 was inconsistent
    // (row openai, rollout krill); the reverse migration moves the whole thread,
    // so its row now follows the rollout to krill.
    let after = f.snapshot();
    for (path, (bytes, mtime)) in &before.files {
        let (b2, m2) = &after.files[path];
        assert_eq!(mtime, m2);
        let compressed = path.to_string_lossy().ends_with(".zst");
        assert_eq!(
            Fixture::plain_bytes(bytes, compressed),
            Fixture::plain_bytes(b2, compressed),
            "{}",
            path.display()
        );
    }
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[8].id).as_deref(),
        Some("krill")
    );
    for t in f.threads.iter().take(7) {
        if t.sqlite {
            assert_eq!(row_provider(&f.state_db(), &t.id).as_deref(), Some("krill"));
        }
    }
    f.assert_projection_consistent();
    assert_eq!(m.history(&f.home).await.unwrap().len(), 2);
}

#[tokio::test]
async fn missing_provider_counts_as_openai_and_can_move() {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "openai").no_sqlite().rollout_provider(None));
    f.add(Thread::new(2, "openai").rollout_provider(None));
    let m = f.migrator();
    let report = m.scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["openai"].rollouts, 2);
    assert_eq!(report.inconsistent, 0);
    let before = f.snapshot();
    let plan = m
        .plan(&f.home, &provider_request("openai", "krill", true))
        .await
        .unwrap();
    assert_eq!(plan.items.len(), 2);
    m.migrate(&f.home, &plan).await.unwrap();
    for t in &f.threads {
        assert_eq!(
            rollout_provider(&f.rollout_path(t)).as_deref(),
            Some("krill")
        );
    }
    f.assert_projection_consistent();
    m.rollback(&f.home, &plan.id).await.unwrap();
    f.snapshot().assert_same(&before);
}

#[tokio::test]
async fn separate_sqlite_home_is_migrated() {
    let mut f = Fixture::new().with_sqlite_home();
    f.add(Thread::new(1, "krill"));
    f.add(Thread::new(2, "krill").zst());
    assert!(f.state_db().starts_with(f.tmp.path().join("sqlite-home")));
    let before = f.snapshot();
    let m = f.migrator();
    let report = m.scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["krill"].threads, 2);
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    assert_eq!(receipt.threads, 2);
    assert!(
        !receipt
            .backup_dir
            .starts_with(f.tmp.path().join("sqlite-home"))
    );
    f.assert_projection_consistent();
    m.rollback(&f.home, &plan.id).await.unwrap();
    f.snapshot().assert_same(&before);
}

#[tokio::test]
async fn config_sqlite_home_with_tilde_is_scanned() {
    let f = Fixture::new();
    let dir = f.home_dir.join("codex-db");
    fs::create_dir_all(&dir).unwrap();
    let c = rusqlite::Connection::open(dir.join("state_5.sqlite")).unwrap();
    c.execute_batch(common::THREADS_DDL).unwrap();
    c.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider, cwd, title, sandbox_policy, approval_mode)
         VALUES ('t-x', '/nowhere', 1, 1, 'exec', 'krill', '/', 't', '{}', 'never')",
        [],
    )
    .unwrap();
    drop(c);
    fs::write(f.home.config_toml(), "sqlite_home = \"~/codex-db\"\n").unwrap();
    let report = f.migrator().scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["krill"].threads, 1);
    // Without the home directory the value cannot be resolved.
    let bare = superba_sessions::CodexSessionMigrator::new(f.data.clone());
    let report = bare.scan(&f.home).await.unwrap();
    assert!(!report.by_provider.contains_key("krill"));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.text.key == "sessions.warn.sqlite_home_unresolved")
    );
}

#[tokio::test]
async fn every_failure_point_restores_the_original_state() {
    let points = [
        FaultPoint::AfterLedger,
        FaultPoint::AfterRollout(0),
        FaultPoint::AfterRollout(3),
        FaultPoint::AfterDatabase(0),
        FaultPoint::AfterDatabase(1),
        FaultPoint::BeforeComplete,
    ];
    for point in points {
        let f = standard();
        let before = f.snapshot();
        let m = f.migrator().with_faults(FaultPlan {
            fail_at: Some(point),
            crash: false,
        });
        let plan = m
            .plan(&f.home, &provider_request("krill", "openai", true))
            .await
            .unwrap();
        let err = m.migrate(&f.home, &plan).await.unwrap_err();
        assert!(err.to_string().contains("injected"), "{point:?}: {err}");
        f.snapshot().assert_same(&before);
        f.assert_projection_consistent();
        assert!(m.history(&f.home).await.unwrap().is_empty(), "{point:?}");
        // Nothing left to recover.
        assert_eq!(m.recover(&f.home).await.unwrap(), RollbackReport::default());
    }
}

#[tokio::test]
async fn crash_mid_migration_is_recovered() {
    for point in [
        FaultPoint::AfterRollout(2),
        FaultPoint::AfterDatabase(0),
        FaultPoint::BeforeComplete,
    ] {
        let f = standard();
        let before = f.snapshot();
        let crashing = f.migrator().with_faults(FaultPlan {
            fail_at: Some(point),
            crash: true,
        });
        let plan = crashing
            .plan(&f.home, &provider_request("krill", "openai", true))
            .await
            .unwrap();
        crashing.migrate(&f.home, &plan).await.unwrap_err();
        assert_ne!(f.snapshot(), before, "{point:?}: something was written");
        let m = f.migrator();
        assert!(m.history(&f.home).await.unwrap().is_empty());
        let report = m.recover(&f.home).await.unwrap();
        assert!(report.restored_rollouts > 0, "{point:?}");
        f.snapshot().assert_same(&before);
        f.assert_projection_consistent();
        assert_eq!(m.recover(&f.home).await.unwrap(), RollbackReport::default());
    }
}

#[tokio::test]
async fn empty_and_foreign_plans() {
    let f = standard();
    let m = f.migrator();
    let plan = m
        .plan(&f.home, &provider_request("nobody", "openai", true))
        .await
        .unwrap();
    assert!(plan.items.is_empty());
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    assert_eq!(receipt.threads, 0);
    assert_eq!(receipt.rollouts, 0);

    let other = Fixture::new();
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    let err = m.migrate(&other.home, &plan).await.unwrap_err();
    assert_eq!(err.code(), "invalid");
}

#[tokio::test]
async fn wal_contents_are_backed_up_and_migrated() {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill"));
    // Simulate Codex exiting without a checkpoint: copy the database and its WAL
    // while a writer holds them, so the rows exist only in the WAL.
    let staging = f.tmp.path().join("staging.sqlite");
    let writer = rusqlite::Connection::open(&staging).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer.execute_batch(common::THREADS_DDL).unwrap();
    for i in 0..50 {
        writer
            .execute(
                "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider, cwd, title, sandbox_policy, approval_mode)
                 VALUES (?1, '/nowhere', 1, 1, 'exec', 'krill', '/', 't', '{}', 'never')",
                [format!("wal-{i}")],
            )
            .unwrap();
    }
    let state = f.state_db();
    fs::remove_file(&state).unwrap();
    let _ = fs::remove_file(common_sidecar(&state, "-wal"));
    let _ = fs::remove_file(common_sidecar(&state, "-shm"));
    fs::copy(&staging, &state).unwrap();
    fs::copy(
        common_sidecar(&staging, "-wal"),
        common_sidecar(&state, "-wal"),
    )
    .unwrap();
    drop(writer);
    assert!(fs::metadata(common_sidecar(&state, "-wal")).unwrap().len() > 0);

    let m = f.migrator();
    let report = m.scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["krill"].threads, 50);
    let plan = m
        .plan(&f.home, &provider_request("krill", "openai", true))
        .await
        .unwrap();
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    assert_eq!(receipt.threads, 50);
    let backup = fs::read_dir(receipt.backup_dir.join("sqlite"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with("state_5.sqlite"))
        .unwrap();
    // Backups are self-contained single files.
    assert!(!common_sidecar(&backup, "-wal").exists());
    let rows = common::dump_db(&backup);
    assert_eq!(rows["threads"].len(), 50);
    assert!(
        rows["threads"]
            .iter()
            .all(|r| r.contains("Text(\"krill\")"))
    );
    let report = m.scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["openai"].threads, 50);
}

fn common_sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    s.into()
}

#[tokio::test]
async fn retention_prunes_old_pre_images_but_keeps_ledgers() {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill"));
    let m = f.migrator();
    let mut ids = Vec::new();
    for i in 0..7 {
        let (from, to) = if i % 2 == 0 {
            ("krill", "openai")
        } else {
            ("openai", "krill")
        };
        let plan = m
            .plan(&f.home, &provider_request(from, to, true))
            .await
            .unwrap();
        m.migrate(&f.home, &plan).await.unwrap();
        ids.push(plan.id);
    }
    let history = m.history(&f.home).await.unwrap();
    assert_eq!(history.len(), 7);
    let with_images = history
        .iter()
        .filter(|r| r.backup_dir.join("rollouts").exists())
        .count();
    assert_eq!(with_images, 5);
    assert!(history.iter().all(|r| r.ledger_path.exists()));
}
