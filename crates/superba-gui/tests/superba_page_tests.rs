#![allow(missing_docs)]
//! Tabbed Superba shell, anchored settings, minimum window and accessible navigation.
mod a11y_harness;
#[path = "unit/font_diagnostics.rs"]
mod font_diagnostics;
mod router_harness;
mod support;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use std::sync::Arc;
use superba_core::{Locale, Settings, SwitchService};
use superba_gui::{
    app::PageStates,
    i18n::Tr,
    pages::{
        Page, SuperbaSection,
        superba::{self, SuperbaTab},
    },
};
use support::fake::{FakeOptions, FakeService};

fn service() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run: false,
        ..Default::default()
    }))
}

#[test]
fn troubleshooting_has_the_same_cards_in_both_interfaces() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = router_harness::Harness::new([1660.0, 1200.0], locale, service(), true)
            .min_height(Page::Troubleshooting.min_height());
        let mut pages = PageStates::default();
        let mut settings = Settings::default();
        for simple in [true, false] {
            let frame = h.settle(|ui, cx, out| {
                if simple {
                    pages.show_simple(Page::Troubleshooting, ui, cx, &mut settings, out);
                } else {
                    pages.show(Page::Troubleshooting, ui, cx, &mut settings, out);
                }
            });
            assert!(frame.has(&tr.t("troubleshooting.title")));
            assert!(frame.has(&tr.t("troubleshooting.environment")));
            let facts = frame.find(&tr.t("troubleshooting.fact.config")).unwrap();
            let common = frame.find(&tr.t("troubleshooting.common")).unwrap();
            assert!(
                common.top() - facts.bottom() < 64.0,
                "environment card must not reserve space below its facts"
            );
            assert!(frame.has(&tr.t("troubleshooting.issue.request")));
            assert!(frame.has(&tr.t("troubleshooting.issue.policy")));
            assert!(frame.has(&tr.t("troubleshooting.issue.server")));
            assert!(!frame.has(&tr.t("troubleshooting.history")));
            assert!(!frame.has(&tr.t("troubleshooting.how")));
            assert!(frame.actions.is_empty());
        }
    }
}

#[test]
fn troubleshooting_simple_mode_rollback_opens_full_mode_config_backups() {
    use superba_gui::pages::{Action, troubleshooting};
    let tr = Tr::new(Locale::ZhCn);
    let mut h = router_harness::Harness::new([1660.0, 1200.0], Locale::ZhCn, service(), true);
    let mut state = troubleshooting::State::default();
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(!frame.has(&tr.t("troubleshooting.rollback_prompt")));
    state.display_options(true, true);
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.rollback_prompt")));
    let prompt = frame
        .find(&tr.t("troubleshooting.rollback_prompt"))
        .unwrap();
    let button = frame
        .find(&tr.t("troubleshooting.rollback_config"))
        .unwrap();
    assert!((prompt.center().y - button.center().y).abs() <= 1.0);
    h.click_text(&frame, &tr.t("troubleshooting.rollback_config"));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(matches!(frame.actions.as_slice(), [
        Action::SetSimpleMode(false),
        Action::PageCommand { page: Page::Superba(superba_gui::pages::SuperbaSection::Data), command }
    ] if command == "focus:superba.backups.title"));
}

#[test]
fn troubleshooting_cards_fit_narrow_and_wide_windows_in_both_themes() {
    for locale in [Locale::En, Locale::ZhCn] {
        for window in [[960.0, 640.0], [1660.0, 1000.0]] {
            for dark in [false, true] {
                let h = router_harness::Harness::new(window, locale, service(), true)
                    .min_height(Page::Troubleshooting.min_height());
                let mut h = if dark { h.dark() } else { h };
                let mut pages = PageStates::default();
                let mut settings = Settings::default();
                let frame = h.settle(|ui, cx, out| {
                    pages.show(Page::Troubleshooting, ui, cx, &mut settings, out);
                });
                assert!(frame.fits(), "{}", frame.all_text());
                assert!(frame.actions.is_empty());
            }
        }
    }
}

fn diagnostic_report(
    issue: superba_core::TroubleshootingIssue,
) -> superba_core::TroubleshootingReport {
    use superba_core::*;
    TroubleshootingReport {
        issue,
        checked_at: Timestamp(1_790_000_000_000),
        profile_id: Some(ProfileId::new("work")),
        line_id: None,
        routed: false,
        checks: vec![TroubleshootingCheck {
            id: "configuration".into(),
            title: LocalizedText::new("troubleshooting.check.config", "Configuration"),
            detail: LocalizedText::new("troubleshooting.config_valid", "Checked"),
            status: TroubleshootingStatus::Passed,
            retry: None,
        }],
        models: Vec::new(),
        legacy_providers: Vec::new(),
        reapply: false,
        config_ok: true,
        restart_recommended: false,
        can_reinitialize: true,
        cleanup: None,
    }
}

