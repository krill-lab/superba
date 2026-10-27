#![allow(missing_docs)]

mod support;

use std::time::Duration;

use egui::{Rect, pos2, vec2};
use superba_core::{
    AuthMode, Locale, LocalizedText, PlanDestination, RestartKind, SwitchEvent, SwitchPhase,
    SwitchPlan, SwitchRequest, Timestamp,
};
use superba_gui::design::{
    fonts, style,
    tokens::{Palette, size},
};
use superba_gui::flow::Flow;
use superba_gui::i18n::Tr;
use superba_gui::pages::{Cx, Data};
use superba_gui::runtime::Command;
use superba_gui::shell::{self, BannerState};

fn plan() -> SwitchPlan {
    SwitchPlan {
        id: "shell-plan".into(),
        created_at: Timestamp::EPOCH,
        request: SwitchRequest::profile("test".into()),
        from: None,
        to: PlanDestination {
            profile_id: Some("test".into()),
            site_id: Some("test".into()),
            line_id: Some("test-line".into()),
            auth_mode: Some(AuthMode::Pat),
            label: LocalizedText::new("test.destination", "Test destination"),
        },
        primary: RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
        restarts: vec![],
        config: None,
        env: None,
        certificate_env: None,
        migration: None,
        remotes: vec![],
        active_threads: vec![],
        model_check: None,
        choices: vec![],
        alternatives: vec![],
        warnings: vec![],
        cleanup: vec![],
    }
}

fn armed() -> Flow {
    let mut flow = Flow::new(5);
    let plan = plan();
    flow.request(plan.request.clone(), Duration::ZERO);
    let commands = flow.take_commands();
    let [Command::Plan { seq, .. }] = commands.as_slice() else {
        panic!("plan command");
    };
    flow.on_planned(*seq, Ok(plan), Duration::ZERO);
    flow
}

fn frame(
    ctx: &egui::Context,
    flow: &mut Flow,
    state: &mut BannerState,
    width: f32,
) -> (f32, Vec<String>) {
    let mut height = 0.0;
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                vec2(width, size::MIN_WINDOW[1]),
            )),
            time: Some(2.0),
            ..Default::default()
        },
        |ui| {
            let cx = Cx {
                tr: &Tr::new(Locale::En),
                pal: Palette::of(ctx),
                data: &Data::default(),
                busy: false,
                now: Timestamp::EPOCH,
                fonts: &fonts::FontReport::default(),
                service: &support::idle_service(),
                log: &[],
            };
            ui.set_width(width - size::SIDEBAR - 64.0);
            assert!(shell::banner(
                ui,
                &cx,
                flow,
                state,
                Duration::from_secs(2),
                f32::INFINITY
            ));
            height = ui.min_rect().height();
        },
    );
    let text = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(s) => Some(s.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    (height, text)
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(egui::Theme::Dark));
    ctx
}

#[test]
fn a_slow_background_turn_check_keeps_the_countdown_and_its_title_running() {
    use std::sync::Arc;
    use superba_gui::flow::Stage;
    use support::fake::{FakeOptions, FakeService};

    let service = Arc::new(FakeService::new(FakeOptions::default()));
    let held = service.hold_turn_read();
    let mut flow = armed();
    flow.watch_turns(service.clone(), Duration::from_secs(1));
    flow.watch_turns(service.clone(), Duration::from_secs(2));
    let Stage::Armed { countdown, .. } = flow.stage() else {
        panic!("counting down")
    };
    assert!(!countdown.is_paused());
    assert_eq!(
        countdown.remaining(Duration::from_secs(2)),
        Duration::from_secs(3)
    );
    let tr = Tr::new(Locale::En);
    let dest = superba_gui::pages::dest_text(&tr, &Data::default(), &plan().to);
    let title = tr.f("banner.armed.title", &[("secs", &3_u64), ("dest", &dest)]);
    let (_, texts) = frame(&context(), &mut flow, &mut BannerState::default(), 1100.0);
    assert!(texts.contains(&title), "{texts:?}");
    flow.tick(Duration::from_millis(4999));
    assert!(flow.take_commands().is_empty());
    flow.watch_turns(service, Duration::from_secs(5));
    flow.tick(Duration::from_secs(5));
    assert!(
        matches!(flow.take_commands().as_slice(), [Command::Execute { options, .. }] if !options.force_turns)
    );
    drop(held);
}

