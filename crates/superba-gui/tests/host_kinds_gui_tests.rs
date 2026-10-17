#![allow(missing_docs)]
//! Hosts Superba does not manage (user bug 36): the ChatGPT remote-control
//! devices and WSL distributions the app reports are listed read-only under the
//! app's display name or their kind, never under the raw app host id (0.1.0
//! showed `remote-control:env_e_…`), and no page offers them an SSH action:
//! no probe, follow, sync, Codex install or session scan.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{Locale, SwitchService, TargetId};
use superba_gui::pages::hosts::{self, State};
use superba_gui::pages::overview::hero;
use superba_gui::pages::{Action, sessions};
use support::fake::{FakeOptions, FakeService};
use support::fit::SIZES;

fn fake() -> Arc<FakeService> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.with_device_hosts();
    service
}

fn device() -> TargetId {
    TargetId::remote(FakeService::DEVICE_HOST_ID)
}

fn harness(size: [f32; 2], locale: Locale, service: &Arc<FakeService>) -> Harness {
    let mut h = Harness::new(size, locale, service.clone(), true);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // The shell's host list, as the sessions switcher and the Overview read it.
    let hosts = rt.block_on(service.hosts());
    h.data.hosts.finish(hosts);
    h
}

fn no_raw_id(text: &str) {
    assert!(
        !text.contains("remote-control:"),
        "raw host id shown:\n{text}"
    );
    assert!(!text.contains("remote-wsl:"), "raw host id shown:\n{text}");
}

#[test]
fn the_remote_hosts_page_lists_devices_read_only_under_their_names() {
    for size in SIZES {
        for (locale, kind, read_only) in [
            (Locale::ZhCn, "远程控制设备", "远程控制设备 · 只读"),
            (
                Locale::En,
                "Remote-control device",
                "Remote-control device · read-only",
            ),
        ] {
            let service = fake();
            let mut h = harness(size, locale, &service);
            let mut state = State::default();
            let f = h.until(
                |ui, cx, out| hosts::show(ui, cx, &mut state, out),
                |f| f.has("devbox") && f.has("Studio Mac"),
            );
            // The named device, the unnamed one by its kind, the WSL distribution.
            for want in ["Studio Mac", kind, read_only, "WSL: Ubuntu"] {
                assert!(f.has(want), "{want} missing:\n{}", f.all_text());
            }
            no_raw_id(&f.all_text());
            assert!(!f.has(FakeService::SSH_CONFIG_ONLY_HOST));
            assert!(f.fits(), "{size:?} {locale:?}");
        }
    }
}

#[test]
fn a_device_gets_a_read_only_detail_without_any_ssh_action() {
    let service = fake();
    let mut h = harness([1280.0, 800.0], Locale::ZhCn, &service);
    let mut state = State::default();
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| f.has("Studio Mac"),
    );
    state.selected = Some(device());
    let f = h.frames(4, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
    for want in [
        "这台设备通过 ChatGPT 远程控制连接，不走 SSH",
        "复制主机 ID",
        "远程控制设备 · app 记录",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // The SSH host's detail has these; the device's has none of them.
    for gone in ["立即检测", "随本机切换", "同步设置…", "查看会话 →", "热切"]
    {
        assert!(
            !f.texts.iter().any(|(t, _)| t == gone),
            "{gone} offered:\n{}",
            f.all_text()
        );
    }
    no_raw_id(&f.all_text());
    // The copy button copies the app's host id.
    h.click_text(&f, "复制主机 ID");
    h.until(
        |ui, cx, out| hosts::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::Copy(id) if id == FakeService::DEVICE_HOST_ID))
        },
    );
    // Commands and the palette never reach the device either.
    h.frame(|_, cx, out| {
        hosts::command(&mut state, cx, &format!("sync:{}", device()), out);
        hosts::command(&mut state, cx, &format!("probe:{}", device()), out);
        state.set_follow(cx, device(), true);
        let titles: Vec<String> = hosts::palette(cx, &state)
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert_eq!(titles.len(), 2, "{titles:?}");
        assert!(titles.iter().all(|t| !t.contains("Studio")), "{titles:?}");
    });
    assert!(state.sync.is_none(), "no sync dialog for a device");
    h.frames(3, |ui, cx, out| hosts::show(ui, cx, &mut state, out));
    let devices = [
        FakeService::DEVICE_HOST_ID,
        FakeService::UNNAMED_DEVICE_HOST_ID,
        FakeService::WSL_HOST_ID,
    ]
    .map(TargetId::remote);
    assert!(
        service.host_probes().iter().all(|id| !devices.contains(id)),
        "{:?}",
        service.host_probes()
    );
    assert!(
        service.follow_calls().is_empty(),
        "{:?}",
        service.follow_calls()
    );
    // The sidebar badge counts SSH hosts only.
    assert_eq!(state.unsynced(), 1);
}