#[test]
fn troubleshooting_500_waits_for_the_only_me_answer_before_checking() {
    let service = service();
    service.set_troubleshooting(Ok(diagnostic_report(
        superba_core::TroubleshootingIssue::Request,
    )));
    let tr = Tr::new(Locale::En);
    let mut h = router_harness::Harness::new([1660.0, 1000.0], Locale::En, service.clone(), true);
    let mut state = superba_gui::pages::troubleshooting::State::default();
    let mut started = false;
    let frame = h.settle(|ui, cx, out| {
        if !started {
            superba_gui::pages::troubleshooting::command(&mut state, cx, "check:server", out);
            started = true;
        }
        superba_gui::pages::troubleshooting::show(ui, cx, &mut state, out);
    });
    assert!(frame.has(&tr.t("troubleshooting.server_kind")));
    assert!(!frame.has(&tr.t("troubleshooting.server_only_me")));
    h.click_text(&frame, &tr.t("troubleshooting.server_401"));
    let frame = h.settle(|ui, cx, out| {
        superba_gui::pages::troubleshooting::show(ui, cx, &mut state, out);
    });
    assert!(frame.has(&tr.t("troubleshooting.advice.key_valid")));
    assert!(!frame.has(&tr.t("troubleshooting.server_only_me")));
    assert!(service.troubleshooting_requests().is_empty());
    h.click_text(&frame, &tr.t("troubleshooting.server_other"));
    let frame = h.settle(|ui, cx, out| {
        superba_gui::pages::troubleshooting::show(ui, cx, &mut state, out);
    });
    assert!(frame.has(&tr.t("troubleshooting.server_only_me")));
    assert!(!frame.has("Not confirmed yet"));
    assert!(service.troubleshooting_requests().is_empty());
    h.click_text(&frame, &tr.t("troubleshooting.server_only_me"));
    let frame = h.until(
        |ui, cx, out| superba_gui::pages::troubleshooting::show(ui, cx, &mut state, out),
        |f| f.has(&tr.t("troubleshooting.checks_passed")),
    );
    assert!(frame.has(&tr.t("troubleshooting.issue.server")));
    assert_eq!(service.troubleshooting_requests().len(), 1);
    assert_eq!(
        service.troubleshooting_requests()[0].issue,
        superba_core::TroubleshootingIssue::Request
    );
    assert!(frame.actions.is_empty());
}

#[test]
fn troubleshooting_drawers_follow_progress_and_allow_manual_expansion() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting;
    for locale in [Locale::En, Locale::ZhCn] {
        let service = service();
        let report = diagnostic_report(TroubleshootingIssue::All);
        let mut config = report.checks[0].clone();
        config.detail = LocalizedText::new("test.config_detail", "Configuration detail");
        let mut backend = config.clone();
        backend.id = "backend:work".into();
        backend.detail = LocalizedText::new("test.backend_detail", "Backend detail");
        service.set_troubleshooting(Ok(report));
        let progress = service.script_troubleshooting_progress();
        let tr = Tr::new(locale);
        let mut h = router_harness::Harness::new([1660.0, 1200.0], locale, service.clone(), true);
        let mut state = troubleshooting::State::default();
        h.settle(|ui, cx, out| {
            if !state.busy() {
                troubleshooting::command(&mut state, cx, "check:all", out);
            }
            troubleshooting::show(ui, cx, &mut state, out);
        });
        progress
            .send(TroubleshootingProgress::Configuration)
            .unwrap();
        progress
            .send(TroubleshootingProgress::Check(config))
            .unwrap();
        let frame = h.until(
            |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
            |f| f.has("Configuration detail"),
        );
        assert!(!frame.has(&tr.t("troubleshooting.live_log")));
        progress.send(TroubleshootingProgress::Backend).unwrap();
        progress
            .send(TroubleshootingProgress::Check(backend))
            .unwrap();
        h.until(
            |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
            |f| f.has("Backend detail"),
        );
        let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
        assert!(!frame.has("Configuration detail"));
        assert!(frame.fits(), "{}", frame.all_text());
        h.click_text(
            &frame,
            &format!("01  {}", tr.t("troubleshooting.stage.config")),
        );
        let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
        assert!(frame.has("Configuration detail"));
        assert!(frame.has("Backend detail"));
        drop(progress);
    }
}

#[test]
fn troubleshooting_retry_preserves_other_results_and_sends_one_target() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting;
    let service = service();
    let mut report = diagnostic_report(TroubleshootingIssue::Request);
    report.checks.push(TroubleshootingCheck {
        id: "backend:work".into(),
        title: LocalizedText::new("test.backend", "Supplier backend"),
        detail: LocalizedText::new("test.failed", "Connection failed"),
        status: TroubleshootingStatus::Failed,
        retry: Some(TroubleshootingRetry::Backend {
            profile: ProfileId::new("work"),
            model: Some("gpt-5".into()),
        }),
    });
    service.set_troubleshooting(Ok(report.clone()));
    let tr = Tr::new(Locale::En);
    let mut h = router_harness::Harness::new([1660.0, 1200.0], Locale::En, service.clone(), true);
    let mut state = troubleshooting::State::default();
    let mut started = false;
    let frame = h.until(
        |ui, cx, out| {
            if !started {
                troubleshooting::command(&mut state, cx, "check:request", out);
                started = true;
            }
            troubleshooting::show(ui, cx, &mut state, out);
        },
        |f| f.has(&tr.t("troubleshooting.retry_check")),
    );
    report.checks.remove(0);
    report.checks[0].status = TroubleshootingStatus::Passed;
    report.checks[0].detail = LocalizedText::new("test.recovered", "Connection recovered");
    report.checks[0].retry = None;
    service.set_troubleshooting(Ok(report));
    let progress = service.script_troubleshooting_progress();
    h.click_text(&frame, &tr.t("troubleshooting.retry_check"));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| f.has(&tr.t("troubleshooting.retrying_check")),
    );
    assert!(!frame.has(&tr.t("troubleshooting.check.config")));
    drop(progress);
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| f.has(&tr.t("troubleshooting.checks_passed")),
    );
    assert!(!frame.has(&tr.t("troubleshooting.check.config")));
    h.click_text(&frame, &tr.t("troubleshooting.stage.backend"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has("Connection recovered"));
    assert!(!frame.has("Connection failed"));
    assert!(!frame.has(&tr.t("troubleshooting.retry_check")));
    assert!(frame.actions.is_empty());
    let requests = service.troubleshooting_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].retry,
        Some(TroubleshootingRetry::Backend {
            profile: ProfileId::new("work"),
            model: Some("gpt-5".into())
        })
    );
}