#[test]
fn a_background_check_still_pauses_the_remaining_time_when_a_turn_is_busy() {
    use std::sync::Arc;
    use superba_core::{ActivitySignal, ThreadActivity, TurnActivity};
    use superba_gui::flow::Stage;
    use support::fake::{FakeOptions, FakeService};

    let service = Arc::new(FakeService::new(FakeOptions::default()));
    let held = service.hold_turn_read();
    let mut flow = armed();
    flow.watch_turns(service.clone(), Duration::from_secs(1));
    held.send(Ok(vec![TurnActivity {
        target_id: "local".into(),
        checked_at: Timestamp::EPOCH,
        complete: true,
        busy: vec![ThreadActivity {
            target_id: "local".into(),
            thread_id: Some("resumed".into()),
            title: Some("Active conversation".into()),
            rollout_path: None,
            last_event_at: Some(Timestamp::EPOCH),
            signal: ActivitySignal::RolloutTail,
            originator: None,
            cwd: None,
        }],
    }]))
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while matches!(flow.stage(), Stage::Armed { .. }) {
        flow.watch_turns(service.clone(), Duration::from_secs(2));
        assert!(
            std::time::Instant::now() < deadline,
            "background activity must arrive"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let Stage::WaitingBeforeSwitch {
        countdown, busy, ..
    } = flow.stage()
    else {
        panic!("waiting for the busy conversation")
    };
    assert!(countdown.is_paused());
    assert_eq!(busy.len(), 1);
    assert_eq!(
        countdown.remaining(Duration::from_secs(60)),
        Duration::from_secs(3)
    );
    flow.tick(Duration::from_secs(60));
    assert!(flow.take_commands().is_empty());
}

#[test]
fn restore_choice_explains_the_default_and_each_button_replans_its_policy() {
    use superba_core::{PlanChoice, ProviderRestore, RestoreTarget, SwitchIntent};
    use support::ui::Harness;

    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for (key, provider) in [
            (
                "banner.restore.reverse",
                ProviderRestore::Baseline {
                    reverse_migrate: true,
                },
            ),
            ("banner.restore.keep", ProviderRestore::KeepOpenai),
            (
                "banner.restore.exact",
                ProviderRestore::Baseline {
                    reverse_migrate: false,
                },
            ),
        ] {
            let mut p = plan();
            p.request =
                superba_gui::app::planned_request(&superba_gui::pages::Action::restore_original())
                    .unwrap();
            p.choices.push(PlanChoice::RestoreProvider {
                baseline_provider: "legacy".into(),
                created_during: 2,
                migrated_in: 1,
            });
            let mut flow = Flow::new(0);
            flow.request(p.request.clone(), Duration::ZERO);
            flow.take_commands();
            flow.on_planned(1, Ok(p), Duration::ZERO);
            let h = Harness::new(size::MIN_WINDOW, egui::Theme::Dark);
            let mut state = BannerState::default();
            let data = Data::default();
            let service = support::idle_service();
            {
                let mut show = |ui: &mut egui::Ui| {
                    ui.set_width(size::MIN_WINDOW[0] - size::SIDEBAR - 64.0);
                    let cx = Cx {
                        tr: &tr,
                        pal: Palette::of(&h.ctx),
                        data: &data,
                        busy: false,
                        now: Timestamp::EPOCH,
                        fonts: &h.fonts,
                        service: &service,
                        log: &[],
                    };
                    shell::banner(
                        ui,
                        &cx,
                        &mut flow,
                        &mut state,
                        Duration::ZERO,
                        f32::INFINITY,
                    );
                };
                let texts = h.settle(&mut show);
                for note in ["banner.restore.reverse_note", "banner.restore.keep_note"] {
                    assert!(
                        texts.iter().any(|(text, _)| text == &tr.t(note)),
                        "{texts:?}"
                    );
                }
                let labels: Vec<&str> = texts.iter().map(|(text, _)| text.as_str()).collect();
                let position = |key| labels.iter().position(|text| *text == tr.t(key)).unwrap();
                assert!(position("banner.restore.reverse") < position("banner.restore.keep"));
                assert!(h.click_text(&tr.t(key), &mut show), "{key}");
            }
            let commands = flow.take_commands();
            let [Command::Plan { request, .. }] = commands.as_slice() else {
                panic!("expected replanning, got {commands:?}");
            };
            assert_eq!(
                request.intent,
                SwitchIntent::Restore {
                    target: RestoreTarget::Original { provider }
                }
            );
        }
    }
}

#[test]
fn risk_disclosure_requires_a_seen_and_completed_switch_and_can_be_reopened() {
    let ctx = context();
    let mut flow = armed();
    let mut state = BannerState::default();
    assert!(!state.complete_switch(&ctx, "shell-plan"));
    let (full_height, text) = frame(&ctx, &mut flow, &mut state, 1180.0);
    assert!(text.iter().any(|s| s == "MCP servers restart"));
    assert!(!state.complete_switch(&ctx, "another-plan"));
    assert!(!BannerState::risks_seen(&ctx));
    assert!(state.complete_switch(&ctx, "shell-plan"));
    let (compact_height, text) = frame(&ctx, &mut flow, &mut state, 1180.0);
    assert!(text.iter().any(|s| s == "What restarts?"));
    assert!(!text.iter().any(|s| s == "MCP servers restart"));
    assert!(
        compact_height < full_height - 60.0,
        "{compact_height} vs {full_height}"
    );
    assert!(
        compact_height <= 125.0,
        "compact banner height: {compact_height}"
    );
    state.show_risks = true;
    let (_, text) = frame(&ctx, &mut flow, &mut state, 1180.0);
    assert!(text.iter().any(|s| s == "MCP servers restart"));
    let mut recreated_state = BannerState::default();
    let (_, text) = frame(&ctx, &mut flow, &mut recreated_state, 1180.0);
    assert!(!text.iter().any(|s| s == "MCP servers restart"));
}

#[test]
fn recovery_banner_displays_failure_without_a_running_timer() {
    let ctx = context();
    let mut flow = armed();
    flow.switch_now(Duration::ZERO);
    flow.on_event(
        &SwitchEvent::Phase {
            phase: SwitchPhase::Recovery,
            at: Timestamp::EPOCH,
        },
        Duration::from_secs(1),
    );
    let (_, text) = frame(
        &ctx,
        &mut flow,
        &mut BannerState::default(),
        size::MIN_WINDOW[0],
    );
    assert!(
        text.iter()
            .any(|s| s == "Switching to “Test destination” failed")
    );
    assert!(!text.iter().any(|s| s == "2.0 s"));
    assert!(!text.iter().any(|s| s == "Switching to “Test destination”"));
}

/// Every sidebar label with its rectangle, the sidebar laid out as the app does
/// (left panel, the shell's frame) in a `window` sized window.
fn sidebar_texts(window: [f32; 2], locale: Locale) -> Vec<(String, Rect)> {
    sidebar_texts_with(window, locale, true)
}

/// [`sidebar_texts`], the tools' entries shown (all six installed) or not.
fn sidebar_texts_with(window: [f32; 2], locale: Locale, tools: bool) -> Vec<(String, Rect)> {
    use superba_gui::pages::{Page, PageStatus};
    let h = support::ui::Harness::new(window, egui::Theme::Light);
    let tr = Tr::new(locale);
    let data = Data::default();
    let svc = support::idle_service();
    let fonts_report = fonts::FontReport::default();
    let draw = |ui: &mut egui::Ui| {
        egui::Panel::left("nav")
            .exact_size(size::SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(shell::sidebar_frame())
            .show(ui, |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data: &data,
                    busy: false,
                    now: Timestamp::EPOCH,
                    fonts: &fonts_report,
                    service: &svc,
                    log: &[],
                };
                let mut page = Page::Overview;
                let mut actions = Vec::new();
                let status = |p: Page| PageStatus {
                    hidden: !tools && matches!(p, Page::Tool(_)),
                    ..PageStatus::default()
                };
                shell::sidebar(ui, &cx, &mut page, &status, &mut actions);
            });
    };
    let mut texts = Vec::new();
    for _ in 0..2 {
        let output = h.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, window.into())),
                ..Default::default()
            },
            draw,
        );
        texts = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some((
                    text.galley.text().to_owned(),
                    text.galley
                        .rect
                        .translate(text.pos.to_vec2())
                        .intersect(shape.clip_rect),
                )),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
    }
    texts
}

