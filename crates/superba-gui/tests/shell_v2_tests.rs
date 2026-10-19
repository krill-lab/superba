#![allow(missing_docs)]

//! Shell: command palette ranking, page groups and layouts, catalog hygiene, a
//! render smoke test of the overview with real-shaped account data (no demo
//! backend exists any more), the wizard profile and router watch toasts.

mod support;

use std::collections::BTreeSet;

use superba_core::{
    AccountReport, AccountSummary, AuthChoice, BillingSummary, DailyUsage, LineId, Locale,
    ModelUsage, ProfileId, SiteId, SwitchService, Timestamp, UsageReport, Usd,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::{Tr, ui_files};
use superba_gui::pages::{
    Action, Cx, Data, NavGroup, Page, PageLayout, PaletteGroup, PaletteItem, overview,
};
use superba_gui::palette::{filter, score};
use support::fake::{FakeOptions, FakeService};

fn item(title: &str, group: PaletteGroup, keywords: &[&str]) -> PaletteItem {
    PaletteItem {
        title: title.into(),
        detail: None,
        keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
        group,
        action: Action::Refresh,
    }
}

#[test]
fn palette_matches_every_word_and_ranks_title_prefixes_first() {
    let a = item("Switch to Overseas", PaletteGroup::Switch, &["overseas"]);
    let b = item("Overview", PaletteGroup::Navigate, &["overview"]);
    let c = item(
        "Settings: model",
        PaletteGroup::Settings,
        &["model_reasoning_effort"],
    );
    assert_eq!(score("", &a), Some(1000));
    assert!(score("over", &b).unwrap() < score("over", &a).unwrap());
    assert!(score("to over", &a).is_some(), "all words must match");
    assert!(score("to xyz", &a).is_none());
    assert!(score("reasoning", &c).is_some(), "keywords are searched");
    assert!(score("OVERSEAS", &a).is_some(), "case-insensitive");
}

#[test]
fn palette_keeps_group_order_and_caps_results() {
    let items = vec![
        item("Go to Overview", PaletteGroup::Navigate, &[]),
        item("Switch to CDN", PaletteGroup::Switch, &[]),
        item("Turn the router on", PaletteGroup::Router, &[]),
    ];
    let out = filter("", items);
    let groups: Vec<_> = out.iter().map(|i| i.group).collect();
    assert_eq!(
        groups,
        vec![
            PaletteGroup::Switch,
            PaletteGroup::Router,
            PaletteGroup::Navigate
        ]
    );
    let many: Vec<_> = (0..200)
        .map(|i| item(&format!("Key {i}"), PaletteGroup::Settings, &[]))
        .collect();
    assert_eq!(filter("key", many).len(), superba_gui::palette::MAX_RESULTS);
}

#[test]
fn pages_are_grouped_and_every_label_is_translated() {
    let groups: Vec<NavGroup> = Page::ALL
        .iter()
        .filter(|p| !p.is_superba())
        .map(|p| p.group())
        .collect();
    // Groups are contiguous in navigation order.
    let mut seen = Vec::new();
    for g in groups {
        if seen.last() != Some(&g) {
            assert!(!seen.contains(&g), "group {g:?} split in the sidebar");
            seen.push(g);
        }
    }
    assert_eq!(
        seen,
        vec![
            NavGroup::Main,
            NavGroup::Connect,
            NavGroup::Codex,
            NavGroup::Claude,
        ]
    );
    assert_eq!(Page::ALL.len(), 12);
    assert_eq!(Page::ALL[1], Page::Tools);
    assert_eq!(
        Page::ALL[11],
        Page::Superba(superba_gui::pages::SuperbaSection::Home)
    );
    assert!(
        Page::DESTINATIONS
            .iter()
            .all(|p| p.layout() == PageLayout::Fit)
    );
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for p in Page::ALL {
            for key in [p.title_key(), p.subtitle_key()] {
                assert_ne!(tr.t(key), key, "{key} missing in {locale:?}");
            }
        }
        for g in [NavGroup::Connect, NavGroup::Codex, NavGroup::Claude] {
            let key = g.caption_key().unwrap();
            assert_ne!(tr.t(key), key);
        }
        for g in [
            PaletteGroup::Switch,
            PaletteGroup::Router,
            PaletteGroup::Settings,
            PaletteGroup::Navigate,
            PaletteGroup::Commands,
        ] {
            assert_ne!(tr.t(g.caption_key()), g.caption_key());
        }
    }
}