#[test]
fn troubleshooting_schedules_only_one_restart_and_waits_for_user_confirmation() {
    use superba_gui::pages::troubleshooting::{self, Repair};
    let service = service();
    let mut report = diagnostic_report(superba_core::TroubleshootingIssue::WebSocket);
    report.restart_recommended = true;
    service.set_troubleshooting(Ok(report));
    let tr = Tr::new(Locale::En);
    let mut h = router_harness::Harness::new([1660.0, 1000.0], Locale::En, service.clone(), true);
    let mut state = troubleshooting::State::default();
    let mut started = false;
    let frame = h.until(
        |ui, cx, out| {
            if !started {
                troubleshooting::command(&mut state, cx, "check:websocket", out);
                started = true;
            }
            troubleshooting::show(ui, cx, &mut state, out);
        },
        |f| {
            f.actions.iter().any(|a| {
                matches!(
                    a,
                    superba_gui::pages::Action::TroubleshootingRepair {
                        repair: Repair::Restart,
                        ..
                    }
                )
            })
        },
    );
    let run = frame
        .actions
        .iter()
        .find_map(|a| match a {
            superba_gui::pages::Action::TroubleshootingRepair { run, .. } => Some(*run),
            _ => None,
        })
        .unwrap();
    state.repair_finished(run, Ok(superba_core::LocalizedText::default()));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| f.has(&tr.t("troubleshooting.checks_passed")),
    );
    assert!(
        frame.actions.is_empty(),
        "a recheck must not restart a second time"
    );
    assert_eq!(service.troubleshooting_requests().len(), 2);
    assert!(!frame.has(&tr.t("troubleshooting.confirmed")));
    h.click_text(&frame, &tr.t("troubleshooting.resolved"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.happy_coding")));
    let frame = h.frames(295, |ui, cx, out| {
        troubleshooting::show(ui, cx, &mut state, out)
    });
    assert!(
        !frame
            .actions
            .iter()
            .any(|a| matches!(a, superba_gui::pages::Action::Go(Page::Overview)))
    );
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, superba_gui::pages::Action::Go(Page::Overview)))
        },
    );
    assert!(
        frame
            .actions
            .iter()
            .any(|a| matches!(a, superba_gui::pages::Action::Go(Page::Overview)))
    );
}

