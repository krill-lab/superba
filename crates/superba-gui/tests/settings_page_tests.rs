#![allow(missing_docs)]
//! Codex settings page (UX §3.6, plan §5 B6): three fixed panes at 1280×800 and
//! 1440×900 in both languages with the shipped catalog (no page scrolling, 11 / 13
//! rows per page), Chinese-first rows, the Common / Changed rail, the detail pane
//! with per-host sync support and the links of reserved keys, and the side sheet
//! on narrow windows.

mod support;

use superba_core::settings::{CodexSettings, SettingValue};
use superba_core::{Locale, ProfileSet, SwitchService};
use superba_gui::pages::settings::model::{Availability, Model, Rail};
use superba_gui::pages::settings::source::{self, Input};
use superba_gui::pages::settings::{self as settings_page, State};
use superba_gui::pages::{Action, Data};
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit};

fn fake() -> FakeService {
    FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    })
}

async fn data(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

/// The page model from the shipped catalog, the fake overview (app 0.162, CLI,
/// hosts devbox 0.162 and gpu-01 0.158) and the fake Krill profile.
fn model(data: &Data, settings: Option<CodexSettings>) -> Model {
    let mut profiles: ProfileSet = data.profiles.value.clone().unwrap();
    if let Some(s) = settings {
        let krill = profiles
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "krill")
            .unwrap();
        krill.codex_settings = s;
    }
    let input = Input {
        overview: data.overview.value.clone(),
        profiles: Some(profiles),
        profile: None,
    };
    let store = superba_catalog::CatalogStore::builtin().unwrap();
    let mut model = source::build(
        &store,
        &input,
        Some(&toml::Table::new()),
        superba_catalog::Os::Linux,
    )
    .unwrap();
    // The curated Common list (curation.toml `[common]`) may still be empty in
    // this tree: mark a few entries the way the curation does.
    if model.entries.iter().all(|e| e.common.is_none()) {
        for (i, path) in ["approval_policy", "sandbox_mode", "web_search"]
            .iter()
            .enumerate()
        {
            if let Some(e) = model.entries.iter_mut().find(|e| e.path == *path) {
                e.common = Some(i);
            }
        }
    }
    model
}

fn state(model: Model, rail: Rail) -> State {
    let mut s = State::default();
    s.set_model(model);
    s.filter.rail = rail;
    s
}

#[tokio::test]
async fn settings_fit_both_sizes_and_locales_with_the_shipped_catalog() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    assert!(
        model.entries.len() >= 340,
        "{} entries",
        model.entries.len()
    );
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let mut st = state(model.clone(), Rail::All);
            let report = render_fit(size, locale, egui::Theme::Light, &data, |ui, cx, out| {
                settings_page::show(ui, cx, &mut st, out);
            });
            assert!(!report.scrolled, "{size:?} {locale:?}");
            assert_eq!(report.scroll_areas, 0, "{size:?} {locale:?}");
            assert!(report.fits(), "{size:?} {locale:?}: {report:?}");
            // The strip's second line (the intent chips above the rail) costs
            // the list one row at the 800 px window; the chips are what tells a
            // reader where to start, so they are worth it.
            let min = if size[1] >= 900.0 { 12 } else { 10 };
            assert!(
                st.table.per_page >= min,
                "{size:?} {locale:?}: {} rows",
                st.table.per_page
            );
            let text = report.all_text();
            // The first row is selected and its detail pane is docked. The row
            // shows its human name; the raw key is only in the technical fold.
            let first = st.rows()[0]
                .title_for(locale == Locale::ZhCn)
                .text
                .to_owned();
            assert!(text.contains(&first), "{text}");
            if locale == Locale::ZhCn {
                assert!(text.contains("本机"), "{text}");
                assert!(text.contains("常用"), "{text}");
                // The pane is three layers: a conclusion, the control, two folds.
                assert!(text.contains("现在："), "{text}");
                assert!(text.contains("为什么要改这个？"), "{text}");
                assert!(text.contains("技术详情"), "{text}");
                assert!(!text.contains("暂缺中文"), "{text}");
            } else {
                assert!(text.contains("This computer"), "{text}");
            }
            // 1–11 / N under the list.
            assert!(
                text.contains(&format!("1–{} / ", st.table.per_page)),
                "{text}"
            );
        }
    }
}

