#![allow(missing_docs)]
//! Updates page (UX §3.9, plan §5 B7): per-OS app update actions (never sudo),
//! the app's Codex pin with compatibility badges and the full-restart plan,
//! pinning hidden where it is not offered (Windows), CLI rows by install kind
//! (npm only gets the command), remote installs, and errors such as a corrupted
//! download surfaced to the user.

mod router_harness;
mod support;

use std::sync::Arc;

use egui::Rect;
use pretty_assertions::assert_eq;
use router_harness::{Frame, Harness};
use superba_core::{
    AppCodexPin, AppOs, AppPackage, AppPackageKind, AppUpdateAction, CodexCandidate, CodexRelease,
    CodexVersion, CompatTier, Error, InstallTarget, Locale, SwitchIntent, VersionsView,
};
use superba_gui::design::widgets::Status;
use superba_gui::pages::updates::{
    self, Marks, PickerItem, PinChoice, State, VersionTag, check_path, compat_badge, picker_items,
    pin_for, version_tags,
};
use superba_gui::pages::{Action, NavBadge};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

fn loaded(f: &Frame) -> bool {
    f.has("26.1013.31256")
}

#[test]
fn header_and_superba_row_share_the_same_check_time() {
    use superba_core::{SelfUpdateStatus, SelfUpdateView, Timestamp};
    for locale in [Locale::En, Locale::ZhCn] {
        let mut h = harness(fake(), locale);
        h.data.selfupdate.finish(Ok(SelfUpdateView {
            current: "0.1.0".into(),
            status: SelfUpdateStatus::UpToDate,
            checked_at: Some(Timestamp::from_millis(
                Timestamp::now().as_millis() - 360_000,
            )),
            ..Default::default()
        }));
        let mut state = State::default();
        let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
        let tr = superba_gui::i18n::Tr::new(locale);
        let age = tr.f("time.minutes_ago", &[("n", &6)]);
        let label = tr.f("updates.checked", &[("age", &age)]);
        assert_eq!(
            f.texts.iter().filter(|(text, _)| text == &label).count(),
            2,
            "header and Superba row must both show {label}: {}",
            f.all_text()
        );
        let now_label = tr.f("updates.checked", &[("age", &tr.t("time.just_now"))]);
        assert!(!f.has(&now_label));
    }
}

fn harness(service: Arc<FakeService>, locale: Locale) -> Harness {
    Harness::new([1280.0, 2400.0], locale, service, false)
}

#[test]
fn linux_offers_a_verified_download_and_the_install_command_without_sudo_by_superba() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    for want in [
        "ChatGPT 桌面版",
        "下载 .deb（502 MB）",
        "Linux 版没有自动更新",
        "sudo apt install ./chatgpt_26.1013.31256_amd64.deb",
        "自带 Codex 0.163.0-alpha.10.2",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // The sidebar dot is the shell's (one source); the page adds none.
    assert_eq!(updates::status(&state).badge, NavBadge::None);
    assert!(updates::update_available(
        state.view.value.as_ref().unwrap()
    ));
    h.click_text(&f, "下载 .deb（502 MB）");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("打开安装器"),
    );
    assert!(f.has("/home/user/Downloads/chatgpt_26.1013.31256_amd64.deb"));
    h.click_text(&f, "打开安装器");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(matches!(&f.actions[0], Action::OpenUrl(p) if p.ends_with(".deb")));
}

#[test]
fn a_corrupted_download_is_reported() {
    let service = fake();
    service.fail_download(Error::other(
        "checksum mismatch: the download was discarded",
    ));
    let mut h = harness(service, Locale::En);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.click_text(&f, "Download .deb (502 MB)");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("checksum mismatch"),
    );
    assert!(!f.has("Open installer"));
}