#[test]
fn troubleshooting_cleanup_stays_collapsed_and_requires_confirmation() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting::{self, Repair};
    let service = service();
    let mut report = diagnostic_report(TroubleshootingIssue::All);
    report.checks[0].status = TroubleshootingStatus::Warning;
    report.checks.push(TroubleshootingCheck {
        id: "configuration:ignored:0".into(),
        title: LocalizedText::new("test.obsolete", "obsolete_option"),
        detail: LocalizedText::new("test.ignored_detail", "Ignored by Codex"),
        status: TroubleshootingStatus::Warning,
        retry: None,
    });
    report.cleanup = Some(TroubleshootingCleanup {
        sha256: "reviewed-hash".into(),
        entries: vec![TroubleshootingConfigEntry {
            path: "obsolete_option".into(),
            start: 0,
            end: 15,
        }],
    });
    service.set_troubleshooting(Ok(report));
    let tr = Tr::new(Locale::En);
    let mut h = router_harness::Harness::new([1660.0, 1000.0], Locale::En, service, true);
    let mut state = troubleshooting::State::default();
    let mut started = false;
    let frame = h.until(
        |ui, cx, out| {
            if !started {
                troubleshooting::command(&mut state, cx, "check:all", out);
                started = true;
            }
            troubleshooting::show(ui, cx, &mut state, out);
        },
        |f| f.has(&tr.t("troubleshooting.ignored_config")),
    );
    assert!(!frame.has("Ignored by Codex"));
    assert!(
        frame.has(&tr.t("troubleshooting.cleanup")),
        "cleanup must be visible while collapsed"
    );
    assert!(!frame.has(&tr.t("troubleshooting.next")));
    let frame = h.frames(12, |ui, cx, out| {
        troubleshooting::show(ui, cx, &mut state, out)
    });
    h.click_text(&frame, &tr.t("troubleshooting.ignored_config"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has("Ignored by Codex"), "{}", frame.all_text());
    h.click_text(&frame, &tr.t("troubleshooting.cleanup"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.cleanup_warning")));
    assert!(frame.actions.is_empty());
    h.click_text(&frame, &tr.t("troubleshooting.cleanup_confirm"));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| {
            f.actions.iter().any(|a| {
                matches!(
                    a,
                    superba_gui::pages::Action::TroubleshootingRepair {
                        repair: Repair::Cleanup(_),
                        ..
                    }
                )
            })
        },
    );
    assert!(frame.actions.iter().any(|a| matches!(
        a,
        superba_gui::pages::Action::TroubleshootingRepair {
            repair: Repair::Cleanup(_),
            ..
        }
    )));
}

#[test]
fn troubleshooting_next_page_has_actions_and_returns_to_problem_selection() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting;
    let service = service();
    service.set_troubleshooting(Ok(diagnostic_report(TroubleshootingIssue::Stability)));
    let tr = Tr::new(Locale::ZhCn);
    let mut h = router_harness::Harness::new([960.0, 800.0], Locale::ZhCn, service, true);
    let mut state = troubleshooting::State::default();
    state.display_options(true, true);
    let mut started = false;
    let frame = h.until(
        |ui, cx, out| {
            if !started {
                troubleshooting::command(&mut state, cx, "check:stability", out);
                started = true;
            }
            troubleshooting::show(ui, cx, &mut state, out);
        },
        |f| f.has(&tr.t("troubleshooting.still_broken")),
    );
    h.click_text(&frame, &tr.t("troubleshooting.still_broken"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.more_actions")));
    assert!(frame.has(&tr.t("troubleshooting.edit_config")));
    assert!(frame.has(&tr.t("troubleshooting.copy_diagnostics")));
    assert!(frame.has(&tr.t("troubleshooting.advice_intro")));
    assert!(frame.has(&tr.t("troubleshooting.bug")));
    assert!(frame.fits(), "{}", frame.all_text());
    h.click_text(&frame, &tr.t("troubleshooting.copy_diagnostics"));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(
        frame.actions.iter().any(|action| matches!(
            action,
            superba_gui::pages::Action::CopyDiagnostics(text)
                if text.contains(&tr.t("troubleshooting.issue.stability"))
        )),
        "{:?}",
        frame.actions
    );
    h.click_text(&frame, &tr.t("troubleshooting.back"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.issue.stability")));
    assert!(!frame.has(&tr.t("troubleshooting.still_broken")));
}

#[test]
fn troubleshooting_results_scroll_without_moving_recovery_buttons() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting;
    let service = service();
    let mut report = diagnostic_report(TroubleshootingIssue::All);
    report.checks = (0..40)
        .map(|i| TroubleshootingCheck {
            id: format!("configuration:row:{i}"),
            title: LocalizedText::new("test.row", format!("Observation {i}")),
            detail: LocalizedText::new("test.detail", "A configuration observation"),
            status: TroubleshootingStatus::Warning,
            retry: None,
        })
        .collect();
    service.set_troubleshooting(Ok(report));
    let mut h = router_harness::Harness::new([960.0, 640.0], Locale::En, service, true);
    let mut state = troubleshooting::State::default();
    let mut started = false;
    let tr = Tr::new(Locale::En);
    h.until(
        |ui, cx, out| {
            if !started {
                troubleshooting::command(&mut state, cx, "check:all", out);
                started = true;
            }
            troubleshooting::show(ui, cx, &mut state, out);
        },
        |f| f.has("Observation 0"),
    );
    let before = h.frames(20, |ui, cx, out| {
        troubleshooting::show(ui, cx, &mut state, out)
    });
    let buttons: Vec<_> = ["resolved", "still_broken", "retry"]
        .map(|key| {
            let label = tr.t(&format!("troubleshooting.{key}"));
            let rect = before.find(&label).unwrap();
            (label, rect)
        })
        .into();
    h.scroll_at(
        before
            .find(&tr.t("troubleshooting.details"))
            .unwrap()
            .center(),
        egui::vec2(0.0, -500.0),
    );
    let after = h.frames(30, |ui, cx, out| {
        troubleshooting::show(ui, cx, &mut state, out)
    });
    for (label, rect) in buttons {
        assert_eq!(
            after.find(&label).unwrap(),
            rect,
            "{label} must stay pinned"
        );
    }
    assert!(
        after.has("Observation 10"),
        "the page scroll reaches the expanded drawer body"
    );
}

#[test]
fn troubleshooting_long_stage_scrolls_inside_the_drawer() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting;
    let service = service();
    service.set_troubleshooting(Ok(diagnostic_report(TroubleshootingIssue::All)));
    let progress = service.script_troubleshooting_progress();
    let mut h = router_harness::Harness::new([960.0, 640.0], Locale::ZhCn, service, true);
    let mut state = troubleshooting::State::default();
    state.display_options(false, true);
    let mut started = false;
    h.settle(|ui, cx, out| {
        if !started {
            troubleshooting::command(&mut state, cx, "check:all", out);
            started = true;
        }
        troubleshooting::show(ui, cx, &mut state, out);
    });
    for i in 0..30 {
        progress
            .send(TroubleshootingProgress::Check(TroubleshootingCheck {
                id: format!("configuration:{i}"),
                title: LocalizedText::new("test.row", format!("Check {i}")),
                detail: LocalizedText::new(
                    "test.detail",
                    "A diagnostic observation with useful details",
                ),
                status: TroubleshootingStatus::Warning,
                retry: None,
            }))
            .unwrap();
    }
    h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| f.has("Check 1"),
    );
    let frame = h.frames(20, |ui, cx, out| {
        troubleshooting::show(ui, cx, &mut state, out)
    });
    let cancel = frame
        .find(&Tr::new(Locale::ZhCn).t("troubleshooting.cancel"))
        .unwrap_or_else(|| {
            panic!(
                "cancel missing; page {:?}; used {}; {}",
                frame.rect,
                frame.used_bottom,
                frame.all_text()
            )
        });
    assert!(
        cancel.bottom() <= frame.rect.bottom(),
        "{}",
        frame.all_text()
    );
    assert!(frame.scroll_areas >= 2);
    drop(progress);
}

#[test]
fn troubleshooting_missing_model_asks_both_questions_and_offers_the_existing_editor() {
    use superba_core::*;
    use superba_gui::pages::troubleshooting;
    let service = service();
    let mut report = diagnostic_report(TroubleshootingIssue::Models);
    report.models.push(TroubleshootingModels {
        profile_id: ProfileId::new("work"),
        name: "Example supplier".into(),
        list: Some(ModelList {
            source: ModelListSource::Upstream {
                path: "/models".into(),
                fetched_at: report.checked_at,
                etag: None,
            },
            models: (0..8)
                .map(|i| ModelSpec {
                    slug: format!("test-model-{i}"),
                    ..Default::default()
                })
                .collect(),
            fallback_reason: None,
            sources: Vec::new(),
        }),
        error: None,
        refreshed: true,
    });
    service.set_troubleshooting(Ok(report));
    let tr = Tr::new(Locale::En);
    let mut h = router_harness::Harness::new([1660.0, 1200.0], Locale::En, service.clone(), true);
    let mut state = troubleshooting::State::default();
    let mut started = false;
    let frame = h.until(
        |ui, cx, out| {
            if !started {
                troubleshooting::command(&mut state, cx, "check:models", out);
                started = true;
            }
            troubleshooting::show(ui, cx, &mut state, out);
        },
        |f| f.has(&tr.t("troubleshooting.still_broken")),
    );
    assert!(!frame.has(&tr.t("troubleshooting.next")));
    assert!(frame.has(&tr.t("troubleshooting.model_restart_prompt")));
    h.click_text(&frame, &tr.t("troubleshooting.restart_chatgpt"));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    let run = frame
        .actions
        .iter()
        .find_map(|a| match a {
            superba_gui::pages::Action::TroubleshootingRepair {
                run,
                repair: troubleshooting::Repair::Restart,
            } => Some(*run),
            _ => None,
        })
        .expect("uses the existing restart recovery");
    state.repair_finished(run, Ok(LocalizedText::default()));
    let frame = h.until(
        |ui, cx, out| troubleshooting::show(ui, cx, &mut state, out),
        |f| f.has(&tr.t("troubleshooting.still_broken")),
    );
    assert!(!frame.has(&tr.t("troubleshooting.restart_chatgpt")));
    assert!(
        service
            .troubleshooting_requests()
            .last()
            .unwrap()
            .refresh_models
    );
    h.click_text(&frame, &tr.t("troubleshooting.still_broken"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.next")));
    let first = frame.find("test-model-0").unwrap();
    let second = frame.find("test-model-1").unwrap();
    let last = frame.find("test-model-7").unwrap();
    assert!((first.top() - second.top()).abs() < 1.0);
    assert!(second.left() > first.right());
    assert!(last.top() > first.bottom());
    h.click_text(&frame, &tr.t("troubleshooting.model_missing"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has(&tr.t("troubleshooting.model_new")));
    assert!(frame.has(&tr.t("troubleshooting.model_today")));
    h.click_text(&frame, &tr.t("troubleshooting.yes"));
    let frame = h.settle(|ui, cx, out| troubleshooting::show(ui, cx, &mut state, out));
    assert!(frame.has("24"));
    assert!(!frame.has(&tr.t("troubleshooting.reinitialize")));
    h.click_text(&frame, &tr.t("troubleshooting.manual_model"));
    let frame = h.frames(6, |ui, cx, out| {
        troubleshooting::show(ui, cx, &mut state, out)
    });
    assert!(
        frame
            .actions
            .iter()
            .any(|a| matches!(a, superba_gui::pages::Action::SetSimpleMode(false)))
    );
    assert!(frame.actions.iter().any(|a| matches!(a, superba_gui::pages::Action::PageCommand { page: Page::Settings, command } if command == "files:config@local")));
}

#[test]
fn clicking_the_simple_codex_node_refreshes_versions_online() {
    let service = service();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut settings = service.profile_set().await.unwrap().settings;
        settings.simple_mode = true;
        service.save_settings(settings).await.unwrap();
    });
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(service.clone(), Locale::En, Page::Overview);
    h.until("known client version", |tree| {
        tree.find(None, &tr.t("overview.simple.client_update"))
            .is_some()
    });
    let before = service.version_refreshes();
    h.click(Some(Role::Button), "Codex");
    h.until("online Codex version refresh", |_| {
        service.version_refreshes() > before
    });
}

#[test]
fn simple_superba_has_two_tabs_with_codex_updates_storage_and_the_shared_settings() {
    let service = service();
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = router_harness::Harness::new([1280.0, 1000.0], locale, service.clone(), true);
        let mut pages = PageStates::default();
        let mut settings = Settings::default();
        let frame = h.until(
            |ui, cx, out| {
                pages.show_simple(
                    Page::Superba(SuperbaSection::Home),
                    ui,
                    cx,
                    &mut settings,
                    out,
                )
            },
            |f| f.has(&tr.t("simple.superba.more_apps")),
        );
        for key in [
            "simple.superba.info",
            "superba.tab.settings",
            "superba.tab.home",
            "prefs.data.storage",
            "settings.about.title",
        ] {
            assert!(frame.has(&tr.t(key)), "{key}: {}", frame.all_text());
        }
        assert!(frame.has(&tr.t("simple.superba.codex_client")));
        assert!(!frame.has("Claude Code"), "{}", frame.all_text());
        assert!(frame.has(&tr.t("diag.run")), "{}", frame.all_text());
        h.click_text(&frame, &tr.t("diag.run"));
        let frame = h.until(
            |ui, cx, out| {
                pages.show_simple(
                    Page::Superba(SuperbaSection::Home),
                    ui,
                    cx,
                    &mut settings,
                    out,
                )
            },
            |f| {
                f.actions
                    .iter()
                    .any(|a| matches!(a, superba_gui::pages::Action::RunDoctor))
            },
        );
        for expected in [
            superba_gui::pages::Action::Refresh,
            superba_gui::pages::Action::CheckUpdates,
            superba_gui::pages::Action::CheckSelfUpdate,
        ] {
            assert!(
                frame
                    .actions
                    .iter()
                    .any(|a| std::mem::discriminant(a) == std::mem::discriminant(&expected)),
                "{:?}",
                frame.actions
            );
        }
        h.click_text(&frame, &tr.t("simple.superba.more_apps"));
        let frame = h.until(
            |ui, cx, out| {
                pages.show_simple(
                    Page::Superba(SuperbaSection::Home),
                    ui,
                    cx,
                    &mut settings,
                    out,
                )
            },
            |f| {
                f.actions
                    .iter()
                    .any(|a| matches!(a, superba_gui::pages::Action::SetSimpleMode(false)))
            },
        );
        assert!(
            frame
                .actions
                .iter()
                .any(|a| matches!(a, superba_gui::pages::Action::SetSimpleMode(false)))
        );
        let frame = h.settle(|ui, cx, out| {
            pages.show_simple(
                Page::Superba(SuperbaSection::General),
                ui,
                cx,
                &mut settings,
                out,
            )
        });
        assert!(frame.has(&tr.t("superba.search")), "{}", frame.all_text());
        assert!(!frame.has(&tr.t("simple.superba.more_apps")));
    }
}

#[test]
fn every_section_fits_and_keeps_all_five_tabs_on_one_line() {
    let data = router_harness::data_from(&service());
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                for section in SuperbaSection::ALL {
                    let mut pages = PageStates::default();
                    let mut settings = Settings::default();
                    settings.notify.enabled = true;
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Superba(section).min_height(),
                        |ui, cx, out| {
                            pages.show(Page::Superba(section), ui, cx, &mut settings, out)
                        },
                    );
                    assert!(
                        report.fits() && !report.scrolled,
                        "{section:?} {locale:?} {window:?}: {report:?}"
                    );
                    let mut tab_y: Option<f32> = None;
                    for entry in SuperbaTab::ALL {
                        let label = tr.t(entry.title_key());
                        let r = report
                            .text_shapes
                            .iter()
                            .find(|(s, r, _)| s == &label && r.top() < report.rect.top() + 110.0)
                            .unwrap_or_else(|| {
                                panic!(
                                    "{entry:?} missing in {section:?} at {window:?}: {}",
                                    report.all_text()
                                )
                            });
                        assert!(
                            r.1.bottom() <= report.rect.bottom(),
                            "tab {entry:?} outside viewport: {r:?}"
                        );
                        assert!(!r.2, "tab label elided: {label}");
                        assert!(
                            r.1.left() >= report.rect.left() && r.1.right() <= report.rect.right(),
                            "{entry:?} outside page: {r:?}"
                        );
                        if let Some(y) = tab_y {
                            assert!((r.1.center().y - y).abs() < 1.0, "tabs wrapped");
                        }
                        tab_y = Some(r.1.center().y);
                    }
                }
            }
        }
    }
}