#[test]
fn ui_catalogs_no_longer_mention_demo_mode_or_context_management() {
    for locale in [Locale::En, Locale::ZhCn] {
        let source: String = ui_files(locale).iter().map(|(_, s)| *s).collect();
        for banned in [
            "Demo data",
            "demo mode",
            "演示",
            "Context management",
            "上下文管理",
        ] {
            assert!(!source.contains(banned), "{banned:?} still in {locale:?}");
        }
    }
}

fn report() -> AccountReport {
    let mut billing = BillingSummary {
        limit: Usd::from_dollars(300.0),
        used: Usd::from_dollars(139.31),
        plan_title: Some("Pro".into()),
        access_until: Some(Timestamp::from_millis(1_793_491_200_000)),
        ..BillingSummary::default()
    };
    billing.derive_remaining();
    let day = 86_400_000;
    let start = Timestamp::from_millis(1_792_454_400_000);
    AccountReport {
        profile_id: ProfileId::new("krill"),
        profile_name: "Krill".into(),
        site_id: SiteId::new("krill"),
        line_id: LineId::new("overseas"),
        summary: Some(AccountSummary {
            email: Some("dev@example.com".into()),
            key_last4: Some("7a3c".into()),
            billing: Some(billing),
            ..AccountSummary::default()
        }),
        summary_error: None,
        usage: Some(UsageReport {
            site_id: SiteId::new("krill"),
            start,
            end: Timestamp::from_millis(start.as_millis() + 7 * day),
            days: 7,
            daily: (0..7)
                .map(|i| DailyUsage {
                    start: Timestamp::from_millis(start.as_millis() + i * day),
                    cost: Usd::from_dollars(4.0 + i as f64),
                    requests: Some(300),
                    input_tokens: Some(1_000_000),
                    output_tokens: Some(200_000),
                })
                .collect(),
            models: vec![ModelUsage {
                model: "gpt-5.5".into(),
                cost: Usd::from_dollars(49.0),
                requests: 2_100,
                input_tokens: 7_000_000,
                cached_input_tokens: 3_000_000,
                output_tokens: 1_400_000,
            }],
            total_cost: Usd::from_dollars(49.0),
            cost_source: Some(superba_core::CostSource::Costs),
            usage_available: true,
            fetched_at: Timestamp::now(),
            from_cache: false,
            notice: None,
        }),
        usage_error: None,
    }
}

fn texts(output: &egui::FullOutput) -> BTreeSet<String> {
    output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_overview_renders_real_account_data_in_both_themes_and_languages() {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data.account.finish(Ok(report()));
    let fonts_report = fonts::FontReport::default();
    let svc = support::idle_service();
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        for locale in [Locale::En, Locale::ZhCn] {
            let ctx = egui::Context::default();
            ctx.set_fonts(fonts::definitions(&fonts_report));
            style::install(&ctx, Some(theme));
            let tr = Tr::new(locale);
            {
                let mut actions = Vec::new();
                let mut account = String::new();
                let mut run = |ctx: &egui::Context| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(1060.0, 2000.0),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            let cx = Cx {
                                tr: &tr,
                                pal: Palette::of(ui.ctx()),
                                data: &data,
                                busy: false,
                                now: Timestamp::now(),
                                fonts: &fonts_report,
                                service: &svc,
                                log: &[],
                            };
                            account = overview::account_lines(&cx).join("\n");
                            overview::show(ui, &cx, &mut overview::State::default(), &mut actions);
                        },
                    )
                };
                run(&ctx).drop_without_applying_deltas();
                let out = run(&ctx);
                let shown = texts(&out);
                out.drop_without_applying_deltas();
                let all = shown.iter().cloned().collect::<Vec<_>>().join("\n");
                // The provider node carries the balance; the provider popover
                // lists the account.
                assert!(all.contains("$160.69"), "remaining balance: {all}");
                assert!(account.contains("dev@example.com"), "{account}");
                assert!(account.contains("$160.69"), "{account}");
                assert!(account.contains("2026-11-01"), "access until: {account}");
                assert!(account.contains("nb_…7a3c"), "{account}");
                assert!(!all.to_lowercase().contains("demo"), "{all}");
            }
        }
    }
}