#[test]
fn macos_and_windows_show_their_own_update_paths() {
    let service = fake();
    let mut view = service.versions_view();
    view.app.action = Some(AppUpdateAction::MacSparkle {
        package: Some(AppPackage {
            os: AppOs::Macos,
            kind: AppPackageKind::MacZip,
            arch: "arm64".into(),
            url: "https://downloads.example.com/ChatGPT.zip".into(),
            sha256: None,
            size: None,
            signature: None,
        }),
    });
    service.set_versions(view.clone());
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    assert!(f.has("ChatGPT 会自行更新（Sparkle）"));
    assert!(f.has("下载完整安装包"));
    assert!(!f.has("下载 .deb"));

    view.app.action = Some(AppUpdateAction::WindowsStore {
        store_url: "ms-windows-store://pdp/?ProductId=9PLM9XGG6VKS".into(),
        command: "winget upgrade --id 9PLM9XGG6VKS --source msstore".into(),
    });
    view.app_codex.supported = false;
    service.set_versions(view);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("Microsoft Store"),
    );
    assert!(f.has("winget upgrade --id 9PLM9XGG6VKS --source msstore"));
    // Windows: pinning the app's Codex is not offered in 0.3.
    assert!(f.has("此版本在这个系统上不提供选择桌面版使用的 Codex。"));
    assert!(!f.has("锁定版本"));
}

#[test]
fn pin_choices_show_compatibility_and_plan_a_full_restart() {
    assert_eq!(compat_badge(CompatTier::Same).1, "updates.compat.match");
    assert_eq!(compat_badge(CompatTier::Older).1, "updates.compat.older");
    assert_eq!(compat_badge(CompatTier::Newer).1, "updates.compat.newer");
    assert_eq!(
        compat_badge(CompatTier::BelowFloor).1,
        "updates.compat.below_floor"
    );
    assert_eq!(
        check_path("/usr/lib/node_modules/@openai/codex/bin/codex.js"),
        Err("updates.pin.not_native")
    );
    assert_eq!(check_path("  "), Err("updates.pin.path_empty"));
    assert!(check_path("/opt/codex/bin/codex").is_ok());
    assert_eq!(
        pin_for(PinChoice::Bundled, None, ""),
        Some(AppCodexPin::Bundled)
    );
    assert_eq!(pin_for(PinChoice::Version, None, ""), None);
    assert_eq!(pin_for(PinChoice::Path, None, "codex.js"), None);

    let service = fake();
    let mut h = harness(service, Locale::ZhCn);
    let mut state = State::default();
    h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    for (version, badge) in [
        ("0.160.0", "较旧"),
        ("0.165.0", "较新"),
        ("0.140.0", "低于 app 最低版本"),
        ("0.163.0", "与 ChatGPT 版本匹配"),
    ] {
        state.choice = PinChoice::Version;
        state.version = Some(v(version));
        let f = h.frames(2, |ui, cx, out| updates::show(ui, cx, &mut state, out));
        assert!(
            f.has(badge),
            "{version}: {badge} missing:\n{}",
            f.all_text()
        );
    }
    // Below the floor cannot be applied; a compatible version can.
    state.version = Some(v("0.140.0"));
    let f = h.frames(2, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    h.click_text(&f, "应用…");
    let f = h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    assert!(f.actions.iter().all(|a| !matches!(a, Action::Plan(_))));
    state.version = Some(v("0.163.0"));
    let f = h.frames(2, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    h.click_text(&f, "应用…");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::Plan(_))),
    );
    let request = f
        .actions
        .into_iter()
        .find_map(|a| match a {
            Action::Plan(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        request.intent,
        SwitchIntent::AppCodexPin {
            pin: AppCodexPin::Version {
                version: v("0.163.0")
            }
        }
    );
}

#[test]
fn cli_rows_follow_their_install_kind() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    for want in [
        "/home/user/.local/bin/codex",
        "独立安装",
        "npm",
        "app 自带",
        "PATH 中第 1 个",
        "复制更新命令",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // npm is never modified: only its command is offered.
    h.click_text(&f, "复制更新命令");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(matches!(&f.actions[0], Action::Copy(c) if c == "npm i -g @openai/codex@0.165.0"));
    assert!(service.installs().is_empty());
    // The standalone install is updated in place, with a rollback afterwards.
    let f = h.frames(2, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    h.click_text(&f, "更新到 0.165.0");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("回退到 0.162.0"),
    );
    assert!(f.has("已安装并校验 Codex 0.165.0"));
    assert_eq!(service.installs()[0].target, InstallTarget::Local);
    assert_eq!(service.installs()[0].version, v("0.165.0"));
    // The rollback switches back to the kept release (no new download).
    h.click_text(&f, "回退到 0.162.0");
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| !service.rollbacks().is_empty(),
    );
    assert_eq!(service.rollbacks(), vec![InstallTarget::Local]);
    assert_eq!(service.installs().len(), 1);
}