#[tokio::test]
async fn every_entry_shown_without_advanced_has_a_chinese_name_and_explanation() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let missing: Vec<&str> = model
        .entries
        .iter()
        .filter(|e| e.stage.in_default_view() && e.legacy.is_none() && !e.advanced_only)
        .filter(|e| e.name_zh.is_none() || e.text.zh.is_none())
        .map(|e| e.path.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "{} without Chinese: {missing:?}",
        missing.len()
    );
}

#[tokio::test]
async fn rows_are_chinese_first_without_key_names_and_common_is_the_first_rail_item() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let mut st = State::default();
    st.set_model(model);
    assert_eq!(st.filter.rail, Rail::Common);
    let report = render_fit(
        SIZES[0],
        Locale::ZhCn,
        egui::Theme::Dark,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut st, out);
        },
    );
    let text = report.all_text();
    assert!(text.contains("命令审批"), "{text}");
    // The middle column is the human name and one sentence, never the raw key:
    // that is under the detail pane's "technical details".
    assert!(!text.contains("approval_policy"), "{text}");
    assert!(text.contains("已修改"), "{text}");
    assert_eq!(st.rows()[0].path, "approval_policy");
}

#[tokio::test]
async fn changed_lists_the_provider_settings_and_pending_edits() {
    let service = fake();
    let data = data(&service).await;
    let mut settings = CodexSettings::new();
    settings
        .set("hide_agent_reasoning", SettingValue::Bool(true))
        .unwrap();
    let model = model(&data, Some(settings));
    let mut st = state(model, Rail::Changed);
    assert_eq!(
        st.rows()
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        vec!["hide_agent_reasoning"]
    );
    st.edit(
        "web_search",
        superba_gui::pages::settings::model::Change::Set(
            superba_gui::pages::settings::model::Value::Str("live".into()),
        ),
    );
    assert_eq!(st.rows().len(), 2);
    assert_eq!(settings_page::status(&st).pending, 1);
}

#[tokio::test]
async fn detail_names_what_each_connected_host_would_skip() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    assert_eq!(
        model
            .hosts
            .iter()
            .map(|h| h.label.as_str())
            .collect::<Vec<_>>(),
        vec!["devbox", "gpu-01"]
    );
    // A key the 0.162 host knows and the 0.158 host does not.
    let gpu = model.hosts[1].key.clone();
    let devbox = model.hosts[0].key.clone();
    let entry = model
        .entries
        .iter()
        .find(|e| {
            e.stage.in_default_view()
                && e.since.is_some()
                && e.host_support.contains(&(gpu.clone(), Availability::No))
                && e.host_support
                    .contains(&(devbox.clone(), Availability::Yes))
        })
        .cloned()
        .expect("a key added between 0.158 and 0.162");
    let mut st = state(model, Rail::Common);
    st.focus(&entry.path);
    assert_eq!(
        st.selected().map(|e| e.path.as_str()),
        Some(entry.path.as_str())
    );
    let report = render_fit(
        SIZES[1],
        Locale::ZhCn,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut st, out);
        },
    );
    let text = report.all_text();
    let since = entry.since.unwrap();
    assert!(text.contains("远程主机（同步时）"), "{text}");
    // Each host after its drawn check or cross mark.
    let needs = format!("gpu-01 需 ≥ {since}");
    assert!(text.lines().any(|l| l == "devbox"), "{text}");
    assert!(text.lines().any(|l| l == needs), "{text}");
}

#[tokio::test]
async fn reserved_keys_link_to_the_page_that_owns_them() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    for (path, link) in [
        ("model_context_window", "在「模型」页管理 →"),
        ("service_tier", "由速度模式管理 →"),
        ("openai_base_url", "由供应商管理 →"),
    ] {
        if model.entry(path).is_none() {
            continue;
        }
        let mut st = state(model.clone(), Rail::Common);
        st.focus(path);
        let report = render_fit(
            SIZES[0],
            Locale::ZhCn,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                settings_page::show(ui, cx, &mut st, out);
            },
        );
        assert!(
            report.all_text().contains(link),
            "{path}: {}",
            report.all_text()
        );
    }
}

#[tokio::test]
async fn narrow_windows_show_the_detail_as_a_sheet_over_the_list() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let mut st = state(model, Rail::All);
    // 1100 px window: the page is narrower than the docking width.
    let report = render_fit(
        [1100.0, 800.0],
        Locale::En,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut st, out);
        },
    );
    assert!(!report.all_text().contains("Official description"));
    let first = st.rows()[0].path.clone();
    st.focus(&first);
    assert!(st.sheet_open);
    let report = render_fit(
        [1100.0, 800.0],
        Locale::ZhCn,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut st, out);
        },
    );
    let text = report.all_text();
    assert!(text.contains("现在："), "{text}");
    assert!(text.contains("为什么要改这个？"), "{text}");
    assert!(text.contains("技术详情"), "{text}");
}

