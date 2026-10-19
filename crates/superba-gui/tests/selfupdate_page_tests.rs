#![allow(missing_docs)]
//! Superba's own update in the GUI: the Updates page's first section in each
//! state, the choices (update now / later / skip), the install outcomes, the
//! Overview to-do, the sidebar dot, the command palette entry and Preferences.

mod router_harness;
mod support;

use std::sync::Arc;

use pretty_assertions::assert_eq;
use router_harness::Harness;
use superba_core::{
    Error, Locale, SelfInstallOutcome, SelfRelease, SelfUpdateReport, SelfUpdateStatus,
    SelfUpdateView, Settings, Timestamp, UpdateChannel,
};
use superba_gui::design::widgets::Status;
use superba_gui::i18n::Tr;
use superba_gui::pages::overview::todo::{self, TodoKind};
use superba_gui::pages::updates::{self, State};
use superba_gui::pages::updates_self::{
    NoteLine, channel_line, inline, no_downgrade_note, note_lines,
};
use superba_gui::pages::{Action, NavBadge, Page, superba::update_settings};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn release(version: &str) -> SelfRelease {
    SelfRelease {
        tag: format!("v{version}"),
        version: version.into(),
        prerelease: version.contains('-'),
        notes:
            "## What's new\n- Faster **switching**\n- Fixes [#12](https://x.example/12)\n\nThanks!"
                .into(),
        published_at: Some("2026-10-17T08:00:00Z".into()),
        url: format!("https://github.com/krill-lab/superba/releases/tag/v{version}"),
        asset: Some(format!("superba-{version}-linux-x86_64.tar.gz")),
        asset_size: Some(1000),
    }
}

fn view(status: SelfUpdateStatus, latest: Option<SelfRelease>) -> SelfUpdateView {
    SelfUpdateView {
        current: "0.1.0".into(),
        status,
        latest,
        checked_at: Some(Timestamp::now()),
        auto_check: true,
        ..SelfUpdateView::default()
    }
}

fn report(outcome: SelfInstallOutcome) -> SelfUpdateReport {
    SelfUpdateReport {
        version: "0.2.0".into(),
        outcome,
        package: None,
        steps: Vec::new(),
        router_restarted: false,
        relaunching: false,
        replaced: vec!["spb".into()],
    }
}

fn harness(service: Arc<FakeService>, locale: Locale, v: SelfUpdateView) -> Harness {
    let mut h = Harness::new([1280.0, 2400.0], locale, service, false);
    h.data.selfupdate.finish(Ok(v));
    h
}

fn page(h: &mut Harness, state: &mut State, want: &str) -> router_harness::Frame {
    h.until(
        |ui, cx, out| updates::show(ui, cx, state, out),
        |f| f.has(want),
    )
}

#[test]
fn up_to_date_shows_both_versions_and_when_it_was_checked() {
    let mut h = harness(
        fake(),
        Locale::ZhCn,
        view(SelfUpdateStatus::UpToDate, Some(release("0.1.0"))),
    );
    let f = page(&mut h, &mut State::default(), "已是最新");
    for want in ["Superba", "已安装", "0.1.0", "最新", "上次检查"] {
        assert!(f.has(want), "{want}:\n{}", f.all_text());
    }
    assert!(!f.has("立即更新"), "{}", f.all_text());
    assert!(!f.has("跳过此版本"));
}

#[test]
fn an_available_update_shows_notes_as_plain_text_and_the_three_choices() {
    for (locale, now, later, skip, notes) in [
        (Locale::ZhCn, "立即更新", "稍后", "跳过此版本", "更新内容"),
        (
            Locale::En,
            "Update now",
            "Later",
            "Skip this version",
            "What's new",
        ),
    ] {
        let mut h = harness(
            fake(),
            locale,
            view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
        );
        let f = page(&mut h, &mut State::default(), now);
        for want in [
            now,
            later,
            skip,
            notes,
            "0.2.0",
            "2026-10-17",
            "Faster switching",
            "Fixes #12",
            "Thanks!",
        ] {
            assert!(f.has(want), "{locale:?} {want}:\n{}", f.all_text());
        }
        // Markdown markers are gone.
        assert!(!f.has("**") && !f.has("]("), "{}", f.all_text());
    }
}