#[test]
fn the_wizard_keeps_an_existing_krill_profile_and_seeds_a_new_one() {
    use superba_core::{CodexSettings, KeyRef};
    use superba_gui::pages::wizard::wizard_profile;
    let krill = superba_core::catalog::krill_site();

    let new = wizard_profile(None, &krill, Some(LineId::new("cdn")), None);
    assert_eq!(new.line_priority(), vec![LineId::new("cdn")]);
    assert_eq!(new.auth_choice(), AuthChoice::Auto);
    assert_eq!(new.codex_settings, CodexSettings::krill_recommended());

    let mut existing = new.clone();
    existing.name = "Work".into();
    existing.key_ref = Some(KeyRef::Env { var: "K".into() });
    existing.set_line_priority([LineId::new("overseas"), LineId::new("cdn")]);
    existing.set_auth(AuthChoice::ApiKey);
    existing.codex_settings = CodexSettings::new();
    existing
        .codex_settings
        .set("model_verbosity", "high")
        .unwrap();
    let again = wizard_profile(
        Some(&existing),
        &krill,
        Some(LineId::new("cdn")),
        Some("gpt-6-luna".into()),
    );
    assert_eq!(again.model.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(again.name, "Work");
    assert_eq!(again.key_ref, existing.key_ref);
    assert_eq!(
        again.line_priority(),
        vec![LineId::new("cdn"), LineId::new("overseas")]
    );
    // A re-run keeps the provider's authentication choice (review ux-visual #6).
    assert_eq!(again.auth_choice(), AuthChoice::ApiKey);
    assert_eq!(
        again.codex_settings, existing.codex_settings,
        "settings kept"
    );
}

#[test]
fn router_watch_toasts_only_on_changes_that_matter() {
    use superba_core::RouterWatchState as W;
    use superba_gui::app::router_watch_notice;
    use superba_gui::design::widgets::Status;
    let suspect = W::Suspect {
        detail: "connection refused".into(),
    };
    let fell = W::FellBack {
        detail: "down for 20 s".into(),
    };
    assert_eq!(router_watch_notice(None, &W::Healthy), None);
    assert_eq!(router_watch_notice(None, &W::Inactive), None);
    assert_eq!(
        router_watch_notice(Some(&W::Healthy), &suspect),
        Some((Status::Warn, "router.watch.suspect"))
    );
    assert_eq!(
        router_watch_notice(Some(&suspect), &suspect),
        None,
        "once per episode"
    );
    assert_eq!(
        router_watch_notice(Some(&suspect), &W::Healthy),
        Some((Status::Ok, "router.watch.recovered"))
    );
    assert_eq!(
        router_watch_notice(Some(&suspect), &fell),
        Some((Status::Error, "router.watch.fell_back"))
    );
    assert_eq!(router_watch_notice(Some(&fell), &W::Inactive), None);
    // User bug 65: the router moved off a taken port and Codex followed.
    let moved = W::Moved {
        from: 18_431,
        to: 19_440,
    };
    assert_eq!(
        router_watch_notice(Some(&suspect), &moved),
        Some((Status::Ok, "router.watch.moved"))
    );
    assert_eq!(router_watch_notice(Some(&moved), &W::Healthy), None);
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let text = tr.f("router.watch.moved", &[("from", &18_431), ("to", &19_440)]);
        assert!(text.contains("18431") && text.contains("19440"), "{text}");
        for key in [
            "router.watch.suspect",
            "router.watch.fell_back",
            "router.watch.fell_back_title",
            "router.watch.recovered",
            "router.watch.moved",
        ] {
            assert_ne!(tr.t(key), key);
        }
    }
}