#[test]
fn remote_rows_install_on_the_host_and_failures_are_shown() {
    let service = fake();
    service.fail_installs(Error::other(
        "not enough space on devbox (needs about 600 MB)",
    ));
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    assert!(f.has("Codex · remote hosts"));
    assert!(f.has("devbox") && f.has("gpu-01"));
    assert!(f.has("Not connected"), "{}", f.all_text());
    // The remote button is the last "Update to".
    let r = f
        .texts
        .iter()
        .rev()
        .find(|(t, _)| t == "Update to 0.165.0")
        .map(|(_, r)| *r)
        .unwrap();
    h.click_at(r.center());
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("not enough space on devbox"),
    );
    let _ = f;
    assert!(matches!(
        &service.installs()[0].target,
        InstallTarget::Remote { target_id } if target_id.as_str() == "ssh:devbox-7c1e"
    ));
}

#[test]
fn check_and_palette_commands() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    h.frame(|_, cx, out| updates::command(&mut state, cx, "check", out));
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| service.version_refreshes() == 1,
    );
    h.frame(|_, cx, _| {
        let titles: Vec<String> = updates::palette(cx, &state)
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert_eq!(
            titles,
            [
                "Check for Superba updates",
                "Check for updates",
                "Update Codex CLI"
            ]
        );
    });
    h.frame(|_, cx, out| updates::command(&mut state, cx, "update_cli", out));
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| !service.installs().is_empty(),
    );
    h.frame(|_, cx, out| updates::command(&mut state, cx, "focus:ssh:devbox-7c1e", out));
    assert!(state.focus.is_some());
}

#[test]
fn an_app_updated_while_superba_runs_offers_a_reopen() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    assert!(!f.has("ChatGPT 已更新"));
    let mut view = service.versions_view();
    if let Some(i) = view.app.install.as_mut() {
        i.app_version = Some("26.1013.31256".into());
    }
    view.app.update_available = false;
    service.set_versions(view.clone());
    assert!(updates::app_updated(Some("26.1013.2"), &view));
    assert!(!updates::app_updated(None, &view));
    h.frame(|_, cx, out| updates::command(&mut state, cx, "check", out));
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("ChatGPT 已更新。通过 Superba 重新打开，之后切换就不用重启。"),
    );
    let f2 = h.frames(2, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    let _ = f;
    h.click_text(&f2, "重新打开 ChatGPT");
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::RestartApp)),
    );
}

// ---- Version pickers (user bug 20) ----------------------------------------------

fn candidate(version: &str, prerelease: bool) -> CodexCandidate {
    CodexCandidate {
        release: CodexRelease {
            version: v(version),
            tag: format!("rust-v{version}"),
            date: None,
            prerelease,
            bundled_by: Vec::new(),
            sha256: Default::default(),
        },
        tier: CompatTier::Compatible,
        stored: false,
    }
}

fn listed(items: &[PickerItem]) -> Vec<String> {
    items.iter().map(|i| i.version.to_string()).collect()
}