#[test]
fn a_private_release_page_is_a_calm_line_not_an_error() {
    let mut h = harness(
        fake(),
        Locale::ZhCn,
        view(SelfUpdateStatus::PageUnavailable, None),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "暂时无法检查更新（发布页不可访问）");
    assert!(!f.has("立即更新"));
    // No toast, no action: the state is quiet.
    assert!(
        f.actions.iter().all(|a| !matches!(a, Action::Toast { .. })),
        "{:?}",
        f.actions
    );
    let mut h = harness(
        fake(),
        Locale::En,
        view(SelfUpdateStatus::PageUnavailable, None),
    );
    let f = page(
        &mut h,
        &mut State::default(),
        "Can't check for updates right now",
    );
    assert!(f.has("release page isn't reachable"));
}

#[test]
fn other_quiet_states() {
    let mut failed = view(SelfUpdateStatus::CheckFailed, None);
    failed.detail = Some("timed out".into());
    let mut h = harness(fake(), Locale::En, failed);
    let f = page(
        &mut h,
        &mut State::default(),
        "The update check failed (timed out)",
    );
    assert!(f.has("try again later"));
    let mut h = harness(
        fake(),
        Locale::En,
        view(SelfUpdateStatus::OnlineChecksOff, None),
    );
    page(&mut h, &mut State::default(), "Online checks are off");
    let mut h = harness(
        fake(),
        Locale::ZhCn,
        view(SelfUpdateStatus::NotChecked, None),
    );
    page(&mut h, &mut State::default(), "尚未检查");
}

#[test]
fn a_release_without_a_package_cannot_be_installed_from_here() {
    let mut v = view(SelfUpdateStatus::Available, Some(release("0.2.0")));
    v.no_package = true;
    v.latest.as_mut().unwrap().asset = None;
    let mut h = harness(fake(), Locale::En, v);
    let f = page(&mut h, &mut State::default(), "no package for this system");
    assert!(!f.has("Update now"));
}

#[test]
fn nothing_is_installed_until_the_button_is_clicked() {
    let service = fake();
    let mut h = harness(
        service.clone(),
        Locale::En,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "Update now");
    for _ in 0..5 {
        h.frame(|ui, cx, out| updates::show(ui, cx, &mut state, out));
    }
    assert!(service.self_update_installs().is_empty());
    assert!(f.actions.iter().all(|a| !matches!(a, Action::QuitApp)));

    service.set_self_update_report(SelfUpdateReport {
        relaunching: true,
        ..report(SelfInstallOutcome::Installed)
    });
    h.click_text(&f, "Update now");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("Restarting"),
    );
    let installs = service.self_update_installs();
    assert_eq!(installs.len(), 1);
    assert!(installs[0].relaunch, "the GUI relaunches itself");
    assert!(f.has("Superba 0.2.0 is installed"), "{}", f.all_text());
}

#[test]
fn a_finished_install_that_relaunches_quits_this_process() {
    let service = fake();
    service.set_self_update_report(SelfUpdateReport {
        relaunching: true,
        router_restarted: true,
        ..report(SelfInstallOutcome::Installed)
    });
    let mut h = harness(
        service,
        Locale::En,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "Update now");
    h.click_text(&f, "Update now");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::QuitApp)),
    );
    assert!(
        f.actions.iter().any(|a| matches!(
            a,
            Action::Toast {
                status: Status::Ok,
                ..
            }
        )),
        "{:?}",
        f.actions
    );
    assert!(f.has("local router was restarted"), "{}", f.all_text());
}

#[test]
fn a_failed_install_shows_the_error_and_keeps_the_button() {
    let service = fake();
    service.fail_self_update_install(Error::other("does not match its published sha256 checksum"));
    let mut h = harness(
        service,
        Locale::En,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "Update now");
    h.click_text(&f, "Update now");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("sha256 checksum"),
    );
    assert!(f.has("Update now"), "the user can try again");
}