async fn fake_data() -> Data {
    let service = FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    });
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

#[tokio::test]
async fn every_page_renders_in_its_layout_at_both_window_sizes() {
    use superba_gui::app::PageStates;
    let data = fake_data().await;
    for window in support::fit::SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            for page in Page::DESTINATIONS {
                let mut states = PageStates::default();
                let mut prefs = superba_core::Settings::default();
                let report = support::fit::render_fit(
                    window,
                    locale,
                    egui::Theme::Dark,
                    &data,
                    |ui, cx, out| states.show(page, ui, cx, &mut prefs, out),
                );
                let all = report.all_text();
                assert!(
                    all.contains(&tr.t(page.title_key())),
                    "{page:?} at {window:?} ({locale:?}) shows its title: {all}"
                );
                assert!(
                    !report.scrolled,
                    "{page:?} at {window:?}: no fallback scroll"
                );
                if page.layout() == PageLayout::Fit {
                    assert!(
                        report.fits(),
                        "{page:?} at {window:?} ({locale:?}) drew below its rectangle: {} > {}",
                        report.used_bottom,
                        report.rect.max.y
                    );
                }
            }
        }
        // The page body below the header (UX §2.3; user bug 85: inside the
        // content card, below the title band on Windows and Linux).
        let report =
            support::fit::render_fit(window, Locale::En, egui::Theme::Dark, &data, |_, _, _| {});
        let expected = support::fit::body_height(window);
        assert_eq!(report.body_height, expected, "{window:?}");
    }
}

#[tokio::test]
async fn scaffold_pages_fit_without_any_scroll_area() {
    use superba_gui::app::PageStates;
    let data = fake_data().await;
    // Pages that are already built for the Fit layout; B6/B7 add theirs here as
    // they replace the Phase 0 `layout::scroll_body` wrappers.
    for page in [Page::Models, Page::Hosts] {
        for window in support::fit::SIZES {
            let mut states = PageStates::default();
            let mut prefs = superba_core::Settings::default();
            let report = support::fit::render_fit(
                window,
                Locale::ZhCn,
                egui::Theme::Light,
                &data,
                |ui, cx, out| states.show(page, ui, cx, &mut prefs, out),
            );
            assert!(report.fits(), "{page:?} at {window:?}");
            assert_eq!(report.scroll_areas, 0, "{page:?} at {window:?}");
        }
    }
}

