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
    use superba_gui::pages::{Page, PageStatus};
    let h = support::ui::Harness::new(window, egui::Theme::Light);
    let tr = Tr::new(locale);
    let data = Data::default();
    let svc = support::idle_service();
    let fonts_report = fonts::FontReport::default();
    h.settle(&mut |ui: &mut egui::Ui| {
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
                shell::sidebar(ui, &cx, &mut page, &|_| PageStatus::default(), &mut actions);
            });
    })
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
                .filter(|p| p.is_superba())
                .map(|p| rect_of(&texts, p))
                .collect();
            let top_pinned = pinned.iter().map(|r| r.min.y).fold(f32::INFINITY, f32::min);
            for page in Page::ALL.into_iter().filter(|p| !p.is_superba()) {
                let r = rect_of(&texts, page);
                // Labels sit centred in their rows: a whole row gap between the
                // last page and the first pinned one, and no overlap at all.
                assert!(
                    r.max.y + 12.0 <= top_pinned,
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
                assert_eq!(step, size::NAV_ROW + size::NAV_GAP, "rows at {window:?}");
            } else {
                assert!(
                    step >= size::NAV_ROW_COMPACT + size::NAV_GAP,
                    "compact rows at {window:?}: {step}"
                );
            }
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
