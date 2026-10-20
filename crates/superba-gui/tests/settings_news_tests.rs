#![allow(missing_docs)]
//! Codex settings "New in latest" (新版本新增): config keys added by the newest
//! catalog releases grouped by version, an explicit line for a release that
//! added nothing, the catalog's freshness, and a click opening the detail pane.

mod support;

use std::sync::Arc;

use egui::{Event, PointerButton, Rect, pos2};
use superba_core::{Locale, ProfileSet, SwitchService, Timestamp};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::settings::model::{Model, Rail, Release};
use superba_gui::pages::settings::source::{self, Input};
use superba_gui::pages::settings::{self as settings_page, State};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::fit::render_fit;

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

fn model(data: &Data) -> Model {
    let profiles: ProfileSet = data.profiles.value.clone().unwrap();
    let input = Input {
        overview: data.overview.value.clone(),
        profiles: Some(profiles),
        profile: None,
    };
    let store = superba_catalog::CatalogStore::builtin().unwrap();
    source::build(
        &store,
        &input,
        Some(&toml::Table::new()),
        superba_catalog::Os::Linux,
    )
    .unwrap()
}

fn state(model: Model) -> State {
    let mut s = State::default();
    s.set_model(model);
    s.filter.rail = Rail::New;
    s
}

#[tokio::test]
async fn the_shipped_catalog_lists_its_newest_releases_newest_first() {
    let service = fake();
    let data = data(&service).await;
    let m = model(&data);
    assert!(!m.releases.is_empty());
    assert!(m.releases.len() <= source::NEW_RELEASES);
    let versions: Vec<superba_core::CodexVersion> = m
        .releases
        .iter()
        .map(|r| superba_core::CodexVersion::parse(&r.version).unwrap())
        .collect();
    assert!(versions.windows(2).all(|w| w[0] > w[1]), "{versions:?}");
    assert_eq!(
        m.freshness.newest.as_deref(),
        Some(m.releases[0].version.as_str())
    );
    // Every listed entry belongs to a release that added its path.
    let st = state(m.clone());
    let rows = st.rows();
    assert!(!rows.is_empty());
    for e in &rows {
        let i = e.new_in.unwrap();
        assert!(
            m.releases[i].added.iter().any(|k| k.path == e.path),
            "{}",
            e.path
        );
    }
    // Rows follow the release order.
    assert!(rows.windows(2).all(|w| w[0].new_in <= w[1].new_in));
}

/// The model with a newest release that added nothing and a local Codex ahead
/// of the catalog.
fn with_empty_alpha(mut m: Model) -> Model {
    m.releases.insert(
        0,
        Release {
            version: "0.164.0-alpha.7".into(),
            prerelease: true,
            added: Vec::new(),
        },
    );
    for e in &mut m.entries {
        e.new_in = e.new_in.map(|i| i + 1);
    }
    m.freshness.ahead = Some("0.164.0-alpha.8".into());
    m.freshness.generated = Some("2026-10-20".into());
    m
}

#[tokio::test]
async fn the_tab_says_when_a_release_adds_nothing_and_when_the_catalog_is_behind() {
    let service = fake();
    let data = data(&service).await;
    let m = with_empty_alpha(model(&data));
    let second = m.releases[1].clone();
    for size in [[960.0, 640.0], [1280.0, 800.0]] {
        for (locale, none, behind, group, tab, generated) in [
            (
                Locale::ZhCn,
                "0.164.0-alpha.7 没有新增配置项".to_owned(),
                "目录尚未包含 0.164.0-alpha.8",
                format!("{} 新增 {} 项", second.version, second.added.len()),
                "新版本新增",
                "生成于 2026-10-20",
            ),
            (
                Locale::En,
                "0.164.0-alpha.7 adds no config keys".to_owned(),
                "The catalog does not include 0.164.0-alpha.8 yet",
                format!("{}: {} new", second.version, second.added.len()),
                "New in latest",
                "generated 2026-10-20",
            ),
        ] {
            for theme in [egui::Theme::Light, egui::Theme::Dark] {
                let mut st = state(m.clone());
                let report = render_fit(size, locale, theme, &data, |ui, cx, out| {
                    settings_page::show(ui, cx, &mut st, out);
                });
                // The page as a whole fits from 1280x800 (settings_page_tests);
                // at 960x640 the tab's own content must still be drawn.
                if size[0] >= 1280.0 {
                    assert!(report.fits(), "{size:?} {locale:?}: {report:?}");
                }
                let text = report.all_text();
                for want in [none.as_str(), behind, group.as_str(), tab, generated] {
                    assert!(
                        text.contains(want),
                        "{size:?} {locale:?}: no {want:?} in {text}"
                    );
                }
            }
        }
    }
}