#[tokio::test]
async fn the_sidebar_lists_every_page_in_four_groups() {
    use superba_gui::pages::PageStatus;
    let data = fake_data().await;
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let ctx = egui::Context::default();
            let fonts_report = fonts::FontReport::default();
            ctx.set_fonts(fonts::definitions(&fonts_report));
            style::install(&ctx, Some(theme));
            let svc = support::idle_service();
            let mut texts_seen = BTreeSet::new();
            for _ in 0..2 {
                let out = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(220.0, 800.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let cx = Cx {
                            tr: &tr,
                            pal: Palette::of(ui.ctx()),
                            data: &data,
                            busy: false,
                            now: Timestamp::now(),
                            fonts: &fonts_report,
                            service: &svc,
                            log: &[],
                        };
                        let mut page = Page::Overview;
                        let mut actions = Vec::new();
                        let status = |p: Page| PageStatus {
                            pending: usize::from(p == Page::Settings),
                            badge: if p == Page::Hosts {
                                superba_gui::pages::NavBadge::Count(2)
                            } else {
                                superba_gui::pages::NavBadge::None
                            },
                            ..PageStatus::default()
                        };
                        superba_gui::shell::sidebar(ui, &cx, &mut page, &status, &mut actions);
                    },
                );
                texts_seen = texts(&out);
                let captions: BTreeSet<_> = [NavGroup::Connect, NavGroup::Codex, NavGroup::Claude]
                    .into_iter()
                    .map(|group| tr.t(group.caption_key().unwrap()))
                    .chain([format!("v{}", env!("CARGO_PKG_VERSION"))])
                    .collect();
                for shape in &out.shapes {
                    let egui::Shape::Text(text) = &shape.shape else {
                        continue;
                    };
                    if !captions.contains(text.galley.text()) {
                        continue;
                    }
                    for section in &text.galley.job.sections {
                        let color = text.override_text_color.unwrap_or_else(|| {
                            if section.format.color == egui::Color32::PLACEHOLDER {
                                text.fallback_color
                            } else {
                                section.format.color
                            }
                        });
                        use superba_gui::design::backdrop::{Backdrop, Background};
                        for bg in [
                            Background::Neutral,
                            Background::Mint,
                            Background::Sky,
                            Background::Sand,
                            Background::Lavender,
                        ] {
                            let backdrop = Backdrop::new(bg, theme == egui::Theme::Dark);
                            for surface in [backdrop.top, backdrop.bottom, backdrop.glow] {
                                let ratio =
                                    superba_gui::design::tokens::contrast_ratio(color, surface);
                                assert!(
                                    ratio >= 4.5,
                                    "{}: {theme:?} {bg:?}, contrast {ratio}",
                                    text.galley.text()
                                );
                            }
                        }
                    }
                }
                out.drop_without_applying_deltas();
            }
            for page in Page::ALL {
                assert!(
                    texts_seen.contains(&tr.t(page.nav_key())),
                    "{page:?} missing in {locale:?}: {texts_seen:?}"
                );
            }
            for group in [NavGroup::Connect, NavGroup::Codex, NavGroup::Claude] {
                assert!(texts_seen.contains(&tr.t(group.caption_key().unwrap())));
            }
            // Superba's own pages are pinned at the bottom without a heading; no
            // brand and no search field above the pages (user bug 85), the
            // version below them.
            assert!(
                texts_seen.contains("Superba") && !texts_seen.contains(&tr.t("shell.search")),
                "{texts_seen:?}"
            );
            assert!(
                texts_seen.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))),
                "{texts_seen:?}"
            );
            assert!(texts_seen.contains("2"), "host badge count");
            assert!(
                !texts_seen.iter().any(|t| t.contains("krill")),
                "no provider link in the sidebar footer any more: {texts_seen:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// v0.3 shell (plan §5 B5)
// ---------------------------------------------------------------------------

#[test]
fn the_shell_never_names_a_provider_outside_presets_and_capabilities() {
    // Shell and Overview code paths (UX §8 "Generic shell").
    for (file, source) in [
        ("shell.rs", include_str!("../src/shell.rs")),
        ("app.rs", include_str!("../src/app.rs")),
        ("palette.rs", include_str!("../src/palette.rs")),
        ("pages/mod.rs", include_str!("../src/pages/mod.rs")),
        (
            "pages/overview/mod.rs",
            include_str!("../src/pages/overview/mod.rs"),
        ),
        (
            "pages/overview/cards.rs",
            include_str!("../src/pages/overview/cards.rs"),
        ),
        (
            "pages/overview/usage.rs",
            include_str!("../src/pages/overview/usage.rs"),
        ),
        (
            "pages/overview/hero.rs",
            include_str!("../src/pages/overview/hero.rs"),
        ),
        (
            "pages/overview/lines.rs",
            include_str!("../src/pages/overview/lines.rs"),
        ),
        (
            "pages/overview/sessions.rs",
            include_str!("../src/pages/overview/sessions.rs"),
        ),
        (
            "pages/overview/today.rs",
            include_str!("../src/pages/overview/today.rs"),
        ),
        (
            "pages/overview/todo.rs",
            include_str!("../src/pages/overview/todo.rs"),
        ),
    ] {
        assert!(
            !source.to_lowercase().contains("krill"),
            "{file} names a provider; use presets or capabilities"
        );
    }
    // Navigation, page titles and subtitles, the sidebar.
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for p in Page::ALL {
            for key in [p.title_key(), p.subtitle_key()] {
                assert!(!tr.t(key).to_lowercase().contains("krill"), "{key}");
            }
        }
        assert_eq!(tr.t("brand.for_krill"), "brand.for_krill", "removed");
    }
}