#[test]
fn the_sidebar_never_draws_pages_under_the_pinned_ones_at_the_minimum_window() {
    // Review ux-visual #2: at 960x640 Updates was drawn over the pinned Diagnostics.
    use superba_gui::pages::Page;
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        // Two rows may share a label ("Settings" of Codex and of Claude
        // Code): the n-th page with a label is its n-th drawing.
        let rect_of = |texts: &[(String, Rect)], page: Page| {
            let label = tr.t(page.nav_key());
            let nth = Page::ALL
                .iter()
                .take_while(|p| **p != page)
                .filter(|p| tr.t(p.nav_key()) == label)
                .count();
            texts
                .iter()
                .filter(|(t, _)| *t == label)
                .nth(nth)
                .map(|(_, r)| *r)
                .unwrap_or_else(|| panic!("{page:?} ({label}) not drawn in {locale:?}"))
        };
        for window in [size::MIN_WINDOW, [1280.0, 800.0]] {
            let texts = sidebar_texts(window, locale);
            let pinned: Vec<Rect> = Page::ALL
                .into_iter()
                .filter(|p| p.is_pinned())
                .map(|p| rect_of(&texts, p))
                .collect();
            let top_pinned = pinned.iter().map(|r| r.min.y).fold(f32::INFINITY, f32::min);
            for page in Page::ALL.into_iter().filter(|p| !p.is_pinned()) {
                let r = rect_of(&texts, page);
                // A short window scrolls the list. Test the visible text,
                // excluding glyphs outside the scroll area's clip rectangle.
                if !r.is_positive() {
                    continue;
                }
                assert!(
                    r.max.y <= top_pinned,
                    "{page:?} at {window:?} ({locale:?}) reaches the pinned pages: {r:?} vs {top_pinned}"
                );
                assert!(
                    pinned.iter().all(|p| !p.intersects(r)),
                    "{page:?} at {window:?}"
                );
                assert!(
                    r.max.y <= window[1],
                    "{page:?} at {window:?} is off the window"
                );
            }
            // The comfortable rows stay where there is room (user bug 85:
            // 34 + 2 px, tighter than the 40 + 2 px they were).
            let step = rect_of(&texts, Page::Models).center().y
                - rect_of(&texts, Page::Providers).center().y;
            if window[1] >= 800.0 {
                // All six tools shown: at least the compact rows.
                assert!(
                    step >= size::NAV_ROW_COMPACT + size::NAV_GAP,
                    "rows at {window:?}: {step}"
                );
            } else {
                // All six tools installed (user request 114): the tight rows.
                assert!(
                    step >= size::NAV_ROW_TIGHT + size::NAV_GAP,
                    "tight rows at {window:?}: {step}"
                );
            }
        }
        // Without the tools: the comfortable rows at 1280x800.
        let texts = sidebar_texts_with([1280.0, 800.0], locale, false);
        let y = |page: Page| {
            texts
                .iter()
                .find(|(t, _)| *t == tr.t(page.nav_key()))
                .map(|(_, r)| r.center().y)
                .unwrap()
        };
        assert_eq!(
            y(Page::Models) - y(Page::Providers),
            size::NAV_ROW + size::NAV_GAP
        );
        // And the compact rows at 960x640.
        let texts = sidebar_texts_with(size::MIN_WINDOW, locale, false);
        let y = |page: Page| {
            texts
                .iter()
                .find(|(t, _)| *t == tr.t(page.nav_key()))
                .map(|(_, r)| r.center().y)
                .unwrap()
        };
        assert!(y(Page::Models) - y(Page::Providers) >= size::NAV_ROW_COMPACT + size::NAV_GAP);
        assert!(!texts.iter().any(|(t, _)| *t == tr.t("nav.group.others")));
    }
}

