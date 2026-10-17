#![allow(missing_docs)]

use std::path::PathBuf;
use std::sync::Mutex;

use superba_core::*;

#[test]
fn consumers_stopped_records_home_root_and_round_trips() {
    let home = CodexHome::new("/tmp/h/.codex").with_sqlite_home("/tmp/elsewhere");
    let token = ConsumersStopped::new(&home, Timestamp(1_700_000_000_000));
    assert_eq!(token.codex_home, PathBuf::from("/tmp/h/.codex"));
    assert_eq!(token.checked_at, Timestamp(1_700_000_000_000));
    let json = serde_json::to_string(&token).unwrap();
    let back: ConsumersStopped = serde_json::from_str(&json).unwrap();
    assert_eq!(back, token);
}

/// Records which writing method was reached.
#[derive(Default)]
struct Recorder(Mutex<Vec<&'static str>>);

#[async_trait]
impl SessionMigrator for Recorder {
    async fn scan(&self, _home: &CodexHome) -> Result<SessionReport> {
        Err(Error::not_implemented("scan"))
    }
    async fn plan(&self, _home: &CodexHome, _r: &MigrationRequest) -> Result<MigrationPlan> {
        Err(Error::not_implemented("plan"))
    }
    async fn migrate(&self, _home: &CodexHome, _p: &MigrationPlan) -> Result<MigrationReceipt> {
        self.0.lock().unwrap().push("migrate");
        Err(Error::not_implemented("migrate"))
    }
    async fn rollback(&self, _home: &CodexHome, _id: &str) -> Result<RollbackReport> {
        self.0.lock().unwrap().push("rollback");
        Ok(RollbackReport::default())
    }
    async fn history(&self, _home: &CodexHome) -> Result<Vec<MigrationReceipt>> {
        Ok(Vec::new())
    }
    async fn recover(&self, _home: &CodexHome) -> Result<RollbackReport> {
        self.0.lock().unwrap().push("recover");
        Ok(RollbackReport::default())
    }
}

#[tokio::test]
async fn stopped_variants_default_to_the_plain_methods() {
    let home = CodexHome::new("/tmp/h/.codex");
    let token = ConsumersStopped::new(&home, Timestamp(1));
    let recorder = std::sync::Arc::new(Recorder::default());
    let migrator: std::sync::Arc<dyn SessionMigrator> = recorder.clone();
    let plan = MigrationPlan {
        id: "m".into(),
        codex_home: home.root().to_path_buf(),
        request: MigrationRequest {
            to_provider: "openai".into(),
            selectors: vec![],
            include_archived: true,
            exclude: vec![],
        },
        thread_count: 0,
        rollout_count: 0,
        by_source_provider: Default::default(),
        backup_dir: PathBuf::from("/tmp/b"),
        backup_bytes: 0,
        items: vec![],
    };
    assert!(
        migrator
            .migrate_stopped(&home, &plan, &token)
            .await
            .is_err()
    );
    assert!(migrator.rollback_stopped(&home, "m", &token).await.is_ok());
    assert!(migrator.recover_stopped(&home, &token).await.is_ok());
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec!["migrate", "rollback", "recover"]
    );
}
