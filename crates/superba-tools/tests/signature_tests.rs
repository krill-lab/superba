//! A trusted signature with the wrong publisher must never authorize an installer.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use superba_core::{Error, NetworkSettings, NoToolProgress, ToolId, ToolOp, ToolRequest};
use superba_tools::platform::{Arch, Os, Platform};
use superba_tools::{ToolHost, ToolManager};
use support::{Mock, Route, context, sources};

#[derive(Debug, Default)]
struct WrongPublisher(Mutex<Vec<PathBuf>>);

#[async_trait]
impl ToolHost for WrongPublisher {
    async fn verify_signature(&self, path: &Path) -> superba_core::Result<String> {
        assert!(
            path.is_file(),
            "signature check must receive the downloaded installer"
        );
        self.0.lock().unwrap().push(path.to_owned());
        Ok("CN=Unrelated Software Company, O=Unrelated Software Company".into())
    }
}

#[tokio::test]
async fn workbuddy_ai_without_checksum_rejects_the_wrong_signer_before_installing() {
    let mock = Mock::start().await;
    mock.route(
        "/wbintl/v2/update",
        Route::ok(
            json!({
                "version": "5.8.4.1234",
                "url": mock.url("/WorkBuddyAI-setup.exe"),
                "sha256hash": ""
            })
            .to_string(),
        ),
    );
    mock.route(
        "/WorkBuddyAI-setup.exe",
        Route::ok(b"untrusted installer".to_vec()),
    );
    let root = tempfile::tempdir().unwrap();
    let host = Arc::new(WrongPublisher::default());
    let tm = ToolManager::with(
        &context(root.path(), &[]),
        NetworkSettings::default(),
        host.clone(),
        Platform::new(Os::Windows, Arch::X64),
        sources(&mock.base),
    );
    let err = tm
        .run(
            ToolRequest::new(ToolId::WorkbuddyAi, ToolOp::Install),
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Invalid { message } if message.contains("Unrelated Software Company") && message.contains("not by") && message.contains("refused")),
        "{err}"
    );
    let checked = host.0.lock().unwrap();
    assert_eq!(checked.len(), 1);
    assert!(!checked[0].exists(), "rejected installer is removed");
    assert!(
        superba_tools::state::State::load(&tm.ctx().data)
            .tool(ToolId::WorkbuddyAi)
            .installed
            .is_none()
    );
    let log =
        std::fs::read_to_string(tm.ctx().data.root().join("logs/tools/workbuddy-ai.log")).unwrap();
    assert!(
        log.contains("signature valid:") && log.contains("refused"),
        "{log}"
    );
    assert!(
        !log.contains("start:"),
        "the installer must never be started: {log}"
    );
}
