#![allow(missing_docs)]

//! User bug 61: whether hot switching is available was only in the hover text
//! of the Overview's "This computer" node; with ChatGPT started from the Start
//! menu the Overview looked the same and the To-do said "All good". Now a
//! ChatGPT Superba did not open (or opened without the line's auth address)
//! is a To-do with the reopen that fixes it, a marker on "This computer", and
//! a note with the same reopen where a switch is chosen (the provider and
//! line popover). States a reopen does not change (no restart bridge, a
//! platform without hot switching, safe mode, ChatGPT not running) add
//! nothing.

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use superba_core::{FullRestartReason, HotSwitchStatus, Locale, SwitchService, Timestamp};
use superba_gui::design::tokens::DARK;
use superba_gui::design::widgets::Status;
use superba_gui::i18n::Tr;
use superba_gui::pages::overview::todo::{self, Spot, TodoKind};
use superba_gui::pages::{Action, Cx, Data, Page};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
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

fn todos(data: &Data, service: Arc<dyn SwitchService>, locale: Locale) -> Vec<todo::Todo> {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &DARK,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts,
        service: &service,
        log: &[],
    };
    todo::todos(&cx, None)
}

#[tokio::test]
async fn a_chatgpt_superba_did_not_open_is_a_todo_with_the_reopen() {
    let service = fake();
    let data = data_of(&service).await;
    assert_eq!(
        data.overview.value.as_ref().unwrap().app.hot_switch,
        HotSwitchStatus::Ready
    );
    let ready = todos(&data, service.clone(), Locale::En);
    assert!(
        !ready.iter().any(|t| t.kind == TodoKind::HotSwitchOff),
        "{ready:?}"
    );

    service.open_app_elsewhere();
    let data = data_of(&service).await;
    for (locale, title, button, reason) in [
        (
            Locale::En,
            "Hot switching is off",
            "Reopen",
            "ChatGPT was not opened from Superba, so it cannot be hot-switched.",
        ),
        (
            Locale::ZhCn,
            "热切换不可用",
            "重新打开",
            "ChatGPT 不是从 Superba 打开的，无法热切。",
        ),
    ] {
        let list = todos(&data, service.clone(), locale);
        let t = list
            .iter()
            .find(|t| t.kind == TodoKind::HotSwitchOff)
            .unwrap_or_else(|| panic!("{locale:?}: no hot-switch to-do in {list:?}"));
        assert_eq!(t.title, title);
        assert_eq!(t.detail.as_deref(), Some(reason));
        // A warning on "This computer": the hero marks that node.
        assert_eq!(t.status, Status::Warn);
        assert_eq!(t.spot, Some(Spot::ThisComputer));
        let tr = Tr::new(locale);
        let fonts = superba_gui::design::fonts::FontReport::default();
        let svc: Arc<dyn SwitchService> = service.clone();
        let cx = Cx {
            tr: &tr,
            pal: &DARK,
            data: &data,
            busy: false,
            now: Timestamp::now(),
            fonts: &fonts,
            service: &svc,
            log: &[],
        };
        assert_eq!(t.fix_label(&cx), button);
        assert!(matches!(t.fix(&cx), Action::RestartApp));
        assert!(
            todo::markers(&list, Spot::ThisComputer)
                .iter()
                .any(|m| m.kind == TodoKind::HotSwitchOff)
        );
    }
}

#[tokio::test]
async fn only_what_a_reopen_fixes_is_offered() {
    let mut overview = fake().overview().await.unwrap();
    for (status, fixes) in [
        (HotSwitchStatus::Ready, false),
        (HotSwitchStatus::NotRunning, false),
        (HotSwitchStatus::NotInstalled, false),
        (HotSwitchStatus::SafeMode, false),
        (
            HotSwitchStatus::Unavailable {
                reason: FullRestartReason::NoCdp,
            },
            true,
        ),
        (
            HotSwitchStatus::Unavailable {
                reason: FullRestartReason::AuthApiMissing,
            },
            true,
        ),
        (
            HotSwitchStatus::Unavailable {
                reason: FullRestartReason::AuthApiUnhealthy,
            },
            true,
        ),
        (
            HotSwitchStatus::Unavailable {
                reason: FullRestartReason::NoBridge,
            },
            false,
        ),
        (
            HotSwitchStatus::Unavailable {
                reason: FullRestartReason::PlatformNoHot,
            },
            false,
        ),
    ] {
        overview.app.hot_switch = status.clone();
        assert_eq!(todo::reopen_fixes(&overview).is_some(), fixes, "{status:?}");
    }
}

#[test]
fn the_overview_shows_it_and_the_reopen_plans_a_full_restart() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = fake();
        service.open_app_elsewhere();
        let mut h = AppHarness::new(service, locale, Page::Overview);
        let title = tr.t("overview.todo.hot_off");
        // The To-do's text is on screen (and in the tree), not only on hover.
        let t = h.until("the hot-switch to-do", |t| {
            t.with_role(Role::Label)
                .iter()
                .any(|id| t.node(*id).value().is_some_and(|v| v.starts_with(&title)))
        });
        // "This computer" carries the problem (its marker's hover text).
        let this = t
            .find(Some(Role::Button), &tr.t("overview.hero.this"))
            .unwrap();
        assert!(
            t.node(this)
                .description()
                .is_some_and(|d| d.starts_with(&title)),
            "{:?}",
            t.node(this).description()
        );
        // Where a switch is chosen: why it will be a full restart, and the reopen.
        let t = h.click(Some(Role::Button), &tr.t("overview.hero.switch_menu"));
        assert!(t.has(&tr.t("overview.hero.reopen")), "{}", t.dump());
        h.key(egui::Key::Escape);
        // The To-do's button plans the reopen: a full restart, confirmed first
        // (titled as the reopen it is, GH#7).
        let title = tr.t("confirm.title_reopen");
        let t = h.click(Some(Role::Button), &tr.t("overview.todo.fix.reopen"));
        let t = if t.has(&title) {
            t
        } else {
            h.wait_for(None, &title)
        };
        assert!(
            t.with_role(Role::Dialog)
                .iter()
                .any(|d| t.name(*d).as_deref() == Some(title.as_str())),
            "{}",
            t.dump()
        );
    }
}