#[test]
fn navigation_texts_and_window_sizes_follow_the_ux_spec() {
    use superba_gui::design::tokens::size;
    assert_eq!(size::DEFAULT_WINDOW, [1280.0, 800.0]);
    assert_eq!(size::MIN_WINDOW, [960.0, 640.0]);
    let en = Tr::new(Locale::En);
    let zh = Tr::new(Locale::ZhCn);
    // Router settings and usage are separate pages (user decision 21).
    assert_eq!(en.t("nav.router"), "Router");
    assert_eq!(zh.t("nav.router"), "路由");
    assert_eq!(en.t("nav.usage"), "Usage");
    assert_eq!(zh.t("nav.usage"), "用量");
    assert_eq!(zh.t("nav.providers"), "供应商");
    assert_eq!(zh.t("nav.hosts"), "远程主机");
    assert_eq!(zh.t("nav.updates"), "更新");
    // The Sessions subtitle names Codex's current provider.
    assert_eq!(
        zh.f("page.sessions.subtitle", &[("provider", &"openai")]),
        "ChatGPT 只显示当前 provider（openai）下的会话。每台主机的会话分开管理。"
    );
}

fn host_view(
    service_overview: &superba_core::Overview,
    pending: Option<u32>,
) -> superba_core::HostView {
    let remote = &service_overview.remotes[0];
    superba_core::HostView {
        host: remote.host.clone(),
        state: remote.state.clone(),
        probe_error: None,
        settings: Default::default(),
        pending_sync: pending,
        hot_restart: true,
        pending: None,
    }
}

#[tokio::test]
async fn sidebar_badges_come_from_the_shells_own_data() {
    use superba_gui::app::shell_badges;
    use superba_gui::design::widgets::Status;
    use superba_gui::pages::NavBadge;
    let mut data = fake_data().await;
    let tr = Tr::new(Locale::ZhCn);
    assert!(shell_badges(&tr, &data).is_empty(), "nothing to say");

    let overview = data.overview.value.clone().unwrap();
    data.hosts.finish(Ok(vec![
        host_view(&overview, Some(3)),
        host_view(&overview, Some(0)),
        host_view(&overview, None),
    ]));
    let mut versions = superba_core::VersionsView::default();
    versions.app.update_available = true;
    data.versions.finish(Ok(versions));
    data.profiles
        .value
        .as_mut()
        .unwrap()
        .settings
        .router
        .enabled = true;

    let badges = shell_badges(&tr, &data);
    let find = |p: Page| badges.iter().find(|(page, ..)| *page == p).cloned();
    assert_eq!(find(Page::Router).unwrap().1, NavBadge::Dot(Status::Ok));
    let (_, hosts, tip) = find(Page::Hosts).unwrap();
    assert_eq!(hosts, NavBadge::Count(1), "hosts with something to sync");
    assert_eq!(tip, "1 台主机有未同步的设置");
    let (_, update, tip) = find(Page::Superba(superba_gui::pages::SuperbaSection::Home)).unwrap();
    assert_eq!(update, NavBadge::Count(1));
    assert_eq!(tip, "有 1 项可更新");

    // A newer CLI for an installed binary also counts.
    let mut versions = superba_core::VersionsView::default();
    versions.cli.push(superba_core::CliVersionRow {
        binary: overview.cli.binaries[0].clone(),
        kind: superba_core::CodexInstallKind::Npm,
        latest: Some(superba_core::CodexVersion::new(0, 170, 0)),
        action: superba_core::CliUpdateAction::Command {
            command: "npm install -g @openai/codex@0.170.0".into(),
        },
    });
    data.versions.finish(Ok(versions));
    assert!(data.update_available());
}

