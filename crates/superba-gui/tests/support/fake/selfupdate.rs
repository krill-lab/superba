//! Fake service, area "selfupdate": Superba's own update (Updates page first
//! section, Overview to-do, sidebar dot).
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here.

use superba_core::*;

use super::FakeService;

/// Fake state of the area.
#[derive(Debug, Default)]
pub(super) struct State {
    view: SelfUpdateView,
    report: Option<SelfUpdateReport>,
    install_error: Option<Error>,
    installs: Vec<SelfUpdateInstallRequest>,
    checks: Vec<SelfUpdateCheckRequest>,
    skips: Vec<Option<String>>,
}

impl FakeService {
    /// What the next checks return.
    pub fn set_self_update(&self, view: SelfUpdateView) {
        self.areas.selfupdate.lock().unwrap().view = view;
    }

    /// What an install returns.
    pub fn set_self_update_report(&self, report: SelfUpdateReport) {
        self.areas.selfupdate.lock().unwrap().report = Some(report);
    }

    /// Make installs fail.
    pub fn fail_self_update_install(&self, error: Error) {
        self.areas.selfupdate.lock().unwrap().install_error = Some(error);
    }

    /// Installs requested.
    pub fn self_update_installs(&self) -> Vec<SelfUpdateInstallRequest> {
        self.areas.selfupdate.lock().unwrap().installs.clone()
    }

    /// Checks requested.
    pub fn self_update_checks(&self) -> Vec<SelfUpdateCheckRequest> {
        self.areas.selfupdate.lock().unwrap().checks.clone()
    }

    /// Skips requested.
    pub fn self_update_skips(&self) -> Vec<Option<String>> {
        self.areas.selfupdate.lock().unwrap().skips.clone()
    }
}

pub(super) async fn view(fake: &FakeService) -> Result<SelfUpdateView> {
    Ok(fake.areas.selfupdate.lock().unwrap().view.clone())
}

pub(super) async fn check(
    fake: &FakeService,
    request: SelfUpdateCheckRequest,
) -> Result<SelfUpdateView> {
    let mut s = fake.areas.selfupdate.lock().unwrap();
    s.checks.push(request);
    Ok(s.view.clone())
}

pub(super) async fn skip(fake: &FakeService, version: Option<String>) -> Result<SelfUpdateView> {
    let mut s = fake.areas.selfupdate.lock().unwrap();
    s.skips.push(version.clone());
    s.view.skipped = version;
    Ok(s.view.clone())
}

pub(super) async fn install(
    fake: &FakeService,
    request: SelfUpdateInstallRequest,
) -> Result<SelfUpdateReport> {
    let mut s = fake.areas.selfupdate.lock().unwrap();
    s.installs.push(request);
    if let Some(e) = s.install_error.clone() {
        return Err(e);
    }
    s.report
        .clone()
        .ok_or_else(|| Error::not_implemented("self update install"))
}
