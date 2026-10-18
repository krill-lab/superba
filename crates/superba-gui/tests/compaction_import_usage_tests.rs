#![allow(missing_docs)]

//! GitHub issues 3 and 1 in the GUI: the Connection tab's remote compaction
//! row (Krill native by Superba's fact without a check; a custom site checked
//! once when opened; not supported: a note that says what that means; the
//! choice saves the profile), the import dialog's usage history (preview,
//! button, result) next to the providers, and imported rows in the Usage log.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_core::{
    CompactionChoice, CompactionSupport, ImportSource, ImportStore, Locale, ProfileId,
    RequestSource, SiteId, StoreStatus, SwitchService, Timestamp, TransportRefresh,
    UsageImportReport, Usd,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::import::{ImportDialog, show_dialog};
use superba_gui::pages::shared::providers::ProviderId;
use superba_gui::pages::{Action, Cx, Data, providers, router, shared};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

async fn data_of(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

/// Draw the providers page until `done` holds (or 3 s); returns the last
/// frame's texts and the actions.
fn drive(
    data: &Data,
    service: Arc<dyn SwitchService>,
    state: &mut providers::State,
    locale: Locale,
    mut done: impl FnMut(&str) -> bool,
) -> (String, Vec<Action>) {
    let ctx = egui::Context::default();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(locale);
    let mut actions = Vec::new();
    let mut text = String::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &report,
                    service: &service,
                    log: &[],
                };
                providers::show(ui, &cx, state, &mut actions);
            },
        );
        text = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.drop_without_applying_deltas();
        if done(&text) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (text, actions)
}

#[tokio::test]
async fn krill_shows_native_compaction_without_a_check() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let (text, _) = drive(&data, svc, &mut state, Locale::En, |t| {
        t.contains("Krill compacts natively")
    });
    assert!(text.contains("Compaction"), "{text}");
    assert!(text.contains("Native remote compaction"), "{text}");
    assert!(text.contains("Krill compacts natively"), "{text}");
    // Nothing is sent: the cache only.
    assert!(
        service
            .compaction_calls()
            .iter()
            .all(|(_, r)| *r == TransportRefresh::Cached),
        "{:?}",
        service.compaction_calls()
    );
}

