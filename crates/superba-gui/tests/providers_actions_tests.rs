#![allow(missing_docs)]

//! Providers page actions (user request: right-click menu, inline rename, Test
//! and Delete in the detail instead of the More menu): the menu offers only the
//! actions an entry has and says why a delete is refused, the name is renamed in
//! place (Enter saves, Escape or a click elsewhere cancels, empty and taken
//! names are refused), and nothing is deleted without the confirmation dialog.
//! Renames and deletes only save or remove the profile; no switch is asked for.

mod support;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use superba_core::{Locale, ProfileId, SiteId, SwitchService, Timestamp};
use superba_gui::design::Palette;
use superba_gui::i18n::Tr;
use superba_gui::pages::shared::providers::ProviderId;
use superba_gui::pages::{Action, Cx, Data, providers, shared};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

/// Krill (applied), a second Krill provider and a custom site without a key.
async fn setup() -> (Arc<FakeService>, Data) {
    let service = fake();
    let site = superba_switch::new_compatible_site(
        SiteId::new("example"),
        "Example API",
        &url::Url::parse("https://api.example.com/v1").unwrap(),
    )
    .unwrap();
    service.save_site(site).await.unwrap();
    let krill = superba_core::catalog::krill_site();
    let mut second = shared::new_profile(&krill, "Krill 2".into());
    second.id = ProfileId::new("krill-2");
    service
        .save_profile(second, Some(superba_core::Secret::new("nb_second")))
        .await
        .unwrap();
    let data = data_of(&service).await;
    (service, data)
}

async fn data_of(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

fn krill2() -> ProviderId {
    ProviderId::Profile(ProfileId::new("krill-2"))
}

/// The Providers page in a headless window, with its state and the actions it
/// returned.
struct Page {
    h: Harness,
    data: Data,
    service: Arc<dyn SwitchService>,
    tr: Tr,
    state: RefCell<providers::State>,
    actions: RefCell<Vec<Action>>,
}

impl Page {
    fn new(data: Data, service: Arc<dyn SwitchService>, locale: Locale) -> Self {
        Self {
            h: Harness::new([1060.0, 760.0], egui::Theme::Light),
            data,
            service,
            tr: Tr::new(locale),
            state: RefCell::new(providers::State::default()),
            actions: RefCell::new(Vec::new()),
        }
    }

    /// One frame with `events`; every drawn text with its rectangle.
    fn frame(&self, events: Vec<Event>) -> Vec<(String, Rect)> {
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
            providers::show(
                ui,
                &cx,
                &mut self.state.borrow_mut(),
                &mut self.actions.borrow_mut(),
            );
        };
        self.h.frame(events, &mut draw)
    }

    fn settle(&self) -> Vec<(String, Rect)> {
        self.frame(Vec::new());
        self.frame(Vec::new())
    }

    /// `n` frames without input (each advances the input clock by 1/60 s).
    fn idle(&self, n: usize) -> Vec<(String, Rect)> {
        let mut texts = Vec::new();
        for _ in 0..n {
            texts = self.frame(Vec::new());
        }
        texts
    }

    fn texts(&self) -> Vec<String> {
        self.settle().into_iter().map(|(t, _)| t).collect()
    }

    /// Where `text` was drawn first (the list is drawn before the detail).
    fn rect_of(&self, text: &str) -> Rect {
        let texts = self.settle();
        texts
            .iter()
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{text:?} not drawn: {texts:?}"))
    }

    fn press(&self, pos: Pos2, button: PointerButton) {
        let event = |pressed| Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::default(),
        };
        self.frame(vec![Event::PointerMoved(pos), event(true)]);
        self.frame(vec![event(false)]);
    }

    fn click(&self, text: &str) {
        let pos = self.rect_of(text).center();
        self.press(pos, PointerButton::Primary);
        self.frame(Vec::new());
    }

    /// Click the last drawn `text` (a menu entry or a button repeating a label).
    fn click_last(&self, text: &str) {
        let texts = self.settle();
        let rect = texts
            .iter()
            .rev()
            .find(|(t, _)| t == text)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{text:?} not drawn: {texts:?}"));
        self.press(rect.center(), PointerButton::Primary);
        self.frame(Vec::new());
    }

    fn right_click(&self, text: &str) {
        let pos = self.rect_of(text).center();
        self.press(pos, PointerButton::Secondary);
        self.frame(Vec::new());
    }

    fn double_click(&self, text: &str) {
        // Clicks closer than 2 x 0.3 s to earlier ones would count as a triple click.
        self.idle(40);
        let pos = self.rect_of(text).center();
        self.press(pos, PointerButton::Primary);
        self.press(pos, PointerButton::Primary);
        self.frame(Vec::new());
        self.frame(Vec::new());
    }

    fn key(&self, key: Key) {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }]);
        self.frame(Vec::new());
    }

    fn type_text(&self, text: &str) {
        self.frame(vec![Event::Text(text.into())]);
        self.frame(Vec::new());
    }

    fn renaming(&self) -> Option<(ProviderId, String)> {
        self.state
            .borrow()
            .renaming()
            .map(|(id, text)| (id.clone(), text.to_owned()))
    }

    fn removing(&self) -> Option<ProviderId> {
        self.state.borrow().removing().cloned()
    }

    fn take_actions(&self) -> Vec<Action> {
        std::mem::take(&mut self.actions.borrow_mut())
    }
}

