#![allow(missing_docs)]

//! Providers → Connection: WebSocket / zstd badges per line (user request 14).
//! Opening a provider checks the lines without a recent result, Test checks
//! them all again, and the badges and the caption say what was found.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_core::{Locale, ProfileId, SwitchService, Timestamp, TransportRefresh};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Cx, Data, providers};
use support::fake::{FakeOptions, FakeService};

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

/// Draw the page until `done` holds (or 3 s); `before` runs before each frame
/// with the context of that frame. Returns the texts of the last frame.
fn drive(
    data: &Data,
    service: Arc<dyn SwitchService>,
    state: &mut providers::State,
    locale: Locale,
    mut before: impl FnMut(&mut providers::State, &Cx<'_>, &mut Vec<Action>, u32),
    mut done: impl FnMut(&str) -> bool,
) -> String {
    let ctx = egui::Context::default();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(locale);
    let mut actions = Vec::new();
    let mut text = String::new();
    let start = Instant::now();
    let mut frame = 0;
    while start.elapsed() < Duration::from_secs(3) {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1060.0, 760.0),
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
                before(state, &cx, &mut actions, frame);
                providers::show(ui, &cx, state, &mut actions);
            },
        );
        frame += 1;
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
    text
}

#[tokio::test]
async fn opening_a_provider_checks_its_lines_and_shows_ws_and_zstd_per_line() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let text = drive(
        &data,
        svc,
        &mut state,
        Locale::ZhCn,
        |_, _, _, _| {},
        |t| t.contains("上次检查 WebSocket 和 zstd"),
    );
    assert!(text.contains("上次检查 WebSocket 和 zstd"), "{text}");
    // One WS and one zstd badge per Krill line.
    let lines = superba_core::catalog::krill_site().lines.len();
    assert_eq!(text.lines().filter(|l| *l == "WS").count(), lines, "{text}");
    assert_eq!(
        text.lines().filter(|l| *l == "zstd").count(),
        lines,
        "{text}"
    );
    // The first request checks only what is missing or stale.
    let calls = service.transport_calls();
    assert_eq!(
        calls.first(),
        Some(&(ProfileId::new("krill"), TransportRefresh::Missing)),
        "{calls:?}"
    );
    assert!(calls.iter().all(|(_, r)| *r != TransportRefresh::All));
}

#[tokio::test]
async fn test_checks_every_line_again() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let mut tested = false;
    let probe = service.clone();
    drive(
        &data,
        svc,
        &mut state,
        Locale::En,
        |state, cx, out, frame| {
            // Once the first check is shown, press Test (its command form).
            if frame == 5 && !tested {
                tested = true;
                providers::command(state, cx, "test:profile:krill", out);
            }
        },
        |_| {
            probe
                .transport_calls()
                .iter()
                .any(|(_, r)| *r == TransportRefresh::All)
        },
    );
    let calls = service.transport_calls();
    assert!(
        calls
            .iter()
            .any(|(p, r)| p.as_str() == "krill" && *r == TransportRefresh::All),
        "{calls:?}"
    );
}

#[tokio::test]
async fn providers_without_a_key_show_no_badges_and_send_nothing() {
    let service = fake();
    let site = superba_switch::new_compatible_site(
        superba_core::SiteId::new("example"),
        "Example API",
        &url::Url::parse("https://api.example.com/v1").unwrap(),
    )
    .unwrap();
    service.save_site(site).await.unwrap();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = providers::State::default();
    let mut frames = 0;
    let text = drive(
        &data,
        svc,
        &mut state,
        Locale::En,
        |state, cx, out, frame| {
            if frame == 0 {
                providers::command(state, cx, "site:example", out);
            }
        },
        |_| {
            frames += 1;
            frames > 6
        },
    );
    assert!(!text.lines().any(|l| l == "WS"), "{text}");
    assert!(
        service
            .transport_calls()
            .iter()
            .all(|(p, _)| p.as_str() != "example"),
        "{:?}",
        service.transport_calls()
    );
}