#[test]
fn an_unwritable_location_shows_the_verified_package_and_manual_steps() {
    let service = fake();
    service.set_self_update_report(SelfUpdateReport {
        package: Some(
            "/home/u/.local/share/superba/downloads/superba-update/superba-0.2.0/p.tar.gz".into(),
        ),
        steps: vec![
            superba_core::LocalizedText::new(
                "selfupdate.manual.package",
                "The update was downloaded and verified: {path}",
            )
            .arg("path", "/home/u/p.tar.gz"),
            superba_core::LocalizedText::new(
                "selfupdate.manual.linux",
                "Quit Superba and extract the archive over {dir}",
            )
            .arg("dir", "/opt/superba"),
        ],
        ..report(SelfInstallOutcome::Manual)
    });
    let mut h = harness(
        service,
        Locale::ZhCn,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "立即更新");
    h.click_text(&f, "立即更新");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.has("在文件夹中显示更新包"),
    );
    for want in ["无法在当前位置替换自己", "已下载并校验通过", "/opt/superba"] {
        assert!(f.has(want), "{want}:\n{}", f.all_text());
    }
    h.click_text(&f, "在文件夹中显示更新包");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::OpenUrl(_))),
    );
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::OpenUrl(p) if p.ends_with("superba-0.2.0")))
    );
}

#[test]
fn later_and_skip_are_actions_and_a_skipped_version_can_be_unskipped() {
    let service = fake();
    let mut h = harness(
        service,
        Locale::En,
        view(SelfUpdateStatus::Available, Some(release("0.2.0"))),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "Later");
    h.click_text(&f, "Later");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::LaterSelfUpdate(_)))
        },
    );
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::LaterSelfUpdate(v) if v == "0.2.0"))
    );
    h.click_text(&f, "Skip this version");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::SkipSelfUpdate(_)))
        },
    );
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::SkipSelfUpdate(Some(v)) if v == "0.2.0"))
    );

    // Once skipped: marked, "later" is gone, and the skip can be undone.
    let mut skipped = view(SelfUpdateStatus::Available, Some(release("0.2.0")));
    skipped.skipped = Some("0.2.0".into());
    let mut h = harness(fake(), Locale::En, skipped);
    let mut state = State::default();
    let f = page(&mut h, &mut state, "Don't skip it");
    assert!(
        f.has("Skipped") && f.has("Update now") && !f.has("Later"),
        "{}",
        f.all_text()
    );
    h.click_text(&f, "Don't skip it");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::SkipSelfUpdate(None)))
        },
    );
    assert!(!f.actions.is_empty());
}

#[test]
fn the_check_button_checks_superba_too() {
    let mut h = harness(
        fake(),
        Locale::En,
        view(SelfUpdateStatus::UpToDate, Some(release("0.1.0"))),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "Check for updates");
    h.click_text(&f, "Check for updates");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::CheckSelfUpdate))
        },
    );
    assert!(!f.actions.is_empty());
}

#[test]
fn offered_only_when_newer_not_skipped_and_not_put_off() {
    let mut data = router_harness::data_from(&fake());
    assert!(data.self_update_offered().is_none());
    assert!(!data.update_available());
    data.selfupdate.finish(Ok(view(
        SelfUpdateStatus::Available,
        Some(release("0.2.0")),
    )));
    assert_eq!(
        data.self_update_offered().map(|r| r.version.as_str()),
        Some("0.2.0")
    );
    assert!(data.update_available());
    // "Later" hides it for the session.
    data.selfupdate_later = Some("0.2.0".into());
    assert!(data.self_update_offered().is_none());
    assert!(!data.update_available());
    // A newer version than the one put off is offered again.
    data.selfupdate.finish(Ok(view(
        SelfUpdateStatus::Available,
        Some(release("0.3.0")),
    )));
    assert!(data.self_update_offered().is_some());
    // Skipped: not offered.
    data.selfupdate_later = None;
    let mut skipped = view(SelfUpdateStatus::Available, Some(release("0.3.0")));
    skipped.skipped = Some("0.3.0".into());
    data.selfupdate.finish(Ok(skipped));
    assert!(data.self_update_offered().is_none());
    // Up to date / unreachable: nothing.
    data.selfupdate
        .finish(Ok(view(SelfUpdateStatus::PageUnavailable, None)));
    assert!(data.self_update_offered().is_none());
}

