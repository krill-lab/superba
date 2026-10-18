//! Coding-agent tools (`SwitchService::tools`, `tool_run`, `tool_settings`):
//! the engine lives in `superba-tools`; this adds the network settings and
//! the app controller's Windows signature check.

use std::path::Path;
use std::sync::Arc;

use superba_core::{
    AppController, Result, ToolId, ToolProgressSink, ToolReport, ToolRequest, ToolRow,
    ToolSettingsChange, ToolsView,
};
use superba_tools::{ToolHost, ToolManager};

use crate::Switcher;

/// Installer signatures through the app controller (`WinVerifyTrust`).
struct AppHost(Arc<dyn AppController>);

#[async_trait::async_trait]
impl ToolHost for AppHost {
    async fn verify_signature(&self, path: &Path) -> Result<String> {
        self.0
            .verify_package_signature(path)
            .await
            .map(|s| s.subject)
    }
}

impl Switcher {
    async fn tool_manager(&self) -> ToolManager {
        let network = self
            .deps
            .profiles
            .load()
            .await
            .map(|set| set.settings.network)
            .unwrap_or_default();
        ToolManager::new(
            &self.deps.ctx,
            network,
            Arc::new(AppHost(self.deps.app.clone())),
        )
    }

    pub(crate) async fn tools_impl(&self, refresh: bool) -> Result<ToolsView> {
        self.tool_manager().await.view(refresh).await
    }

    pub(crate) async fn tool_run_impl(
        &self,
        request: ToolRequest,
        progress: Arc<dyn ToolProgressSink>,
    ) -> Result<ToolReport> {
        self.tool_manager().await.run(request, progress).await
    }

    pub(crate) async fn tool_settings_impl(
        &self,
        tool: ToolId,
        change: ToolSettingsChange,
    ) -> Result<ToolRow> {
        self.tool_manager().await.settings(tool, change).await
    }
}
