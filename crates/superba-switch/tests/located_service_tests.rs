//! Exercise the production trait object, including its data-relocation lock.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::harness::{HarnessFileSave, HarnessId, HarnessSettings};
use superba_core::{
    BackupSource, Error, LocalContext, MapEnv, NoToolProgress, ProfileId, ProfileStore, Result,
    RouterCatalogSettings, RouterModelTest, Secret, SecretBytes, SwitchService, Timestamp, ToolId,
    ToolInstallKind, ToolOp, ToolProgress, ToolProgressSink, ToolRequest, ToolSettingsChange,
    ToolStage,
};
use superba_storage::DataLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn fixture() -> (tempfile::TempDir, LocalContext, Arc<dyn SwitchService>) {
    let temp = tempfile::tempdir().unwrap();
    let mut ctx = LocalContext::for_tests(&temp.path().canonicalize().unwrap());
    ctx.env = Arc::new(MapEnv::new().with("SUPERBA_SECRET_BACKEND", "file"));
    std::fs::create_dir_all(&ctx.home_dir).unwrap();
    let profiles = superba_config::JsonProfileStore::new(ctx.clone());
    let mut set = profiles.load().await.unwrap();
    set.settings.network.online_checks = false;
    profiles.save(&set).await.unwrap();
    let service = superba_switch::default_service(ctx.clone());
    (temp, ctx, service)
}

#[tokio::test]
async fn catalog_is_saved_and_model_probe_holds_the_data_lock_until_the_reply() {
    let (_temp, ctx, service) = fixture().await;
    let mut profile: superba_core::Profile = serde_json::from_value(serde_json::json!({
        "id": "provider",
        "name": "Test provider",
        "site_id": superba_core::catalog::KRILL_SITE_ID,
        "auth_mode": superba_core::AuthMode::ApiKey,
    }))
    .unwrap();
    profile.set_auth(superba_core::AuthChoice::ApiKey);
    let profile = service
        .save_profile(profile, Some(Secret::new("delegates-test-key")))
        .await
        .unwrap();
    let profiles = superba_config::JsonProfileStore::new(ctx.clone());
    let mut set = profiles.load().await.unwrap();
    set.settings.router.members.push(profile.id);
    profiles.save(&set).await.unwrap();
    let catalog = RouterCatalogSettings {
        show_prefixed: false,
        visibility: [("provider/model".into(), true)].into(),
        ..Default::default()
    };
    service.save_router_catalog(catalog.clone()).await.unwrap();
    let saved = superba_config::JsonProfileStore::new(ctx.clone())
        .load()
        .await
        .unwrap();
    assert_eq!(saved.settings.router.catalog, catalog);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let paths = superba_router::paths::RouterPaths::new(&ctx.data_dir);
    superba_router::runtime::RuntimeInfo {
        pid: std::process::id(),
        port: listener.local_addr().unwrap().port(),
        instance_id: "delegates-test".into(),
        admin_token: Secret::new("delegates-test-admin"),
        started_at_ms: 0,
        version: "test".into(),
    }
    .save(&paths)
    .unwrap();
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let expected = RouterModelTest {
        model: "provider/model".into(),
        passed: true,
        detail: "tool round trip completed".into(),
        tested_at: 123,
    };
    let response_body = serde_json::to_string(&expected).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
            assert!(head.len() < 8192);
        }
        let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
        assert!(head.starts_with("post /_superba/v1/catalog/test http/1.1\r\n"));
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).await.unwrap();
        request_tx
            .send(serde_json::from_slice::<serde_json::Value>(&body).unwrap())
            .unwrap();
        reply_rx.await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    let call =
        tokio::spawn(async move { service.test_router_model("provider/model".into()).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        assert_eq!(
            request_rx.await.unwrap(),
            serde_json::json!({"model": "provider/model"})
        );
        // The call is suspended inside the real router client. Readers may
        // coexist, but relocation must remain excluded until the reply arrives.
        drop(DataLock::shared(ctx.data_dir.anchor()).unwrap());
        assert_busy(DataLock::exclusive(ctx.data_dir.anchor()));
        reply_tx.send(()).unwrap();
        assert_eq!(call.await.unwrap().unwrap(), expected);
        server.await.unwrap();
    })
    .await
    .unwrap();
    drop(DataLock::exclusive(ctx.data_dir.anchor()).unwrap());
}