#[test]
fn brand_header_keeps_version_channel_and_status_on_one_line() {
    use superba_core::{SelfUpdateStatus, SelfUpdateView, Timestamp, UpdateChannel};
    let mut data = router_harness::data_from(&service());
    data.selfupdate.finish(Ok(SelfUpdateView {
        current: env!("CARGO_PKG_VERSION").into(),
        status: SelfUpdateStatus::UpToDate,
        checked_at: Some(Timestamp::from_millis(
            Timestamp::now().as_millis() - 180_000,
        )),
        ..Default::default()
    }));
    let default_channel = if env!("CARGO_PKG_VERSION_PRE").is_empty() {
        UpdateChannel::Stable
    } else {
        UpdateChannel::Preview
    };
    for (configured, expected) in [
        (None, default_channel),
        (Some(UpdateChannel::Stable), UpdateChannel::Stable),
        (Some(UpdateChannel::Preview), UpdateChannel::Preview),
    ] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            for theme in [egui::Theme::Light, egui::Theme::Dark] {
                for window in [[960.0, 640.0], [1280.0, 800.0]] {
                    let mut pages = PageStates::default();
                    let mut settings = Settings {
                        update_channel: configured,
                        ..Settings::default()
                    };
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Superba(SuperbaSection::Home).min_height(),
                        |ui, cx, out| {
                            pages.show(
                                Page::Superba(SuperbaSection::Home),
                                ui,
                                cx,
                                &mut settings,
                                out,
                            )
                        },
                    );
                    let title = report
                        .text_shapes
                        .iter()
                        .find(|(s, _, _)| s == "Superba")
                        .unwrap()
                        .1;
                    for label in [
                        format!("v{}", env!("CARGO_PKG_VERSION")),
                        tr.t(match expected {
                            UpdateChannel::Stable => "updates.superba.channel_stable",
                            UpdateChannel::Preview => "updates.superba.channel_preview",
                        }),
                    ] {
                        let (_, rect, elided) = report
                            .text_shapes
                            .iter()
                            .find(|(s, _, _)| s == &label)
                            .unwrap_or_else(|| panic!("{label}: {}", report.all_text()));
                        assert!(!elided);
                        assert!(
                            (rect.center().y - title.center().y).abs() < 5.0,
                            "{label} wrapped at {window:?}: {rect:?} / {title:?}"
                        );
                        assert!(rect.right() <= report.rect.right() + 0.5);
                    }
                    let (_, rect, elided) = report
                        .text_shapes
                        .iter()
                        .find(|(s, _, _)| s.starts_with(&tr.t("superba.home.current")))
                        .unwrap();
                    assert!(
                        !elided && rect.height() < 25.0,
                        "status wrapped at {window:?}: {rect:?}"
                    );
                    assert!((rect.center().y - title.center().y).abs() < 5.0);
                }
            }
        }
    }
}