#[test]
fn the_sidebar_updates_entry_gets_a_dot_naming_the_version() {
    use superba_gui::app::shell_badges;
    let mut data = router_harness::data_from(&fake());
    let tr = Tr::new(Locale::ZhCn);
    assert!(
        shell_badges(&tr, &data)
            .iter()
            .all(|(p, ..)| *p != Page::Superba(superba_gui::pages::SuperbaSection::Home))
    );
    data.selfupdate.finish(Ok(view(
        SelfUpdateStatus::Available,
        Some(release("0.2.0")),
    )));
    let badges = shell_badges(&tr, &data);
    let (_, badge, tip) = badges
        .iter()
        .find(|(p, ..)| *p == Page::Superba(superba_gui::pages::SuperbaSection::Home))
        .cloned()
        .unwrap();
    assert_eq!(badge, NavBadge::Count(1));
    assert_eq!(tip, "有 1 项可更新");
}

#[test]
fn the_overview_todo_offers_the_update_and_opens_the_updates_page() {
    for locale in [Locale::ZhCn, Locale::En] {
        let mut h = Harness::new([1280.0, 800.0], locale, fake(), false);
        h.data.selfupdate.finish(Ok(view(
            SelfUpdateStatus::Available,
            Some(release("0.2.0")),
        )));
        h.frame(|_, cx, _| {
            let todos = todo::todos(cx, None);
            let t = todos
                .iter()
                .find(|t| t.kind == TodoKind::SuperbaUpdate)
                .unwrap_or_else(|| panic!("no Superba to-do: {todos:?}"));
            assert_eq!(t.status, Status::Idle, "an offer, not a problem");
            assert!(
                t.title.contains("Superba") && t.title.contains("0.2.0"),
                "{}",
                t.title
            );
            assert!(matches!(
                t.fix(cx),
                Action::Go(Page::Superba(superba_gui::pages::SuperbaSection::Updates))
            ));
            assert!(t.spot.is_none(), "offers mark no hero node");
        });
        // Put off: gone.
        h.data.selfupdate_later = Some("0.2.0".into());
        h.frame(|_, cx, _| {
            assert!(
                todo::todos(cx, None)
                    .iter()
                    .all(|t| t.kind != TodoKind::SuperbaUpdate)
            );
        });
    }
}

#[test]
fn the_palette_has_a_superba_update_command_that_opens_the_page_and_checks() {
    for (locale, title) in [
        (Locale::ZhCn, "检查 Superba 更新"),
        (Locale::En, "Check for Superba updates"),
    ] {
        let mut h = Harness::new([1280.0, 800.0], locale, fake(), false);
        h.frame(|_, cx, _| {
            let items = updates::palette(cx, &State::default());
            let item = items.iter().find(|i| i.title == title).expect(title);
            assert!(
                matches!(&item.action, Action::PageCommand { page: Page::Superba(superba_gui::pages::SuperbaSection::Updates), command } if command == "check_superba"),
                "{:?}",
                item.action
            );
        });
        let mut state = State::default();
        let mut out = Vec::new();
        h.frame(|_, cx, _| updates::command(&mut state, cx, "check_superba", &mut out));
        assert!(out.iter().any(|a| matches!(a, Action::CheckSelfUpdate)));
    }
}