#[test]
fn pickers_list_versions_newest_first_by_semver_precedence() {
    let mut view = VersionsView::default();
    // Scrambled, with alpha / beta / rc, numeric and alphanumeric parts, and a
    // release published as a pre-release without a pre-release part.
    for (s, pre) in [
        ("0.164.0-alpha.2", true),
        ("0.163.0", false),
        ("0.164.0-rc.1", true),
        ("0.164.0", false),
        ("0.164.0-alpha.10", true),
        ("0.163.0-alpha.10.2", true),
        ("0.164.0-beta.1", true),
        ("0.165.0-alpha.1", true),
        ("0.164.0-alpha.1", true),
        ("0.163.0-alpha.10", true),
        ("0.164.0-alpha.beta", true),
        ("0.161.0", true),
        ("0.162.0", false),
    ] {
        view.app_codex.candidates.push(candidate(s, pre));
    }
    let marks = Marks::default();
    assert_eq!(
        listed(&picker_items(&view, true, false, &marks)),
        [
            "0.165.0-alpha.1",
            "0.164.0",
            "0.164.0-rc.1",
            "0.164.0-beta.1",
            "0.164.0-alpha.beta",
            "0.164.0-alpha.10",
            "0.164.0-alpha.2",
            "0.164.0-alpha.1",
            "0.163.0",
            "0.163.0-alpha.10.2",
            "0.163.0-alpha.10",
            "0.162.0",
            "0.161.0",
        ]
    );
    // The box cleared: releases only (the release published as a pre-release too
    // is left out).
    assert_eq!(
        listed(&picker_items(&view, false, false, &marks)),
        ["0.164.0", "0.163.0", "0.162.0"]
    );
    // Every pre-release carries its tag.
    for item in picker_items(&view, true, false, &marks) {
        let pre = item.version.pre.is_some() || item.version == v("0.161.0");
        assert_eq!(
            item.tags.contains(&VersionTag::Prerelease),
            pre,
            "{}",
            item.version
        );
    }
    // Without a release list the CLI rows' newest release is offered.
    let bare = VersionsView {
        cli: fake().versions_view().cli,
        ..Default::default()
    };
    assert_eq!(
        listed(&picker_items(&bare, true, false, &marks)),
        ["0.165.0"]
    );
}

#[test]
fn tags_mark_prereleases_the_bundled_and_the_current_version() {
    let marks = Marks {
        bundled: Some(v("0.163.0-alpha.10.2")),
        current: Some(v("0.162.0")),
    };
    use VersionTag::*;
    assert_eq!(
        version_tags(&v("0.163.0-alpha.10.2"), true, false, &marks),
        [Prerelease, Bundled]
    );
    assert_eq!(
        version_tags(&v("0.162.0"), false, true, &marks),
        [Current, Stored]
    );
    // A pre-release part alone makes a pre-release; so does the published flag.
    assert_eq!(
        version_tags(&v("0.164.0-alpha.1"), false, false, &marks),
        [Prerelease]
    );
    assert_eq!(
        version_tags(&v("0.161.0"), true, false, &marks),
        [Prerelease]
    );
    assert!(version_tags(&v("0.165.0"), false, false, &marks).is_empty());
    // Amber pre-release, green current, neutral bundled / downloaded.
    assert_eq!(Prerelease.status(), Some(Status::Warn));
    assert_eq!(Current.status(), Some(Status::Ok));
    assert_eq!(Bundled.status(), None);
    assert_eq!(Stored.status(), None);

    // Pin picker: the pinned version is current, the bundled one while the app
    // uses its own Codex.
    let mut view = fake().versions_view();
    let bundled = v("0.162.0-alpha.17.2");
    assert_eq!(
        updates::pin_marks(&view),
        Marks {
            bundled: Some(bundled.clone()),
            current: Some(bundled.clone()),
        }
    );
    view.app_codex.pin = AppCodexPin::Version {
        version: v("0.163.0"),
    };
    assert_eq!(updates::pin_marks(&view).current, Some(v("0.163.0")));
    // Local picker: the standalone install; remote: the host's Codex.
    assert_eq!(updates::local_marks(&view).current, Some(v("0.162.0")));
    let row = &view.remote[0];
    assert_eq!(updates::remote_marks(&view, row).current, row.version);
    // Picker items carry the marks.
    let items = picker_items(&view, true, true, &updates::pin_marks(&view));
    let tags = |s: &str| {
        items
            .iter()
            .find(|i| i.version == v(s))
            .map(|i| i.tags.clone())
            .unwrap()
    };
    assert_eq!(tags("0.162.0-alpha.17.2"), [Prerelease, Bundled]);
    assert_eq!(tags("0.163.0"), [Current, Stored]);
    assert!(tags("0.165.0").is_empty());
}