#[test]
fn the_sessions_switcher_names_devices_and_never_scans_them() {
    let service = fake();
    let mut h = harness([1280.0, 800.0], Locale::ZhCn, &service);
    let labels: Vec<(String, bool)> = {
        let mut out = Vec::new();
        h.frame(|_, cx, _| {
            out = sessions::hosts(cx)
                .into_iter()
                .map(|e| (e.label, e.kind.is_managed()))
                .collect();
        });
        out
    };
    assert_eq!(
        labels,
        vec![
            ("devbox".to_owned(), true),
            ("gpu-01".to_owned(), true),
            ("Studio Mac".to_owned(), false),
            ("远程控制设备".to_owned(), false),
            ("WSL: Ubuntu".to_owned(), false),
        ]
    );
    let mut state = sessions::State::default();
    let f = h.frames(2, |ui, cx, out| sessions::show(ui, cx, &mut state, out));
    assert!(f.has("Studio Mac"), "{}", f.all_text());
    no_raw_id(&f.all_text());
    // Selecting the device explains instead of scanning over SSH.
    h.frame(|_, cx, out| sessions::command(&mut state, cx, &format!("host:{}", device()), out));
    let f = h.frames(4, |ui, cx, out| sessions::show(ui, cx, &mut state, out));
    for want in [
        "Superba 无法读取 Studio Mac 的会话",
        "它是远程控制设备：ChatGPT app 通过 ChatGPT 而不是 SSH 连接它",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    assert!(!f.has("正在扫描"), "{}", f.all_text());
    no_raw_id(&f.all_text());
    h.frame(|_, cx, out| {
        sessions::command(&mut state, cx, "rescan", out);
        state.scan(cx, &device());
        let palette: Vec<String> = sessions::palette(cx, &state)
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert_eq!(palette.len(), 2, "{palette:?}");
    });
    h.frames(3, |ui, cx, out| sessions::show(ui, cx, &mut state, out));
    assert!(
        service.host_scans().iter().all(|id| id != &device()),
        "{:?}",
        service.host_scans()
    );
}

#[test]
fn the_overview_path_counts_ssh_hosts_and_names_the_others() {
    let service = fake();
    let mut h = harness([1280.0, 800.0], Locale::ZhCn, &service);
    let overview = h.data.overview.value.clone().unwrap();
    h.frame(|_, cx, _| {
        let hero = hero::model(cx, &overview, &[], None);
        // devbox and gpu-01 only (old-box is an ssh-config entry).
        assert_eq!(hero.remotes, 2);
        assert!(
            hero.this.tip.contains("app 记录了 2 台远程主机"),
            "{}",
            hero.this.tip
        );
        assert!(
            hero.this
                .tip
                .contains("不由 Superba 管理：Studio Mac, 远程控制设备, WSL: Ubuntu"),
            "{}",
            hero.this.tip
        );
        no_raw_id(&hero.this.tip);
    });
}

#[test]
fn names_drop_characters_no_font_can_draw() {
    use superba_gui::design::fonts::drawable;
    assert_eq!(drawable("Studio Mac 💻"), "Studio Mac");
    assert_eq!(drawable("  Lab\u{1F9EA}\tbox  "), "Lab box");
    assert_eq!(drawable("工作站"), "工作站");
    assert_eq!(drawable("Ethan’s MacBook Pro"), "Ethan’s MacBook Pro");
    assert_eq!(drawable("\u{E000} \u{1F600}"), "");
}