/// Actions that would plan or run a switch (and so restart Codex).
fn switches(actions: &[Action]) -> Vec<&Action> {
    actions
        .iter()
        .filter(|a| matches!(a, Action::Switch { .. } | Action::Restore(_)))
        .collect()
}

fn saved_names(actions: &[Action]) -> Vec<(String, String)> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::SaveProfile { profile, key: None } => {
                Some((profile.id.to_string(), profile.name.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn right_click_opens_the_entry_menu_and_its_actions_work() {
    let (service, data) = setup().await;
    let page = Page::new(data, service.clone(), Locale::En);
    page.settle();
    page.right_click("Krill 2");
    let texts = page.texts();
    for entry in [
        "Rename",
        "Duplicate as new provider",
        "Test connection",
        "Delete…",
    ] {
        assert!(texts.iter().any(|t| t == entry), "{entry}: {texts:?}");
    }
    assert_eq!(
        page.state.borrow().selected,
        Some(krill2()),
        "right click selects"
    );

    // Duplicate saves a copy without a key and selects it.
    page.click_last("Duplicate as new provider");
    let actions = page.take_actions();
    assert_eq!(
        saved_names(&actions),
        vec![("krill-2-copy".to_owned(), "Krill 2 copy".to_owned())],
        "{actions:?}"
    );
    assert!(switches(&actions).is_empty(), "{actions:?}");

    // Test probes the lines and checks the key, on the Connection tab.
    page.state.borrow_mut().select(krill2());
    page.right_click("Krill 2");
    page.click_last("Test connection");
    let actions = page.take_actions();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Probe(site) if site.as_str() == "krill")),
        "{actions:?}"
    );
    assert_eq!(page.state.borrow().tab, providers::Tab::Connection);

    // Rename starts the inline edit with the current name.
    page.right_click("Krill 2");
    page.click_last("Rename");
    assert_eq!(page.renaming(), Some((krill2(), "Krill 2".into())));
    page.key(Key::Escape);
    assert_eq!(page.renaming(), None);

    // Delete only opens the confirmation.
    page.right_click("Krill 2");
    page.click_last("Delete…");
    assert_eq!(page.removing(), Some(krill2()));
    page.idle(10);
    let set = service.profile_set().await.unwrap();
    assert!(
        set.profile(&ProfileId::new("krill-2")).is_some(),
        "nothing is deleted before the confirmation"
    );
}