/// Keep page/action state across real press/release frames inside the app's panel.
fn simple_sidebar_page<'a>(
    harness: &'a support::ui::Harness,
    tr: &'a Tr,
    data: &'a Data,
    busy: bool,
    modal: bool,
    page: &'a mut superba_gui::pages::Page,
    actions: &'a mut Vec<superba_gui::pages::Action>,
) -> impl FnMut(&mut egui::Ui) + 'a {
    use superba_gui::pages::{Page, PageStatus};
    let service = support::idle_service();
    move |ui| {
        egui::Panel::left("simple-test-nav")
            .exact_size(size::SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(shell::sidebar_frame())
            .show(ui, |ui| {
                let cx = Cx {
                    tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy,
                    now: Timestamp::EPOCH,
                    fonts: &harness.fonts,
                    service: &service,
                    log: &[],
                };
                // A dialog from a hidden page must also prevent a mode change.
                let status = |p: Page| PageStatus {
                    modal: modal && p == Page::Models,
                    ..Default::default()
                };
                shell::simple_sidebar(ui, &cx, page, &status, actions);
            });
        let cx = Cx {
            tr,
            pal: Palette::of(ui.ctx()),
            data,
            busy,
            now: Timestamp::EPOCH,
            fonts: &harness.fonts,
            service: &service,
            log: &[],
        };
        superba_gui::mode_switch::title_button(
            ui.ctx(),
            &cx,
            true,
            data.profiles.value.is_some() && !busy && !modal,
            actions,
        );
    }
}

