#![allow(missing_docs)]
//! Diagnostics page (task-20): the checks section says what to do by hand
//! instead of drawing a button that can never be pressed, and the environment
//! block stops showing the app's internal index numbers.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{DoctorFinding, FeedOrigin, FeedsStatus, Locale, LocalizedText, Severity};
use superba_gui::i18n::Tr;
use superba_gui::pages::diagnostics;
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

/// A page whose checks found one thing a later Superba could repair, with the
/// feed status the environment block reports.
fn harness(locale: Locale) -> Harness {
    let mut h = Harness::new([1280.0, 1400.0], locale, fake(), false);
    h.data.doctor.value = Some(vec![DoctorFinding {
        id: "ide_missing_authapi".to_owned(),
        severity: Severity::Warning,
        title: LocalizedText::new(
            "test.doctor.title",
            "An IDE app-server lacks CODEX_AUTHAPI_BASE_URL",
        ),
        detail: Some(LocalizedText::new(
            "test.doctor.detail",
            "Restart the IDE after switching.",
        )),
        fixable: true,
    }]);
    h.data.feeds.value = Some(FeedsStatus {
        serial: 7,
        builtin_serial: 3,
        source: FeedOrigin::Cache,
        ..Default::default()
    });
    h
}

#[test]
fn a_fixable_finding_says_what_to_do_instead_of_a_dead_fix_button() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let mut h = harness(locale);
        let f = h.until(diagnostics::show, |f| {
            f.has("An IDE app-server lacks CODEX_AUTHAPI_BASE_URL")
        });
        // The sentence stands where the button that could never be pressed used
        // to be, and says what to do by hand instead.
        assert!(
            f.has(&tr.t("diag.fix_manual")),
            "{locale:?}: {}",
            f.all_text()
        );
        assert!(
            !f.texts.iter().any(|(t, _)| t == "修复" || t == "Fix"),
            "{locale:?}: a fix button is still drawn: {}",
            f.all_text()
        );
    }
}

#[test]
fn the_environment_block_hides_the_internal_index_numbers() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let mut h = harness(locale);
        let f = h.until(diagnostics::show, |f| f.has(&tr.t("diag.env.feed_cache")));
        // The feed row is the origin alone — no " · #7".
        assert!(
            f.texts
                .iter()
                .any(|(t, _)| t == &tr.t("diag.env.feed_cache")),
            "{locale:?}: {}",
            f.all_text()
        );
        assert!(
            !f.all_text().contains('#'),
            "{locale:?}: an internal serial is still shown: {}",
            f.all_text()
        );
    }
}