fn models_view(
    profile: &superba_core::Profile,
    offered: Vec<superba_core::SpeedTier>,
) -> superba_core::ModelsView {
    superba_core::ModelsView {
        profile_id: profile.id.clone(),
        site_id: profile.site_id.clone(),
        read_only: false,
        list: superba_core::ModelList {
            source: superba_core::ModelListSource::Upstream {
                path: "/backend-api/codex/models".into(),
                fetched_at: Timestamp::EPOCH,
                etag: None,
            },
            models: Vec::new(),
            fallback_reason: None,
            sources: Vec::new(),
        },
        defaults: profile.model.clone(),
        overrides: Default::default(),
        picker: Default::default(),
        picker_available: false,
        speed: superba_core::SpeedSupport {
            offered,
            cli: superba_core::SpeedEffect::Applies,
            app: superba_core::SpeedEffect::AppForcesStandard,
            tiers: Vec::new(),
        },
        catalog_file: Default::default(),
        notes: Vec::new(),
        per_model: true,
        applied: None,
    }
}

/// The rectangle of the first drawn text that starts with `prefix`.
fn text_rect(texts: &[(String, egui::Rect)], prefix: &str) -> egui::Rect {
    texts
        .iter()
        .find(|(t, _)| t.starts_with(prefix))
        .map(|(_, r)| *r)
        .unwrap_or_else(|| panic!("{prefix} not drawn: {texts:?}"))
}

/// The title of the hero's model node.
fn model_title(tr: &Tr, data: &Data) -> String {
    data.overview
        .value
        .as_ref()
        .and_then(|o| o.config.as_ref())
        .and_then(|c| c.model.clone())
        .unwrap_or_else(|| tr.t("overview.hero.default_model"))
}