#[tokio::test]
async fn palette_titles_are_chinese_names_and_focus_selects_the_row() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let st = state(model, Rail::Common);
    let svc: std::sync::Arc<dyn SwitchService> = std::sync::Arc::new(fake());
    let tr = superba_gui::i18n::Tr::new(Locale::ZhCn);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let cx = superba_gui::pages::Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data: &data,
        busy: false,
        now: superba_core::Timestamp::now(),
        fonts: &fonts,
        service: &svc,
        log: &[],
    };
    let items = settings_page::palette(&cx, &st);
    let approval = items
        .iter()
        .find(|i| i.keywords.iter().any(|k| k == "approval_policy"))
        .unwrap();
    assert_eq!(approval.title, "命令审批");
    assert!(matches!(
        &approval.action,
        Action::PageCommand { command, .. } if command == "focus:approval_policy"
    ));
}

#[tokio::test]
async fn explanations_show_code_spans_without_markdown_backticks() {
    // Review ux-visual #14: "以 `developer` 角色消息加入上下文…" showed the
    // backticks of the catalog's Markdown.
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let entry = model
        .entries
        .iter()
        .find(|e| e.path == "approval_policy")
        .cloned()
        .unwrap();
    assert!(
        entry.text.zh.as_deref().is_some_and(|t| t.contains('`')),
        "the catalog text has code spans"
    );
    for (locale, start) in [
        (Locale::ZhCn, entry.text.zh.clone().unwrap()),
        (Locale::En, entry.text.en.clone().unwrap()),
    ] {
        let mut st = state(model.clone(), Rail::Common);
        st.focus("approval_policy");
        let report = render_fit(
            SIZES[0],
            locale,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                settings_page::show(ui, cx, &mut st, out);
            },
        );
        // The explanation as drawn: its first words, without backticks.
        let head: String = start.chars().take(8).collect();
        let drawn = report
            .texts
            .iter()
            .find(|t| t.starts_with(&head))
            .unwrap_or_else(|| panic!("{head} not drawn: {:?}", report.texts));
        assert!(!drawn.contains('`'), "{locale:?}: {drawn}");
        if start.contains("`on-request`") {
            assert!(drawn.contains("on-request"), "{locale:?}: {drawn}");
        }
    }
    // One-line row summaries drop them too.
    for e in &model.entries {
        for zh in [true, false] {
            if let Some(s) = e.subtitle_for(zh) {
                assert!(!s.contains('`'), "{}: {s}", e.path);
            }
        }
    }
}

#[tokio::test]
async fn rail_categories_are_never_cut_and_use_the_width_of_large_windows() {
    // Review ux-visual #12: "Context and instructi…", "Approvals and sandb…" and
    // "Telemetry and feedba…" at 1280 and 1440 (rail fixed at 176 px).
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let tr = superba_gui::i18n::Tr::new(Locale::En);
    let labels: Vec<String> = model
        .categories
        .iter()
        .map(|c| {
            let key = format!("catalog.category.{}", c.id);
            match tr.t(&key) {
                t if t == key => c.label.clone(),
                t => t,
            }
        })
        .collect();
    assert!(
        labels.iter().any(|l| l == "Context and instructions"),
        "{labels:?}"
    );
    for size in SIZES {
        let mut st = state(model.clone(), Rail::Common);
        let report = render_fit(
            size,
            Locale::En,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                settings_page::show(ui, cx, &mut st, out);
            },
        );
        for label in &labels {
            assert!(
                report.whole(label),
                "{size:?}: {label} cut: {:?}",
                report
                    .text_shapes
                    .iter()
                    .filter(|t| t.0 == *label)
                    .collect::<Vec<_>>()
            );
        }
        // At 1440 the rail is wide enough for one line each.
        if size[0] >= 1440.0 {
            let tall = report
                .text_shapes
                .iter()
                .filter(|t| labels.contains(&t.0) && t.1.height() > 22.0)
                .count();
            assert_eq!(tall, 0, "{size:?}: one line per category");
        }
    }
}

