#![allow(missing_docs)]

//! Sessions page as a session browser (sessions2): the list of every host's
//! sessions, fuzzy search, filters, the right-click menu (links with the app's
//! `hostId` encoding, exports), multi-selection and the migration tool behind
//! the header button.

mod support;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Event, Modifiers, PointerButton, Pos2, Rect};
use superba_core::{Locale, SwitchService, Timestamp};
use superba_gui::design::Palette;
use superba_gui::i18n::Tr;
use superba_gui::pages::sessions::{self, State};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

const LOCAL: &str = "019a2f5b-6a7b-7d46-8e85-4f0a6b8c2d05";
const REMOTE: &str = "01a158c7-a09e-7cd2-ba08-41459547a695";

struct Page {
    h: Harness,
    data: Data,
    service: Arc<dyn SwitchService>,
    tr: Tr,
    state: RefCell<State>,
    actions: RefCell<Vec<Action>>,
}

impl Page {
    async fn new(locale: Locale) -> Self {
        let fake = Arc::new(FakeService::new(FakeOptions {
            fast: true,
            ..Default::default()
        }));
        let mut data = Data::default();
        data.overview.finish(fake.overview().await);
        data.sessions.finish(fake.sessions().await);
        let page = Self {
            h: Harness::new([1060.0, 760.0], egui::Theme::Light),
            data,
            service: fake,
            tr: Tr::new(locale),
            state: RefCell::new(State::default()),
            actions: RefCell::new(Vec::new()),
        };
        page.until_loaded();
        page
    }

    fn frame_with(&self, events: Vec<Event>) -> Vec<(String, Rect)> {
        self.frame_mods(events, Modifiers::default())
    }

    fn frame_mods(&self, events: Vec<Event>, modifiers: Modifiers) -> Vec<(String, Rect)> {
        let mut draw = |ui: &mut egui::Ui| {
            let cx = Cx {
                tr: &self.tr,
                pal: Palette::of(ui.ctx()),
                data: &self.data,
                busy: false,
                now: Timestamp::now(),
                fonts: &self.h.fonts,
                service: &self.service,
                log: &[],
            };
            sessions::show(
                ui,
                &cx,
                &mut self.state.borrow_mut(),
                &mut self.actions.borrow_mut(),
            );
        };
        self.h.frame_with_modifiers(events, modifiers, &mut draw)
    }

