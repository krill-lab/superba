#![allow(missing_docs)]
//! M3 inventory, serial operations, deep links, accessible names and narrow panels.
mod router_harness;
mod support;
use egui::accesskit::{Node, Role};
use std::sync::Arc;
use std::time::Instant;
use superba_core::{AppUpdateAction, Error, Locale, SelfUpdateStatus, SelfUpdateView, TargetId};
use superba_gui::pages::superba::updates::{
    self, Batch, Queue, State, UpdateItem, UpdateStatus,
    items::{self, Entry, Operation},
    rows::Filter,
    tools::{Source, ToolId, ToolsSource},
};
use superba_gui::{
    app::PageStates,
    design::{Palette, fonts, style},
    i18n::Tr,
    pages::{Action, Cx, Data, Page, SuperbaSection},
};
use support::{
    fake::{FakeOptions, FakeService},
    update_tools::DemoTools,
};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run: false,
        ..Default::default()
    }))
}
fn prepared() -> (Arc<FakeService>, Data, State, Arc<DemoTools>) {
    let service = fake();
    let mut data = router_harness::data_from(&service);
    data.versions.finish(Ok(service.versions_view()));
    data.selfupdate.finish(Ok(SelfUpdateView {
        current: "0.1.1-alpha.7".into(),
        status: SelfUpdateStatus::CheckFailed,
        detail: Some("Version server unavailable".into()),
        ..Default::default()
    }));
    let mut state = State::default();
    state.view.value = Some(service.versions_view());
    state.view.at = Some(Instant::now());
    let tools = Arc::new(DemoTools::default());
    state.tools = Source(tools.clone());
    (service, data, state, tools)
}
fn with_cx<R>(
    data: &Data,
    service: &Arc<FakeService>,
    locale: Locale,
    f: impl FnOnce(&Cx<'_>) -> R,
) -> R {
    let tr = Tr::new(locale);
    let font = fonts::FontReport::default();
    let ctx = egui::Context::default();
    let service: Arc<dyn superba_core::SwitchService> = service.clone();
    f(&Cx {
        tr: &tr,
        pal: Palette::of(&ctx),
        data,
        busy: false,
        now: superba_core::Timestamp::now(),
        fonts: &font,
        service: &service,
        log: &[],
    })
}
#[test]
fn every_row_identity_round_trips_including_punctuated_remote_ids() {
    let mut ids = vec![
        UpdateItem::Superba,
        UpdateItem::App,
        UpdateItem::AppPin,
        UpdateItem::CodexCli,
        UpdateItem::CodexRemote(TargetId::new("ssh:user@devbox:22")),
        UpdateItem::CodexRemote(TargetId::new("wsl:Ubuntu")),
    ];
    ids.extend(ToolId::ALL.into_iter().map(UpdateItem::Tool));
    for id in ids {
        assert_eq!(UpdateItem::parse(&id.key()), Some(id));
    }
    assert_eq!(
        UpdateItem::parse("ssh:devbox"),
        Some(UpdateItem::CodexRemote(TargetId::new("ssh:devbox")))
    );
    assert!(UpdateItem::parse("tool:unknown").is_none());
}
#[test]
fn one_inventory_drives_status_filter_count_and_batch_eligibility() {
    let (service, data, mut state, tools) = prepared();
    state.view.value.as_mut().unwrap().app.action =
        Some(AppUpdateAction::MacSparkle { package: None });
    with_cx(&data, &service, Locale::En, |cx| {
        let entries = items::inventory(cx, &state, &tools.view(cx));
        assert_eq!(entries.len(), 13);
        assert_eq!(
            entries
                .iter()
                .find(|e| e.id == UpdateItem::App)
                .unwrap()
                .status,
            UpdateStatus::Vendor
        );
        let statuses: Vec<_> = entries.iter().map(|e| e.status).collect();
        for status in [
            UpdateStatus::Current,
            UpdateStatus::Available,
            UpdateStatus::Missing,
            UpdateStatus::Installing,
            UpdateStatus::Failed,
            UpdateStatus::Vendor,
            UpdateStatus::Blocked,
            UpdateStatus::Disconnected,
            UpdateStatus::Unknown,
        ] {
            assert!(statuses.contains(&status), "{status:?}");
        }
        let batch = Batch::from_entries(&entries);
        assert!(batch.jobs.iter().all(|e| e.status.actionable()));
        assert!(
            batch
                .jobs
                .iter()
                .all(|e| !matches!(e.operation, Some(Operation::Check | Operation::Cancel)))
        );
        assert!(
            batch
                .jobs
                .iter()
                .any(|e| e.id == UpdateItem::Tool(ToolId::Zcode))
        );
        assert!(!batch.jobs.iter().any(|e| e.id == UpdateItem::App));
        assert!(
            !batch
                .jobs
                .iter()
                .any(|e| e.id == UpdateItem::Tool(ToolId::WorkbuddyAi))
        );
    });
}

#[test]
fn app_without_an_update_action_describes_only_known_state() {
    for locale in [Locale::En, Locale::ZhCn] {
        for action in [None, Some(AppUpdateAction::None)] {
            for (installed, latest, status, key) in [
                (
                    false,
                    false,
                    UpdateStatus::Missing,
                    "updates.app.not_installed",
                ),
                (
                    false,
                    true,
                    UpdateStatus::Missing,
                    "updates.app.not_installed",
                ),
                (
                    true,
                    false,
                    UpdateStatus::Unknown,
                    "updates.app.latest_unknown",
                ),
                (true, true, UpdateStatus::Current, "updates.app.up_to_date"),
            ] {
                let (service, data, mut state, tools) = prepared();
                let app = &mut state.view.value.as_mut().unwrap().app;
                app.action = action.clone();
                app.update_available = false;
                if !installed {
                    app.install = None;
                }
                if !latest {
                    app.latest = None;
                }
                with_cx(&data, &service, locale, |cx| {
                    let row = items::inventory(cx, &state, &tools.view(cx))
                        .into_iter()
                        .find(|e| e.id == UpdateItem::App)
                        .unwrap();
                    assert_eq!(row.status, status);
                    assert_eq!(row.description, cx.tr.t(key));
                    assert_eq!(
                        row.operation,
                        (status == UpdateStatus::Unknown).then_some(Operation::Check)
                    );
                });
            }
        }
    }
}

#[test]
fn app_descriptions_follow_known_platform_actions_and_missing_installs() {
    let linux = fake().versions_view().app.action.unwrap();
    let actions = [
        (linux, "updates.app.linux_note", UpdateStatus::Available),
        (
            AppUpdateAction::MacSparkle { package: None },
            "updates.app.mac_note",
            UpdateStatus::Vendor,
        ),
        (
            AppUpdateAction::WindowsStore {
                store_url: "ms-windows-store://pdp/?ProductId=example".into(),
                command: "winget upgrade --id example --source msstore".into(),
            },
            "updates.list.app_vendor",
            UpdateStatus::Vendor,
        ),
        (
            AppUpdateAction::WindowsPackage {
                package: None,
                source: Default::default(),
                store_url: None,
                command: None,
                native_arch: Some("x64".into()),
                installed_arch: Some("x64".into()),
            },
            "updates.list.app_vendor",
            UpdateStatus::Vendor,
        ),
    ];
    for locale in [Locale::En, Locale::ZhCn] {
        for (action, key, status) in &actions {
            for installed in [true, false] {
                let (service, data, mut state, tools) = prepared();
                let app = &mut state.view.value.as_mut().unwrap().app;
                app.action = Some(action.clone());
                if !installed {
                    app.install = None;
                }
                with_cx(&data, &service, locale, |cx| {
                    let row = items::inventory(cx, &state, &tools.view(cx))
                        .into_iter()
                        .find(|e| e.id == UpdateItem::App)
                        .unwrap();
                    assert_eq!(
                        row.status,
                        if installed {
                            *status
                        } else {
                            UpdateStatus::Missing
                        }
                    );
                    assert_eq!(
                        row.description,
                        cx.tr.t(if installed {
                            key
                        } else {
                            "updates.app.not_installed"
                        })
                    );
                    assert_eq!(
                        row.operation,
                        matches!(action, AppUpdateAction::LinuxPackage { .. })
                            .then_some(Operation::AppDownload)
                    );
                    assert!(!row.description.contains("was checked"));
                    assert!(!row.description.contains("已按发布的校验值核对"));
                });
            }
        }
    }
}

#[test]
fn app_verification_message_requires_a_completed_download() {
    for locale in [Locale::En, Locale::ZhCn] {
        let (service, data, mut state, tools) = prepared();
        let Some(AppUpdateAction::LinuxPackage { package, command }) =
            state.view.value.as_ref().unwrap().app.action.clone()
        else {
            panic!("expected Linux fixture");
        };
        state.download_error = Some("checksum mismatch".into());
        with_cx(&data, &service, locale, |cx| {
            let row = items::inventory(cx, &state, &tools.view(cx))
                .into_iter()
                .find(|e| e.id == UpdateItem::App)
                .unwrap();
            assert_eq!(row.status, UpdateStatus::Failed);
            assert_eq!(row.description, "checksum mismatch");
            assert_eq!(row.operation, Some(Operation::AppDownload));
        });
        state.download_error = None;
        state.downloaded = Some(superba_core::AppDownloadReport {
            package,
            path: "chatgpt.deb".into(),
            sha256: "a".repeat(64),
            command: Some(command),
            windows: None,
        });
        with_cx(&data, &service, locale, |cx| {
            let row = items::inventory(cx, &state, &tools.view(cx))
                .into_iter()
                .find(|e| e.id == UpdateItem::App)
                .unwrap();
            assert_eq!(row.description, cx.tr.t("updates.list.package_ready"));
            assert_eq!(
                row.operation,
                Some(Operation::OpenInstaller("chatgpt.deb".into()))
            );
        });
    }
}

#[test]
fn a_remote_error_keeps_the_last_version_without_offering_an_install() {
    let (service, data, mut state, tools) = prepared();
    let row = &mut state.view.value.as_mut().unwrap().remote[0];
    row.error = Some(Error::other("Host offline"));
    let id = UpdateItem::CodexRemote(row.target_id.clone());
    let version = row.version.as_ref().unwrap().to_string();
    with_cx(&data, &service, Locale::En, |cx| {
        let row = items::inventory(cx, &state, &tools.view(cx))
            .into_iter()
            .find(|e| e.id == id)
            .unwrap();
        assert_eq!(row.status, UpdateStatus::Disconnected);
        assert_eq!(row.current.as_deref(), Some(version.as_str()));
        assert_eq!(row.versions(), version);
        assert!(row.operation.is_none());
    });
}
#[test]
fn skipped_self_update_and_failure_are_distinct() {
    let (service, mut data, state, tools) = prepared();
    let view = SelfUpdateView {
        current: "0.1.0".into(),
        status: SelfUpdateStatus::Available,
        skipped: Some("0.1.1".into()),
        latest: Some(superba_core::SelfRelease {
            tag: "v0.1.1".into(),
            version: "0.1.1".into(),
            prerelease: false,
            notes: String::new(),
            published_at: None,
            url: "https://example.com/release".into(),
            asset: Some("package.tar.gz".into()),
            asset_size: None,
        }),
        ..Default::default()
    };
    data.selfupdate.finish(Ok(view));
    with_cx(&data, &service, Locale::En, |cx| {
        let row = items::inventory(cx, &state, &tools.view(cx)).remove(0);
        assert_eq!(row.status, UpdateStatus::Skipped);
        assert_eq!(row.operation, Some(Operation::Unskip));
        assert!(!row.batchable());
    });
}

fn job(id: UpdateItem) -> Entry {
    let op = match id {
        UpdateItem::Superba => Operation::SelfUpdate,
        UpdateItem::Tool(id) => Operation::Tool(id),
        _ => Operation::AppDownload,
    };
    let mut e = Entry::new(id, "demo", UpdateStatus::Available);
    e.operation = Some(op);
    e.cautions = vec!["Finish running tasks".into()];
    e
}
#[test]
fn batch_is_finite_serial_and_self_update_is_last() {
    let jobs = vec![
        job(UpdateItem::Superba),
        job(UpdateItem::Tool(ToolId::ClaudeCode)),
        job(UpdateItem::Tool(ToolId::Zcode)),
    ];
    let batch = Batch::from_entries(&jobs);
    assert_eq!(batch.jobs.last().unwrap().id, UpdateItem::Superba);
    assert_eq!(batch.cautions.len(), 1);
    let mut queue = Queue::default();
    queue.start(batch);
    assert_eq!(
        queue.next(false, false).unwrap().id,
        UpdateItem::Tool(ToolId::ClaudeCode)
    );
    assert!(queue.next(true, false).is_none());
    assert_eq!(
        queue.next(false, false).unwrap().id,
        UpdateItem::Tool(ToolId::Zcode)
    );
    // Failure is still a completed attempt, never an implicit retry.
    assert_eq!(queue.next(false, false).unwrap().id, UpdateItem::Superba);
    assert!(queue.next(false, false).is_none());
    assert!(!queue.busy());
    assert_eq!(queue.completed, 3);
}
#[test]
fn cancellation_waits_for_the_backend_then_continues_without_overlap() {
    let (service, data, mut state, tools) = prepared();
    tools.idle();
    state.queue.start(Batch::from_entries(&[
        job(UpdateItem::Tool(ToolId::ClaudeCode)),
        job(UpdateItem::Tool(ToolId::Zcode)),
    ]));
    with_cx(&data, &service, Locale::En, |cx| {
        state.advance_queue(cx, &mut vec![]);
        updates::command(&mut state, cx, "update_cli", &mut vec![]);
        assert!(!state.installing(&superba_core::InstallTarget::Local));
        state.perform(
            cx,
            &UpdateItem::Tool(ToolId::ClaudeCode),
            &Operation::Cancel,
            &mut vec![],
        );
        state.advance_queue(cx, &mut vec![]);
        assert_eq!(*tools.started.lock().unwrap(), vec![ToolId::ClaudeCode]);
        assert_eq!(*tools.cancelled.lock().unwrap(), vec![ToolId::ClaudeCode]);
        tools.set(ToolId::ClaudeCode, UpdateStatus::Failed);
        state.advance_queue(cx, &mut vec![]);
        assert_eq!(
            *tools.started.lock().unwrap(),
            vec![ToolId::ClaudeCode, ToolId::Zcode]
        );
        state.cancel_queue();
        state.advance_queue(cx, &mut vec![]);
        assert!(state.queue.busy());
        tools.set(ToolId::Zcode, UpdateStatus::Current);
        state.advance_queue(cx, &mut vec![]);
        assert!(!state.queue.busy());
    });
}
#[test]
fn explicit_tool_version_and_removal_are_never_part_of_update_all() {
    let (service, data, mut state, tools) = prepared();
    tools.idle();
    let id = ToolId::ClaudeCode;
    let mut version = job(UpdateItem::Tool(id));
    version.operation = Some(Operation::ToolVersion {
        id,
        version: "1.0.0".into(),
    });
    let mut remove = version.clone();
    remove.operation = Some(Operation::ToolRemove {
        id,
        remove_data: false,
    });
    assert!(
        Batch::from_entries(&[version.clone(), remove.clone()])
            .jobs
            .is_empty()
    );
    with_cx(&data, &service, Locale::En, |cx| {
        state.queue.start(Batch {
            jobs: vec![version],
            cautions: vec![],
        });
        state.advance_queue(cx, &mut vec![]);
        assert_eq!(
            *tools.selected_versions.lock().unwrap(),
            vec![(id, "1.0.0".into())]
        );
        tools.set(id, UpdateStatus::Current);
        state.advance_queue(cx, &mut vec![]);
        state.queue.start(Batch {
            jobs: vec![remove],
            cautions: vec![],
        });
        state.advance_queue(cx, &mut vec![]);
        assert_eq!(*tools.removals.lock().unwrap(), vec![(id, false)]);
    });
}
#[test]
fn check_refreshes_all_sources_and_deep_links_only_expand() {
    let (service, data, state, tools) = prepared();
    let mut pages = PageStates {
        updates: state,
        ..Default::default()
    };
    pages.updates.filter = Filter::Available;
    with_cx(&data, &service, Locale::En, |cx| {
        let mut out = vec![];
        pages.command(
            Page::Superba(SuperbaSection::Updates),
            cx,
            "check",
            &mut out,
        );
        assert!(out.iter().any(|a| matches!(a, Action::CheckSelfUpdate)));
        assert_eq!(*tools.checks.lock().unwrap(), 1);
        pages.command(
            Page::Superba(SuperbaSection::Updates),
            cx,
            "focus:tool:workbuddy-ai",
            &mut out,
        );
        assert_eq!(
            pages.updates.focus,
            Some(UpdateItem::Tool(ToolId::WorkbuddyAi))
        );
        assert_eq!(pages.updates.filter, Filter::All);
        assert!(tools.started.lock().unwrap().is_empty());
    });
}
fn render_nodes(
    state: &mut State,
    data: &Data,
    service: &Arc<FakeService>,
    locale: Locale,
    width: f32,
) -> Vec<Node> {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    style::install(&ctx, Some(egui::Theme::Light));
    let font = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&font));
    let tr = Tr::new(locale);
    let service: Arc<dyn superba_core::SwitchService> = service.clone();
    let mut nodes = vec![];
    for _ in 0..3 {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 1600.0),
                )),
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(&ctx),
                    data,
                    busy: false,
                    now: superba_core::Timestamp::now(),
                    fonts: &font,
                    service: &service,
                    log: &[],
                };
                updates::show(ui, &cx, state, &mut vec![]);
            },
        );
        nodes = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .map(|(_, n)| n.clone())
            .collect();
        output.drop_without_applying_deltas();
    }
    nodes
}
#[test]
fn accessible_rows_include_each_tool_status_versions_and_error_in_both_languages() {
    for locale in [Locale::En, Locale::ZhCn] {
        assert!(
            !Tr::new(locale)
                .t("updates.list.current")
                .starts_with("updates.")
        );
        for width in [500.0, 820.0] {
            let (service, data, mut state, tools) = prepared();
            let expected = with_cx(&data, &service, locale, |cx| tools.view(cx));
            let nodes = render_nodes(&mut state, &data, &service, locale, width);
            for tool in expected.rows {
                let row = nodes
                    .iter()
                    .find(|n| {
                        n.role() == Role::Button
                            && n.is_expanded().is_some()
                            && n.label()
                                .is_some_and(|s| s.starts_with(&format!("{},", tool.entry.name)))
                    })
                    .expect("tool expander");
                let name = row.label().unwrap();
                assert!(
                    name.contains(&Tr::new(locale).t(tool.entry.status.key())),
                    "{name}"
                );
                assert!(name.contains(&tool.entry.versions()), "{name}");
                if tool.entry.status == UpdateStatus::Failed {
                    assert!(name.contains(&tool.entry.description));
                }
            }
        }
    }
}
#[test]
fn collapsed_and_expanded_inventory_stays_inside_the_superba_detail() {
    for locale in [Locale::En, Locale::ZhCn] {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0], [1440.0, 900.0]] {
                for expand in [
                    None,
                    Some(UpdateItem::App),
                    Some(UpdateItem::AppPin),
                    Some(UpdateItem::CodexCli),
                    Some(UpdateItem::Tool(ToolId::ClaudeCode)),
                ] {
                    let (_, data, mut state, _) = prepared();
                    if let Some(id) = expand.clone() {
                        state.expanded.insert(id);
                    }
                    let mut pages = PageStates {
                        updates: state,
                        ..Default::default()
                    };
                    let mut settings = superba_core::Settings::default();
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Superba(SuperbaSection::Updates).min_height(),
                        |ui, cx, out| {
                            pages.show(
                                Page::Superba(SuperbaSection::Updates),
                                ui,
                                cx,
                                &mut settings,
                                out,
                            )
                        },
                    );
                    assert!(
                        report.fits() && !report.scrolled,
                        "{window:?} {locale:?} {expand:?}: {}",
                        report.all_text()
                    );
                    assert!(
                        report.beyond_right().is_empty(),
                        "{window:?} {locale:?} {expand:?}, page {:?}: {:?}",
                        report.rect,
                        report
                            .text_shapes
                            .iter()
                            .filter(|(_, r, _)| r.max.x > report.rect.max.x + 0.5)
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}

#[test]
fn removing_a_tool_requires_confirmation_and_preserves_data_by_default() {
    use support::ui::Harness;
    for locale in [Locale::En, Locale::ZhCn] {
        let (service, data, mut state, tools) = prepared();
        tools.idle();
        state.expanded.insert(UpdateItem::Tool(ToolId::ClaudeCode));
        let harness = Harness::new([820.0, 1800.0], egui::Theme::Light);
        let tr = Tr::new(locale);
        let service: Arc<dyn superba_core::SwitchService> = service;
        let mut draw = |ui: &mut egui::Ui| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ui.ctx()),
                data: &data,
                busy: false,
                now: superba_core::Timestamp::now(),
                fonts: &harness.fonts,
                service: &service,
                log: &[],
            };
            updates::show(ui, &cx, &mut state, &mut vec![]);
        };
        assert!(harness.click_text(&tr.t("updates.list.remove"), &mut draw));
        assert!(tools.removals.lock().unwrap().is_empty());
        let texts = harness.settle(&mut draw);
        assert!(
            texts
                .iter()
                .any(|(s, _)| s == &tr.t("updates.list.confirm_remove"))
        );
        assert!(texts.iter().any(|(s, _)| s == "~/.config/claude"));
        assert!(harness.click_last_text(&tr.t("updates.list.remove"), &mut draw));
        assert_eq!(
            *tools.removals.lock().unwrap(),
            vec![(ToolId::ClaudeCode, false)]
        );
    }
}