struct Progress {
    anchor: PathBuf,
    stages: Mutex<Vec<(ToolId, ToolStage)>>,
}

impl ToolProgressSink for Progress {
    fn report(&self, progress: ToolProgress) {
        assert_busy(DataLock::exclusive(&self.anchor));
        self.stages
            .lock()
            .unwrap()
            .push((progress.tool, progress.stage));
    }
}

#[tokio::test]
async fn tools_list_settings_and_uninstall_reach_the_real_manager() {
    let (_temp, ctx, service) = fixture().await;
    let view = service.tools(false).await.unwrap();
    assert_eq!(
        view.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        ToolId::ALL
    );
    let row = service
        .tool_settings(
            ToolId::ClaudeCode,
            ToolSettingsChange {
                channel: Some("stable".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.channel.as_deref(), Some("stable"));
    assert_eq!(
        superba_tools::state::State::load(&ctx.data_dir)
            .tool(ToolId::ClaudeCode)
            .channel,
        Some("stable".into())
    );

    // A recorded standalone install needs no executable probe or network access.
    let executable = ctx
        .data_dir
        .root()
        .join("tools/pi/app")
        .join(if cfg!(windows) { "pi.exe" } else { "pi" });
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, b"test install").unwrap();
    superba_tools::state::State::update(&ctx.data_dir, ToolId::Pi, |state| {
        state.installed = Some(superba_tools::state::InstalledRecord {
            kind: ToolInstallKind::Standalone,
            version: "0.1.0".into(),
            path: executable.clone(),
            at: Timestamp::now(),
        });
    })
    .unwrap();
    let progress = Arc::new(Progress {
        anchor: ctx.data_dir.anchor().into(),
        stages: Mutex::new(Vec::new()),
    });
    let mut request = ToolRequest::new(ToolId::Pi, ToolOp::Uninstall);
    request.confirmed = true;
    let report = service.tool_run(request, progress.clone()).await.unwrap();
    assert_eq!(report.tool, ToolId::Pi);
    assert_eq!(report.op, ToolOp::Uninstall);
    assert!(!executable.exists());
    assert!(
        superba_tools::state::State::load(&ctx.data_dir)
            .tool(ToolId::Pi)
            .installed
            .is_none()
    );
    assert_eq!(
        *progress.stages.lock().unwrap(),
        [(ToolId::Pi, ToolStage::Removing)]
    );
    drop(DataLock::exclusive(ctx.data_dir.anchor()).unwrap());
}

#[tokio::test]
async fn harness_drafts_errors_edits_and_reviewed_restore_reach_the_real_writer() {
    let (_temp, ctx, service) = fixture().await;
    let settings = HarnessSettings {
        provider: Some(ProfileId::new("missing-provider")),
        ..Default::default()
    };
    let preview = service
        .harness_preview(HarnessId::Pi, settings.clone())
        .await
        .unwrap();
    assert_eq!(preview.harness, HarnessId::Pi);
    assert_eq!(preview.settings, settings);
    assert!(!ctx.data_dir.harness_config_json().exists());
    assert!(preview.files.iter().all(|file| !file.path.exists()));
    assert!(matches!(
        service.harness_models(ProfileId::new("missing-provider"), true).await,
        Err(Error::NotFound { what }) if what == "provider missing-provider"
    ));
    assert!(matches!(
        service.harness_plan_preview(HarnessId::Pi, settings).await,
        Err(Error::NotFound { what }) if what == "provider missing-provider (with a key)"
    ));

    let file = "settings.json".to_owned();
    let empty = service
        .harness_file(HarnessId::Pi, file.clone())
        .await
        .unwrap();
    let original = b"{\"theme\":\"dark\"}\n";
    std::fs::create_dir_all(empty.file.path.parent().unwrap()).unwrap();
    std::fs::write(&empty.file.path, original).unwrap();
    let doc = service
        .harness_file(HarnessId::Pi, file.clone())
        .await
        .unwrap();
    assert_eq!(doc.contents.as_ref().unwrap().expose(), original);
    let save = HarnessFileSave {
        harness: HarnessId::Pi,
        file: file.clone(),
        base_sha256: doc.sha256,
        contents: SecretBytes::new(b"{\"theme\":\"light\"}\n".to_vec()),
    };
    let saved = service.save_harness_file(save.clone()).await.unwrap();
    assert_eq!(
        std::fs::read(&saved.file.path).unwrap(),
        save.contents.expose()
    );
    assert!(matches!(
        service.save_harness_file(save).await,
        Err(Error::Conflict { .. })
    ));
    let backups = service.harness_backups(HarnessId::Pi).await.unwrap();
    let backup = backups
        .iter()
        .find(|backup| backup.source == BackupSource::BeforeEdit)
        .unwrap();
    assert_eq!(
        service
            .harness_backup_contents(backup.id.clone())
            .await
            .unwrap()
            .expose(),
        original
    );

    let changed = b"{\"theme\":\"changed after preview\"}\n";
    std::fs::write(&saved.file.path, changed).unwrap();
    assert!(matches!(
        service
            .harness_restore_reviewed_backup(HarnessId::Pi, backup.id.clone(), saved.sha256)
            .await,
        Err(Error::Conflict { .. })
    ));
    assert_eq!(std::fs::read(&saved.file.path).unwrap(), changed);
    let current = service.harness_file(HarnessId::Pi, file).await.unwrap();
    let restored = service
        .harness_restore_reviewed_backup(HarnessId::Pi, backup.id.clone(), current.sha256)
        .await
        .unwrap();
    assert_eq!(restored.harness, HarnessId::Pi);
    assert_eq!(std::fs::read(&saved.file.path).unwrap(), original);
}

fn assert_busy<T>(result: Result<T>) {
    assert!(matches!(result, Err(Error::Locked { what }) if what == "data.busy"));
}

#[tokio::test]
async fn every_added_delegate_refuses_calls_while_data_is_exclusively_locked() {
    let (_temp, ctx, service) = fixture().await;
    let _exclusive = DataLock::exclusive(ctx.data_dir.anchor()).unwrap();
    assert_busy(service.save_router_catalog(Default::default()).await);
    assert_busy(service.test_router_model("model".into()).await);
    assert_busy(service.tools(false).await);
    assert_busy(
        service
            .tool_run(
                ToolRequest::new(ToolId::Pi, ToolOp::Install),
                Arc::new(NoToolProgress),
            )
            .await,
    );
    assert_busy(service.tool_settings(ToolId::Pi, Default::default()).await);
    assert_busy(
        service
            .harness_preview(HarnessId::Pi, Default::default())
            .await,
    );
    assert_busy(
        service
            .harness_models(ProfileId::new("missing"), false)
            .await,
    );
    assert_busy(
        service
            .harness_file(HarnessId::Pi, "settings.json".into())
            .await,
    );
    assert_busy(
        service
            .save_harness_file(HarnessFileSave {
                harness: HarnessId::Pi,
                file: "settings.json".into(),
                base_sha256: None,
                contents: SecretBytes::new(b"{}".to_vec()),
            })
            .await,
    );
    assert_busy(service.harness_backup_contents("backup".into()).await);
    assert_busy(
        service
            .harness_plan_preview(HarnessId::Pi, Default::default())
            .await,
    );
    assert_busy(
        service
            .harness_restore_reviewed_backup(HarnessId::Pi, "backup".into(), None)
            .await,
    );
    assert!(!ctx.home_dir.join(".pi").exists());
}