#[test]
fn home_fact_explanations_and_updates_subtitle_are_never_elided() {
    let data = router_harness::data_from(&service());
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                for (section, keys) in [
                    (
                        SuperbaSection::Home,
                        &[
                            "superba.home.backups_desc",
                            "superba.home.router_desc",
                            "superba.home.cli_desc",
                        ][..],
                    ),
                    (
                        SuperbaSection::Updates,
                        &[Page::Superba(SuperbaSection::Updates).subtitle_key()][..],
                    ),
                ] {
                    let mut pages = PageStates::default();
                    let mut settings = Settings::default();
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Superba(section).min_height(),
                        |ui, cx, out| {
                            pages.show(Page::Superba(section), ui, cx, &mut settings, out)
                        },
                    );
                    for key in keys {
                        let expected = tr.t(key);
                        let (_, rect, elided) = report
                            .text_shapes
                            .iter()
                            .find(|(text, _, _)| text == &expected)
                            .unwrap_or_else(|| panic!("{key} missing: {}", report.all_text()));
                        assert!(!elided, "{key} elided at {window:?} {locale:?} {theme:?}");
                        assert!(
                            rect.left() >= report.rect.left()
                                && rect.right() <= report.rect.right() + 0.5,
                            "{key} outside page: {rect:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_inline_filter_finds_every_setting_without_changing_it() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        h.resize([960.0, 640.0]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let before = rt.block_on(h.service.profile_set()).unwrap().settings;
        h.settle();
        let search = tr.t("superba.search");
        for (section, key) in superba::search::ROWS {
            if *key == "prefs.network.app_source" && !cfg!(windows) {
                continue;
            }
            if matches!(section, SuperbaSection::Data | SuperbaSection::Updates) {
                // Rows of the Data and Updates tabs are not in the settings list.
                continue;
            }
            let label = tr.t(superba::search::label_key(key));
            h.set_value(&search, &label);
            let tree = h.until(key, |t| t.find(Some(Role::Group), &label).is_some());
            let rows = tree.with_role(Role::Group).len();
            assert!(
                rows < 12,
                "{locale:?} {key}: the filter hides other rows ({rows}):\n{}",
                tree.dump()
            );
            assert!(tree.unnamed().is_empty(), "{key}: {:?}", tree.unnamed());
        }
        h.set_value(&search, "zzqx-no-such-setting");
        let empty = tr.f(
            "superba.search_empty",
            &[("query", &"zzqx-no-such-setting")],
        );
        let t = h.until("empty", |t| t.has(&empty));
        assert!(
            t.find(Some(Role::Group), &tr.t("settings.theme.title"))
                .is_none()
        );
        h.set_value(&search, "");
        let t = h.settle();
        assert!(
            t.find(Some(Role::Group), &tr.t("settings.theme.title"))
                .is_some(),
            "{}",
            t.dump()
        );
        assert_eq!(
            rt.block_on(h.service.profile_set()).unwrap().settings,
            before
        );
    }
}

#[test]
fn tabs_have_names_roles_and_keyboard_navigation_into_the_panel() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        h.resize([960.0, 640.0]);
        let t = h.settle();
        let list = t
            .find(Some(Role::TabList), "Superba")
            .expect("named tab list");
        let tabs: Vec<_> = t
            .node(list)
            .children()
            .iter()
            .filter(|id| t.node(**id).role() == Role::Tab)
            .copied()
            .collect();
        assert_eq!(tabs.len(), 5, "{}", t.dump());
        for (id, tab) in tabs.iter().zip(SuperbaTab::ALL) {
            assert_eq!(t.name(*id), Some(tr.t(tab.title_key())));
            assert!(t.node(*id).supports_action(egui::accesskit::Action::Click));
        }
        h.act(tabs[2], egui::accesskit::Action::Focus);
        h.settle();
        let t = h.key(egui::Key::ArrowRight);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaTab::Diagnostics.title_key()))
        );
        assert_eq!(t.node(t.focus).is_selected(), Some(true));
        let t = h.key(egui::Key::ArrowDown);
        assert_eq!(t.node(t.focus).role(), Role::Group, "{}", t.dump());
        let t = h.key(egui::Key::Tab);
        assert_ne!(t.node(t.focus).role(), Role::Tab, "{}", t.dump());
        let t = h.key(egui::Key::Escape);
        assert_eq!(t.node(t.focus).role(), Role::Tab);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaTab::Diagnostics.title_key()))
        );
        let t = h.key(egui::Key::ArrowLeft);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaTab::Settings.title_key()))
        );
        let t = h.key(egui::Key::Home);
        assert_eq!(t.name(t.focus), Some(tr.t(SuperbaTab::Home.title_key())));
        let t = h.key(egui::Key::End);
        assert_eq!(t.name(t.focus), Some(tr.t(SuperbaTab::Data.title_key())));
    }
}

