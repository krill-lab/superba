#![allow(missing_docs)]
//! Real adapter wiring: snapshot races, settings, queue cancellation and badges.
mod router_harness;
mod support;
use router_harness::Harness;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use superba_core::{Error, Locale, ToolId as CoreId, ToolOp, ToolSupport, ToolUpdater};
use superba_gui::{
    app::shell_badges,
    i18n::Tr,
    pages::superba::updates::{
        Batch, State, UpdateItem, UpdateStatus,
        items::{self, Operation},
        tools::{ToolId, ToolsView},
    },
    pages::{Cx, Data, NavBadge, Page, SuperbaSection},
};
use support::fake::{FakeOptions, FakeService};
fn setup() -> (Arc<FakeService>, Harness, State) {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let h = Harness::new([1280.0, 1400.0], Locale::En, service.clone(), false);
    (service, h, State::default())
}
fn snapshot(h: &mut Harness, state: &State) -> ToolsView {
    let mut snapshot = ToolsView::default();
    h.frame(|_, cx, _| snapshot = state.tools.view(cx));
    snapshot
}
fn wait(h: &mut Harness, state: &State, done: impl Fn(&ToolsView) -> bool) -> ToolsView {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = snapshot(h, state);
        if done(&view) {
            return view;
        }
        assert!(Instant::now() < deadline, "{view:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn loaded(h: &mut Harness, state: &State) -> ToolsView {
    wait(h, state, |v| {
        !v.checking && v.rows.iter().any(|r| r.entry.current.is_some())
    })
}
fn row(view: &ToolsView, id: ToolId) -> &superba_gui::pages::superba::updates::tools::ToolRow {
    view.rows
        .iter()
        .find(|r| r.entry.id == UpdateItem::Tool(id))
        .unwrap()
}
fn with_cx(h: &mut Harness, f: impl FnMut(&Cx<'_>)) {
    let mut f = f;
    h.frame(|_, cx, _| f(cx));
}
#[test]
fn default_source_reads_service_versions_checks_online_and_keeps_rows_on_failure() {
    let (service, mut h, state) = setup();
    let view = loaded(&mut h, &state);
    assert_eq!(
        row(&view, ToolId::ClaudeCode).entry.current.as_deref(),
        Some("2.1.292")
    );
    assert_eq!(service.tool_reads(), vec![false]);
    service.fail_tool_reads(Some(Error::other("inventory unavailable")));
    state.tools.check();
    let view = wait(&mut h, &state, |v| !v.checking && v.error.is_some());
    assert!(service.tool_reads().contains(&true));
    assert_eq!(
        row(&view, ToolId::ClaudeCode).entry.current.as_deref(),
        Some("2.1.292")
    );
    assert!(view.error.unwrap().contains("inventory unavailable"));
    service.fail_tool_reads(None);
    state.tools.check();
    let view = wait(&mut h, &state, |v| !v.checking && v.error.is_none());
    assert_eq!(view.rows.len(), 7);
}
#[test]
fn policies_unknown_versions_and_unavailable_platforms_are_not_fabricated_updates() {
    let (service, mut h, state) = setup();
    let mut view = support::fake::tools::sample_view();
    view.rows[0].updater = ToolUpdater::Blocked {
        by: "DISABLE_UPDATES".into(),
    };
    view.rows[1].support = ToolSupport::Unavailable;
    view.rows[2].latest = None;
    view.rows[2].latest_error = Some("offline".into());
    let mut vendor = view.rows[0].installs.clone();
    vendor[0].version = Some("5.0.0".into());
    view.rows[5].installs = vendor;
    view.rows[5].support = ToolSupport::InstallOnly;
    service.set_tools(view);
    let view = loaded(&mut h, &state);
    assert_eq!(view.unavailable, 1);
    assert_eq!(view.rows.len(), 6);
    assert_eq!(
        row(&view, ToolId::ClaudeCode).entry.status,
        UpdateStatus::Blocked
    );
    assert!(row(&view, ToolId::ClaudeCode).entry.operation.is_none());
    assert_eq!(
        row(&view, ToolId::Opencode).entry.status,
        UpdateStatus::Unknown
    );
    assert_eq!(
        row(&view, ToolId::Workbuddy).entry.status,
        UpdateStatus::Vendor
    );
    assert!(!row(&view, ToolId::Workbuddy).entry.batchable());
    assert!(matches!(
        row(&view, ToolId::Zcode).entry.operation,
        Some(Operation::DownloadPage(_))
    ));
}
#[test]
fn failed_install_and_available_tool_are_counted_once_by_the_sidebar() {
    let (service, mut h, state) = setup();
    loaded(&mut h, &state);
    service.fail_tools(Error::invalid("checksum failed"));
    state.tools.start(ToolId::Pi).unwrap();
    let view = wait(&mut h, &state, |v| {
        row(v, ToolId::Pi).entry.status == UpdateStatus::Failed
    });
    let mut data = Data::default();
    with_cx(&mut h, |cx| {
        let isolated = Cx {
            data: &Data::default(),
            ..*cx
        };
        data.update_inventory_count = Some(
            items::inventory(&isolated, &state, &view)
                .iter()
                .filter(|e| e.status.actionable())
                .count() as u32,
        );
    });
    assert_eq!(data.update_count(), 2);
    assert!(
        shell_badges(&Tr::new(Locale::En), &data)
            .iter()
            .any(|(p, b, _)| *p == Page::Superba(SuperbaSection::Home) && *b == NavBadge::Count(2))
    );
    service.clear_tool_failure();
    state.tools.start(ToolId::Pi).unwrap();
    let view = wait(&mut h, &state, |v| {
        !v.checking && row(v, ToolId::Pi).entry.status == UpdateStatus::Current
    });
    assert_eq!(
        view.rows
            .iter()
            .filter(|r| r.entry.status.actionable())
            .count(),
        1
    );
}
#[test]
fn settings_and_exact_version_install_reach_the_real_service_contract() {
    let (service, mut h, state) = setup();
    loaded(&mut h, &state);
    state
        .tools
        .channel(ToolId::ClaudeCode, "stable".into())
        .unwrap();
    wait(&mut h, &state, |v| {
        !v.checking && row(v, ToolId::ClaudeCode).channel == "stable"
    });
    state.tools.managed(ToolId::ClaudeCode, true).unwrap();
    wait(&mut h, &state, |v| {
        !v.checking && row(v, ToolId::ClaudeCode).managed == Some(true)
    });
    assert_eq!(service.tool_changes().len(), 2);
    state
        .tools
        .install_version(ToolId::ClaudeCode, "2.1.100".into())
        .unwrap();
    let view = wait(&mut h, &state, |v| {
        !v.checking && row(v, ToolId::ClaudeCode).entry.current.as_deref() == Some("2.1.100")
    });
    let request = &service.tool_runs()[0];
    assert_eq!(request.op, ToolOp::Install);
    assert_eq!(request.channel.as_deref(), Some("stable"));
    assert_eq!(request.version.as_deref(), Some("2.1.100"));
    assert!(request.confirmed);
    assert!(row(&view, ToolId::ClaudeCode).log.is_some());
}
#[test]
fn stale_reads_cannot_overwrite_a_completed_settings_change() {
    let (service, mut h, state) = setup();
    loaded(&mut h, &state);
    service.delay_tool_reads(Duration::from_millis(180));
    state.tools.check();
    snapshot(&mut h, &state);
    let deadline = Instant::now() + Duration::from_secs(5);
    while service.tool_reads().len() < 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    state
        .tools
        .channel(ToolId::ClaudeCode, "stable".into())
        .unwrap();
    wait(&mut h, &state, |v| {
        row(v, ToolId::ClaudeCode).channel == "stable"
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let view = snapshot(&mut h, &state);
        assert_eq!(row(&view, ToolId::ClaudeCode).channel, "stable");
        if !view.checking {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn cancellation_keeps_the_queue_waiting_until_the_service_acknowledges_it() {
    let (service, mut h, mut state) = setup();
    let mut raw = support::fake::tools::sample_view();
    raw.rows[3].installs = raw.rows[0].installs.clone();
    raw.rows[3].update_available = true;
    raw.rows[3].actions.update = true;
    service.set_tools(raw);
    service.hold_tool_runs(true);
    let view = loaded(&mut h, &state);
    state.queue.start(Batch::from_entries(
        &view
            .rows
            .iter()
            .map(|r| r.entry.clone())
            .collect::<Vec<_>>(),
    ));
    with_cx(&mut h, |cx| state.advance_queue(cx, &mut vec![]));
    wait(&mut h, &state, |v| {
        row(v, ToolId::ClaudeCode)
            .entry
            .progress
            .as_ref()
            .is_some_and(|p| p.fraction == Some(0.5))
    });
    state.tools.cancel(ToolId::ClaudeCode);
    with_cx(&mut h, |cx| state.advance_queue(cx, &mut vec![]));
    assert_eq!(service.tool_runs().len(), 1);
    let until = Instant::now() + Duration::from_secs(5);
    while service.tool_runs().len() < 2 {
        with_cx(&mut h, |cx| state.advance_queue(cx, &mut vec![]));
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(service.tool_runs()[1].tool, CoreId::Pi);
    service.hold_tool_runs(false);
    wait(&mut h, &state, |v| {
        row(v, ToolId::Pi).entry.status != UpdateStatus::Installing
    });
    with_cx(&mut h, |cx| state.advance_queue(cx, &mut vec![]));
    assert!(!state.queue.busy());
    assert_eq!(state.queue.completed, 2);
}
#[test]
fn a_failed_removal_never_becomes_a_batch_update_and_must_be_confirmed_again() {
    let (service, mut h, state) = setup();
    loaded(&mut h, &state);
    service.fail_tools(Error::other("uninstaller failed"));
    state.tools.remove(ToolId::Opencode, false).unwrap();
    let view = wait(&mut h, &state, |v| {
        row(v, ToolId::Opencode).entry.status == UpdateStatus::Failed
    });
    let entry = &row(&view, ToolId::Opencode).entry;
    assert_eq!(
        entry.operation,
        Some(Operation::ToolRemove {
            id: ToolId::Opencode,
            remove_data: false
        })
    );
    assert!(!entry.batchable());
}

#[test]
fn supported_package_manager_updates_are_counted_but_install_beside_is_not_update_all() {
    let (service, mut h, state) = setup();
    let mut raw = support::fake::tools::sample_view();
    raw.rows[0].updater = ToolUpdater::PackageManager;
    raw.rows[2].updater = ToolUpdater::PackageManager;
    raw.rows[2].actions.install = true;
    service.set_tools(raw);
    let view = loaded(&mut h, &state);
    let update = &row(&view, ToolId::ClaudeCode).entry;
    assert_eq!(update.status, UpdateStatus::Available);
    assert!(update.batchable());
    let install = &row(&view, ToolId::Opencode).entry;
    assert_eq!(install.status, UpdateStatus::Vendor);
    assert_eq!(
        install.operation,
        Some(Operation::ToolInstall(ToolId::Opencode))
    );
    assert!(!install.batchable());
    assert!(row(&view, ToolId::Opencode).versions.is_empty());
}

#[test]
fn retry_keeps_the_failed_explicit_version_and_install_method() {
    let (service, mut h, state) = setup();
    loaded(&mut h, &state);
    service.fail_tools(Error::other("download interrupted"));
    state
        .tools
        .install_version(ToolId::ClaudeCode, "2.1.100".into())
        .unwrap();
    let view = wait(&mut h, &state, |v| {
        row(v, ToolId::ClaudeCode).entry.status == UpdateStatus::Failed
    });
    assert_eq!(
        row(&view, ToolId::ClaudeCode).entry.latest.as_deref(),
        Some("2.1.100")
    );
    service.clear_tool_failure();
    state.tools.start(ToolId::ClaudeCode).unwrap();
    wait(&mut h, &state, |v| {
        !v.checking && row(v, ToolId::ClaudeCode).entry.current.as_deref() == Some("2.1.100")
    });
    let runs = service.tool_runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0], runs[1]);
    assert_eq!(runs[1].op, ToolOp::Install);
}