#[tokio::test]
async fn a_cut_rail_says_how_many_categories_it_hides() {
    // Review: a short window cut the rail mid-list ("功能开…") without a word.
    // A window too short for every category must say how many are left, and the
    // number must be the one really hidden.
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let total = model.categories.len();
    let mut seen_hint = false;
    for (li, locale) in [Locale::ZhCn, Locale::En].into_iter().enumerate() {
        let tag = ["zh", "en"][li];
        let tr = superba_gui::i18n::Tr::new(locale);
        let labels: Vec<String> = model
            .categories
            .iter()
            .map(|c| match (&c.label_zh, locale) {
                (Some(zh), Locale::ZhCn) => zh.clone(),
                _ => {
                    let key = format!("catalog.category.{}", c.id);
                    let t = tr.t(&key);
                    if t == key { c.label.clone() } else { t }
                }
            })
            .collect();
        // Narrow windows wrap labels to two lines, so the rail is cut even
        // though the page still fits; the hint has to appear within the page.
        for width in [960.0, 1120.0, 1280.0] {
            for height in [760.0, 800.0, 900.0] {
                let size = [width, height];
                let mut st = state(model.clone(), Rail::Common);
                let report = render_fit(size, locale, egui::Theme::Light, &data, |ui, cx, out| {
                    settings_page::show(ui, cx, &mut st, out);
                });
                // The page starts right of the sidebar: rail labels sit in the
                // first 176 px of the page, the list column starts after them.
                let rail_right = report.rect.min.x + 176.0;
                let drawn = labels
                    .iter()
                    .filter(|l| {
                        report
                            .text_shapes
                            .iter()
                            .any(|t| t.0 == **l && t.1.min.x < rail_right)
                    })
                    .count();
                let hint = report.texts.iter().find(|t| {
                    (t.starts_with("还有 ") && t.contains(" 项"))
                        || (t.ends_with(" to search") && t.contains(" more · "))
                });
                match hint {
                    Some(hint) => {
                        seen_hint = true;
                        let n: usize = hint
                            .trim_start_matches("还有 ")
                            .split(' ')
                            .next()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or_else(|| panic!("no count in {hint:?}"));
                        assert_eq!(
                            n,
                            total - drawn,
                            "{tag} {size:?}: {hint:?} with {drawn}/{total} drawn"
                        );
                    }
                    None => assert!(
                        report.scrolled || drawn == total,
                        "{tag} {size:?}: {drawn}/{total} drawn, no hint"
                    ),
                }
            }
        }
    }
    assert!(seen_hint, "no size cut the rail with a visible hint");
}

/// The three layers are reachable by pointer: the two folds are ordinary
/// buttons, so a click opens the technical details and (through the same
/// widget) the keyboard and AccessKit reach them too.
#[tokio::test]
async fn the_detail_pane_folds_open_with_a_click() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let mut st = state(model, Rail::Common);
    st.focus("approval_policy");
    let h = support::ui::Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = superba_gui::i18n::Tr::new(Locale::ZhCn);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let idle = support::idle_service();
    let mut show = |ui: &mut egui::Ui| {
        let cx = superba_gui::pages::Cx {
            tr: &tr,
            pal: superba_gui::design::Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: superba_core::Timestamp::now(),
            fonts: &fonts,
            service: &idle,
            log: &[],
        };
        let page = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1280.0, 800.0));
        let mut area = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(page)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let mut actions = Vec::new();
        settings_page::show(&mut area, &cx, &mut st, &mut actions);
    };
    let drawn = |texts: &[(String, egui::Rect)]| {
        texts
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let closed = h.settle(&mut show);
    let before = drawn(&closed);
    // Layer 1 states the current value in the user's words; the two folds are
    // offered, with the raw key, default and English原文 kept inside them.
    assert!(before.contains("现在："), "{before}");
    assert!(before.contains("为什么要改这个？"), "{before}");
    assert!(before.contains("技术详情"), "{before}");
    assert!(!before.contains("官方说明"), "{before}");
    // One click on the fold header opens the technical details.
    assert!(
        h.click_text("技术详情", &mut show),
        "the fold header is clickable"
    );
    let open = h.settle(&mut show);
    let after = drawn(&open);
    assert!(after.contains("官方说明"), "{after}");
    assert!(after.contains("Default approval policy"), "{after}");
}