#[test]
fn the_prerelease_box_is_remembered_per_picker_and_filters_its_list() {
    let mut state = State::default();
    let items_all = picker_items(&fake().versions_view(), true, false, &Marks::default());
    let items_stable = picker_items(&fake().versions_view(), false, false, &Marks::default());
    // Untouched: on only when the version in use there is a pre-release.
    assert!(!state.shows_prerelease("local", Some(&v("0.162.0"))));
    assert!(state.shows_prerelease("local", Some(&v("0.162.0-alpha.17.2"))));
    assert!(!state.shows_prerelease("local", None));
    state.prerelease.insert("local".into(), true);
    assert!(state.shows_prerelease("local", None));
    assert!(!state.shows_prerelease("pin", None));
    // The target is the newest offered unless another one was picked, and a pick
    // the box now hides falls back to the newest offered.
    assert_eq!(state.pick("local", &items_all), Some(v("0.165.0")));
    state.picks.insert("local".into(), v("0.163.0-alpha.10.2"));
    assert_eq!(
        state.pick("local", &items_all),
        Some(v("0.163.0-alpha.10.2"))
    );
    assert_eq!(state.pick("local", &items_stable), Some(v("0.165.0")));
}

fn menu_versions(f: &Frame, field: Rect) -> Vec<(String, Rect)> {
    let mut rows: Vec<(String, Rect)> = f
        .texts
        .iter()
        .filter(|(t, r)| {
            CodexVersion::parse(t).is_ok_and(|p| p.to_string() == *t)
                && r.min.y > field.max.y
                && r.min.y < field.max.y + 320.0
                && r.min.x >= field.min.x
                && r.min.x < field.max.x
        })
        .cloned()
        .collect();
    rows.sort_by(|a, b| a.1.min.y.total_cmp(&b.1.min.y));
    rows
}

/// Texts `text` on the line of `row` and right of it.
fn after_on_line(f: &Frame, row: Rect, text: &str) -> bool {
    f.texts.iter().any(|(t, r)| {
        t == text && (r.center().y - row.center().y).abs() < 3.0 && r.min.x > row.max.x
    })
}

