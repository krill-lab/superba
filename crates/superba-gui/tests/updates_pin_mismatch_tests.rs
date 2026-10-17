#![allow(missing_docs)]
//! Updates page, "Codex used by the desktop app" (user bug 18): a pin mismatch
//! names both sides briefly as the service words it (no two long paths in one
//! sentence); the full paths are on hover and behind a copy button.

mod router_harness;
mod support;

use std::path::PathBuf;
use std::sync::Arc;

use router_harness::{Frame, Harness};
use superba_core::{AppCodexPin, CodexVersion, Locale, LocalizedText};
use superba_gui::pages::Action;
use superba_gui::pages::updates::{self, State};
use support::fake::{FakeOptions, FakeService};

const RUNNING: &str = "/home/user/.local/share/fnm/node-versions/v22.11.0/installation/lib/node_modules/@openai/codex/vendor/x86_64-unknown-linux-musl/codex/codex";

/// `switch.pin.mismatch` as the switch service builds it for a foreign
/// `CODEX_CLI_PATH`.
fn mismatch() -> LocalizedText {
    LocalizedText::new("switch.pin.mismatch", "")
        .arg_text(
            "actual",
            LocalizedText::new("switch.pin.who_path", "")
                .arg("path", "~/\u{2026}/x86_64-unknown-linux-musl/codex/codex"),
        )
        .arg_text(
            "expected",
            LocalizedText::new("switch.pin.who_bundled_version", "")
                .arg("version", "0.163.0-alpha.10.2"),
        )
        .arg_text("cause", LocalizedText::new("switch.pin.cause_override", ""))
}

fn loaded(f: &Frame) -> bool {
    f.has("26.1013.31256")
}

#[test]
fn a_mismatch_names_both_sides_briefly_and_copies_the_full_paths() {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let mut view = service.versions_view();
    view.app_codex.pin = AppCodexPin::Bundled;
    view.app_codex.effective_exe = Some(PathBuf::from(RUNNING));
    view.app_codex.effective_version = Some(CodexVersion::new(0, 150, 1));
    view.app_codex.mismatch = Some(mismatch());
    let expected = view.expected_codex().expect("the bundled Codex");
    service.set_versions(view);

    let mut h = Harness::new([1280.0, 2400.0], Locale::ZhCn, service, false);
    let mut state = State::default();
    let f = h.until(|ui, cx, out| updates::show(ui, cx, &mut state, out), loaded);
    let sentence = "app 运行的是 ~/\u{2026}/x86_64-unknown-linux-musl/codex/codex 处的 Codex，\
                    而不是 app 自带的 Codex 0.163.0-alpha.10.2。";
    assert!(f.has(sentence), "{}", f.all_text());
    assert!(f.has("优先生效"), "{}", f.all_text());
    assert!(
        !f.has(RUNNING),
        "the full path belongs on hover:\n{}",
        f.all_text()
    );
    h.click_text(&f, "复制路径");
    let f = h.until(
        |ui, cx, out| updates::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::Copy(_))),
    );
    let copied = f
        .actions
        .iter()
        .find_map(|a| match a {
            Action::Copy(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        copied,
        format!("正在运行：{RUNNING}\n应当运行：{}", expected.display())
    );
}

#[test]
fn the_expected_codex_follows_the_pin() {
    let service = FakeService::new(FakeOptions::default());
    let mut view = service.versions_view();
    let bundled = view
        .app
        .install
        .as_ref()
        .and_then(|i| i.bundled_codex.clone());
    assert_eq!(view.expected_codex(), bundled);
    view.app_codex.pin = AppCodexPin::Path {
        path: PathBuf::from("/opt/codex/bin/codex"),
    };
    assert_eq!(
        view.expected_codex(),
        Some(PathBuf::from("/opt/codex/bin/codex"))
    );
    view.app_codex.pin = AppCodexPin::Version {
        version: CodexVersion::new(9, 9, 9),
    };
    assert_eq!(view.expected_codex(), None);
}
