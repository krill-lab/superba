//! The desktop app location of the settings (Settings → ChatGPT app location,
//! `spb app path`): the source the app controller asks at every detection, the
//! check of a picked location, saving and clearing it, and its doctor findings.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use superba_core::{
    AppLocationCheck, DoctorFinding, Error, ProfileStore, Result, Settings, Severity,
};

use crate::Switcher;
use crate::messages::text;

/// [`superba_appctl::AppLocationSource`] reading [`Settings::app_location`]
/// from the profile store, so a saved or cleared location applies to the next
/// detection (switch planning, launch, the model catalog, doctor) at once.
pub(crate) struct ProfileAppLocation(pub(crate) Arc<dyn ProfileStore>);

#[async_trait]
impl superba_appctl::AppLocationSource for ProfileAppLocation {
    async fn app_location(&self) -> Option<PathBuf> {
        match self.0.load().await {
            Ok(set) => set.settings.app_location,
            Err(e) => {
                tracing::debug!(error = %e, "profiles unreadable; no app location");
                None
            }
        }
    }
}

/// `settings` after its app location changed from `before`: a new location
/// is the install to use, so an older install choice no longer overrides the
/// detection order (`SUPERBA_APP_PATH`, this location, the defaults, ...).
pub(crate) fn app_location_changed(before: &Settings, settings: &mut Settings) -> bool {
    if before.app_location == settings.app_location {
        return false;
    }
    if settings.app_location.is_some() {
        settings.preferred_app_id = None;
    }
    true
}

impl Switcher {
    pub(crate) async fn check_app_location_impl(&self, path: PathBuf) -> Result<AppLocationCheck> {
        if path.as_os_str().is_empty() {
            return Err(Error::invalid("no app location given"));
        }
        // A relative path would resolve against Superba's working directory.
        if !path.is_absolute() {
            return Err(Error::invalid(format!(
                "the app location must be a full path: {}",
                path.display()
            )));
        }
        self.deps.app.check_app_location(&path).await
    }

    pub(crate) async fn set_app_location_impl(
        &self,
        path: Option<PathBuf>,
    ) -> Result<Option<AppLocationCheck>> {
        let check = match &path {
            Some(path) => {
                let check = self.check_app_location_impl(path.clone()).await?;
                if !check.usable() {
                    return Err(Error::invalid(check.text(path).render_fallback()));
                }
                Some(check)
            }
            None => None,
        };
        let mut set = self.deps.profiles.load().await?;
        let before = set.settings.clone();
        set.settings.app_location = path;
        if app_location_changed(&before, &mut set.settings) {
            self.deps.profiles.save(&set).await?;
            self.monitor.invalidate();
        }
        Ok(check)
    }

    /// Doctor: an app location that holds no usable app, and no app found at
    /// all (with what to do).
    pub(crate) async fn app_location_findings(&self) -> Vec<DoctorFinding> {
        let d = &self.deps;
        let mut out = Vec::new();
        let set = d.profiles.load().await.unwrap_or_default();
        if let Some(path) = &set.settings.app_location
            && let Ok(check) = d.app.check_app_location(path).await
            && !matches!(check, AppLocationCheck::Install { .. })
        {
            out.push(crate::service::finding(
                "app_location",
                Severity::Warning,
                text("switch.doctor.app_location").arg_text("reason", check.text(path)),
            ));
        }
        // `SUPERBA_APP_PATH` set to a folder of the Store app's package (one
        // version's): detection uses the installed version of that package
        // instead, and the variable is not needed.
        for var in [
            superba_core::codex::ENV_SUPERBA_APP_PATH,
            superba_appctl::install::ENV_SUPERBA_APP_DIR,
        ] {
            if let Some(value) = d.ctx.env.var_non_empty(var)
                && let Some(package) =
                    superba_appctl::install::windows_apps_package(std::path::Path::new(&value))
            {
                out.push(crate::service::finding(
                    "app_path_store_package",
                    Severity::Warning,
                    text("switch.doctor.app_path_store_package")
                        .arg("var", var)
                        .arg("path", &value)
                        .arg("family", &package.family_name),
                ));
            }
        }
        if let Ok(installs) = d.app.detect_installs().await
            && installs.is_empty()
        {
            out.push(crate::service::finding(
                "no_app_install",
                Severity::Info,
                text("switch.doctor.no_app_install"),
            ));
        }
        out
    }
}
