//! A dismissed first-run wizard stays dismissed across application starts.
mod a11y_harness;
mod support;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use std::sync::Arc;
use superba_core::{Locale, ProfileSet, SwitchService};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Page, SuperbaSection};
use support::fake::{FakeOptions, FakeService};

#[test]
fn skip_survives_restart_and_settings_can_run_setup_again() {
    let service = Arc::new(FakeService::new(FakeOptions {
        first_run: true,
        fast: true,
        ..Default::default()
    }));
    let tr = Tr::new(Locale::En);
    let mut first = AppHarness::new(service.clone(), Locale::En, Page::Overview);
    first.click(Some(Role::Button), &tr.t("wizard.skip"));
    let rt = tokio::runtime::Runtime::new().unwrap();
    first.until("dismissal saved", |_| {
        rt.block_on(service.profile_set())
            .unwrap()
            .settings
            .wizard_dismissed
    });
    let saved = rt.block_on(service.profile_set()).unwrap();
    assert!(
        saved.profiles.iter().all(|p| p.builtin.is_some()),
        "skip creates no provider"
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), serde_json::to_vec(&saved).unwrap()).unwrap();
    drop(first);
    let reloaded: ProfileSet =
        serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
    let next_service = Arc::new(FakeService::new(FakeOptions {
        first_run: true,
        fast: true,
        ..Default::default()
    }));
    next_service.edit_profiles(|p| *p = reloaded);
    let mut next = AppHarness::new(
        next_service,
        Locale::En,
        Page::Superba(SuperbaSection::General),
    );
    let tree = next.wait_for(Some(Role::Button), &tr.t("prefs.wizard.run"));
    assert!(
        !tree.has(&tr.t("wizard.skip")),
        "wizard does not reappear after restart"
    );
    next.click(Some(Role::Button), &tr.t("prefs.wizard.run"));
    next.wait_for(Some(Role::Button), &tr.t("wizard.skip"));
}

#[test]
fn chinese_estimated_price_heading_has_no_extra_suffix() {
    assert_eq!(Tr::new(Locale::ZhCn).t("prices.title"), "价格（估算费用）");
}
