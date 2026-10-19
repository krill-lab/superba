#![allow(missing_docs)]
//! Original t1 interactions, exercised through the unified inventory and the
//! production adapter against an isolated service (never the user's tools).
mod router_harness;
mod support;

use router_harness::{Frame, Harness};
use std::sync::Arc;
use superba_core::{Error, Locale, ToolId as CoreId, ToolInstallKind, ToolOp};
use superba_gui::pages::Action;
use superba_gui::pages::superba::updates::{self, State, UpdateItem, tools::ToolId};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}
fn harness(service: Arc<FakeService>, locale: Locale) -> Harness {
    Harness::new([1280.0, 2800.0], locale, service, false)
}
fn show(h: &mut Harness, state: &mut State, want: &str) -> Frame {
    h.until(
        |ui, cx, out| updates::show(ui, cx, state, out),
        |f| f.has(want),
    )
}
fn click_row(h: &mut Harness, f: &Frame, name: &str, action: &str) {
    let y = f
        .texts
        .iter()
        .find(|(s, _)| s == name)
        .expect("tool name")
        .1
        .center()
        .y;
    let target = f
        .texts
        .iter()
        .filter(|(s, _)| s == action)
        .min_by(|a, b| {
            (a.1.center().y - y)
                .abs()
                .total_cmp(&(b.1.center().y - y).abs())
        })
        .expect("row action")
        .1
        .center();
    h.click_at(target);
}
fn click_dialog(h: &mut Harness, f: &Frame, action: &str) {
    h.click_at(
        f.texts
            .iter()
            .rfind(|(s, _)| s == action)
            .expect("dialog action")
            .1
            .center(),
    );
}
#[test]
fn every_tool_is_listed_with_its_versions_and_buttons_in_both_languages() {
    for locale in [Locale::En, Locale::ZhCn] {
        let mut h = harness(fake(), locale);
        let mut state = State::default();
        state.expanded.insert(UpdateItem::Tool(ToolId::ClaudeCode));
        let f = show(&mut h, &mut state, "2.1.292 → 2.1.302");
        assert!(!f.has("Forces its own updates") && !f.has("强制自行更新"));
        for id in CoreId::ALL {
            assert!(f.has(id.display_name()), "{}", f.all_text());
        }
        for want in if locale == Locale::En {
            vec![
                "Update available",
                "Up to date",
                "Not installed",
                "Download page",
                "Native installer",
                "Superba manages updates",
            ]
        } else {
            vec![
                "有新版本",
                "已是最新",
                "未安装",
                "下载页面",
                "原生安装器",
                "由 Superba 管理更新",
            ]
        } {
            assert!(f.has(want), "{want}: {}", f.all_text());
        }
    }
}
#[test]
fn an_update_with_a_caution_is_confirmed_first_then_runs_at_once() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "2.1.292 → 2.1.302");
    click_row(&mut h, &f, "Claude Code", "Update");
    let f = show(&mut h, &mut state, "npm uninstall -g");
    assert!(service.tool_runs().is_empty());
    click_dialog(&mut h, &f, "Cancel");
    h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    assert!(state.confirm.is_none());
    assert!(service.tool_runs().is_empty());
    let f = show(&mut h, &mut state, "2.1.292 → 2.1.302");
    click_row(&mut h, &f, "Claude Code", "Update");
    let f = show(&mut h, &mut state, "npm uninstall -g");
    click_dialog(&mut h, &f, "Start updates");
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| {
            f.actions.iter().any(
                |a| matches!(a, Action::Toast { text, .. } if text.contains("Updated Claude Code")),
            )
        },
    );
    let runs = service.tool_runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (runs[0].tool, runs[0].op, runs[0].confirmed),
        (CoreId::ClaudeCode, ToolOp::Update, true)
    );
    assert_eq!(runs[0].version.as_deref(), Some("2.1.302"));
}
#[test]
fn an_install_without_cautions_starts_on_the_click() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::ZhCn);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "— → 1.3.0");
    click_row(&mut h, &f, "pi", "安装");
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| !service.tool_runs().is_empty(),
    );
    let runs = service.tool_runs();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].tool, runs[0].op), (CoreId::Pi, ToolOp::Install));
    assert!(!runs[0].remove_data);
}
#[test]
fn a_removal_is_always_confirmed_and_keeps_data_unless_asked() {
    for remove_data in [false, true] {
        let service = fake();
        let mut h = harness(service.clone(), Locale::En);
        let mut state = State::default();
        state.expanded.insert(UpdateItem::Tool(ToolId::Opencode));
        let f = show(&mut h, &mut state, "Uninstall…");
        h.click_text(&f, "Uninstall…");
        let f = show(&mut h, &mut state, "Review removal");
        assert!(f.has("Only the program is removed"));
        assert!(f.has("/home/me/.config/opencode"));
        assert!(service.tool_runs().is_empty());
        let f = if remove_data {
            h.click_text(&f, "Also remove these settings and data directories");
            h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out))
        } else {
            f
        };
        click_dialog(&mut h, &f, "Uninstall…");
        h.until(
            |ui, cx, out| updates::show(ui, cx, &mut state, out),
            |_| !service.tool_runs().is_empty(),
        );
        let runs = service.tool_runs();
        assert_eq!(
            (runs[0].tool, runs[0].op),
            (CoreId::Opencode, ToolOp::Uninstall)
        );
        assert_eq!(runs[0].remove_data, remove_data);
    }
}
#[test]
fn removing_global_npm_claude_requires_a_separate_gui_confirmation() {
    for (locale, title, cancel, uninstall) in [
        (Locale::En, "Review removal", "Cancel", "Uninstall…"),
        (Locale::ZhCn, "确认卸载", "取消", "卸载…"),
    ] {
        let service = fake();
        let mut view = support::fake::tools::sample_view();
        let row = &mut view.rows[0];
        row.installs[0].kind = ToolInstallKind::NpmGlobal;
        row.installs[0].by_superba = false;
        row.installs[0].path = "/usr/bin/claude".into();
        row.uninstall_cautions = vec![superba_core::LocalizedText::new(
            "tools.claude.caution_npm_uninstall",
            "This removes the global npm Claude Code install in {path} with `npm uninstall -g @anthropic-ai/claude-code`.",
        ).arg("path", "/usr/lib/node_modules/@anthropic-ai/claude-code")];
        service.set_tools(view);
        let mut h = harness(service.clone(), locale);
        let mut state = State::default();
        state.expanded.insert(UpdateItem::Tool(ToolId::ClaudeCode));
        let f = show(&mut h, &mut state, uninstall);
        h.click_text(&f, uninstall);
        let f = show(&mut h, &mut state, title);
        assert!(f.has("/usr/lib/node_modules/@anthropic-ai/claude-code"));
        assert!(f.has("npm uninstall -g"));
        assert!(service.tool_runs().is_empty());
        click_dialog(&mut h, &f, cancel);
        h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out));
        assert!(state.confirm.is_none());
        assert!(service.tool_runs().is_empty());
        let f = show(&mut h, &mut state, uninstall);
        h.click_text(&f, uninstall);
        let f = show(&mut h, &mut state, title);
        click_dialog(&mut h, &f, uninstall);
        h.until(
            |ui, cx, out| updates::show(ui, cx, &mut state, out),
            |_| !service.tool_runs().is_empty(),
        );
        let runs = service.tool_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            (runs[0].tool, runs[0].op, runs[0].confirmed),
            (CoreId::ClaudeCode, ToolOp::Uninstall, true)
        );
        assert!(!runs[0].remove_data);
    }
}
#[test]
fn a_failed_operation_shows_its_error_under_the_row() {
    let service = fake();
    service.fail_tools(Error::invalid("boom: checksum"));
    let mut h = harness(service, Locale::En);
    let mut state = State::default();
    let f = show(&mut h, &mut state, "— → 1.3.0");
    click_row(&mut h, &f, "pi", "Install");
    show(&mut h, &mut state, "boom: checksum");
}
#[test]
fn the_switch_and_the_channel_go_to_the_service_and_links_open() {
    let service = fake();
    let mut h = harness(service.clone(), Locale::En);
    let mut state = State::default();
    state.expanded.insert(UpdateItem::Tool(ToolId::ClaudeCode));
    show(&mut h, &mut state, "Superba manages updates");
    let f = h.frames(5, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    let label = f.find("Superba manages updates").unwrap();
    let toggle = f
        .outlines
        .iter()
        .map(|(rect, _)| rect)
        .filter(|r| (r.width() - 36.0).abs() < 1.0 && (r.height() - 20.0).abs() < 1.0)
        .min_by(|a, b| {
            (a.center().y - label.center().y)
                .abs()
                .total_cmp(&(b.center().y - label.center().y).abs())
        })
        .expect("drawn updater toggle");
    h.click_at(toggle.center());
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| !service.tool_changes().is_empty(),
    );
    let (tool, change) = service.tool_changes()[0].clone();
    assert_eq!(
        (tool, change.manage_updates),
        (CoreId::ClaudeCode, Some(true))
    );
    show(&mut h, &mut state, "Updates managed by Superba");
    let f = show(&mut h, &mut state, "Channel latest");
    h.click_text(&f, "Channel latest");
    // Collapsed vendor rows also name their channel. Process the dropdown's
    // click before selecting the popup item, drawn after the underlying rows.
    let f = h.frames(4, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    let option = f
        .texts
        .iter()
        .rfind(|(text, _)| text == "Channel stable")
        .expect("stable channel option")
        .1
        .center();
    h.click_at(option);
    h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |_| {
            service
                .tool_changes()
                .iter()
                .any(|(_, c)| c.channel.as_deref() == Some("stable"))
        },
    );
    let f = show(&mut h, &mut state, "Download page");
    click_row(&mut h, &f, "ZCode", "Download page");
    let f = h.frames(3, |ui, cx, out| updates::show(ui, cx, &mut state, out));
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, Action::OpenUrl(u) if u == "https://zcode.z.ai")),
        "{:?}",
        f.actions
    );
}