#[test]
fn preferences_has_the_two_update_choices_with_the_right_defaults() {
    let defaults = Settings::default();
    assert!(
        defaults.auto_update_check,
        "automatic checks are on by default"
    );
    assert_eq!(
        defaults.update_channel, None,
        "no channel chosen: the running build decides"
    );
    assert_eq!(
        defaults.update_channel_for("0.1.1-alpha.3"),
        UpdateChannel::Preview
    );
    assert_eq!(defaults.update_channel_for("0.1.0"), UpdateChannel::Stable);
    for (locale, auto, channel, stable, preview) in [
        (
            Locale::ZhCn,
            "自动检查 Superba 更新",
            "更新通道",
            "稳定版",
            "预览版",
        ),
        (
            Locale::En,
            "Check for Superba updates automatically",
            "Update channel",
            "Stable",
            "Preview",
        ),
    ] {
        let mut h = Harness::new([1280.0, 2600.0], locale, fake(), false);
        let mut settings = Settings::default();
        let f = h.until(
            |ui, cx, out| update_settings::show(ui, cx, &mut settings, out),
            |f| f.has(auto),
        );
        for want in [channel, stable, preview] {
            assert!(f.has(want), "{locale:?} {want}: {}", f.all_text());
        }
    }
}

#[test]
fn picking_a_channel_in_preferences_saves_it() {
    let mut h = Harness::new([1280.0, 2600.0], Locale::En, fake(), false);
    let mut settings = Settings {
        update_channel: Some(UpdateChannel::Stable),
        ..Settings::default()
    };
    let f = h.until(
        |ui, cx, out| update_settings::show(ui, cx, &mut settings, out),
        |f| f.has("Update channel"),
    );
    h.click_text(&f, "Preview");
    let f = h.until(
        |ui, cx, out| update_settings::show(ui, cx, &mut settings, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::SaveSettings)),
    );
    assert!(!f.actions.is_empty());
    assert_eq!(settings.update_channel, Some(UpdateChannel::Preview));
}

