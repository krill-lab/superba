//! Superba's own update (`SwitchService::self_update_*`): the engine lives in
//! `superba_versions::selfupdate`; this adds the Preferences, the network
//! settings and the local router.

use superba_core::{
    Error, Result, SelfInstallOutcome, SelfUpdateCheckRequest, SelfUpdateInstallRequest,
    SelfUpdateReport, SelfUpdateView,
};
use superba_versions::selfupdate::{SelfUpdater, finish};

use crate::Switcher;

impl Switcher {
    async fn self_updater(&self) -> SelfUpdater {
        let settings = self
            .deps
            .profiles
            .load()
            .await
            .map(|set| set.settings)
            .unwrap_or_default();
        SelfUpdater::new(&self.deps.ctx.data_dir, settings.network.clone())
            .with_preferences(settings.auto_update_check, settings.include_prereleases)
    }

    pub(crate) async fn self_update_view_impl(&self) -> Result<SelfUpdateView> {
        Ok(self.self_updater().await.view())
    }

    pub(crate) async fn self_update_check_impl(
        &self,
        request: SelfUpdateCheckRequest,
    ) -> Result<SelfUpdateView> {
        Ok(self.self_updater().await.check(request.manual).await)
    }

    pub(crate) async fn self_update_skip_impl(
        &self,
        version: Option<String>,
    ) -> Result<SelfUpdateView> {
        Ok(self.self_updater().await.skip(version))
    }

    /// Restart the router service when it is installed and running.
    pub(crate) async fn restart_router_if_running(&self) -> Result<bool> {
        let (info, live) = self.deps.router.overview().await;
        // After an MSI upgrade the installer has stopped the router on purpose.
        let forced = std::env::var_os(superba_versions::selfupdate::msi::ENV_FORCE_ROUTER_RESTART)
            .is_some_and(|v| v == "1");
        if !(info.installed && (live.is_up() || forced)) {
            return Ok(false);
        }
        self.deps.router.restart().await?;
        Ok(true)
    }

    pub(crate) async fn self_update_install_impl(
        &self,
        request: SelfUpdateInstallRequest,
    ) -> Result<SelfUpdateReport> {
        let updater = self.self_updater().await;
        let done = updater.install(request.relaunch).await?;
        let mut report = SelfUpdateReport {
            version: done.version,
            outcome: done.outcome,
            package: done.package,
            steps: done.steps,
            router_restarted: false,
            relaunching: false,
            replaced: done.replaced,
        };
        let router_running = {
            let (info, live) = self.deps.router.overview().await;
            info.installed && live.is_up()
        };
        let mut plan = done.finish;
        match report.outcome {
            SelfInstallOutcome::Installed => {
                // The files are in place: the service restarts onto the new binary.
                match self.restart_router_if_running().await {
                    Ok(restarted) => report.router_restarted = restarted,
                    Err(e) => {
                        tracing::warn!(error = %e, "could not restart the router after the update")
                    }
                }
            }
            SelfInstallOutcome::Handoff => {
                // The new superba.exe swaps the files and restarts the router.
                if let Some(p) = plan.as_mut() {
                    p.args.restart_router = router_running;
                    report.router_restarted = router_running;
                }
            }
            _ => {}
        }
        if let Some(plan) = plan {
            let handoff = report.outcome == SelfInstallOutcome::Handoff;
            match finish::spawn(&plan) {
                Ok(()) => {
                    report.relaunching =
                        plan.args.relaunch.is_some() || plan.args.open_app.is_some();
                }
                Err(e) if handoff => return Err(e),
                Err(e) => tracing::warn!(error = %e, "could not schedule the restart"),
            }
        }
        if report.outcome == SelfInstallOutcome::Handoff && report.replaced.is_empty() {
            return Err(Error::other("nothing to replace"));
        }
        Ok(report)
    }
}