#[tokio::test]
async fn the_menu_offers_only_valid_actions_and_explains_a_refused_delete() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::En);
    page.settle();

    // The official login: neither renamed, duplicated, tested nor deleted.
    page.right_click("Official ChatGPT login");
    let texts = page.texts();
    for absent in ["Rename", "Duplicate as new provider", "Test connection"] {
        assert!(!texts.iter().any(|t| t == absent), "{absent}: {texts:?}");
    }
    assert!(
        texts.iter().any(|t| t
            == "The official ChatGPT login is always offered; it can't be renamed or deleted."),
        "{texts:?}"
    );
    page.click_last("Delete…");
    assert_eq!(page.removing(), None, "a disabled entry does nothing");
    page.key(Key::Escape);

    // The provider in use can be renamed, not deleted.
    page.right_click("Krill");
    let texts = page.texts();
    assert!(texts.iter().any(|t| t == "Rename"), "{texts:?}");
    assert!(
        texts.iter().any(|t| t
            == "Codex uses this provider now. Switch to another provider first, then delete it."),
        "{texts:?}"
    );
    page.click_last("Delete…");
    assert_eq!(page.removing(), None);
    page.key(Key::Escape);

    // A custom site without a key: test and delete, no name of its own to edit.
    page.right_click("Example API");
    let texts = page.texts();
    assert!(!texts.iter().any(|t| t == "Rename"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Test connection"), "{texts:?}");
    page.click_last("Delete…");
    assert_eq!(
        page.removing(),
        Some(ProviderId::Site(SiteId::new("example")))
    );
}

#[tokio::test]
async fn the_router_keeps_its_last_member() {
    let (service, mut data) = setup().await;
    let router = &mut data.profiles.value.as_mut().unwrap().settings.router;
    router.enabled = true;
    router.members = vec![ProfileId::new("krill-2")];
    let page = Page::new(data, service, Locale::En);
    page.settle();
    page.right_click("Krill 2");
    let why = "This is the local router's only member. Switch to another provider or turn \
               router mode off first, then delete it.";
    assert!(page.texts().iter().any(|t| t == why));
    page.click_last("Delete…");
    assert_eq!(page.removing(), None);
    page.key(Key::Escape);
    // The detail's Delete button is disabled as well.
    page.click_last("Delete");
    assert_eq!(page.removing(), None);
}

#[tokio::test]
async fn a_double_click_renames_in_place_and_enter_saves_the_profile_only() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::En);
    page.settle();
    page.double_click("Krill 2");
    assert_eq!(page.renaming(), Some((krill2(), "Krill 2".into())));
    let texts = page.texts();
    assert!(
        texts.iter().any(|t| t == "Enter to save · Esc to cancel"),
        "{texts:?}"
    );
    // The whole name is selected: typing replaces it.
    page.type_text("Team Krill");
    assert_eq!(page.renaming(), Some((krill2(), "Team Krill".into())));
    page.key(Key::Enter);
    assert_eq!(page.renaming(), None);
    let actions = page.take_actions();
    assert_eq!(
        saved_names(&actions),
        vec![("krill-2".to_owned(), "Team Krill".to_owned())]
    );
    assert!(
        switches(&actions).is_empty(),
        "a double click no longer switches: {actions:?}"
    );
}

#[tokio::test]
async fn escape_or_a_click_elsewhere_cancels_the_rename() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::En);
    page.settle();
    page.double_click("Krill 2");
    page.type_text("Other");
    page.key(Key::Escape);
    assert_eq!(page.renaming(), None);
    assert!(page.texts().iter().any(|t| t == "Krill 2"));

    page.double_click("Krill 2");
    page.type_text("Other");
    assert!(page.renaming().is_some());
    page.click("Example API");
    assert_eq!(page.renaming(), None);
    assert_eq!(
        page.state.borrow().selected,
        Some(ProviderId::Site(SiteId::new("example")))
    );
    assert!(saved_names(&page.take_actions()).is_empty());
}

#[tokio::test]
async fn empty_and_taken_names_are_refused_with_a_hint() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::En);
    page.settle();
    page.double_click("Krill 2");
    page.type_text("krill");
    page.key(Key::Enter);
    assert_eq!(
        page.renaming(),
        Some((krill2(), "krill".into())),
        "the field stays open"
    );
    let texts = page.texts();
    assert!(
        texts.iter().any(|t| t == "“krill” is already in the list."),
        "{texts:?}"
    );
    // Still focused: typing goes on in the field.
    page.key(Key::Backspace);
    page.type_text(" 3");
    assert_eq!(page.renaming(), Some((krill2(), "kril 3".into())));
    page.key(Key::Escape);

    page.double_click("Krill 2");
    page.key(Key::Backspace);
    page.key(Key::Enter);
    assert_eq!(page.renaming(), Some((krill2(), String::new())));
    assert!(page.texts().iter().any(|t| t == "Enter a name."));
    assert!(saved_names(&page.take_actions()).is_empty());
}

#[tokio::test]
async fn a_click_on_the_selected_name_renames_after_the_double_click_delay() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::En);
    page.state.borrow_mut().select(krill2());
    page.settle();
    page.click("Krill 2");
    assert_eq!(page.renaming(), None, "not before the double-click delay");
    page.idle(30);
    assert_eq!(page.renaming(), Some((krill2(), "Krill 2".into())));
    page.key(Key::Escape);

    // A click on another entry's name only selects it.
    page.click("Krill");
    page.idle(30);
    assert_eq!(page.renaming(), None);
    assert_eq!(
        page.state.borrow().selected,
        Some(ProviderId::Profile(ProfileId::new("krill")))
    );

    // F2 renames the selected entry.
    page.key(Key::F2);
    assert_eq!(
        page.renaming().map(|(id, _)| id),
        Some(ProviderId::Profile(ProfileId::new("krill")))
    );
}

