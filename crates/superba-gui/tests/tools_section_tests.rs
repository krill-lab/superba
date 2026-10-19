#![allow(missing_docs)]
//! The Updates page's coding-agent tools section: every row in both
//! languages, the buttons each row's actions allow, the confirmation before
//! an operation with cautions and before every removal, operations that start
//! at once and report their result, the channel picker and the "Superba
//! manages updates" switch, download links where Superba does not install.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{Error, Locale, ToolId, ToolOp, ToolUpdater};
use superba_gui::pages::Action;
use superba_gui::pages::updates::tools::{self, State};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn harness(service: Arc<FakeService>, locale: Locale) -> Harness {
    Harness::new([1280.0, 2400.0], locale, service, false)
}

fn show(h: &mut Harness, state: &mut State, want: &str) -> router_harness::Frame {
    h.until(
        |ui, cx, out| tools::section(ui, cx, state, out),
        |f| f.has(want),
    )
}

#[test]
fn every_tool_is_listed_with_its_versions_and_buttons_in_both_languages() {
    for (locale, wants) in [
        (
            Locale::ZhCn,
            vec![
                "Claude Code",
                "2.1.292",
                "有更新",
                "更新到 2.1.302",
                "卸载",
                "安装 0.2.0-rc.2",
                "安装 1.3.0",
                "已是最新",
                "未安装",
                "下载页面",
                "原生安装器 · 由 Superba 安装",
                "由 Superba 管理更新",
                "最新 2.1.302",
            ],
        ),
        (
            Locale::En,
            vec![
                "Update to 2.1.302",
                "Uninstall",
                "Install 1.3.0",
                "Up to date",
                "Download page",
                "Superba manages updates",
                "Native installer · installed by Superba",
            ],
        ),
    ] {
        let mut h = harness(fake(), locale);
        let mut state = State::default();
        let f = show(&mut h, &mut state, "2.1.292");
        for want in wants {
            assert!(f.has(want), "{locale:?} {want}:\n{}", f.all_text());
        }
        for id in ToolId::ALL {
            assert!(f.has(id.display_name()), "{id}");
        }
        // The tool's own updater is shown once it is installed.
        assert!(!f.has("强制自行更新") && !f.has("Forces its own updates"));
    }
}

#[test]
fn an_update_with_a_caution_is_confirmed_first_then_runs_at_once() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "Update to 2.1.302");
    h.click_text(&f, "Update to 2.1.302");
    let f = h.until(
        |ui, cx, out| tools::section(ui, cx, &mut state, out),
        |f| f.has("Update Claude Code?"),
    );
    assert!(f.has("npm uninstall -g"), "{}", f.all_text());
    assert!(
        service.tool_runs().is_empty(),
        "nothing runs before the confirmation"
    );
    h.click_text(&f, "Cancel");
    let f = h.frames(4, |ui, cx, out| tools::section(ui, cx, &mut state, out));
    assert!(state.confirm.is_none() && !f.has("Update Claude Code?"));
    assert!(service.tool_runs().is_empty());

    let f = show(&mut h, &mut state, "Update to 2.1.302");
    h.click_text(&f, "Update to 2.1.302");
    let f = h.until(
        |ui, cx, out| tools::section(ui, cx, &mut state, out),
        |f| f.has("Update Claude Code?"),
    );
    // The dialog's own button (the row's is behind the backdrop).
    let ok = f
        .texts
        .iter()
        .rfind(|(t, _)| t == "Update")
        .map(|(_, r)| r.center())
        .expect("dialog button");
    h.click_at(ok);
    let f = h.until(
        |ui, cx, out| tools::section(ui, cx, &mut state, out),
        |f| f.has("Updated Claude Code to 9.9.9"),
    );
    let runs = service.tool_runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].tool, runs[0].op, runs[0].confirmed),
        (ToolId::ClaudeCode, ToolOp::Update, true)
    );
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::Toast { text, .. } if text.contains("9.9.9"))),
        "{:?}",
        f.actions
    );
}

#[test]
fn an_install_without_cautions_starts_on_the_click() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "安装 1.3.0");
    h.click_text(&f, "安装 1.3.0");
    show(&mut h, &mut state, "已安装 pi 9.9.9");
    let runs = service.tool_runs();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].tool, runs[0].op), (ToolId::Pi, ToolOp::Install));
    assert!(!runs[0].remove_data);
}