#[test]
fn old_settings_files_keep_the_defaults_and_only_changes_are_written() {
    // A profiles.json written before these choices existed.
    let old: Settings = serde_json::from_str(r#"{"countdown_secs":3}"#).unwrap();
    assert!(old.auto_update_check);
    assert_eq!(old.update_channel, None);
    let json = serde_json::to_value(Settings::default()).unwrap();
    assert!(
        json.get("auto_update_check").is_none() && json.get("update_channel").is_none(),
        "defaults are not written"
    );
    let changed = Settings {
        auto_update_check: false,
        update_channel: Some(UpdateChannel::Preview),
        ..Settings::default()
    };
    let json = serde_json::to_value(&changed).unwrap();
    assert_eq!(json["auto_update_check"], false);
    assert_eq!(json["update_channel"], "preview");
    assert_eq!(serde_json::from_value::<Settings>(json).unwrap(), changed);
}

/// The reported case: 0.1.1-alpha.3 with 0.1.0 as the newest final release.
fn preview_build(status: SelfUpdateStatus, channel: UpdateChannel) -> SelfUpdateView {
    let preview = release("0.1.1-alpha.4");
    let stable = release("0.1.0");
    SelfUpdateView {
        current: "0.1.1-alpha.3".into(),
        status,
        latest: Some(match channel {
            UpdateChannel::Stable => stable.clone(),
            UpdateChannel::Preview => preview.clone(),
        }),
        channel,
        latest_stable: Some(stable),
        latest_preview: (channel == UpdateChannel::Preview).then_some(preview),
        checked_at: Some(Timestamp::now()),
        auto_check: true,
        ..SelfUpdateView::default()
    }
}

#[test]
fn the_channel_line_names_this_build_and_what_each_channel_has() {
    let v = preview_build(SelfUpdateStatus::Available, UpdateChannel::Preview);
    assert_eq!(
        channel_line(&Tr::new(Locale::En), &v),
        "You're on preview 0.1.1-alpha.3 · latest preview 0.1.1-alpha.4 · latest stable 0.1.0"
    );
    assert_eq!(
        channel_line(&Tr::new(Locale::ZhCn), &v),
        "当前为预览版 0.1.1-alpha.3 · 最新预览版 0.1.1-alpha.4 · 最新稳定版 0.1.0"
    );
    let stable = SelfUpdateView {
        current: "0.1.0".into(),
        latest_stable: Some(release("0.2.0")),
        ..SelfUpdateView::default()
    };
    assert_eq!(
        channel_line(&Tr::new(Locale::En), &stable),
        "You're on stable 0.1.0 · latest stable 0.2.0"
    );
    // Only a preview build on Stable that has nothing newer gets the note.
    let waiting = preview_build(SelfUpdateStatus::UpToDate, UpdateChannel::Stable);
    let note = no_downgrade_note(&Tr::new(Locale::En), &waiting).unwrap();
    assert!(note.contains("never installs an older version"), "{note}");
    assert!(
        no_downgrade_note(&Tr::new(Locale::ZhCn), &waiting)
            .unwrap()
            .contains("不会自动安装更旧的版本")
    );
    assert_eq!(no_downgrade_note(&Tr::new(Locale::En), &v), None);
}

#[test]
fn the_page_shows_the_channel_and_offers_the_next_preview() {
    for (locale, want) in [
        (
            Locale::En,
            [
                "Channel",
                "Preview",
                "You're on preview 0.1.1-alpha.3 · latest preview 0.1.1-alpha.4 · latest stable 0.1.0",
                "Update now",
            ],
        ),
        (
            Locale::ZhCn,
            [
                "更新通道",
                "预览版",
                "当前为预览版 0.1.1-alpha.3 · 最新预览版 0.1.1-alpha.4 · 最新稳定版 0.1.0",
                "立即更新",
            ],
        ),
    ] {
        let mut h = harness(
            fake(),
            locale,
            preview_build(SelfUpdateStatus::Available, UpdateChannel::Preview),
        );
        let f = page(&mut h, &mut State::default(), want[3]);
        for w in want {
            assert!(f.has(w), "{locale:?} {w}:\n{}", f.all_text());
        }
        assert!(f.has("0.1.1-alpha.4"), "{}", f.all_text());
    }
}

#[test]
fn a_preview_build_on_stable_is_never_offered_the_older_stable() {
    let mut h = harness(
        fake(),
        Locale::En,
        preview_build(SelfUpdateStatus::UpToDate, UpdateChannel::Stable),
    );
    let mut state = State::default();
    let f = page(&mut h, &mut state, "No stable release is newer");
    assert!(
        f.has("You're on preview 0.1.1-alpha.3 · latest stable 0.1.0"),
        "{}",
        f.all_text()
    );
    assert!(!f.has("Update now"), "{}", f.all_text());
    // Switching the channel on the page is the same choice as in Preferences.
    h.click_text(&f, "Preview");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, Action::SetUpdateChannel(_)))
        },
    );
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::SetUpdateChannel(UpdateChannel::Preview)))
    );
}

#[test]
fn the_rate_limit_is_a_quiet_line_with_the_time_it_ends() {
    let mut v = preview_build(SelfUpdateStatus::Available, UpdateChannel::Preview);
    v.detail = Some("rate limited".into());
    v.limited_until = Some(Timestamp::from_millis(
        Timestamp::now().as_millis() + 600_000,
    ));
    let mut h = harness(fake(), Locale::En, v);
    let f = page(&mut h, &mut State::default(), "GitHub's limit");
    assert!(f.has("the last answer is shown"), "{}", f.all_text());
    assert!(f.has("Update now"), "the cached answer is still offered");
}

#[test]
fn release_notes_are_rendered_as_plain_text() {
    assert_eq!(
        note_lines(
            "# Title\n\n- one **bold**\n* two `code`\n1. third [link](https://x.example)\n\n\n> quote\n```\nfence\n```\nlast"
        ),
        vec![
            NoteLine::Heading("Title".into()),
            NoteLine::Gap,
            NoteLine::Bullet("one bold".into()),
            NoteLine::Bullet("two code".into()),
            NoteLine::Bullet("third link".into()),
            NoteLine::Gap,
            NoteLine::Text("quote".into()),
            NoteLine::Text("fence".into()),
            NoteLine::Text("last".into()),
        ]
    );
    assert_eq!(
        inline("__a__ and <https://x.example> and <b>x</b>"),
        "a and https://x.example and x"
    );
    assert_eq!(inline("a [broken(link"), "a [broken(link");
    assert!(note_lines("").is_empty());
    assert!(note_lines("\n\n").is_empty());
}