/// The six chips, in the order the rail draws them.
const INTENT_LABELS: [&str; 6] = [
    "codex_settings.intent.ask_less",
    "codex_settings.intent.my_files",
    "codex_settings.intent.network",
    "codex_settings.intent.model",
    "codex_settings.intent.tokens",
    "codex_settings.intent.defaults",
];

/// Every chip narrows the list to the keys that decide its question (five of
/// them; "restore defaults" opens the changed list instead). No chip is empty
/// and none names a key the catalog does not have.
#[tokio::test]
async fn intent_chips_narrow_the_list_to_their_few_keys() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let paths: Vec<String> = model.entries.iter().map(|e| e.path.clone()).collect();
    let mut filtering = 0;
    for i in superba_gui::pages::settings::model::INTENTS {
        if i.paths.is_empty() {
            continue;
        }
        filtering += 1;
        for p in i.paths {
            assert!(paths.iter().any(|e| e == p), "{}: no key {p}", i.id);
        }
        let st = state(model.clone(), Rail::Intent(i.id));
        let rows: Vec<String> = st.rows().iter().map(|e| e.path.clone()).collect();
        assert_eq!(rows.len(), i.paths.len(), "{}: {rows:?}", i.id);
        for p in i.paths {
            assert!(
                rows.iter().any(|r| r == p),
                "{}: missing {p}: {rows:?}",
                i.id
            );
        }
    }
    assert_eq!(filtering, 5, "five chips filter; `defaults` opens a rail");
}

/// The chips are ordinary buttons drawn above the rail: all six are on screen
/// at the 1280 px window, one click answers its question (the list narrows to
/// that one key), and a second click goes back to the rail.
#[tokio::test]
async fn intent_chips_are_buttons_that_change_the_list() {
    let service = fake();
    let data = data(&service).await;
    let model = model(&data, None);
    let mut st = state(model, Rail::Common);
    let h = support::ui::Harness::new([1280.0, 800.0], egui::Theme::Light);
    let tr = superba_gui::i18n::Tr::new(Locale::ZhCn);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let idle = support::idle_service();
    let mut show = |ui: &mut egui::Ui| {
        let cx = superba_gui::pages::Cx {
            tr: &tr,
            pal: superba_gui::design::Palette::of(ui.ctx()),
            data: &data,
            busy: false,
            now: superba_core::Timestamp::now(),
            fonts: &fonts,
            service: &idle,
            log: &[],
        };
        let page = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1280.0, 800.0));
        let mut area = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(page)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let mut actions = Vec::new();
        settings_page::show(&mut area, &cx, &mut st, &mut actions);
    };
    let drawn = |texts: &[(String, egui::Rect)]| {
        texts
            .iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = drawn(&h.settle(&mut show));
    for key in INTENT_LABELS {
        assert!(before.contains(&tr.t(key)), "{key} missing:\n{before}");
    }
    // The Common rail's rows are listed.
    assert!(before.contains("文件与网络权限"), "{before}");
    assert!(
        h.click_text(&tr.t("codex_settings.intent.ask_less"), &mut show),
        "the chip is clickable"
    );
    let asked = drawn(&h.settle(&mut show));
    assert!(asked.contains("命令审批"), "{asked}");
    assert!(!asked.contains("文件与网络权限"), "{asked}");
    // The same chip again goes back to the rail.
    assert!(h.click_text(&tr.t("codex_settings.intent.ask_less"), &mut show));
    let back = drawn(&h.settle(&mut show));
    assert!(back.contains("文件与网络权限"), "{back}");
}

/// A window too narrow for the six labels leaves the chips out (the rail and
/// the search field still answer the same questions) and keeps the one-line
/// strip, so the list is not squeezed twice.
#[tokio::test]
async fn narrow_windows_leave_the_intent_chips_out() {
    // "少问几次" is a chip and nothing else, so its presence is the chip row's.
    let tr = superba_gui::i18n::Tr::new(Locale::ZhCn);
    let label = tr.t("codex_settings.intent.ask_less");
    for (size, chips) in [([1280.0, 800.0], true), ([760.0, 640.0], false)] {
        let service = fake();
        let data = data(&service).await;
        let model = model(&data, None);
        let mut st = state(model, Rail::Common);
        let report = render_fit(
            size,
            Locale::ZhCn,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                settings_page::show(ui, cx, &mut st, out);
            },
        );
        let text = report.all_text();
        assert_eq!(
            text.contains(&label),
            chips,
            "{size:?}: {label} should {} be drawn:\n{text}",
            if chips { "" } else { "not" }
        );
    }
}
