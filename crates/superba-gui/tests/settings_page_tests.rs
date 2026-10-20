#![allow(missing_docs)]
//! Codex settings page (UX §3.6, plan §5 B6): three fixed panes at 1280×800 and
//! 1440×900 in both languages with the shipped catalog (no page scrolling, 11 / 13
//! rows per page), Chinese-first rows, the Common / Changed rail, the detail pane
//! with per-host sync support and the links of reserved keys, and the side sheet
//! on narrow windows.

mod support;

use superba_core::settings::{CodexSettings, SettingValue};
use superba_core::{Locale, ProfileSet, SwitchService};
use superba_gui::pages::Data;
use superba_gui::pages::settings::model::{Availability, Model, Rail};
use superba_gui::pages::settings::source::{self, Input};
use superba_gui::pages::settings::{self as settings_page, State};
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
            let min = if size[1] >= 900.0 { 13 } else { 11 };
            assert!(
                st.table.per_page >= min,
                "{size:?} {locale:?}: {} rows",
                st.table.per_page
            );
            let text = report.all_text();
            // The first row is selected and its detail pane is docked.
            let first = st.rows()[0].path.clone();
            assert!(text.contains(&first), "{text}");
            if locale == Locale::ZhCn {
                assert!(text.contains("本机"), "{text}");
                assert!(text.contains("常用"), "{text}");
                assert!(text.contains("官方说明"), "{text}");
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
async fn rows_are_chinese_first_and_common_is_the_first_rail_item() {
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
    assert!(text.contains("审批策略"), "{text}");
    assert!(text.contains("approval_policy"), "{text}");
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
        ("service_tier", "由速度档位管理 →"),
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
    assert!(report.all_text().contains("取值"), "{}", report.all_text());
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