fn at_anchor(t: &a11y_harness::Tree, tr: &Tr, section: SuperbaSection) -> bool {
    let name = tr.t(section.title_key());
    let active = t
        .find(Some(Role::Tab), &name)
        .is_some_and(|id| t.node(id).is_selected() == Some(true));
    let header = t.find_all(Some(Role::Label), &name).into_iter().any(|id| {
        t.node(id)
            .bounds()
            .is_some_and(|r| r.y0 > 150.0 && r.y1 < 350.0)
    });
    active && header
}

#[test]
fn every_settings_deep_link_opens_its_anchor_in_the_settings_tab() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for section in [
            SuperbaSection::General,
            SuperbaSection::Appearance,
            SuperbaSection::Network,
            SuperbaSection::Notify,
        ] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                let mut h = AppHarness::new(service(), locale, Page::Superba(section));
                h.resize(window);
                let t = h.until("initial deep link", |t| at_anchor(t, &tr, section));
                assert!(
                    t.find(
                        Some(Role::TabPanel),
                        &tr.t(SuperbaTab::Settings.title_key())
                    )
                    .is_some()
                );
                assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
            }
        }
    }
}

#[test]
fn jump_directory_tracks_scroll_and_sidebar_and_comma_restore_anchors() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        h.resize([960.0, 640.0]);
        for section in [
            SuperbaSection::Notify,
            SuperbaSection::Appearance,
            SuperbaSection::Network,
        ] {
            h.click(Some(Role::Tab), &tr.t(section.title_key()));
            h.until("jump", |t| at_anchor(t, &tr, section));
        }
        h.scroll_page(-1200.0);
        h.until("scroll spy", |t| {
            t.find(Some(Role::Tab), &tr.t(SuperbaSection::Notify.title_key()))
                .is_some_and(|id| t.node(id).is_selected() == Some(true))
        });
        h.go(Page::Overview, &tr);
        h.click(Some(Role::Tab), "Superba");
        h.until("sidebar restores anchor", |t| {
            at_anchor(t, &tr, SuperbaSection::Notify)
        });
        h.shortcut(egui::Key::Comma);
        h.until("comma opens general", |t| {
            at_anchor(t, &tr, SuperbaSection::General)
        });
        // The current Page can already be General after manually scrolling away.
        h.click(Some(Role::Tab), &tr.t(SuperbaSection::Notify.title_key()));
        h.until("notify again", |t| {
            at_anchor(t, &tr, SuperbaSection::Notify)
        });
        h.shortcut(egui::Key::Comma);
        h.until("same-page comma", |t| {
            at_anchor(t, &tr, SuperbaSection::General)
        });
    }
}