#[test]
fn simple_sidebar_has_four_destinations_and_pins_superba_at_the_bottom() {
    use superba_gui::pages::Page;
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut data = Data::default();
        data.profiles
            .finish(Ok(superba_core::ProfileSet::default()));
        for window in [[960.0, 640.0], [1280.0, 800.0]] {
            let harness = support::ui::Harness::new(window, egui::Theme::Light);
            let mut page = Page::Overview;
            let mut actions = Vec::new();
            let mut show =
                simple_sidebar_page(&harness, &tr, &data, false, false, &mut page, &mut actions);
            let texts = harness.settle(&mut show);
            let navigation = [
                tr.t("nav.overview"),
                tr.t("nav.providers"),
                tr.t("nav.router"),
                tr.t("simple.usage.title"),
            ];
            let allowed = [
                navigation[0].clone(),
                navigation[1].clone(),
                navigation[2].clone(),
                navigation[3].clone(),
                tr.t("simple.short_simple"),
                tr.t("simple.short_full"),
                tr.t("simple.troubleshooting"),
                tr.t("superba.title"),
                format!("v{}", env!("CARGO_PKG_VERSION")),
            ];
            assert_eq!(
                texts.len(),
                allowed.len(),
                "{locale:?} {window:?}: {texts:?}"
            );
            for label in &allowed {
                assert_eq!(
                    texts.iter().filter(|(text, _)| text == label).count(),
                    1,
                    "missing or duplicated {label}: {texts:?}"
                );
            }
            let sidebar = Rect::from_min_size(pos2(0.0, 0.0), vec2(size::SIDEBAR, window[1]));
            for (text, rect) in &texts {
                assert!(
                    sidebar.expand(0.5).contains_rect(*rect),
                    "{text} is outside {window:?}: {rect:?}"
                );
                assert!(
                    allowed.contains(text),
                    "extra group/page/maintenance entry: {text}"
                );
            }
            let rect = |label: &str| texts.iter().find(|(text, _)| text == label).unwrap().1;
            assert!(rect(&navigation[0]).max.y < rect(&navigation[1]).min.y);
            assert!(rect(&navigation[1]).max.y < rect(&navigation[2]).min.y);
            assert!(rect(&navigation[2]).max.y < rect(&navigation[3]).min.y);
            assert!(
                rect(&navigation[3]).max.y < rect(&tr.t("superba.title")).min.y,
                "Superba must stay below the main navigation: {texts:?}"
            );
            assert!(rect(&tr.t("simple.short_simple")).max.y < rect(&navigation[0]).min.y);
            assert!(
                rect(&tr.t("simple.short_simple")).max.x < rect(&tr.t("simple.short_full")).min.x
            );
            assert!(
                rect(&tr.t("simple.troubleshooting")).max.y < rect(&tr.t("superba.title")).min.y
            );
        }
    }
}

