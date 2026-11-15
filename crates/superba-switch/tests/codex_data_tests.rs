//! Codex data location (GitHub issue 15) through the service: refused while
//! Codex runs, a move Superba's own session scan follows, doctor checks and
//! start-up recovery.

#![allow(missing_docs)]

mod fakes;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fakes::World;
use superba_core::data::{CodexDirPlacement, DataMoveProgressSink};
use superba_core::{SessionMigrator, Severity, SwitchService};
use superba_sessions::CodexSessionMigrator;
use superba_storage::codex_data::{self, CodexDataFs, Scope};

const THREAD: &str = "019a0000-0000-7000-8000-000000000001";

fn rollout(id: &str, provider: &str) -> String {
    format!(
        "{{\"timestamp\":\"2026-10-01T08:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-10-01T08:00:00.000Z\",\"cwd\":\"/work\",\"originator\":\"codex_cli_rs\",\"cli_version\":\"0.162.0\",\"source\":\"cli\",\"model_provider\":\"{provider}\"}}}}\n{{\"timestamp\":\"2026-10-01T08:00:01.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"你好\"}}}}\n"
    )
}

/// A home with one active and one archived conversation, Codex closed.
fn world() -> (World, PathBuf) {
    let w = World::new();
    w.app.processes.lock().unwrap().clear();
    *w.app.session.lock().unwrap() = None;
    let home = w.ctx.codex_home();
    let day = home.sessions_dir().join("2026/10/01");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-10-01T08-00-00-{THREAD}.jsonl")),
        rollout(THREAD, "openai"),
    )
    .unwrap();
    fs::create_dir_all(home.archived_sessions_dir()).unwrap();
    fs::write(
        home.archived_sessions_dir()
            .join("rollout-2026-09-01T08-00-00-019a0000-0000-7000-8000-000000000002.jsonl"),
        rollout("019a0000-0000-7000-8000-000000000002", "openai"),
    )
    .unwrap();
    fs::create_dir_all(w.ctx.data_dir.root()).unwrap();
    let dest = w.tmp.path().join("other-disk").join("Codex");
    (w, dest)
}

fn quiet() -> DataMoveProgressSink {
    Arc::new(|_| true)
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| superba_storage::validation::is_link(&m))
}

#[tokio::test]
async fn refused_while_codex_runs() {
    // The default world has the app and its app-server running.
    let w = World::new();
    let home = w.ctx.codex_home();
    fs::create_dir_all(home.sessions_dir()).unwrap();
    fs::create_dir_all(w.ctx.data_dir.root()).unwrap();
    let service = w.switcher();
    let dest = w.tmp.path().join("d");
    let plan = service.codex_data_plan(Some(dest.clone())).await.unwrap();
    assert_eq!(plan.blockers[0].key, "switch.codex_data.running");
    let running = plan.blockers[0]
        .args
        .iter()
        .find(|(k, _)| k == "processes")
        .map(|(_, v)| v.to_string())
        .unwrap();
    assert!(running.contains("ChatGPT (100)"), "{running}");
    let error = service
        .codex_data_move(Some(dest.clone()), quiet())
        .await
        .unwrap_err();
    assert_eq!(error.detail(), "data.codex.running");
    assert!(!dest.exists());
    assert!(!is_link(&home.sessions_dir()));
}

#[tokio::test]
async fn a_codex_cli_alone_also_blocks_the_move() {
    let (w, dest) = world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(fakes::app_server(321, 1, fakes::T0));
    w.app.processes.lock().unwrap()[0].kind = superba_core::CodexProcessKind::CliTui;
    let error = w
        .switcher()
        .codex_data_move(Some(dest), quiet())
        .await
        .unwrap_err();
    assert_eq!(error.detail(), "data.codex.running");
}

#[tokio::test]
async fn moved_sessions_are_still_found_by_superba_and_codex_paths() {
    let (w, dest) = world();
    let service = w.switcher();
    let home = w.ctx.codex_home();
    let migrator = CodexSessionMigrator::new(w.ctx.data_dir.clone());
    let before = migrator.scan(&home).await.unwrap();
    assert_eq!(before.distribution().get("openai"), Some(&2));

    let plan = service.codex_data_plan(Some(dest.clone())).await.unwrap();
    assert!(plan.ready(), "{:?}", plan.blockers);
    assert_eq!(plan.moves.len(), 2);
    let report = service
        .codex_data_move(Some(dest.clone()), quiet())
        .await
        .unwrap();
    assert!(report.leftovers.is_empty());
    assert!(is_link(&home.sessions_dir()));

    // Superba's session scan (sessions page, migrations, usage) follows the links.
    let after = migrator.scan(&home).await.unwrap();
    assert_eq!(after.distribution(), before.distribution());
    let location = service.codex_data_where().await.unwrap();
    assert_eq!(location.directory.as_deref(), Some(dest.as_path()));
    assert_eq!(location.dirs[0].files, 1);
    let doctor = service.doctor().await.unwrap();
    assert!(
        doctor
            .iter()
            .any(|f| f.id == "codex_data_moved" && f.severity == Severity::Info)
    );

    // The drive goes away: doctor says so loudly.
    let parked = w.tmp.path().join("parked");
    fs::rename(&dest, &parked).unwrap();
    let doctor = service.doctor().await.unwrap();
    let unreachable = doctor
        .iter()
        .find(|f| f.id == "codex_data_unreachable")
        .expect("unreachable finding");
    assert_eq!(unreachable.severity, Severity::Danger);
    assert!(!doctor.iter().any(|f| f.id == "codex_data_moved"));
    fs::rename(&parked, &dest).unwrap();

    // And back into the Codex home.
    service.codex_data_move(None, quiet()).await.unwrap();
    assert!(!is_link(&home.sessions_dir()));
    assert_eq!(
        migrator.scan(&home).await.unwrap().distribution(),
        before.distribution()
    );
    let location = service.codex_data_where().await.unwrap();
    assert!(
        location
            .dirs
            .iter()
            .all(|d| d.placement == CodexDirPlacement::InHome)
    );
}

#[tokio::test]
async fn an_interrupted_move_is_rolled_back_by_start_up_recovery() {
    let (w, dest) = world();
    let home = w.ctx.codex_home();
    let fs_ops = CodexDataFs {
        link: Arc::new(|_, _| panic!("simulated crash")),
        ..CodexDataFs::default()
    };
    let scope = Scope {
        home: &home,
        data: &w.ctx.data_dir,
        env: w.ctx.env.as_ref(),
        fs: &fs_ops,
    };
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        codex_data::execute(scope, Some(&dest), &quiet())
    }));
    assert!(crashed.is_err());
    let service = w.switcher();
    let plan = service.codex_data_plan(Some(dest.clone())).await.unwrap();
    assert!(plan.blockers.iter().any(|b| b.key == "data.codex.pending"));
    assert!(
        service
            .doctor()
            .await
            .unwrap()
            .iter()
            .any(|f| f.id == "codex_data_pending")
    );
    let report = service.recover().await.unwrap();
    assert!(
        report
            .notices
            .iter()
            .any(|n| n.text.key == "switch.codex_data.rolled_back"),
        "{:?}",
        report.notices
    );
    assert!(!dest.exists());
    assert!(home.sessions_dir().is_dir() && !is_link(&home.sessions_dir()));
    assert!(service.codex_data_plan(Some(dest)).await.unwrap().ready());
}