#[tokio::test]
async fn the_overview_speed_control_offers_listed_tiers_and_plans_a_switch() {
    use superba_core::SpeedTier;
    use support::ui::Harness;
    let mut data = fake_data().await;
    let profile = data.active_profile().unwrap().clone();
    data.active_models.finish(Ok(models_view(
        &profile,
        vec![SpeedTier::Standard, SpeedTier::Fast],
    )));
    let svc = support::idle_service();
    for fast in [false, true] {
        if fast {
            // Fast chosen: the popover explains where it applies.
            let mut p = profile.clone();
            p.model.speed = Some(SpeedTier::Fast);
            let set = data.profiles.value.as_mut().unwrap();
            let id = p.id.clone();
            *set.profiles.iter_mut().find(|x| x.id == id).unwrap() = p;
        }
        for locale in [Locale::En, Locale::ZhCn] {
            // A fresh window: the popover starts closed.
            let harness = Harness::new([1280.0, 800.0], egui::Theme::Dark);
            let tr = Tr::new(locale);
            let mut out = Vec::new();
            let mut state = overview::State::default();
            let mut draw = |ui: &mut egui::Ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data: &data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &harness.fonts,
                    service: &svc,
                    log: &[],
                };
                overview::show(ui, &cx, &mut state, &mut out);
            };
            // The model node's chevron opens the speed popover.
            let texts = harness.settle(&mut draw);
            let tier = if fast {
                SpeedTier::Fast
            } else {
                SpeedTier::Standard
            };
            let tier = tr.f("overview.hero.tier", &[("tier", &tr.text(&tier.text()))]);
            assert!(texts.iter().any(|(t, _)| *t == tier), "{tier}: {texts:?}");
            let r = text_rect(&texts, &model_title(&tr, &data));
            harness.click_at(egui::pos2(r.max.x + 12.0, r.center().y), &mut draw);
            let texts: Vec<String> = harness
                .settle(&mut draw)
                .into_iter()
                .map(|(t, _)| t)
                .collect();
            assert!(texts.contains(&tr.t("overview.speed.label")), "{texts:?}");
            assert!(texts.contains(&"Fast".to_owned()));
            assert!(
                !texts.contains(&"Ultrafast".to_owned()),
                "not offered: hidden"
            );
            let effect = texts
                .iter()
                .any(|t| t.contains(&tr.t("speed_effect.app_forces_standard")));
            assert_eq!(effect, fast, "the effect line only off Standard: {texts:?}");
            if !fast {
                assert!(harness.click_text("Fast", &mut draw));
                assert!(
                    out.iter().any(|a| matches!(
                        a,
                        Action::SetSpeed { profile: p, tier: SpeedTier::Fast } if *p == profile.id
                    )),
                    "{out:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_official_login_hides_the_speed_control_and_shows_its_account_line() {
    let mut data = fake_data().await;
    let o = data.overview.value.as_mut().unwrap();
    o.active = Some(superba_core::ActiveState {
        profile_id: None,
        site_id: Some(superba_core::catalog::OFFICIAL_SITE_ID.into()),
        line_id: None,
        auth_mode: Some(superba_core::AuthMode::OfficialChatGpt),
        generation: 2,
        applied_at: Timestamp::EPOCH,
    });
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let report = support::fit::render_fit(
            [1280.0, 1400.0],
            locale,
            egui::Theme::Dark,
            &data,
            |ui, cx, out| overview::show(ui, cx, &mut overview::State::default(), out),
        );
        let all = report.all_text();
        // No speed control: the model node says the app chooses the tier and
        // has no popover.
        assert!(all.contains(&tr.t("overview.hero.tier_app")), "{all}");
        assert!(!report.texts.iter().any(|t| t == "Fast"), "{all}");
        assert!(all.contains(&tr.t("overview.account.official")), "{all}");
        let o = data.overview.value.clone().unwrap();
        let svc = support::idle_service();
        let fonts = fonts::FontReport::default();
        let cx = Cx {
            tr: &tr,
            pal: &superba_gui::design::tokens::DARK,
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &fonts,
            service: &svc,
            log: &[],
        };
        let hero = overview::hero::model(&cx, &o, &[], None);
        assert!(!hero.model.menu);
        assert_eq!(hero.provider.title, tr.t("overview.official_chip"));
    }
}

#[tokio::test]
async fn the_overview_lists_other_providers_hosts_and_adds_a_provider() {
    use support::ui::Harness;
    let mut data = fake_data().await;
    let overview_value = data.overview.value.clone().unwrap();
    data.hosts
        .finish(Ok(vec![host_view(&overview_value, Some(2))]));
    let svc = support::idle_service();
    let harness = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = Tr::new(Locale::ZhCn);
    let mut out = Vec::new();
    let mut state = overview::State::default();
    let mut draw = |ui: &mut egui::Ui| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &harness.fonts,
            service: &svc,
            log: &[],
        };
        overview::show(ui, &cx, &mut state, &mut out);
    };
    // The app's remote hosts stack on the 本机 node.
    let texts = harness.settle(&mut draw);
    assert!(texts.iter().any(|(t, _)| t == "+1"), "{texts:?}");
    assert!(
        texts.iter().any(|(t, _)| *t == tr.t("overview.hero.this")),
        "{texts:?}"
    );
    // The provider node's chevron opens the other providers, the official
    // login and Add.
    let r = text_rect(&texts, "Krill");
    harness.click_at(egui::pos2(r.max.x + 12.0, r.center().y), &mut draw);
    let shown = harness
        .settle(&mut draw)
        .into_iter()
        .map(|(t, _)| t)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains(&tr.t("overview.other_providers")), "{shown}");
    assert!(
        shown.contains("官方 ChatGPT"),
        "official login entry: {shown}"
    );
    assert!(harness.click_text(&tr.t("overview.add_provider"), &mut draw));
    assert!(
        out.iter().any(|a| matches!(
            a,
            Action::PageCommand { page: Page::Providers, command } if command == "add"
        )),
        "{out:?}"
    );
}