#[test]
fn simple_sidebar_clicks_select_all_pages_and_request_full_mode() {
    use superba_gui::pages::{Action, Page};
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut data = Data::default();
        data.profiles
            .finish(Ok(superba_core::ProfileSet::default()));
        let harness = support::ui::Harness::new([960.0, 640.0], egui::Theme::Dark);
        let mut page = Page::Overview;
        let mut actions = Vec::new();
        for (target, key) in [
            (Page::Providers, "nav.providers"),
            (Page::Router, "nav.router"),
            (Page::Usage, "simple.usage.title"),
            (Page::Troubleshooting, "simple.troubleshooting"),
            (
                Page::Superba(superba_gui::pages::SuperbaSection::Home),
                "superba.title",
            ),
            (Page::Overview, "nav.overview"),
        ] {
            {
                let mut show = simple_sidebar_page(
                    &harness,
                    &tr,
                    &data,
                    false,
                    false,
                    &mut page,
                    &mut actions,
                );
                assert!(
                    harness.click_text(&tr.t(key), &mut show),
                    "{key} is not clickable"
                );
            }
            assert_eq!(page, target, "{locale:?} {key}");
            assert!(
                actions.is_empty(),
                "navigation should only change Page: {actions:?}"
            );
        }
        {
            let mut show =
                simple_sidebar_page(&harness, &tr, &data, false, false, &mut page, &mut actions);
            assert!(harness.click_text(&tr.t("simple.short_full"), &mut show));
        }
        assert!(
            matches!(actions.as_slice(), [Action::SetSimpleMode(false)]),
            "{actions:?}"
        );
        assert_eq!(page, Page::Overview);
    }
}

#[test]
fn simple_sidebar_cannot_change_mode_while_busy_in_a_dialog_or_before_loading_settings() {
    use superba_gui::pages::Page;
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for (loaded, busy, modal) in [
            (true, true, false),
            (true, false, true),
            (false, false, false),
        ] {
            let harness = support::ui::Harness::new([960.0, 640.0], egui::Theme::Light);
            let mut data = Data::default();
            if loaded {
                data.profiles
                    .finish(Ok(superba_core::ProfileSet::default()));
            }
            let mut page = Page::Overview;
            let mut actions = Vec::new();
            {
                let mut show =
                    simple_sidebar_page(&harness, &tr, &data, busy, modal, &mut page, &mut actions);
                assert!(
                    harness.click_text(&tr.t("simple.short_full"), &mut show),
                    "disabled switch remains visible"
                );
            }
            assert!(
                actions.is_empty(),
                "loaded={loaded} busy={busy} modal={modal}: {actions:?}"
            );
            assert_eq!(page, Page::Overview);
        }
    }
}

#[tokio::test]
async fn the_toast_after_a_switch_names_what_was_applied() {
    // User bug 25: re-applying the provider in use (its settings changed) said
    // "已切换到 <line>".
    use superba_core::{ActiveState, HostSyncSelection, SwitchIntent, SwitchService, TargetId};
    use superba_gui::shell::Applied;
    let service = support::fake::FakeService::new(support::fake::FakeOptions {
        fast: true,
        ..Default::default()
    });
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    let active = data.active_state().cloned().expect("krill on overseas");
    assert_eq!(
        active.line_id.as_ref().map(|l| l.as_str()),
        Some("overseas")
    );
    let tr = Tr::new(Locale::ZhCn);
    let host = |id: &TargetId| id.as_str().trim_start_matches("ssh:").to_owned();
    let to = |line: &str| PlanDestination {
        profile_id: Some("krill".into()),
        site_id: Some("krill".into()),
        line_id: Some(line.into()),
        auth_mode: Some(AuthMode::Pat),
        label: LocalizedText::new("test.destination", "Krill"),
    };
    let from = |a: &ActiveState| SwitchPlan {
        from: Some(a.clone()),
        request: SwitchRequest::profile("krill".into()),
        ..plan()
    };
    // Same provider and line: a re-apply.
    let reapply = SwitchPlan {
        to: to("overseas"),
        ..from(&active)
    };
    assert!(shell::is_reapply(&reapply));
    let applied = shell::applied(&tr, &data, &reapply, &host);
    assert!(matches!(applied, Applied::Reapplied(_)), "{applied:?}");
    let title = applied.title(&tr);
    assert!(title.starts_with("已把改动应用到「Krill"), "{title}");
    assert!(title.contains("海外线路"), "{title}");
    assert!(!title.contains("已切换"), "{title}");
    assert!(applied.log_text(&tr, 1200).ends_with("用时 1.2 s"));
    // Another line: a switch that names the line.
    let line = SwitchPlan {
        to: to("cdn"),
        ..from(&active)
    };
    assert!(!shell::is_reapply(&line));
    let title = shell::applied(&tr, &data, &line, &host).title(&tr);
    assert_eq!(title, "已切换到 三网优化 CDN");
    // Router mode and a host sync say what they did.
    let mut router = reapply.clone();
    router.request.intent = SwitchIntent::Router { enable: true };
    assert_eq!(
        shell::applied(&tr, &data, &router, &host).title(&tr),
        "本地路由已开启"
    );
    let mut sync = reapply.clone();
    sync.request.intent = SwitchIntent::HostSync {
        target_id: TargetId::remote("devbox"),
        selection: HostSyncSelection::default(),
    };
    assert_eq!(
        shell::applied(&tr, &data, &sync, &host).title(&tr),
        "已同步设置到 devbox"
    );
    // English.
    let en = Tr::new(Locale::En);
    let title = shell::applied(&en, &data, &reapply, &host).title(&en);
    assert!(
        title.starts_with("Applied the changes to “Krill"),
        "{title}"
    );
}