/// Render the page in a 1280x800 window and click the text `target`.
fn click_row(data: &Data, st: &mut State, target: &str) -> bool {
    let ctx = egui::Context::default();
    let fonts = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&fonts));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(Locale::En);
    let service: Arc<dyn superba_core::SwitchService> = Arc::new(fake());
    let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 800.0));
    let frame = |events: Vec<Event>, st: &mut State| -> Vec<(String, Rect)> {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &fonts,
                    service: &service,
                    log: &[],
                };
                let page = Rect::from_min_max(pos2(220.0, 0.0), screen.max).shrink(24.0);
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(page)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                let mut out: Vec<Action> = Vec::new();
                settings_page::show(&mut child, &cx, st, &mut out);
            },
        );
        let texts = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley
                        .rect
                        .translate(t.pos.to_vec2())
                        .intersect(s.clip_rect),
                )),
                _ => None,
            })
            .filter(|(_, r)| r.is_positive())
            .collect();
        output.drop_without_applying_deltas();
        texts
    };
    frame(Vec::new(), st);
    let texts = frame(Vec::new(), st);
    let Some((_, r)) = texts.iter().find(|(t, _)| {
        t == target
            || t.strip_suffix('…')
                .is_some_and(|p| !p.is_empty() && target.starts_with(p))
    }) else {
        return false;
    };
    let pos = r.center();
    let button = |pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(vec![Event::PointerMoved(pos), button(true)], st);
    frame(vec![button(false)], st);
    frame(Vec::new(), st);
    true
}

#[tokio::test]
async fn clicking_a_new_key_selects_it_in_the_detail_pane() {
    let service = fake();
    let data = data(&service).await;
    let mut m = model(&data);
    // Only keys with an entry, so the second row is near the top.
    let known = m.clone();
    for r in &mut m.releases {
        r.added.retain(|k| known.entry(&k.path).is_some());
    }
    let mut st = state(m.clone());
    // The second key drawn (the first row is selected when the tab opens).
    let target = m
        .releases
        .iter()
        .flat_map(|r| &r.added)
        .nth(1)
        .unwrap()
        .path
        .clone();
    let title = m.entry(&target).unwrap().title_for(false).text.to_owned();
    assert_ne!(st.selected().map(|e| e.path.clone()), Some(target.clone()));
    assert!(click_row(&data, &mut st, &title), "{title} not drawn");
    assert_eq!(st.selected().map(|e| e.path.clone()), Some(target));
}

/// The keys of the "Config keys added" list of a change summary.
fn summary_keys(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            inside = rest.starts_with("**Config keys added**");
        } else if inside && let Some(k) = line.trim_start().strip_prefix("- `") {
            out.push(k.split('`').next().unwrap().to_owned());
        }
    }
    out.sort();
    out
}

#[test]
fn the_added_keys_match_the_catalog_change_summaries() {
    let store = superba_catalog::CatalogStore::builtin().unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog/dist/changes");
    let releases = source::releases(&store, source::NEW_RELEASES, superba_catalog::Os::Linux);
    assert!(!releases.is_empty());
    for r in releases {
        let md = std::fs::read_to_string(dir.join(format!("codex-{}.md", r.version))).unwrap();
        let paths: Vec<String> = r.added.iter().map(|k| k.path.clone()).collect();
        assert_eq!(paths, summary_keys(&md), "{}", r.version);
    }
}
