//! Every production entry holds the shared data lock for its entire operation.
//! Keep these forwards aligned with SwitchService when adding a service method.

use super::LocatedService;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use superba_core::*;

#[async_trait]
impl SwitchService for LocatedService {
    async fn overview(&self) -> Result<Overview> {
        let (service, _lock) = self.read().await?;
        service.overview().await
    }

    async fn sites(&self) -> Result<Vec<Site>> {
        let (service, _lock) = self.read().await?;
        service.sites().await
    }

    async fn profile_set(&self) -> Result<ProfileSet> {
        let (service, _lock) = self.read().await?;
        service.profile_set().await
    }

    async fn save_profile(&self, profile: Profile, key: Option<Secret>) -> Result<Profile> {
        let (service, _lock) = self.read().await?;
        service.save_profile(profile, key).await
    }

    async fn delete_profile(&self, id: &ProfileId) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.delete_profile(id).await
    }

    async fn save_settings(&self, settings: Settings) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.save_settings(settings).await
    }

    async fn autostart_status(&self) -> Result<autostart::AutostartStatus> {
        let (service, _lock) = self.read().await?;
        service.autostart_status().await
    }

    async fn set_autostart(&self, enabled: bool) -> Result<autostart::AutostartStatus> {
        let (service, _lock) = self.read().await?;
        service.set_autostart(enabled).await
    }

    async fn save_site(&self, site: Site) -> Result<Site> {
        let (service, _lock) = self.read().await?;
        service.save_site(site).await
    }

    async fn delete_site(&self, id: &SiteId) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.delete_site(id).await
    }

    async fn probe_lines(&self, site_id: &SiteId) -> Result<Vec<LineCaps>> {
        let (service, _lock) = self.read().await?;
        service.probe_lines(site_id).await
    }

    async fn check_key(
        &self,
        site_id: &SiteId,
        line_id: &LineId,
        mode: AuthMode,
        key: KeyInput,
    ) -> Result<KeyCheck> {
        let (service, _lock) = self.read().await?;
        service.check_key(site_id, line_id, mode, key).await
    }

    async fn line_transports(&self, request: LineTransportsRequest) -> Result<Vec<LineTransports>> {
        let (service, _lock) = self.read().await?;
        service.line_transports(request).await
    }

    async fn provider_compaction(
        &self,
        request: superba_core::CompactionRequest,
    ) -> Result<superba_core::ProviderCompaction> {
        let (service, _lock) = self.read().await?;
        service.provider_compaction(request).await
    }

    async fn probe_site(&self, request: SiteProbeRequest) -> Result<SiteProbeReport> {
        let (service, _lock) = self.read().await?;
        service.probe_site(request).await
    }

    async fn plan_settings(&self, plan_id: &str) -> Result<PlanSettings> {
        let (service, _lock) = self.read().await?;
        service.plan_settings(plan_id).await
    }

    async fn settings_view(&self, request: SettingsViewRequest) -> Result<SettingsView> {
        let (service, _lock) = self.read().await?;
        service.settings_view(request).await
    }

    async fn edit_settings(&self, edit: SettingsEdit) -> Result<SettingsEditReport> {
        let (service, _lock) = self.read().await?;
        service.edit_settings(edit).await
    }

    async fn router_status(&self) -> Result<RouterView> {
        let (service, _lock) = self.read().await?;
        service.router_status().await
    }

    async fn router_install(&self, request: RouterInstallRequest) -> Result<RouterView> {
        let (service, _lock) = self.read().await?;
        service.router_install(request).await
    }

    async fn router_set_enabled(&self, enabled: bool) -> Result<RouterView> {
        let (service, _lock) = self.read().await?;
        service.router_set_enabled(enabled).await
    }

    async fn router_uninstall(&self) -> Result<RouterView> {
        let (service, _lock) = self.read().await?;
        service.router_uninstall().await
    }

    async fn router_fall_back(&self, reason: String) -> Result<Option<SwitchOutcome>> {
        let (service, _lock) = self.read().await?;
        service.router_fall_back(reason).await
    }

    async fn router_watch(&self) -> Result<RouterWatch> {
        let (service, _lock) = self.read().await?;
        service.router_watch().await
    }

    async fn router_doctor(&self) -> Result<superba_core::RouterDoctorReport> {
        let (service, _lock) = self.read().await?;
        service.router_doctor().await
    }

    async fn account(&self, request: AccountRequest) -> Result<AccountReport> {
        let (service, _lock) = self.read().await?;
        service.account(request).await
    }

    async fn self_update_view(&self) -> Result<superba_core::SelfUpdateView> {
        let (service, _lock) = self.read().await?;
        service.self_update_view().await
    }

    async fn self_update_check(
        &self,
        request: superba_core::SelfUpdateCheckRequest,
    ) -> Result<superba_core::SelfUpdateView> {
        let (service, _lock) = self.read().await?;
        service.self_update_check(request).await
    }

    async fn self_update_skip(
        &self,
        version: Option<String>,
    ) -> Result<superba_core::SelfUpdateView> {
        let (service, _lock) = self.read().await?;
        service.self_update_skip(version).await
    }

    async fn self_update_install(
        &self,
        request: superba_core::SelfUpdateInstallRequest,
    ) -> Result<superba_core::SelfUpdateReport> {
        let (service, _lock) = self.read().await?;
        service.self_update_install(request).await
    }

    async fn self_update_acknowledge(&self) -> Result<superba_core::SelfUpdateView> {
        let (service, _lock) = self.read().await?;
        service.self_update_acknowledge().await
    }

    async fn self_update_restart_router(&self) -> Result<bool> {
        let (service, _lock) = self.read().await?;
        service.self_update_restart_router().await
    }

    async fn import_scan(
        &self,
        request: superba_core::ImportScanRequest,
    ) -> Result<superba_core::ImportScan> {
        let (service, _lock) = self.read().await?;
        service.import_scan(request).await
    }

    async fn import_apply(
        &self,
        request: superba_core::ImportApplyRequest,
    ) -> Result<superba_core::ImportReport> {
        let (service, _lock) = self.read().await?;
        service.import_apply(request).await
    }

    async fn import_usage(
        &self,
        request: superba_core::UsageImportRequest,
    ) -> Result<superba_core::UsageImportReport> {
        let (service, _lock) = self.read().await?;
        service.import_usage(request).await
    }

    async fn models_view(&self, request: ModelsViewRequest) -> Result<ModelsView> {
        let (service, _lock) = self.read().await?;
        service.models_view(request).await
    }

    async fn save_models(&self, edit: ModelsEdit) -> Result<ModelsEditReport> {
        let (service, _lock) = self.read().await?;
        service.save_models(edit).await
    }

    async fn hosts(&self) -> Result<Vec<HostView>> {
        let (service, _lock) = self.read().await?;
        service.hosts().await
    }

    async fn hosts_now(&self) -> Result<Vec<HostView>> {
        let (service, _lock) = self.read().await?;
        service.hosts_now().await
    }

    async fn host_probe(&self, target_id: &TargetId) -> Result<HostView> {
        let (service, _lock) = self.read().await?;
        service.host_probe(target_id).await
    }

    async fn set_host_follow(&self, target_id: &TargetId, follow: bool) -> Result<HostView> {
        let (service, _lock) = self.read().await?;
        service.set_host_follow(target_id, follow).await
    }

    async fn host_sync_plan(&self, target_id: &TargetId) -> Result<HostSyncPlan> {
        let (service, _lock) = self.read().await?;
        service.host_sync_plan(target_id).await
    }

    async fn host_sessions(&self, target_id: &TargetId) -> Result<HostSessions> {
        let (service, _lock) = self.read().await?;
        service.host_sessions(target_id).await
    }

    async fn wsl_overview(&self) -> Result<superba_core::wsl::WslOverview> {
        let (service, _lock) = self.read().await?;
        service.wsl_overview().await
    }

    async fn host_migrate_sessions(
        &self,
        target_id: &TargetId,
        request: &superba_core::sessions::MigrationRequest,
        locale: superba_core::Locale,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<superba_core::remote::HostMigrationReport> {
        let (service, _lock) = self.read().await?;
        service
            .host_migrate_sessions(target_id, request, locale, events)
            .await
    }

    async fn wsl_router_access(
        &self,
        target_id: &TargetId,
    ) -> Result<superba_core::wsl::WslAccessReport> {
        let (service, _lock) = self.read().await?;
        service.wsl_router_access(target_id).await
    }

    async fn versions(&self, refresh: bool) -> Result<VersionsView> {
        let (service, _lock) = self.read().await?;
        service.versions(refresh).await
    }

    async fn install_codex(&self, request: InstallRequest) -> Result<InstallReport> {
        let (service, _lock) = self.read().await?;
        service.install_codex(request).await
    }

    async fn install_codex_controlled(
        &self,
        request: InstallRequest,
        control: Arc<UpdateControl>,
    ) -> Result<InstallReport> {
        let (service, _lock) = self.read().await?;
        service.install_codex_controlled(request, control).await
    }

    async fn self_update_install_controlled(
        &self,
        request: SelfUpdateInstallRequest,
        control: Arc<UpdateControl>,
    ) -> Result<SelfUpdateReport> {
        let (service, _lock) = self.read().await?;
        service
            .self_update_install_controlled(request, control)
            .await
    }

    async fn rollback_codex(&self, target: InstallTarget) -> Result<InstallReport> {
        let (service, _lock) = self.read().await?;
        service.rollback_codex(target).await
    }

    async fn download_app_update(&self) -> Result<AppDownloadReport> {
        let (service, _lock) = self.read().await?;
        service.download_app_update().await
    }

    async fn download_app_package(
        &self,
        request: AppDownloadRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        let (service, _lock) = self.read().await?;
        service.download_app_package(request, progress).await
    }

    async fn open_app_package(&self, path: PathBuf) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.open_app_package(path).await
    }

    async fn router_requests(&self, query: RequestQuery) -> Result<RequestPage> {
        let (service, _lock) = self.read().await?;
        service.router_requests(query).await
    }

    async fn router_request(&self, id: i64) -> Result<RequestDetail> {
        let (service, _lock) = self.read().await?;
        service.router_request(id).await
    }

    async fn router_inflight(&self) -> Result<Vec<InflightRequest>> {
        let (service, _lock) = self.read().await?;
        service.router_inflight().await
    }

    async fn router_stats(&self, query: StatsQuery) -> Result<RequestStats> {
        let (service, _lock) = self.read().await?;
        service.router_stats(query).await
    }

    async fn router_clear(&self, request: ClearRequest) -> Result<ClearReport> {
        let (service, _lock) = self.read().await?;
        service.router_clear(request).await
    }

    async fn router_set_stats(&self, settings: RouterStatsSettings) -> Result<RouterView> {
        let (service, _lock) = self.read().await?;
        service.router_set_stats(settings).await
    }

    async fn set_ui_locale(&self, locale: superba_core::i18n::Locale) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.set_ui_locale(locale).await
    }

    async fn session_titles(&self, keys: Vec<String>) -> Result<BTreeMap<String, SessionTitle>> {
        let (service, _lock) = self.read().await?;
        service.session_titles(keys).await
    }

    async fn price_table(&self) -> Result<PriceTable> {
        let (service, _lock) = self.read().await?;
        service.price_table().await
    }

    async fn feeds_status(&self) -> Result<FeedsStatus> {
        let (service, _lock) = self.read().await?;
        service.feeds_status().await
    }

    async fn refresh_feeds(&self) -> Result<FeedsStatus> {
        let (service, _lock) = self.read().await?;
        service.refresh_feeds().await
    }

    async fn sessions(&self) -> Result<(SessionReport, Vec<MigrationReceipt>)> {
        let (service, _lock) = self.read().await?;
        service.sessions().await
    }

    async fn doctor(&self) -> Result<Vec<DoctorFinding>> {
        let (service, _lock) = self.read().await?;
        service.doctor().await
    }

    async fn runtime(&self) -> Result<superba_core::runtime::RuntimeView> {
        let (service, _lock) = self.read().await?;
        service.runtime().await
    }

    async fn stop_processes(
        &self,
        processes: Vec<ProcessInfo>,
        force: bool,
    ) -> Result<Vec<superba_core::runtime::StopReport>> {
        let (service, _lock) = self.read().await?;
        service.stop_processes(processes, force).await
    }

    async fn codex_file(
        &self,
        target: TargetId,
        kind: superba_core::config::CodexFileKind,
    ) -> Result<superba_core::codex_files::CodexFileDoc> {
        let (service, _lock) = self.read().await?;
        service.codex_file(target, kind).await
    }

    async fn save_codex_file(
        &self,
        save: superba_core::codex_files::CodexFileSave,
    ) -> Result<superba_core::codex_files::CodexFileSaved> {
        let (service, _lock) = self.read().await?;
        service.save_codex_file(save).await
    }

    async fn codex_file_backups(
        &self,
        query: superba_core::codex_files::CodexFileBackupQuery,
    ) -> Result<Vec<superba_core::codex_files::CodexFileBackup>> {
        let (service, _lock) = self.read().await?;
        service.codex_file_backups(query).await
    }

    async fn codex_file_backup(
        &self,
        id: &str,
    ) -> Result<superba_core::codex_files::CodexFileBackupContents> {
        let (service, _lock) = self.read().await?;
        service.codex_file_backup(id).await
    }

    async fn claude_view(&self) -> Result<superba_core::claude::ClaudeView> {
        let (service, _lock) = self.read().await?;
        service.claude_view().await
    }

    async fn save_claude_config(
        &self,
        config: superba_core::claude::ClaudeConfig,
    ) -> Result<superba_core::claude::ClaudeView> {
        let (service, _lock) = self.read().await?;
        service.save_claude_config(config).await
    }

    async fn claude_models(
        &self,
        request: superba_core::claude::ClaudeModelsRequest,
    ) -> Result<superba_core::claude::ClaudeModelsView> {
        let (service, _lock) = self.read().await?;
        service.claude_models(request).await
    }

    async fn claude_plan(
        &self,
        kind: superba_core::claude::ClaudePlanKind,
    ) -> Result<superba_core::claude::ClaudePlan> {
        let (service, _lock) = self.read().await?;
        service.claude_plan(kind).await
    }

    async fn claude_apply(
        &self,
        plan: superba_core::claude::ClaudePlan,
        take_over: bool,
    ) -> Result<superba_core::claude::ClaudeApplyReport> {
        let (service, _lock) = self.read().await?;
        service.claude_apply(plan, take_over).await
    }

    async fn claude_diagnostics(&self) -> Result<superba_core::claude::ClaudeDiagnostics> {
        let (service, _lock) = self.read().await?;
        service.claude_diagnostics().await
    }

    async fn claude_probe(
        &self,
        model: Option<String>,
    ) -> Result<superba_core::claude::ClaudeProbe> {
        let (service, _lock) = self.read().await?;
        service.claude_probe(model).await
    }

    async fn notify_save_channel(
        &self,
        channel: superba_core::NotifyChannel,
        secrets: superba_core::NotifySecrets,
    ) -> Result<superba_core::NotifyChannel> {
        let (service, _lock) = self.read().await?;
        service.notify_save_channel(channel, secrets).await
    }

    async fn notify_delete_channel(&self, id: &str) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.notify_delete_channel(id).await
    }

    async fn notify_send(
        &self,
        event: superba_core::NotifyEvent,
        options: superba_core::SendOptions,
    ) -> Result<superba_core::NotifyReport> {
        let (service, _lock) = self.read().await?;
        service.notify_send(event, options).await
    }

    async fn notify_hook(
        &self,
        harness: &str,
        payload: &str,
    ) -> Result<superba_core::NotifyReport> {
        let (service, _lock) = self.read().await?;
        service.notify_hook(harness, payload).await
    }

    async fn notify_hooks(&self) -> Result<superba_core::NotifyHooks> {
        let (service, _lock) = self.read().await?;
        service.notify_hooks().await
    }

    async fn notify_set_hook(
        &self,
        request: superba_core::HookRequest,
    ) -> Result<superba_core::NotifyHooks> {
        let (service, _lock) = self.read().await?;
        service.notify_set_hook(request).await
    }

    async fn notify_watch(&self) -> Result<superba_core::NotifyReport> {
        let (service, _lock) = self.read().await?;
        service.notify_watch().await
    }

    async fn open_app(&self) -> Result<AppSession> {
        let (service, _lock) = self.read().await?;
        service.open_app().await
    }

    async fn recover(&self) -> Result<RecoveryReport> {
        let (service, _lock) = self.read().await?;
        service.recover().await
    }

    async fn plan(&self, request: SwitchRequest) -> Result<SwitchPlan> {
        let (service, _lock) = self.read().await?;
        service.plan(request).await
    }

    async fn plan_with_events(
        &self,
        request: SwitchRequest,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchPlan> {
        let (service, _lock) = self.read().await?;
        service.plan_with_events(request, events).await
    }

    async fn execute(
        &self,
        plan: SwitchPlan,
        options: ExecuteOptions,
        decider: Arc<dyn SwitchDecider>,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome> {
        let (service, _lock) = self.read().await?;
        service.execute(plan, options, decider, events).await
    }

    async fn harness_list(&self) -> Result<Vec<superba_core::harness::HarnessSummary>> {
        let (service, _lock) = self.read().await?;
        service.harness_list().await
    }

    async fn harness_view(
        &self,
        harness: superba_core::harness::HarnessId,
    ) -> Result<superba_core::harness::HarnessView> {
        let (service, _lock) = self.read().await?;
        service.harness_view(harness).await
    }

    async fn save_harness_settings(
        &self,
        harness: superba_core::harness::HarnessId,
        settings: superba_core::harness::HarnessSettings,
    ) -> Result<superba_core::harness::HarnessView> {
        let (service, _lock) = self.read().await?;
        service.save_harness_settings(harness, settings).await
    }

    async fn save_provider_protocols(
        &self,
        provider: ProfileId,
        protocols: superba_core::harness::ProviderProtocols,
    ) -> Result<()> {
        let (service, _lock) = self.read().await?;
        service.save_provider_protocols(provider, protocols).await
    }

    async fn set_harness_router_token(
        &self,
        harness: superba_core::harness::HarnessId,
        token: Option<Secret>,
    ) -> Result<superba_core::harness::HarnessView> {
        let (service, _lock) = self.read().await?;
        service.set_harness_router_token(harness, token).await
    }

    async fn harness_plan(
        &self,
        harness: superba_core::harness::HarnessId,
        kind: superba_core::harness::HarnessPlanKind,
    ) -> Result<superba_core::harness::HarnessPlan> {
        let (service, _lock) = self.read().await?;
        service.harness_plan(harness, kind).await
    }

    async fn harness_apply(
        &self,
        plan: superba_core::harness::HarnessPlan,
        take_over: bool,
    ) -> Result<superba_core::harness::HarnessApplyReport> {
        let (service, _lock) = self.read().await?;
        service.harness_apply(plan, take_over).await
    }

    async fn harness_backups(
        &self,
        harness: superba_core::harness::HarnessId,
    ) -> Result<Vec<superba_core::harness::HarnessBackup>> {
        let (service, _lock) = self.read().await?;
        service.harness_backups(harness).await
    }

    async fn harness_restore_backup(
        &self,
        harness: superba_core::harness::HarnessId,
        id: String,
    ) -> Result<superba_core::harness::HarnessApplyReport> {
        let (service, _lock) = self.read().await?;
        service.harness_restore_backup(harness, id).await
    }

    fn environment_event(&self, event: superba_core::app::EnvironmentEvent) {
        if let Ok(service) = self.inner.try_read()
            && let Ok(_lock) = superba_storage::DataLock::shared(&self.anchor)
        {
            service.environment_event(event);
        }
    }

    fn environment_generation(&self) -> u64 {
        self.inner
            .try_read()
            .map_or(0, |service| service.environment_generation())
    }

    async fn data_where(&self) -> Result<superba_core::data::DataLocation> {
        let (service, _lock) = self.read().await?;
        Ok(crate::data::location(&service.deps().ctx))
    }

    async fn data_usage(&self) -> Result<superba_core::data::DataUsage> {
        let (service, _lock) = self.read().await?;
        let ctx = service.deps().ctx.clone();
        tokio::task::spawn_blocking(move || superba_storage::usage(&ctx))
            .await
            .map_err(|e| Error::other(format!("data usage: {e}")))
    }

    async fn data_move(
        &self,
        directory: PathBuf,
        progress: superba_core::data::DataMoveProgressSink,
    ) -> Result<superba_core::data::DataMoveReport> {
        self.move_to(directory, progress).await
    }

    async fn data_cleanup(&self) -> Result<superba_core::data::DataCleanupReport> {
        self.cleanup().await
    }
}