#[test]
fn a_removal_is_always_confirmed_and_keeps_data_unless_asked() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "2.1.292");
    // opencode's Uninstall: the second "Uninstall" on the page (Claude Code first).
    let second = f
        .texts
        .iter()
        .filter(|(t, _)| t == "Uninstall")
        .nth(1)
        .map(|(_, r)| r.center())
        .unwrap();
    h.click_at(second);
    let f = h.until(
        |ui, cx, out| tools::section(ui, cx, &mut state, out),
        |f| f.has("Uninstall opencode?"),
    );
    assert!(f.has("Only the program is removed"), "{}", f.all_text());
    assert!(f.has("Also remove its settings and data"));
    assert!(service.tool_runs().is_empty());
    let ok = f
        .texts
        .iter()
        .rfind(|(t, _)| t == "Uninstall")
        .map(|(_, r)| r.center())
        .unwrap();
    h.click_at(ok);
    show(&mut h, &mut state, "Removed opencode.");
    let runs = service.tool_runs();
    assert_eq!(
        (runs[0].tool, runs[0].op),
        (ToolId::Opencode, ToolOp::Uninstall)
    );
    assert!(!runs[0].remove_data, "data stays unless the box is ticked");
}

#[test]
fn removing_global_npm_claude_requires_a_separate_gui_confirmation() {
    for (locale, kind, title, cancel, uninstall, done) in [
        (
            Locale::En,
            "npm (global)",
            "Uninstall Claude Code?",
            "Cancel",
            "Uninstall",
            "Removed Claude Code.",
        ),
        (
            Locale::ZhCn,
            "npm（全局安装）",
            "卸载 Claude Code？",
            "取消",
            "卸载",
            "已卸载 Claude Code",
        ),
    ] {
        let service = fake();
        let mut view = support::fake::tools::sample_view();
        let row = &mut view.rows[0];
        row.installs[0].kind = superba_core::ToolInstallKind::NpmGlobal;
        row.installs[0].by_superba = false;
        row.installs[0].path = "/usr/bin/claude".into();
        row.uninstall_cautions = vec![superba_core::LocalizedText::new(
            "tools.claude.caution_npm_uninstall",
            "This removes the global npm Claude Code install in {path} with `npm uninstall -g @anthropic-ai/claude-code`.",
        ).arg("path", "/usr/lib/node_modules/@anthropic-ai/claude-code")];
        service.set_tools(view);
        let mut h = harness(service.clone(), locale);
        let mut state = State::default();
        let f = show(&mut h, &mut state, kind);
        h.click_text(&f, uninstall);
        let f = show(&mut h, &mut state, title);
        assert!(f.has("/usr/lib/node_modules/@anthropic-ai/claude-code"));
        assert!(f.has("npm uninstall -g"));
        assert!(service.tool_runs().is_empty());
        h.click_text(&f, cancel);
        h.frames(4, |ui, cx, out| tools::section(ui, cx, &mut state, out));
        assert!(state.confirm.is_none());
        assert!(service.tool_runs().is_empty());
        let f = show(&mut h, &mut state, kind);
        h.click_text(&f, uninstall);
        let f = show(&mut h, &mut state, title);
        let ok = f
            .texts
            .iter()
            .rfind(|(t, _)| t == uninstall)
            .unwrap()
            .1
            .center();
        h.click_at(ok);
        show(&mut h, &mut state, done);
        let runs = service.tool_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            (runs[0].tool, runs[0].op, runs[0].confirmed),
            (ToolId::ClaudeCode, ToolOp::Uninstall, true)
        );
        assert!(!runs[0].remove_data);
    }
}

#[test]
fn a_failed_operation_shows_its_error_under_the_row() {
    let service = fake();
    service.fail_tools(Error::Invalid {
        message: "boom: checksum".into(),
    });
    let mut h = harness(service, Locale::En);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "Install 1.3.0");
    h.click_text(&f, "Install 1.3.0");
    show(&mut h, &mut state, "boom: checksum");
}

#[test]
fn the_switch_and_the_channel_go_to_the_service_and_links_open() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "Superba manages updates");
    // The toggle sits left of its label.
    let label = f.find("Superba manages updates").unwrap();
    h.click_at(egui::pos2(label.left() - 20.0, label.center().y));
    h.until(
        |ui, cx, out| tools::section(ui, cx, &mut state, out),
        |_| !service.tool_changes().is_empty(),
    );
    let (tool, change) = service.tool_changes()[0].clone();
    assert_eq!(
        (tool, change.manage_updates),
        (ToolId::ClaudeCode, Some(true))
    );
    let f = h.until(
        |ui, cx, out| tools::section(ui, cx, &mut state, out),
        |f| f.has("Updates managed by Superba"),
    );
    h.click_text(&f, "Download page");
    let f = h.frames(3, |ui, cx, out| tools::section(ui, cx, &mut state, out));
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::OpenUrl(u) if u == "https://zcode.z.ai")),
        "{:?}",
        f.actions
    );
    let row = state
        .view
        .value
        .as_ref()
        .unwrap()
        .row(ToolId::ClaudeCode)
        .unwrap();
    assert_eq!(row.updater, ToolUpdater::Superba);
}
