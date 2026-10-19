//! Data relocation orchestration. The caller holds the exclusive location lock
//! and has drained GUI/CLI operations and environment probes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use superba_core::data::{
    DataCleanupReport, DataLocation, DataMovePhase, DataMoveProgress, DataMoveProgressSink,
    DataMoveReport,
};
use superba_core::{AppCodexPin, DataDir, Error, LocalContext, Result, SwitchService};
use superba_router::manager::RouterManager;
use superba_storage::{MoveJournal, PreparedMove};

use crate::Switcher;

pub(crate) fn location(ctx: &LocalContext) -> DataLocation {
    DataLocation {
        directory: ctx.data_dir.root().to_path_buf(),
        pointer: ctx.data_dir.anchor().join(superba_storage::LOCATION_FILE),
        codex_home: ctx.codex_home().root().to_path_buf(),
    }
}

#[async_trait]
trait DataRouter: Send + Sync {
    async fn active(&self, dir: &DataDir) -> Result<bool>;
    async fn stop(&self, dir: &DataDir) -> Result<()>;
    async fn start(&self, dir: &DataDir) -> Result<()>;
}

struct SystemDataRouter {
    home: PathBuf,
}

impl SystemDataRouter {
    fn manager(&self, data: &DataDir) -> RouterManager {
        RouterManager::system(
            data,
            self.home.clone(),
            superba_router::manager::current_router_exe(),
        )
    }
}

#[async_trait]
impl DataRouter for SystemDataRouter {
    async fn active(&self, dir: &DataDir) -> Result<bool> {
        let manager = self.manager(dir);
        if !manager.paths().config_json().exists() {
            return Ok(false);
        }
        let overview = manager.overview().await;
        Ok(overview.liveness.is_up() || overview.service?.installed)
    }
    async fn stop(&self, dir: &DataDir) -> Result<()> {
        self.manager(dir)
            .stop_for_data_move(Duration::from_secs(15))
            .await
    }
    async fn start(&self, dir: &DataDir) -> Result<()> {
        self.manager(dir)
            .install_existing(crate::router::ROUTER_START_WAIT)
            .await
    }
}

fn emit(progress: &DataMoveProgressSink, phase: DataMovePhase, bytes: u64) -> Result<()> {
    if progress(DataMoveProgress {
        phase,
        copied_bytes: if phase == DataMovePhase::Copying {
            0
        } else {
            bytes
        },
        total_bytes: bytes,
    }) {
        Ok(())
    } else {
        Err(Error::Cancelled)
    }
}