#[tokio::test]
async fn the_detail_shows_test_and_delete_instead_of_a_more_menu() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::En);
    page.state.borrow_mut().select(krill2());
    let texts = page.texts();
    for shown in ["Test", "Delete", "Switch to this provider"] {
        assert!(texts.iter().any(|t| t == shown), "{shown}: {texts:?}");
    }
    for gone in ["More", "Rename", "⋯", "Duplicate as new provider"] {
        assert!(!texts.iter().any(|t| t == gone), "{gone}: {texts:?}");
    }
    // Test from the header.
    page.click_last("Test");
    assert!(
        page.take_actions()
            .iter()
            .any(|a| matches!(a, Action::Probe(_)))
    );

    // The official login has neither button.
    page.state
        .borrow_mut()
        .select(ProviderId::Profile(ProfileId::new("official")));
    let texts = page.texts();
    assert!(
        !texts.iter().any(|t| t == "Test" || t == "Delete"),
        "{texts:?}"
    );

    // The provider in use: Delete is disabled.
    page.state
        .borrow_mut()
        .select(ProviderId::Profile(ProfileId::new("krill")));
    page.click_last("Delete");
    assert_eq!(page.removing(), None);
}

#[tokio::test]
async fn delete_asks_for_confirmation_naming_the_provider() {
    let (service, data) = setup().await;
    let page = Page::new(data, service.clone(), Locale::En);
    page.state.borrow_mut().select(krill2());
    page.settle();
    page.click_last("Delete");
    assert_eq!(page.removing(), Some(krill2()));
    let texts = page.texts();
    assert!(texts.iter().any(|t| t == "Delete “Krill 2”?"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Delete “Krill 2”"), "{texts:?}");
    page.idle(10);
    let set = service.profile_set().await.unwrap();
    assert!(set.profile(&ProfileId::new("krill-2")).is_some());

    // Escape closes it; nothing was deleted.
    page.key(Key::Escape);
    assert_eq!(page.removing(), None);
    let set = service.profile_set().await.unwrap();
    assert!(set.profile(&ProfileId::new("krill-2")).is_some());

    // The second step deletes (the profile only; no switch).
    page.click_last("Delete");
    page.click_last("Delete “Krill 2”");
    let start = Instant::now();
    while page.removing().is_some() && start.elapsed() < Duration::from_secs(3) {
        page.frame(Vec::new());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(page.removing(), None);
    let set = service.profile_set().await.unwrap();
    assert!(set.profile(&ProfileId::new("krill-2")).is_none());
    assert!(set.profile(&ProfileId::new("krill")).is_some());
    let actions = page.take_actions();
    assert!(
        actions.iter().any(|a| matches!(a, Action::Refresh)),
        "{actions:?}"
    );
    assert!(switches(&actions).is_empty(), "{actions:?}");
}

#[tokio::test]
async fn the_menu_and_hints_are_in_chinese() {
    let (service, data) = setup().await;
    let page = Page::new(data, service, Locale::ZhCn);
    page.settle();
    page.right_click("Krill 2");
    let texts = page.texts();
    for entry in ["重命名", "复制为新供应商", "测试连接", "删除…"] {
        assert!(texts.iter().any(|t| t == entry), "{entry}: {texts:?}");
    }
    page.key(Key::Escape);
    page.right_click("官方 ChatGPT 登录");
    assert!(
        page.texts()
            .iter()
            .any(|t| t == "官方 ChatGPT 登录始终保留，不能重命名或删除。")
    );
    page.key(Key::Escape);
    page.double_click("Krill 2");
    let texts = page.texts();
    assert!(
        texts.iter().any(|t| t == "Enter 保存 · Esc 取消"),
        "{texts:?}"
    );
    page.type_text("krill");
    assert!(page.texts().iter().any(|t| t == "列表里已有「krill」。"));
    page.key(Key::Escape);
    page.state.borrow_mut().select(krill2());
    let texts = page.texts();
    assert!(
        texts.iter().any(|t| t == "测试") && texts.iter().any(|t| t == "删除"),
        "{texts:?}"
    );
    page.click_last("删除");
    assert!(page.texts().iter().any(|t| t == "删除「Krill 2」"));
}