fn problem(installed: bool, seen: bool) -> superba_core::SelfUpdateProblem {
    superba_core::SelfUpdateProblem {
        version: "0.2.0".into(),
        at: Timestamp::now(),
        installed,
        reason: superba_core::LocalizedText::new(
            "selfupdate.problem.msiexec",
            "The installer (msiexec) ended with code {code}: {meaning}",
        )
        .arg("code", 1603)
        .arg_text(
            "meaning",
            superba_core::LocalizedText::new(
                "selfupdate.msiexec.c1603",
                "a fatal error occurred during the installation",
            ),
        ),
        log: "/home/user/.local/share/superba/logs/selfupdate.log".into(),
        seen,
    }
}

#[test]
fn a_failed_update_stays_on_the_page_with_its_reason_and_log() {
    // User bug 44: the next start says why the last update failed.
    let mut v = view(SelfUpdateStatus::Available, Some(release("0.2.0")));
    v.problem = Some(problem(false, true));
    let mut h = harness(fake(), Locale::ZhCn, v);
    let mut state = State::default();
    let f = page(&mut h, &mut state, "更新到 Superba 0.2.0 失败");
    for want in [
        "安装程序（msiexec）以代码 1603 结束：安装过程中发生严重错误",
        "每一步都记录在日志里：/home/user/.local/share/superba/logs/selfupdate.log",
        "打开日志",
        // The update can be tried again.
        "立即更新",
    ] {
        assert!(f.has(want), "{want}:\n{}", f.all_text());
    }
    h.click_text(&f, "打开日志");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| !f.actions.is_empty(),
    );
    assert!(
        matches!(&f.actions[0], Action::OpenUrl(p) if p.ends_with("logs/selfupdate.log")),
        "{:?}",
        f.actions
    );

    // Installed with a problem: a warning in English.
    let mut v = view(SelfUpdateStatus::UpToDate, Some(release("0.2.0")));
    v.problem = Some(problem(true, false));
    let mut h = harness(fake(), Locale::En, v);
    let f = page(
        &mut h,
        &mut State::default(),
        "Superba 0.2.0 was installed with a problem",
    );
    assert!(f.has("Every step is in the log: "), "{}", f.all_text());
}

#[test]
fn the_start_up_toast_of_a_failed_update_is_shown_once_with_the_log() {
    use superba_gui::app::ToastAction;
    use superba_gui::notices::selfupdate_problem_toast;
    let tr = Tr::new(Locale::ZhCn);
    let render = |t: &superba_core::LocalizedText| tr.text(t);
    let mut v = view(SelfUpdateStatus::UpToDate, None);
    assert!(selfupdate_problem_toast(&tr, &v, render).is_none());
    v.problem = Some(problem(false, false));
    let toast = selfupdate_problem_toast(&tr, &v, render).expect("a toast");
    assert_eq!(toast.status, Status::Error);
    assert_eq!(toast.title, "更新到 Superba 0.2.0 失败");
    let body = toast.body.clone().unwrap();
    assert!(body.contains("以代码 1603 结束"), "{body}");
    assert!(
        body.contains("/home/user/.local/share/superba/logs/selfupdate.log"),
        "{body}"
    );
    assert_eq!(toast.ttl, None, "stays until closed");
    assert!(matches!(
        &toast.actions[..],
        [(label, ToastAction::OpenPath(p))] if label == "打开日志" && p.ends_with("selfupdate.log")
    ));
    // Installed, a later step failed: a warning.
    v.problem = Some(problem(true, false));
    let toast = selfupdate_problem_toast(&tr, &v, render).unwrap();
    assert_eq!(toast.status, Status::Warn);
    assert_eq!(toast.title, "Superba 0.2.0 已安装，但有问题");
    // Shown once: an acknowledged problem has no toast.
    v.problem = Some(problem(false, true));
    assert!(selfupdate_problem_toast(&tr, &v, render).is_none());
}