#[test]
fn cmd_k_opens_nothing_any_more() {
    for locale in [Locale::En, Locale::ZhCn] {
        let mut h = AppHarness::new(service(), locale, Page::Overview);
        h.settle();
        let t = h.shortcut(egui::Key::K);
        assert!(t.with_role(Role::Dialog).is_empty(), "{}", t.dump());
        assert!(t.with_role(Role::TextInput).is_empty(), "{}", t.dump());
    }
    // The palette's strings are gone from both catalogs.
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for key in [
            "palette.placeholder",
            "palette.run_wizard",
            "overview.menu.title",
        ] {
            assert_eq!(tr.t(key), key, "{locale:?} still has {key}");
        }
    }
}

#[test]
fn preference_edits_still_emit_an_immediate_save() {
    let mut h = router_harness::Harness::new([960.0, 640.0], Locale::En, service(), true);
    let mut pages = PageStates::default();
    let mut settings = Settings {
        reduce_motion: true,
        ..Settings::default()
    };
    let page = Page::Superba(SuperbaSection::Appearance);
    let f = h.settle(|ui, cx, out| pages.show(page, ui, cx, &mut settings, out));
    h.click_text(&f, "Light");
    let f = h.until(
        |ui, cx, out| pages.show(page, ui, cx, &mut settings, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, superba_gui::pages::Action::SaveSettings))
        },
    );
    assert_eq!(settings.theme, superba_core::Theme::Light);
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, superba_gui::pages::Action::SaveSettings))
    );
}

#[test]
fn tabs_do_not_repeat_page_titles_and_settings_keep_compact_anchor_titles() {
    let data = router_harness::data_from(&service());
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for window in [[960.0, 640.0], [1280.0, 800.0]] {
            for section in [
                SuperbaSection::Updates,
                SuperbaSection::Diagnostics,
                SuperbaSection::Data,
                SuperbaSection::General,
            ] {
                let mut pages = PageStates::default();
                let mut settings = Settings::default();
                let report = support::fit::render_fit_min_height(
                    window,
                    locale,
                    egui::Theme::Light,
                    &data,
                    Page::Superba(section).min_height(),
                    |ui, cx, out| pages.show(Page::Superba(section), ui, cx, &mut settings, out),
                );
                let name = tr.t(section.title_key());
                let titles: Vec<_> = report
                    .text_shapes
                    .iter()
                    .filter(|(s, r, _)| s == &name && r.top() < report.rect.top() + 250.0)
                    .collect();
                if section == SuperbaSection::Data {
                    assert!(titles.is_empty(), "duplicate data page title: {titles:?}");
                } else if section == SuperbaSection::General {
                    // The directory and one anchor, with no duplicate General subsection.
                    assert_eq!(titles.len(), 2, "{titles:?}");
                    assert!(
                        titles.iter().all(|(_, r, _)| r.height() < 29.0),
                        "oversized anchor: {titles:?}"
                    );
                } else {
                    assert_eq!(
                        titles.len(),
                        1,
                        "only the tab should carry this title: {titles:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn updates_inventory_uses_the_full_panel_width() {
    use superba_gui::design::{layout, tokens::size};
    for locale in [Locale::En, Locale::ZhCn] {
        for window in [[960.0, 640.0], [1280.0, 800.0], [1440.0, 900.0]] {
            let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::Updates));
            h.resize(window);
            let tree = h.until("update inventory", |t| {
                t.with_role(Role::Button)
                    .into_iter()
                    .any(|id| t.name(id).is_some_and(|n| n.starts_with("Superba, ")))
            });
            let row = tree
                .with_role(Role::Button)
                .into_iter()
                .find(|id| tree.name(*id).is_some_and(|n| n.starts_with("Superba, ")))
                .unwrap();
            let rect = tree.node(row).bounds().unwrap();
            let central = egui::Rect::from_min_max(
                egui::pos2(size::SIDEBAR, 0.0),
                egui::pos2(window[0], window[1]),
            );
            let panel = layout::page_rect(central, true);
            assert!(
                (rect.x0 - f64::from(panel.left())).abs() < 2.0,
                "{rect:?} / {panel:?}"
            );
            assert!(
                f64::from(panel.right()) - rect.x1 <= 20.0,
                "inventory leaves unused width: {rect:?} / {panel:?}"
            );
            assert!(
                rect.x1 <= f64::from(panel.right()),
                "inventory overflows: {rect:?}"
            );
        }
    }
}
