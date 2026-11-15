//! Superba → Data: the Codex session data card (GitHub issue 15).

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::{Frame, Harness};
use superba_core::data::{
    CodexDataDir, CodexDataLocation, CodexDataMoveItem, CodexDataPlan, CodexDirPlacement,
};
use superba_core::{Locale, LocalizedText, Settings};
use superba_gui::pages::data;
use support::fake::{FakeOptions, FakeService};

fn location(moved: bool, reachable: bool) -> CodexDataLocation {
    let dir = |name: &str| CodexDataDir {
        name: name.into(),
        path: format!("/fixture/codex/{name}").into(),
        placement: if moved {
            CodexDirPlacement::Linked {
                target: format!("/mnt/d/Codex/{name}").into(),
                managed: true,
                reachable,
            }
        } else {
            CodexDirPlacement::InHome
        },
        bytes: 3 * 1024 * 1024,
        files: 4,
    };
    CodexDataLocation {
        codex_home: "/fixture/codex".into(),
        directory: moved.then(|| "/mnt/d/Codex".into()),
        dirs: vec![dir("sessions"), dir("archived_sessions")],
        pending: false,
        leftovers: Vec::new(),
    }
}

fn plan() -> CodexDataPlan {
    CodexDataPlan {
        location: location(false, true),
        destination: Some("/mnt/d/Codex".into()),
        moves: vec![CodexDataMoveItem {
            name: "sessions".into(),
            from: "/fixture/codex/sessions".into(),
            to: "/mnt/d/Codex/sessions".into(),
            bytes: 3 * 1024 * 1024,
            files: 4,
        }],
        total_bytes: 3 * 1024 * 1024,
        available_bytes: Some(10 * 1024 * 1024 * 1024),
        blockers: vec![
            LocalizedText::new(
                "switch.codex_data.running",
                "Codex is running ({processes}).",
            )
            .arg("processes", "ChatGPT (100)"),
        ],
        notes: Vec::new(),
    }
}

fn until(h: &mut Harness, state: &mut data::State, settings: &mut Settings, needle: &str) -> Frame {
    let mut last = Frame::default();
    for _ in 0..200 {
        last = h.frame(|ui, cx, out| data::body_ui(ui, cx, settings, state, out));
        if last.has(needle) {
            return last;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("{needle:?} never appeared:\n{}", last.all_text());
}

#[test]
fn the_card_shows_where_sessions_are_and_previews_a_blocked_move_in_both_languages() {
    for locale in Locale::ALL {
        let service = Arc::new(FakeService::new(FakeOptions::default()));
        service.set_codex_data(location(false, true), Some(plan()));
        let mut h = Harness::new([1280.0, 2200.0], locale, service, false);
        let mut settings = Settings::default();
        let mut state = data::State::default();
        let zh = locale == Locale::ZhCn;
        let title = if zh {
            "Codex 会话数据"
        } else {
            "Codex session data"
        };
        let frame = until(&mut h, &mut state, &mut settings, "/fixture/codex");
        assert!(frame.has(title), "{}", frame.all_text());
        assert!(frame.has("6.0 MiB"), "{}", frame.all_text());
        state.codex.destination = "/mnt/d/Codex".into();
        let preview = if zh { "预览迁移" } else { "Preview move" };
        let frame = until(&mut h, &mut state, &mut settings, preview);
        h.click_text(&frame, preview);
        let frame = until(&mut h, &mut state, &mut settings, "ChatGPT (100)");
        let blocked = if zh {
            "现在无法迁移"
        } else {
            "Cannot move now"
        };
        assert!(frame.has(blocked), "{}", frame.all_text());
        assert!(frame.has("/mnt/d/Codex/sessions"), "{}", frame.all_text());
    }
}

#[test]
fn an_unplugged_drive_is_shown() {
    let service = Arc::new(FakeService::new(FakeOptions::default()));
    service.set_codex_data(location(true, false), None);
    let mut h = Harness::new([1280.0, 2200.0], Locale::ZhCn, service, false);
    let mut settings = Settings::default();
    let mut state = data::State::default();
    let frame = until(&mut h, &mut state, &mut settings, "重新连接该磁盘");
    assert!(frame.has("迁回 Codex 主目录"), "{}", frame.all_text());
}