#[test]
fn pickers_put_the_box_first_and_mark_versions_in_the_menu_and_the_field() {
    let service = fake();
    let mut view = service.versions_view();
    view.app_codex
        .candidates
        .push(candidate("0.166.0-alpha.1", true));
    service.set_versions(view);
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    // Pin, local CLI and devbox (gpu-01 is not connected): each box sits in front
    // of its dropdown, on the dropdown's centre line.
    let boxes: Vec<Rect> = f
        .texts
        .iter()
        .filter(|(t, _)| t == "包含预发布版本")
        .map(|(_, r)| *r)
        .collect();
    assert_eq!(boxes.len(), 3, "{}", f.all_text());
    let pin_field = f.find("选择版本…").unwrap();
    assert!(boxes[0].max.x < pin_field.min.x);
    assert!((boxes[0].center().y - pin_field.center().y).abs() < 1.5);
    let fields: Vec<Rect> = f
        .texts
        .iter()
        .filter(|(t, _)| t == "0.165.0")
        .map(|(_, r)| *r)
        .collect();
    assert_eq!(fields.len(), 2, "local and devbox target 0.165.0");
    for (b, field) in boxes[1..].iter().zip(&fields) {
        assert!(b.max.x < field.min.x);
        assert!((b.center().y - field.center().y).abs() < 1.5);
    }
    assert!(f.has("更新到 0.165.0"));

    // The pin menu, box cleared: releases only, newest first.
    h.click_text(&f, "选择版本…");
    let f = h.frames(5, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    let rows = menu_versions(&f, pin_field);
    let names: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        names,
        ["0.165.0", "0.163.0", "0.162.0", "0.160.0", "0.140.0"]
    );
    assert!(after_on_line(&f, rows[2].1, "已下载"));
    assert!(!f.texts.iter().any(|(t, _)| t == "预发布"));

    // Tick the pin's box: the newest pre-release comes first, every pre-release is
    // tagged, the bundled one is "内置" and (the app uses it) "当前".
    // (A click outside closes the menu first.)
    h.frames(2, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    h.click_at(boxes[0].center());
    let f2 = h.frames(5, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    assert_eq!(state.prerelease.get("pin"), Some(&true));
    assert_eq!(state.prerelease.get("local"), None);
    h.click_text(&f2, "选择版本…");
    let f = h.frames(5, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    let rows = menu_versions(&f, pin_field);
    let names: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        names,
        [
            "0.166.0-alpha.1",
            "0.165.0",
            "0.163.0",
            "0.163.0-alpha.10.2",
            "0.162.0",
            "0.162.0-alpha.17.2",
            "0.160.0",
            "0.140.0",
        ]
    );
    for (name, r) in &rows {
        assert_eq!(
            after_on_line(&f, *r, "预发布"),
            name.contains('-'),
            "{name}"
        );
    }
    let bundled = rows[5].1;
    assert!(after_on_line(&f, bundled, "内置") && after_on_line(&f, bundled, "当前"));
    // Choose the newest pre-release: the closed field shows it with its tag.
    h.click_at(rows[0].1.center());
    let f = h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    assert_eq!(state.version, Some(v("0.166.0-alpha.1")));
    assert_eq!(state.choice, PinChoice::Version);
    let field = f
        .texts
        .iter()
        .find(|(t, r)| t == "0.166.0-alpha.1" && (r.center().y - pin_field.center().y).abs() < 1.5)
        .map(|(_, r)| *r)
        .expect("the field shows the choice");
    assert!(after_on_line(&f, field, "预发布"));

    // The local CLI box is its own: ticking it targets the newest pre-release
    // there, devbox keeps its release.
    h.click_at(boxes[1].center());
    let f = h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    assert_eq!(state.prerelease.get("local"), Some(&true));
    assert!(f.has("更新到 0.166.0-alpha.1"), "{}", f.all_text());
    let local = f
        .texts
        .iter()
        .find(|(t, r)| t == "0.166.0-alpha.1" && (r.center().y - boxes[1].center().y).abs() < 1.5)
        .map(|(_, r)| *r)
        .expect("the local field shows the pre-release");
    assert!(after_on_line(&f, local, "预发布"));
    assert!(f.texts.iter().any(|(t, r)| t == "更新到 0.165.0"
        && (r.center().y - boxes[2].center().y).abs() < 1.5));
    // Remembered across navigation (the state outlives the page).
    let mut h2 = harness(service, Locale::ZhCn);
    let f = h2.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    assert!(f.has("更新到 0.166.0-alpha.1"));
}

#[test]
fn pickers_read_in_english() {
    let service = fake();
    let mut h = harness(service, Locale::En);
    let mut state = State::default();
    state.prerelease.insert(updates::PIN_PICKER.into(), true);
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    assert!(f.has("Include pre-releases"));
    h.click_text(&f, "Choose a version…");
    let f = h.frames(5, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    for want in ["Pre-release", "Bundled", "Current", "Downloaded"] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
}

#[test]
fn cli_paths_keep_their_file_name_and_versions_are_never_cut() {
    // Review ux-visual #12: "/tmp/…/home/.local/bin…" hid the file name and
    // "0.163.0-alpha.1…" the version.
    let service = fake();
    let mut view = service.versions_view();
    let long = "/tmp/sbshots-8kxgc1k4/home/.local/share/superba/codex/packages/standalone/current/bin/codex";
    view.cli[0].binary.path = long.into();
    view.cli[2].binary.version = Some(v("0.163.0-alpha.10.2"));
    service.set_versions(view.clone());
    let data = router_harness::data_from(&service);
    for locale in [Locale::ZhCn, Locale::En] {
        let mut state = State::default();
        // Loaded just now: the page does not read the (idle) service again.
        state.view.value = Some(service.versions_view());
        state.view.at = Some(std::time::Instant::now());
        let report = support::fit::render_fit(
            [1280.0, 2400.0],
            locale,
            egui::Theme::Light,
            &data,
            |ui, cx, out| updates::show(ui, cx, &mut state, out),
        );
        let path = report
            .texts
            .iter()
            .find(|t| t.starts_with("/tmp/sbshots"))
            .unwrap_or_else(|| panic!("no path: {}", report.all_text()));
        assert!(path.contains('…'), "{locale:?}: shortened: {path}");
        assert!(
            path.ends_with("…/codex"),
            "{locale:?}: the file name stays: {path}"
        );
        assert!(report.whole(path), "{locale:?}: drawn whole: {path}");
        assert!(
            report.whole("0.163.0-alpha.10.2"),
            "{locale:?}: the version cell has room: {:?}",
            report
                .text_shapes
                .iter()
                .filter(|t| t.0.starts_with("0.163"))
                .collect::<Vec<_>>()
        );
    }
}