pub(crate) async fn move_data(
    service: &Switcher,
    destination: PathBuf,
    progress: DataMoveProgressSink,
) -> Result<DataMoveReport> {
    service.refuse_elevated()?;
    let ctx = &service.deps().ctx;
    let profiles = service.deps().profiles.load().await?;
    // A pinned program may be named by a running app, launchctl or a shell rc
    // file outside Superba's data. Require unpinning before deleting that path.
    if !matches!(profiles.settings.app_codex.pin, AppCodexPin::Bundled) {
        return Err(Error::invalid("data.pin"));
    }
    // The relocation never rewrites Codex itself. It cannot safely contain the
    // live Codex home in the source or destination tree.
    let codex = ctx.codex_home();
    let codex_root = codex
        .root()
        .canonicalize()
        .unwrap_or_else(|_| codex.root().to_path_buf());
    if codex_root.starts_with(ctx.data_dir.root()) || ctx.data_dir.root().starts_with(&codex_root) {
        return Err(Error::invalid("data.path.overlap"));
    }
    // Finish any interrupted switch before its receipts change directories.
    // recover itself takes state.lock, so pending switch journals are refused
    // here; the existing startup recovery can finish them before retrying.
    for sub in ["journal/config", "journal/switch"] {
        let dir = ctx.data_dir.root().join(sub);
        if dir.is_dir() {
            for entry in fs::read_dir(&dir).map_err(|e| Error::io(&dir, &e))? {
                let path = entry.map_err(|e| Error::io(&dir, &e))?.path();
                if path.extension().is_some_and(|e| e == "json") {
                    let value: serde_json::Value =
                        serde_json::from_slice(&fs::read(&path).map_err(|e| Error::io(&path, &e))?)
                            .map_err(|e| Error::parse("switch journal", e.to_string()))?;
                    if value
                        .get("status")
                        .or_else(|| value.get("phase"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| {
                            matches!(s, "applying" | "rolling_back" | "pending" | "publishing")
                        })
                    {
                        return Err(Error::invalid("data.pending_switch"));
                    }
                }
            }
        }
    }
    // StateLock is redundant with the location lock for new clients, but guards
    // against an already running older client. Keep it through copying and commit.
    let router = SystemDataRouter {
        home: ctx.home_dir.clone(),
    };
    move_with_router(ctx, &destination, &progress, &router).await
}

async fn move_with_router(
    ctx: &LocalContext,
    destination: &Path,
    progress: &DataMoveProgressSink,
    router: &dyn DataRouter,
) -> Result<DataMoveReport> {
    let state_lock = crate::journal::StateLock::acquire(&ctx.data_dir)?;
    emit(progress, DataMovePhase::Checking, 0)?;
    // The dedicated move thread performs blocking file IO without occupying an
    // egui thread or releasing the transaction lock on cancellation.
    let mut prepared = PreparedMove::prepare(ctx, destination)?;
    let restart = router.active(&ctx.data_dir).await?;
    prepared.begin(restart)?;
    let result: Result<()> = async {
        emit(progress, DataMovePhase::Stopping, prepared.total_bytes())?;
        if restart {
            router.stop(&ctx.data_dir).await?;
        }
        prepared.copy(progress)?;
        emit(progress, DataMovePhase::Switching, prepared.total_bytes())?;
        prepared.publish()?;
        if restart {
            // Once published, ignore cancellation until either validation or
            // rollback finishes. The caller never observes a half-completed move.
            let _ = emit(progress, DataMovePhase::Restarting, prepared.total_bytes());
            router
                .start(&DataDir::at_location(
                    &prepared.journal.target,
                    ctx.data_dir.anchor(),
                ))
                .await?;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = emit(progress, DataMovePhase::RollingBack, prepared.total_bytes());
        prepared.journal.rollback = true;
        prepared.journal.write(ctx.data_dir.anchor())?;
        if let Err(rollback) = rollback(ctx, &prepared.journal, router).await {
            return Err(Error::other(format!(
                "{}; data rollback: {}",
                error.detail(),
                rollback.detail()
            )));
        }
        return Err(error);
    }
    let _ = emit(progress, DataMovePhase::Cleaning, prepared.total_bytes());
    drop(state_lock);
    let pending = finish(ctx, &prepared.journal)?;
    let mut moved = ctx.clone();
    moved.data_dir = DataDir::at_location(&prepared.journal.target, ctx.data_dir.anchor());
    Ok(DataMoveReport {
        location: location(&moved),
        previous: prepared.journal.source.clone(),
        copied_bytes: prepared.total_bytes(),
        source_cleanup_pending: pending,
    })
}

fn finish(ctx: &LocalContext, journal: &MoveJournal) -> Result<bool> {
    let pending = match superba_storage::cleanup_source(&journal.source, ctx.data_dir.anchor()) {
        Ok(()) => false,
        Err(error) => {
            tracing::warn!(error = %error, "old data copies could not all be removed");
            true
        }
    };
    MoveJournal::clear(ctx.data_dir.anchor())?;
    journal.forget_owner();
    Ok(pending)
}

async fn rollback(
    ctx: &LocalContext,
    journal: &MoveJournal,
    router: &dyn DataRouter,
) -> Result<()> {
    let old = DataDir::at_location(&journal.source, ctx.data_dir.anchor());
    let target = DataDir::at_location(&journal.target, ctx.data_dir.anchor());
    if journal.restart_router && journal.owns_target() {
        router.stop(&target).await?;
    }
    journal.restore_pointer(ctx.data_dir.anchor())?;
    if journal.restart_router {
        router.start(&old).await?;
    }
    journal.discard_copies()?;
    MoveJournal::clear(ctx.data_dir.anchor())
}

pub(crate) async fn recover(ctx: &LocalContext) -> Result<()> {
    recover_with_router(
        ctx,
        &SystemDataRouter {
            home: ctx.home_dir.clone(),
        },
    )
    .await
}

async fn recover_with_router(ctx: &LocalContext, router: &dyn DataRouter) -> Result<()> {
    let Some(journal) = MoveJournal::read(ctx.data_dir.anchor())? else {
        return Ok(());
    };
    let current = superba_storage::resolve(ctx.data_dir.anchor())?;
    if current.root() == journal.target && !journal.rollback {
        if !journal.owns_target() {
            return Err(Error::invalid("data.journal.invalid"));
        }
        if journal.restart_router {
            router.start(&current).await?;
        }
        finish(ctx, &journal)?;
        Ok(())
    } else {
        rollback(ctx, &journal, router).await
    }
}

pub(crate) async fn cleanup(service: &Switcher) -> Result<DataCleanupReport> {
    service.refuse_elevated()?;
    let ctx = &service.deps().ctx;
    let before = superba_storage::usage(ctx);
    let settings = service.deps().profiles.load().await?.settings.router.stats;
    let cutoff = superba_core::Timestamp::from_millis(
        superba_core::Timestamp::now().as_millis()
            - i64::from(settings.usage_days.max(1)) * 86_400_000,
    );
    service
        .router_clear(superba_core::ClearRequest {
            before: Some(cutoff),
        })
        .await?;
    // Refetchable data only. Backups, imported history, installed programs and
    // downloaded installers are deliberately retained and separately measured.
    for dir in [ctx.data_dir.cache_dir(), ctx.data_dir.feeds_dir()] {
        match fs::symlink_metadata(&dir) {
            Ok(meta) if superba_storage::validation::is_link(&meta) => {
                return Err(Error::invalid("data.path.link"));
            }
            Ok(_) => fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, &e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&dir, &e)),
        }
    }
    let usage = superba_storage::usage(ctx);
    Ok(DataCleanupReport {
        reclaimed_bytes: before.total_bytes.saturating_sub(usage.total_bytes),
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use superba_core::{
        BackupSource, CodexFileKind, MapEnv, ProfileStore, SecretBytes, TargetId, Timestamp,
    };

    #[derive(Default)]
    struct Router {
        calls: Mutex<Vec<(String, PathBuf)>>,
        fail_start: AtomicBool,
        fail_stop: AtomicBool,
    }

    #[async_trait]
    impl DataRouter for Router {
        async fn active(&self, _dir: &DataDir) -> Result<bool> {
            Ok(true)
        }
        async fn stop(&self, dir: &DataDir) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(("stop".into(), dir.root().into()));
            if self.fail_stop.swap(false, Ordering::SeqCst) {
                Err(Error::other("stop failed"))
            } else {
                Ok(())
            }
        }
        async fn start(&self, dir: &DataDir) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(("start".into(), dir.root().into()));
            if self.fail_start.swap(false, Ordering::SeqCst) {
                Err(Error::other("start failed"))
            } else {
                Ok(())
            }
        }
    }

    fn fixture() -> (tempfile::TempDir, LocalContext, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let mut ctx = LocalContext::for_tests(&temp.path().canonicalize().unwrap());
        ctx.env = Arc::new(MapEnv::new().with("SUPERBA_SECRET_BACKEND", "file"));
        fs::create_dir_all(ctx.data_dir.root()).unwrap();
        fs::create_dir_all(&ctx.home_dir).unwrap();
        fs::write(ctx.data_dir.root().join("untouched.bin"), b"original data").unwrap();
        let target = temp.path().canonicalize().unwrap().join("迁移 目标");
        (temp, ctx, target)
    }

    fn progress() -> DataMoveProgressSink {
        Arc::new(|_| true)
    }

    #[tokio::test]
    async fn router_stops_before_copy_and_restarts_at_new_location() {
        let (_temp, ctx, target) = fixture();
        let router = Router::default();
        let _lock = superba_storage::DataLock::exclusive(ctx.data_dir.anchor()).unwrap();
        let report = move_with_router(&ctx, &target, &progress(), &router)
            .await
            .unwrap();
        let calls = router.calls.lock().unwrap();
        assert_eq!(
            calls.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["stop", "start"]
        );
        assert_eq!(calls[0].1, ctx.data_dir.root());
        assert_eq!(calls[1].1, report.location.directory);
        assert!(!report.source_cleanup_pending);
        assert_eq!(
            fs::read(target.join("untouched.bin")).unwrap(),
            b"original data"
        );
        assert!(!ctx.data_dir.root().join("untouched.bin").exists());
    }

    #[tokio::test]
    async fn failed_restart_rolls_back_pointer_data_and_router() {
        let (_temp, ctx, target) = fixture();
        let router = Router::default();
        router.fail_start.store(true, Ordering::SeqCst);
        let error = move_with_router(&ctx, &target, &progress(), &router)
            .await
            .unwrap_err();
        assert_eq!(error.detail(), "start failed");
        assert_eq!(
            superba_storage::resolve(ctx.data_dir.anchor())
                .unwrap()
                .root(),
            ctx.data_dir.root()
        );
        assert_eq!(
            fs::read(ctx.data_dir.root().join("untouched.bin")).unwrap(),
            b"original data"
        );
        assert!(!target.exists());
        assert!(MoveJournal::read(ctx.data_dir.anchor()).unwrap().is_none());
        let calls = router.calls.lock().unwrap();
        assert_eq!(
            calls.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["stop", "start", "stop", "start"]
        );
        assert_eq!(calls.last().unwrap().1, ctx.data_dir.root());
    }

    #[tokio::test]
    async fn failed_stop_never_copies_or_commits() {
        let (_temp, ctx, target) = fixture();
        let router = Router::default();
        router.fail_stop.store(true, Ordering::SeqCst);
        assert!(
            move_with_router(&ctx, &target, &progress(), &router)
                .await
                .is_err()
        );
        assert!(!target.exists());
        assert!(
            !ctx.data_dir
                .root()
                .join(superba_storage::LOCATION_FILE)
                .exists()
        );
        assert!(ctx.data_dir.root().join("untouched.bin").is_file());
    }

    #[tokio::test]
    async fn interrupted_copy_recovers_to_source_and_restarts_router() {
        let (_temp, ctx, target) = fixture();
        let router = Router::default();
        let mut prepared = PreparedMove::prepare(&ctx, &target).unwrap();
        prepared.begin(true).unwrap();
        prepared.copy(&progress()).unwrap();
        let stage = prepared.journal.staging.clone();
        std::mem::forget(prepared); // Simulate process death without a destructor.
        recover_with_router(&ctx, &router).await.unwrap();
        assert!(!stage.exists());
        assert!(!target.exists());
        assert!(ctx.data_dir.root().join("untouched.bin").exists());
        assert_eq!(router.calls.lock().unwrap().last().unwrap().0, "start");
    }

    #[tokio::test]
    async fn interrupted_committed_move_keeps_target_and_finishes_cleanup() {
        let (_temp, ctx, target) = fixture();
        let router = Router::default();
        let mut prepared = PreparedMove::prepare(&ctx, &target).unwrap();
        prepared.begin(true).unwrap();
        prepared.copy(&progress()).unwrap();
        prepared.publish().unwrap();
        drop(prepared);
        recover_with_router(&ctx, &router).await.unwrap();
        assert!(target.join("untouched.bin").is_file());
        assert!(!ctx.data_dir.root().join("untouched.bin").exists());
        assert!(MoveJournal::read(ctx.data_dir.anchor()).unwrap().is_none());
    }

    #[tokio::test]
    async fn two_live_services_follow_pointer_and_encrypted_backups_survive() {
        let (_temp, ctx, target) = fixture();
        let store = superba_config::codex_files::EditorStore::new(ctx.clone());
        let saved = store
            .backup(
                &TargetId::local(),
                "This computer",
                CodexFileKind::ConfigToml,
                b"# preserved backup\nmodel = 'test'\n",
                BackupSource::BeforeEdit,
                Timestamp::now(),
            )
            .unwrap();
        let one = crate::default_service(ctx.clone());
        let two = crate::default_service(ctx.clone());
        assert_eq!(
            two.data_where().await.unwrap().directory,
            ctx.data_dir.root()
        );
        let report = one.data_move(target.clone(), progress()).await.unwrap();
        assert_eq!(
            two.data_where().await.unwrap().directory,
            report.location.directory
        );
        let moved = LocalContext::new(
            ctx.home_dir.clone(),
            superba_storage::resolve(ctx.data_dir.anchor()).unwrap(),
            ctx.env.clone(),
        );
        let store = superba_config::codex_files::EditorStore::new(moved.clone());
        let restored = store
            .read(&saved.id, moved.codex_home().root(), "This computer")
            .unwrap();
        assert_eq!(
            restored.contents,
            SecretBytes::new(b"# preserved backup\nmodel = 'test'\n".to_vec())
        );
        let mut set = superba_config::JsonProfileStore::new(moved.clone())
            .load()
            .await
            .unwrap();
        set.settings.countdown_secs = 10;
        two.save_settings(set.settings).await.unwrap();
        assert!(target.join("profiles.json").exists());
        assert!(!ctx.data_dir.root().join("profiles.json").exists());
    }
}