#[test]
fn the_progress_banner_of_a_reapply_says_it_applies_the_changes() {
    // User bug 25: re-applying the provider in use showed "Switching to …".
    use superba_core::ActiveState;
    let ctx = context();
    for (reapply, want) in [
        (true, "Applying the changes to “Test destination”"),
        (false, "Switching to “Test destination”"),
    ] {
        let mut plan = plan();
        if reapply {
            plan.from = Some(ActiveState {
                profile_id: Some("test".into()),
                site_id: Some("test".into()),
                line_id: Some("test-line".into()),
                auth_mode: Some(AuthMode::Pat),
                generation: 1,
                applied_at: Timestamp::EPOCH,
            });
        }
        let mut flow = Flow::new(0);
        flow.request(plan.request.clone(), Duration::ZERO);
        let commands = flow.take_commands();
        let [Command::Plan { seq, .. }] = commands.as_slice() else {
            panic!("plan command");
        };
        flow.on_planned(*seq, Ok(plan), Duration::ZERO);
        assert!(matches!(
            flow.take_commands().as_slice(),
            [Command::Execute { .. }]
        ));
        let mut state = BannerState::default();
        let (_, texts) = frame(&ctx, &mut flow, &mut state, 1280.0);
        assert!(texts.iter().any(|t| t == want), "{want}: {texts:?}");
    }
}

#[test]
fn a_remote_step_is_named_by_the_host_title_not_its_app_id() {
    // User bug 68 run: "Host remote-wsl:Ubuntu-WSL1" in the switch banner.
    let tr = Tr::new(superba_core::Locale::En);
    let step = |id: &str| superba_core::SwitchStep::Remote {
        target_id: superba_core::TargetId::new(id),
    };
    assert_eq!(
        shell::step_label(&tr, &step("ssh:remote-wsl:Ubuntu-WSL1")),
        "Host WSL: Ubuntu-WSL1"
    );
    assert_eq!(shell::step_label(&tr, &step("ssh:devbox")), "Host devbox");
}

#[test]
fn mode_ripple_covers_every_corner_and_commits_only_after_coverage() {
    use superba_gui::mode_switch::{Transition, cover_radius};
    let ctx = egui::Context::default();
    let mut transition = Transition::default();
    for size in [vec2(960.0, 640.0), vec2(1440.0, 900.0)] {
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        let origin = pos2(70.0, 16.0);
        let radius = cover_radius(screen, origin);
        for p in [
            screen.left_top(),
            screen.right_top(),
            screen.left_bottom(),
            screen.right_bottom(),
        ] {
            assert!(radius >= p.distance(origin));
        }
    }
    let mut at = |time, begin, reduce| {
        let mut committed = None;
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                ..Default::default()
            },
            |_| {
                if begin {
                    transition.start(&ctx, true, reduce);
                }
                committed = transition.advance(&ctx);
            },
        );
        output.drop_without_applying_deltas();
        committed
    };
    assert_eq!(at(0.0, true, false), None);
    assert_eq!(at(0.3, false, false), None);
    assert_eq!(at(0.43, false, false), Some(true));
    assert_eq!(at(0.6, false, false), None);
    assert_eq!(at(1.0, false, false), None);
    assert_eq!(at(1.1, true, true), Some(true));
}