async fn custom_provider(service: &FakeService) {
    let site = superba_switch::new_compatible_site(
        SiteId::new("example"),
        "Example API",
        &url::Url::parse("https://api.example.com/v1").unwrap(),
    )
    .unwrap();
    service.save_site(site.clone()).await.unwrap();
    let mut profile = shared::new_profile(&site, "Example API".into());
    profile.id = ProfileId::new("example");
    service
        .save_profile(profile, Some(superba_core::Secret::new("sk-example")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_custom_site_is_checked_once_and_a_refusal_explains_itself() {
    let service = fake();
    custom_provider(&service).await;
    service.set_compaction("example", CompactionSupport::Unsupported);
    let data = data_of(&service).await;
    for (locale, expect) in [
        (
            Locale::En,
            ["No remote compaction", "cannot compact Codex conversations"],
        ),
        (Locale::ZhCn, ["不支持远程压缩", "无法压缩 Codex 对话"]),
    ] {
        let svc: Arc<dyn SwitchService> = service.clone();
        let mut state = providers::State::default();
        state.select(ProviderId::Profile(ProfileId::new("example")));
        let (text, _) = drive(&data, svc, &mut state, locale, |t| t.contains(expect[1]));
        for e in expect {
            assert!(text.contains(e), "{locale:?} {e:?}:\n{text}");
        }
    }
    // The automatic check asked for a missing / stale result, never "All".
    let calls = service.compaction_calls();
    assert!(
        calls
            .iter()
            .any(|(p, r)| p.as_str() == "example" && *r == TransportRefresh::Missing),
        "{calls:?}"
    );
    assert!(calls.iter().all(|(_, r)| *r != TransportRefresh::All));
}

#[tokio::test]
async fn the_choice_is_saved_on_the_profile() {
    let service = fake();
    let mut set = service.profile_set().await.unwrap();
    let krill = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "krill")
        .unwrap();
    krill.compaction = CompactionChoice::Unsupported;
    service.save_profile(krill.clone(), None).await.unwrap();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let (text, _) = drive(&data, svc, &mut state, Locale::En, |t| {
        t.contains("set on this provider")
    });
    assert!(text.contains("No remote compaction"), "{text}");
    assert!(text.contains("set on this provider"), "{text}");
    assert!(
        text.contains("cannot compact Codex conversations"),
        "{text}"
    );
}

fn usage_report() -> UsageImportReport {
    let mut r = UsageImportReport::new(ImportSource::Ccs, true);
    r.stores.push(ImportStore {
        path: "/home/u/.cc-switch/cc-switch.db".into(),
        display_path: "~/.cc-switch/cc-switch.db".into(),
        format: "CC Switch database, schema 20".into(),
        status: StoreStatus::Read,
        note: None,
    });
    r.requests = 1284;
    r.days = 37;
    r.day_requests = 9120;
    r.first = Some(Timestamp::from_millis(1_785_542_400_000));
    r.last = Some(Timestamp::from_millis(1_791_709_300_000));
    r.cost = Usd::from_micros(48_210_000);
    r.unpriced = 12;
    r.covered = 310;
    r.other_apps = 655;
    r
}

#[tokio::test]
async fn the_import_dialog_offers_each_tool_s_usage_history() {
    let service = fake();
    service.set_import_scan(superba_core::ImportScan {
        sources: vec![superba_core::SourceScan {
            source: ImportSource::Ccs,
            stores: usage_report().stores,
            candidates: Vec::new(),
        }],
    });
    service.set_usage_report(usage_report());
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let tr = Tr::new(Locale::En);
    let h = Harness::new([1280.0, 900.0], egui::Theme::Light);
    let mut actions = Vec::new();
    let mut d: Option<ImportDialog> = None;
    let mut all = Vec::new();
    let start = Instant::now();
    let mut clicked = false;
    while start.elapsed() < Duration::from_secs(4) {
        let mut f = |ui: &mut egui::Ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data: &data,
                busy: false,
                now: Timestamp::now(),
                fonts: &h.fonts,
                service: &svc,
                log: &[],
            };
            let dialog = d.get_or_insert_with(|| ImportDialog::open(&cx));
            if !clicked
                && dialog
                    .usage
                    .get(&ImportSource::Ccs)
                    .is_some_and(|u| u.preview.is_some())
            {
                clicked = true;
                dialog.start_usage_import(&cx, ImportSource::Ccs);
            }
            show_dialog(ui.ctx(), &cx, dialog, &mut actions);
        };
        let shown = h.frame(Vec::new(), &mut f);
        all = shown.into_iter().map(|(t, _)| t).collect();
        if all.iter().any(|t| t.starts_with("Usage history imported")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        all.iter()
            .any(|t| t.contains("Usage history imported: 1,284 requests")
                || t.contains("Usage history imported: 1284 requests")),
        "{all:#?}"
    );
    let requests = service.usage_requests();
    assert_eq!(
        requests.len(),
        3,
        "a preview per tool, then the import: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.source == ImportSource::Ccs && !r.dry_run)
    );
}

#[test]
fn the_preview_line_names_counts_range_and_cost() {
    let tr = Tr::new(Locale::En);
    let r = usage_report();
    let text = tr.f(
        "import_dialog.usage.preview",
        &[
            ("n", &r.requests),
            ("days", &r.days),
            ("range", &"2026-08-01 – 2026-10-11"),
            ("cost", &r.cost),
        ],
    );
    assert!(
        text.contains("1,284 requests") || text.contains("1284 requests"),
        "{text}"
    );
    assert!(text.contains("37 daily totals"), "{text}");
    let cost = tr.f("import_dialog.usage.cost", &[("cost", &r.cost)]);
    assert!(cost.contains("$48.21"), "{cost}");
    let zh = Tr::new(Locale::ZhCn).f(
        "import_dialog.usage.preview",
        &[
            ("n", &r.requests),
            ("days", &r.days),
            ("range", &"2026-08-01 – 2026-10-11"),
            ("cost", &r.cost),
        ],
    );
    assert!(zh.contains("用量记录"), "{zh}");
}

#[test]
fn imported_rows_name_their_tool_and_the_log_filters_by_source() {
    let mut row: superba_core::RequestRow = serde_json::from_value(serde_json::json!({
        "id": i64::MIN + 5,
        "started_at": 1_791_619_196_000_i64,
        "member_id": "import:ccs:packy",
        "member_name": "PackyCode",
        "status": {"kind": "ok"},
        "source": "cc_switch"
    }))
    .unwrap();
    assert_eq!(
        router::requests::provider_cell(&row),
        "PackyCode · CC Switch"
    );
    row.source = RequestSource::AiToolbox;
    assert_eq!(
        router::requests::provider_cell(&row),
        "PackyCode · AI Toolbox"
    );
    let mut state = router::State::default();
    let recent = &mut state.recent;
    recent.source = Some(RequestSource::CcSwitch);
    let q = recent.query(Timestamp::now(), "");
    assert_eq!(q.source, Some(RequestSource::CcSwitch));
    assert!(recent.filtered(""));
    assert!(recent.shows_sources());
}