    fn texts(&self) -> Vec<String> {
        self.frame_with(Vec::new());
        self.frame_with(Vec::new())
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    fn until_loaded(&self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            self.frame_with(Vec::new());
            if self.state.borrow().browser.list.is_some() {
                self.frame_with(Vec::new());
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the session list never loaded");
    }

    fn rect_of(&self, text: &str, last: bool) -> Rect {
        self.frame_with(Vec::new());
        let texts = self.frame_with(Vec::new());
        let found = if last {
            texts.iter().rev().find(|(t, _)| t == text)
        } else {
            texts.iter().find(|(t, _)| t == text)
        };
        found
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{text:?} not drawn: {texts:?}"))
    }

    fn press(&self, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
        let event = |pressed| Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        };
        self.frame_mods(vec![Event::PointerMoved(pos), event(true)], modifiers);
        self.frame_mods(vec![event(false)], modifiers);
        self.frame_with(Vec::new());
    }

    fn click(&self, text: &str) {
        let pos = self.rect_of(text, false).center();
        self.press(pos, PointerButton::Primary, Modifiers::default());
    }

    fn click_last(&self, text: &str) {
        let pos = self.rect_of(text, true).center();
        self.press(pos, PointerButton::Primary, Modifiers::default());
    }

    fn right_click(&self, text: &str) {
        let pos = self.rect_of(text, false).center();
        self.press(pos, PointerButton::Secondary, Modifiers::default());
    }

    fn take_actions(&self) -> Vec<Action> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    fn copied(&self) -> Vec<String> {
        self.take_actions()
            .into_iter()
            .filter_map(|a| match a {
                Action::Copy(t) => Some(t),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test]
async fn the_list_shows_every_hosts_sessions_newest_first() {
    let p = Page::new(Locale::En).await;
    let texts = p.texts();
    for t in [
        "Plan the database migration",
        "Fix flaky test",
        "Refactor the router",
        "bj",
        "This computer",
        "w/superba",
        "3 sessions",
        "Session migration…",
    ] {
        assert!(texts.iter().any(|x| x == t), "{t:?} missing: {texts:?}");
    }
    let pos = |t: &str| texts.iter().position(|x| x == t).unwrap();
    assert!(pos("Plan the database migration") < pos("Fix flaky test"));
    assert!(pos("Fix flaky test") < pos("Refactor the router"));
    // The migration tool's provider table is not on the list view.
    assert!(!texts.iter().any(|x| x == "Migration history"), "{texts:?}");
}

#[tokio::test]
async fn fuzzy_search_and_filters_narrow_the_list() {
    let p = Page::new(Locale::En).await;
    p.state.borrow_mut().browser.search = "dbmig".into();
    let texts = p.texts();
    assert!(texts.iter().any(|x| x == "Plan the database migration"));
    assert!(!texts.iter().any(|x| x == "Fix flaky test"), "{texts:?}");
    assert!(texts.iter().any(|x| x == "1 of 3"), "{texts:?}");
    // Ids and directories match too.
    p.state.borrow_mut().browser.search = "01a158".into();
    let texts = p.texts();
    assert!(texts.iter().any(|x| x == "Fix flaky test"));
    assert!(!texts.iter().any(|x| x == "Plan the database migration"));
    p.state.borrow_mut().browser.search = "srv app".into();
    assert!(p.texts().iter().any(|x| x == "Fix flaky test"));
    // Nothing matches: the empty state, and Clear search brings everything back.
    p.state.borrow_mut().browser.search = "zzzz".into();
    assert!(p.texts().iter().any(|x| x == "No matching sessions"));
    p.click("Clear search");
    assert!(p.state.borrow().browser.search.is_empty());
    assert!(p.texts().iter().any(|x| x == "3 sessions"));
    // Host filter.
    p.state.borrow_mut().browser.host = Some("remote-ssh-discovered:bj".into());
    let texts = p.texts();
    assert!(texts.iter().any(|x| x == "Fix flaky test"));
    assert!(!texts.iter().any(|x| x == "Refactor the router"));
}

#[tokio::test]
async fn the_context_menu_copies_links_with_the_apps_host_id() {
    let p = Page::new(Locale::En).await;
    p.right_click("Fix flaky test");
    let texts = p.texts();
    for item in [
        "Copy link",
        "Open in ChatGPT",
        "Copy session ID",
        "Copy as Markdown",
        "Export as Claude Code session",
        "Export as pi session",
        "Export as dsh session",
    ] {
        assert!(
            texts.iter().any(|x| x == item),
            "{item:?} missing: {texts:?}"
        );
    }
    p.click_last("Copy link");
    assert_eq!(
        p.copied(),
        [format!(
            "codex://threads/{REMOTE}?hostId=remote-ssh-discovered%3Abj"
        )]
    );
    // This computer's sessions link without a host.
    p.right_click("Plan the database migration");
    p.click_last("Copy link");
    assert_eq!(p.copied(), [format!("codex://threads/{LOCAL}")]);
    p.right_click("Plan the database migration");
    p.click_last("Open in ChatGPT");
    assert!(
        p.take_actions()
            .iter()
            .any(|a| matches!(a, Action::OpenUrl(u) if u == &format!("codex://threads/{LOCAL}")))
    );
    p.right_click("Plan the database migration");
    p.click_last("Copy session ID");
    assert_eq!(p.copied(), [LOCAL.to_owned()]);
}

#[tokio::test]
async fn exports_run_in_the_background_and_copy_the_result() {
    let p = Page::new(Locale::En).await;
    p.right_click("Plan the database migration");
    p.click_last("Copy as Markdown");
    let start = Instant::now();
    let mut copied = Vec::new();
    while copied.is_empty() && start.elapsed() < Duration::from_secs(5) {
        p.frame_with(Vec::new());
        copied = p.copied();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(copied, [format!("# {LOCAL}\n")]);
    assert!(
        p.texts()
            .iter()
            .any(|x| x == "Copied 1 session as Markdown.")
    );
    // A remote session cannot be exported yet: its export items do nothing.
    p.right_click("Fix flaky test");
    p.click_last("Export as Claude Code session");
    p.texts();
    assert!(p.copied().is_empty());
    assert_eq!(
        p.state.borrow().browser.notice.as_ref().unwrap().1,
        "Copied 1 session as Markdown."
    );
}

#[tokio::test]
async fn ctrl_click_selects_several_for_bulk_actions() {
    let p = Page::new(Locale::En).await;
    p.click("Plan the database migration");
    let pos = p.rect_of("Fix flaky test", false).center();
    p.press(pos, PointerButton::Primary, Modifiers::COMMAND);
    assert_eq!(p.state.borrow().browser.checked.len(), 2);
    let texts = p.texts();
    assert!(texts.iter().any(|x| x == "2 selected"), "{texts:?}");
    p.click("Copy links");
    let copied = p.copied();
    assert_eq!(copied.len(), 1);
    let lines: Vec<&str> = copied[0].lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.contains(&format!("codex://threads/{LOCAL}").as_str()));
    p.click("Clear selection");
    assert!(p.state.borrow().browser.checked.is_empty());
}

#[tokio::test]
async fn the_migration_tool_opens_from_the_header_and_goes_back() {
    let p = Page::new(Locale::En).await;
    p.click("Session migration…");
    assert!(p.state.borrow().migration);
    let texts = p.texts();
    assert!(
        texts
            .iter()
            .any(|x| x == "Migration history" || x.starts_with("Migration history")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|x| x == "← Sessions"));
    p.click("← Sessions");
    assert!(!p.state.borrow().migration);
    assert!(p.texts().iter().any(|x| x == "Fix flaky test"));
}

#[tokio::test]
async fn chinese_labels() {
    let p = Page::new(Locale::ZhCn).await;
    let texts = p.texts();
    for t in ["会话迁移…", "3 个会话", "本机", "全部主机"] {
        assert!(texts.iter().any(|x| x == t), "{t:?} missing: {texts:?}");
    }
    p.right_click("Fix flaky test");
    let texts = p.texts();
    for t in ["复制链接", "在 ChatGPT 中打开", "导出为 Claude Code 会话"] {
        assert!(texts.iter().any(|x| x == t), "{t:?} missing: {texts:?}");
    }
}
