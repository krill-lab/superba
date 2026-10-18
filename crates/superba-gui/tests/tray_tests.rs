#![allow(missing_docs)]

//! Item 70 (GH#10): the tray icon / menu-bar item and "close to tray".
//!
//! - The window's life as a state machine: a close request hides into the
//!   tray, asks once (Windows, Linux) or quits; Quit always quits; a lost
//!   tray brings a hidden window back; Wayland hides by restarting through
//!   XWayland.
//! - The menu model built from the app's data: Open Superba, what Codex uses
//!   now, the switch submenu, ChatGPT through Superba, the router, Quit.
//! - The icon assets, the Linux X11 choice.
//! - The whole app with a simulated tray: the first-close question and its
//!   saved answer, hide / show through the tray, quit from the tray, a second
//!   start showing the hidden window, the Preferences rows (disabled with the
//!   reason when there is no tray), "start in the tray".

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::ViewportCommand;
use egui::accesskit::Role;
use superba_core::{CloseAction, Locale, Settings, SwitchService};
use superba_gui::AppOptions;
use superba_gui::i18n::Tr;
use superba_gui::instance::{self, Request};
use superba_gui::pages::{Data, Page};
use superba_gui::tray::close::{CloseContext, CloseOutcome, Unset};
use superba_gui::tray::{
    HideSupport, Sink, TrayAction, TrayEvent, TrayItem, TrayOption, TrayStatus, Unavailable,
    WindowCommand, WindowLife, icon, linux_hide_support, menu, wants_x11,
};
use support::fake::{FakeOptions, FakeService};

fn cx(setting: Option<CloseAction>, hide: Option<HideSupport>, unset: Unset) -> CloseContext {
    CloseContext {
        setting,
        hide,
        unset,
    }
}

const TRAY: Option<HideSupport> = Some(HideSupport::InPlace);

// ---------------------------------------------------------------------------
// The state machine.

#[test]
fn a_close_request_quits_hides_or_asks_by_setting_tray_and_platform() {
    use CloseOutcome::*;
    let hide = Hide(HideSupport::InPlace);
    for (setting, tray, unset, outcome) in [
        // No tray: always quit.
        (None, None, Unset::Ask, Quit),
        (None, None, Unset::Tray, Quit),
        (Some(CloseAction::Tray), None, Unset::Ask, Quit),
        // The saved choice.
        (Some(CloseAction::Quit), TRAY, Unset::Ask, Quit),
        (Some(CloseAction::Quit), TRAY, Unset::Tray, Quit),
        (Some(CloseAction::Tray), TRAY, Unset::Ask, hide),
        // Not chosen: Windows and Linux ask, macOS keeps running.
        (None, TRAY, Unset::Ask, Ask),
        (None, TRAY, Unset::Tray, hide),
    ] {
        assert_eq!(
            cx(setting, tray, unset).outcome(),
            outcome,
            "{setting:?} {tray:?} {unset:?}"
        );
    }
    // Preferences shows the choice in effect.
    assert_eq!(
        cx(None, None, Unset::Ask).effective(),
        Some(CloseAction::Quit)
    );
    assert_eq!(cx(None, TRAY, Unset::Ask).effective(), None);
    assert_eq!(
        cx(None, TRAY, Unset::Tray).effective(),
        Some(CloseAction::Tray)
    );
    assert_eq!(
        cx(Some(CloseAction::Quit), TRAY, Unset::Tray).effective(),
        Some(CloseAction::Quit)
    );
    assert_eq!(
        Unset::platform(),
        if cfg!(target_os = "macos") {
            Unset::Tray
        } else {
            Unset::Ask
        }
    );
}

#[test]
fn hide_show_and_quit_from_the_tray() {
    let tray = cx(Some(CloseAction::Tray), TRAY, Unset::Ask);
    let mut life = WindowLife::new();
    life.close_requested(&tray);
    assert_eq!(
        life.take_commands(),
        [WindowCommand::CancelClose, WindowCommand::Hide]
    );
    assert!(life.is_hidden());
    // Open Superba from the tray.
    life.show();
    assert_eq!(life.take_commands(), [WindowCommand::Show]);
    assert!(!life.is_hidden());
    // Hidden again, then Quit from the tray: the window closes for real ...
    life.close_requested(&tray);
    life.take_commands();
    life.quit();
    assert_eq!(life.take_commands(), [WindowCommand::Close]);
    assert!(life.is_quitting());
    // ... and the close request that follows goes through.
    life.close_requested(&tray);
    assert_eq!(life.take_commands(), []);
    // Nothing shows a quitting window again.
    life.show();
    assert_eq!(life.take_commands(), []);
}

#[test]
fn the_first_close_asks_once_and_remembers_the_answer() {
    let unset = cx(None, TRAY, Unset::Ask);
    // Keep running in the tray.
    let mut life = WindowLife::new();
    life.close_requested(&unset);
    assert_eq!(life.take_commands(), [WindowCommand::CancelClose]);
    assert!(life.is_asking());
    // A second close while asking keeps the question.
    life.close_requested(&unset);
    assert_eq!(life.take_commands(), [WindowCommand::CancelClose]);
    life.answer(Some(CloseAction::Tray), TRAY);
    assert_eq!(
        life.take_commands(),
        [
            WindowCommand::Remember(CloseAction::Tray),
            WindowCommand::Hide
        ]
    );
    assert!(life.is_hidden() && !life.is_asking());

    // Quit.
    let mut life = WindowLife::new();
    life.close_requested(&unset);
    life.take_commands();
    life.answer(Some(CloseAction::Quit), TRAY);
    assert_eq!(
        life.take_commands(),
        [
            WindowCommand::Remember(CloseAction::Quit),
            WindowCommand::Close
        ]
    );
    assert!(life.is_quitting());

    // Cancel: nothing saved, the window stays.
    let mut life = WindowLife::new();
    life.close_requested(&unset);
    life.take_commands();
    life.answer(None, TRAY);
    assert_eq!(life.take_commands(), []);
    assert!(!life.is_asking() && !life.is_hidden() && !life.is_quitting());
    // An answer nobody asked for does nothing.
    life.answer(Some(CloseAction::Tray), TRAY);
    assert_eq!(life.take_commands(), []);

    // The tray went away while asking: "keep running" can only quit.
    let mut life = WindowLife::new();
    life.close_requested(&unset);
    life.take_commands();
    life.answer(Some(CloseAction::Tray), None);
    assert_eq!(
        life.take_commands(),
        [
            WindowCommand::Remember(CloseAction::Tray),
            WindowCommand::Close
        ]
    );
}

#[test]
fn without_a_tray_closing_quits_and_a_lost_tray_shows_the_window() {
    let mut life = WindowLife::new();
    life.close_requested(&cx(Some(CloseAction::Tray), None, Unset::Tray));
    assert_eq!(life.take_commands(), []);
    assert!(life.is_quitting());

    let mut life = WindowLife::new();
    life.close_requested(&cx(Some(CloseAction::Tray), TRAY, Unset::Ask));
    life.take_commands();
    life.tray_lost();
    assert_eq!(life.take_commands(), [WindowCommand::Show]);
    assert!(!life.is_hidden());
    // A shown window stays as it is.
    life.tray_lost();
    assert_eq!(life.take_commands(), []);
}

#[test]
fn start_in_the_tray_and_the_wayland_restart() {
    let mut life = WindowLife::new();
    life.start_hidden(TRAY);
    assert_eq!(life.take_commands(), [WindowCommand::Hide]);
    assert!(life.is_hidden());
    // Without a tray the window shows.
    let mut life = WindowLife::new();
    life.start_hidden(None);
    assert_eq!(life.take_commands(), []);
    assert!(!life.is_hidden());

    // A native Wayland window: the close goes through and Superba starts
    // again hidden (no CancelClose).
    let mut life = WindowLife::new();
    life.close_requested(&cx(
        Some(CloseAction::Tray),
        Some(HideSupport::Relaunch),
        Unset::Ask,
    ));
    assert_eq!(
        life.take_commands(),
        [WindowCommand::RelaunchHidden, WindowCommand::Close]
    );
    assert!(life.is_quitting());
    // Also as the first-close answer.
    let mut life = WindowLife::new();
    let wayland = cx(None, Some(HideSupport::Relaunch), Unset::Ask);
    life.close_requested(&wayland);
    life.take_commands();
    life.answer(Some(CloseAction::Tray), Some(HideSupport::Relaunch));
    assert_eq!(
        life.take_commands(),
        [
            WindowCommand::Remember(CloseAction::Tray),
            WindowCommand::RelaunchHidden,
            WindowCommand::Close
        ]
    );
}

#[test]
fn linux_opens_through_xwayland_only_to_hide_and_only_when_it_can() {
    // Keeps Superba in the tray on Wayland with XWayland: X11.
    assert!(wants_x11(Some("wayland-0"), Some(":0"), true));
    // Not in the tray, an X11 session, or no XWayland: the default backend.
    assert!(!wants_x11(Some("wayland-0"), Some(":0"), false));
    assert!(!wants_x11(None, Some(":0"), true));
    assert!(!wants_x11(Some(""), Some(":0"), true));
    assert!(!wants_x11(Some("wayland-0"), None, true));
    assert!(!wants_x11(Some("wayland-0"), Some(" "), true));

    assert_eq!(
        linux_hide_support(None, Some(":0"), false),
        Some(HideSupport::InPlace)
    );
    assert_eq!(
        linux_hide_support(Some("wayland-0"), Some(":0"), true),
        Some(HideSupport::InPlace)
    );
    assert_eq!(
        linux_hide_support(Some("wayland-0"), Some(":0"), false),
        Some(HideSupport::Relaunch)
    );
    assert_eq!(linux_hide_support(Some("wayland-0"), None, false), None);
}

// ---------------------------------------------------------------------------
// Icons.

#[test]
fn the_icons_decode_at_every_size() {
    for (px, png) in icon::COLOUR {
        let rgba = icon::decode(png).expect("valid PNG");
        assert_eq!(rgba.size, px);
        assert_eq!(rgba.rgba.len(), (px * px * 4) as usize);
        assert!(
            rgba.rgba.chunks(4).any(|p| p[3] == 255),
            "{px}: opaque pixels"
        );
        assert!(
            rgba.rgba.chunks(4).any(|p| p[3] == 0),
            "{px}: transparent corners"
        );
    }
    // The nearest size at or above the request.
    assert_eq!(icon::colour(16).size, 16);
    assert_eq!(icon::colour(18).size, 20);
    assert_eq!(icon::colour(30).size, 32);
    assert_eq!(icon::colour(200).size, 64);
    assert_eq!(icon::colour_all().len(), icon::COLOUR.len());
    // macOS: black on transparency only (macOS tints a template image).
    let template = icon::template();
    assert_eq!(template.size, 36);
    assert!(
        template
            .rgba
            .chunks(4)
            .all(|p| p[3] == 0 || (p[0] == 0 && p[1] == 0 && p[2] == 0))
    );
    assert!(template.rgba.chunks(4).any(|p| p[3] == 255));
    assert_eq!(
        icon::to_argb32(&[1, 2, 3, 4, 5, 6, 7, 8]),
        [4, 1, 2, 3, 8, 5, 6, 7]
    );
    // The Windows resource the exe embeds.
    let ico = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/icon/tray/superba-tray.ico"),
    )
    .unwrap();
    assert_eq!(&ico[..4], &[0, 0, 1, 0], "an icon directory");
    assert_eq!(u16::from_le_bytes([ico[4], ico[5]]), 7, "16 to 64 px");
}

// ---------------------------------------------------------------------------
// The menu model.

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

fn labels(items: &[TrayItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            TrayItem::Action { label, .. } | TrayItem::Label(label) => label.clone(),
            TrayItem::Submenu { label, .. } => format!("{label} >"),
            TrayItem::Separator => "---".into(),
        })
        .collect()
}

#[test]
fn the_menu_lists_open_the_current_line_switching_chatgpt_router_and_quit() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let service = fake();
    let data = rt.block_on(data_of(&service));
    for locale in Locale::ALL {
        let tr = Tr::new(locale);
        let m = menu::build(&tr, &data, false);
        let current = menu::current_text(&tr, &data);
        assert_eq!(
            labels(&m.items),
            [
                tr.t("tray.menu.open"),
                "---".into(),
                current.clone(),
                format!("{} >", tr.t("tray.menu.switch")),
                "---".into(),
                tr.t("tray.menu.show_app"),
                "---".into(),
                tr.t("tray.router.off"),
                tr.t("tray.router.turn_on"),
                "---".into(),
                tr.t("tray.menu.quit"),
            ],
            "{locale:?}"
        );
        // The provider and its line.
        assert!(current.starts_with("Krill · "), "{current}");
        assert_eq!(menu::tooltip(&tr, &data), format!("Superba · {current}"));
    }

    let tr = Tr::new(Locale::En);
    let m = menu::build(&tr, &data, false);
    let TrayItem::Submenu { items, enabled, .. } = &m.items[3] else {
        panic!("switch submenu");
    };
    assert!(enabled);
    // Krill has several lines: a submenu with the line in use checked and
    // not clickable; then the official login.
    let TrayItem::Submenu {
        label,
        items: lines,
        ..
    } = &items[0]
    else {
        panic!("Krill's lines: {items:?}");
    };
    assert_eq!(
        label,
        &tr.f("tray.menu.provider_current", &[("provider", &"Krill")])
    );
    let checked: Vec<_> = lines
        .iter()
        .filter_map(|l| match l {
            TrayItem::Action {
                label,
                checked: Some(true),
                enabled,
                action,
            } => Some((label.clone(), *enabled, action.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(checked.len(), 1, "{lines:?}");
    assert!(!checked[0].1, "the line in use is not clickable");
    assert!(matches!(
        &checked[0].2,
        TrayAction::Switch { profile, line: Some(line) }
            if profile.as_str() == "krill" && line.as_str() == "overseas"
    ));
    assert!(lines.len() > 1);
    for line in lines {
        let TrayItem::Action {
            action:
                TrayAction::Switch {
                    profile,
                    line: Some(_),
                },
            ..
        } = line
        else {
            panic!("line item: {line:?}");
        };
        assert_eq!(profile.as_str(), "krill");
    }
    assert_eq!(items[items.len() - 2], TrayItem::Separator);
    assert_eq!(
        items[items.len() - 1],
        TrayItem::Action {
            label: tr.t("tray.menu.official"),
            action: TrayAction::RestoreOfficial,
            enabled: true,
            checked: Some(false),
        }
    );
    // Every action is reachable; Open and Quit come first and last.
    let actions: Vec<_> = m.actions().into_iter().map(|(_, a, _)| a.clone()).collect();
    assert_eq!(actions.first(), Some(&TrayAction::Open));
    assert_eq!(actions.last(), Some(&TrayAction::Quit));
    for want in [
        TrayAction::LaunchApp,
        TrayAction::RouterPage,
        TrayAction::RestoreOfficial,
    ] {
        assert!(m.find(&want).is_some(), "{want:?}");
    }
    assert!(m.find(&TrayAction::RouterOff).is_none());
}

#[test]
fn the_menu_follows_chatgpt_the_router_and_a_running_switch() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let tr = Tr::new(Locale::En);
    let service = fake();
    // ChatGPT not running: open it through Superba; opened elsewhere: reopen.
    service.quit_app();
    let data = rt.block_on(data_of(&service));
    let m = menu::build(&tr, &data, false);
    assert!(labels(&m.items).contains(&tr.t("tray.menu.open_app")));
    service.open_app_elsewhere();
    let mut data = rt.block_on(data_of(&service));
    let m = menu::build(&tr, &data, false);
    assert!(labels(&m.items).contains(&tr.t("tray.menu.reopen_app")));

    // Router mode on: its address and "Turn router off"; a router that does
    // not answer says so.
    if let Some(o) = data.overview.value.as_mut()
        && let Some(c) = o.config.as_mut()
    {
        c.openai_base_url = Some("http://127.0.0.1:17421/v1".into());
    }
    let m = menu::build(&tr, &data, false);
    let l = labels(&m.items);
    assert!(
        l.contains(&tr.f("tray.router.on", &[("addr", &"127.0.0.1:17421")])),
        "{l:?}"
    );
    assert!(m.find(&TrayAction::RouterOff).is_some());
    data.router_watch = Some(superba_core::RouterWatchState::Suspect {
        detail: "refused".into(),
    });
    assert!(labels(&menu::build(&tr, &data, false).items).contains(&tr.t("tray.router.suspect")));

    // While a switch runs: switching, ChatGPT and the router wait; Open and
    // Quit never do.
    let m = menu::build(&tr, &data, true);
    for (label, action, enabled) in m.actions() {
        let always = matches!(
            action,
            TrayAction::Open | TrayAction::Quit | TrayAction::RouterPage
        );
        assert_eq!(enabled, always, "{label}");
    }
    let TrayItem::Submenu { enabled, .. } = &m.items[3] else {
        panic!("switch submenu");
    };
    assert!(!enabled);
    // While ChatGPT is being opened its item waits too.
    data.launching_app = true;
    let m = menu::build(&tr, &data, false);
    let TrayItem::Action { enabled, .. } = m.find(&TrayAction::LaunchApp).unwrap() else {
        unreachable!()
    };
    assert!(!enabled);
}

#[test]
fn the_menu_without_providers_or_with_the_official_login() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let tr = Tr::new(Locale::En);
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run: true,
        ..Default::default()
    }));
    let mut data = rt.block_on(data_of(&service));
    data.overview.value.as_mut().unwrap().active = None;
    assert_eq!(menu::current_text(&tr, &data), tr.t("tray.current.none"));
    let m = menu::build(&tr, &data, false);
    // Only the official login to switch to.
    let TrayItem::Submenu { items, .. } = &m.items[3] else {
        panic!("{:?}", m.items);
    };
    assert_eq!(labels(items), [tr.t("tray.menu.official")]);
    // Labels with mnemonics characters stay as they are in the model (the
    // platform renderers escape them).
    assert!(m.actions().iter().all(|(l, ..)| !l.is_empty()));
}

// ---------------------------------------------------------------------------
// The whole app with a simulated tray.

fn app(service: Arc<FakeService>, tray: TrayOption, start_in_tray: bool) -> AppHarness {
    AppHarness::with_options(
        service,
        AppOptions {
            theme: Some(egui::Theme::Light),
            locale: Some(Locale::En),
            page: Some(Page::Overview),
            tray,
            start_in_tray,
            ..AppOptions::default()
        },
    )
}

fn saved_close_action(service: &FakeService) -> Option<CloseAction> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(service.profile_set())
        .unwrap()
        .settings
        .close_action
}

fn hides(commands: &[ViewportCommand]) -> bool {
    commands.contains(&ViewportCommand::CancelClose)
        && commands.contains(&ViewportCommand::Visible(false))
}

#[test]
fn closing_the_window_asks_hides_into_the_tray_and_the_tray_brings_it_back() {
    let tr = Tr::new(Locale::En);
    let service = fake();
    let sink = Sink::default();
    let mut h = app(service.clone(), TrayOption::Simulated(sink.clone()), false);
    h.wait_for(Some(Role::Tab), &tr.t(Page::Overview.title_key()));
    frames_until(&mut h, "the tray menu names the line in use", |h| {
        menu_labels(h).iter().any(|l| l.starts_with("Krill · "))
    });
    assert!(h.app().tray_tooltip().starts_with("Superba · Krill · "));
    h.take_commands();

    // The first close: Windows and Linux ask, macOS keeps running.
    let t = h.close_window();
    if cfg!(target_os = "macos") {
        assert!(hides(&h.take_commands()));
        assert!(!h.visible());
    } else {
        assert!(t.has(&tr.t("tray.prompt.title")), "{}", t.dump());
        let commands = h.take_commands();
        assert!(
            commands.contains(&ViewportCommand::CancelClose),
            "{commands:?}"
        );
        assert!(h.visible());
        assert_eq!(saved_close_action(&service), None);
        h.click(Some(Role::Button), &tr.t("tray.prompt.keep"));
        let commands = h.take_commands();
        assert!(
            commands.contains(&ViewportCommand::Visible(false)),
            "{commands:?}"
        );
        assert!(!h.visible());
        h.until("the answer is saved", |_| {
            saved_close_action(&service) == Some(CloseAction::Tray)
        });
    }
    assert!(h.app().is_hidden_in_tray());

    // "Open Superba" from the tray.
    sink.push(TrayEvent::Action(TrayAction::Open));
    h.settle();
    let commands = h.take_commands();
    assert!(
        commands.contains(&ViewportCommand::Visible(true)),
        "{commands:?}"
    );
    assert!(commands.contains(&ViewportCommand::Focus));
    assert!(h.visible() && !h.app().is_hidden_in_tray());

    // Closing again hides at once (the choice is saved / the macOS default).
    h.close_window();
    assert!(hides(&h.take_commands()));
    // A click on the icon (Windows: left button) shows it too.
    sink.push(TrayEvent::Activate);
    h.settle();
    assert!(h.visible());

    // The host coming back changes nothing for a hidden window.
    h.close_window();
    assert!(!h.visible());
    sink.push(TrayEvent::HostBack);
    h.settle();
    assert!(!h.visible());
    // The tray host goes away while hidden: the window comes back.
    sink.push(TrayEvent::HostLost);
    h.settle();
    assert!(h.visible());
    // Without a tray now, closing quits.
    h.take_commands();
    h.close_window();
    let commands = h.take_commands();
    assert!(
        !commands.contains(&ViewportCommand::CancelClose),
        "{commands:?}"
    );
}

/// Frames until `done` holds for the app (at most 20 s).
fn frames_until(h: &mut AppHarness, what: &str, done: impl Fn(&AppHarness) -> bool) {
    let start = std::time::Instant::now();
    loop {
        h.frame();
        if done(h) {
            return;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "{what}: not reached in 20 s"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn menu_labels(h: &AppHarness) -> Vec<String> {
    h.app()
        .tray_menu()
        .map(|m| labels(&m.items))
        .unwrap_or_default()
}

#[test]
fn quit_from_the_tray_closes_the_hidden_window_for_real() {
    let service = fake();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(service.save_settings(Settings {
        close_action: Some(CloseAction::Tray),
        ..Settings::default()
    }))
    .unwrap();
    let sink = Sink::default();
    let mut h = app(service.clone(), TrayOption::Simulated(sink.clone()), false);
    frames_until(&mut h, "the provider is known", |h| {
        menu_labels(h).iter().any(|l| l.starts_with("Krill · "))
    });
    h.close_window();
    assert!(hides(&h.take_commands()));
    sink.push(TrayEvent::Action(TrayAction::Quit));
    h.settle();
    let commands = h.take_commands();
    assert!(commands.contains(&ViewportCommand::Close), "{commands:?}");
    // The close request that follows is not cancelled.
    h.close_window();
    assert!(!h.take_commands().contains(&ViewportCommand::CancelClose));
}

#[test]
fn quit_and_cancel_in_the_first_close_question() {
    if cfg!(target_os = "macos") {
        // macOS never asks: closing keeps Superba in the menu bar.
        return;
    }
    let tr = Tr::new(Locale::En);
    let service = fake();
    let mut h = app(
        service.clone(),
        TrayOption::Simulated(Sink::default()),
        false,
    );
    h.wait_for(Some(Role::Tab), &tr.t(Page::Overview.title_key()));
    // Cancel (Escape): nothing saved, the window stays.
    let t = h.close_window();
    assert!(t.has(&tr.t("tray.prompt.title")));
    let t = h.key(egui::Key::Escape);
    assert!(!t.has(&tr.t("tray.prompt.title")));
    assert!(h.visible());
    assert_eq!(saved_close_action(&service), None);
    // Asked again on the next close; "Quit Superba" saves Quit and closes.
    h.close_window();
    h.take_commands();
    h.click(Some(Role::Button), &tr.t("tray.prompt.quit"));
    let commands = h.take_commands();
    assert!(commands.contains(&ViewportCommand::Close), "{commands:?}");
    h.until("Quit is saved", |_| {
        saved_close_action(&service) == Some(CloseAction::Quit)
    });
}

#[test]
fn without_a_tray_closing_quits_and_preferences_says_why() {
    let tr = Tr::new(Locale::En);
    let service = fake();
    let mut h = app(service.clone(), TrayOption::Off, false);
    h.wait_for(Some(Role::Tab), &tr.t(Page::Overview.title_key()));
    let t = h.close_window();
    assert!(!t.has(&tr.t("tray.prompt.title")));
    assert!(!h.take_commands().contains(&ViewportCommand::CancelClose));
    assert!(h.app().tray_menu().is_none());

    // Preferences → Window: both rows disabled, the reason shown.
    let t = h.go(Page::Preferences, &tr);
    assert!(t.has(&tr.t("tray.prefs.section")), "{}", t.dump());
    assert!(t.has_containing(&tr.t(Unavailable::Off.reason_key())));
    for key in ["tray.prefs.close.tray", "tray.prefs.close.tray_mac"] {
        if let Some(id) = t.find(None, &tr.t(key)) {
            assert!(t.node(id).is_disabled(), "{key} disabled");
        }
    }
}

#[test]
fn preferences_saves_the_choice_and_start_in_the_tray_hides_at_once() {
    let tr = Tr::new(Locale::En);
    let service = fake();
    let mut h = app(
        service.clone(),
        TrayOption::Simulated(Sink::default()),
        false,
    );
    let t = h.go(Page::Preferences, &tr);
    assert!(t.has(&tr.t("tray.prefs.close.title")), "{}", t.dump());
    if !cfg!(target_os = "macos") {
        // Not chosen yet: the description says Superba will ask.
        assert!(t.has_containing(&tr.t("tray.prefs.close.ask")));
    }
    let keep = if cfg!(target_os = "macos") {
        "tray.prefs.close.tray_mac"
    } else {
        "tray.prefs.close.tray"
    };
    // Quit first: on macOS "keep running" is already the choice in effect
    // (unset), so choosing it again changes nothing.
    h.click(None, &tr.t("tray.prefs.close.quit"));
    h.until("Quit is saved", |_| {
        saved_close_action(&service) == Some(CloseAction::Quit)
    });
    h.click(None, &tr.t(keep));
    h.until("Tray is saved", |_| {
        saved_close_action(&service) == Some(CloseAction::Tray)
    });
    // Start hidden in the tray: off by default, a toggle in Preferences.
    assert!(!Settings::default().start_in_tray);
    let start = if cfg!(target_os = "macos") {
        "tray.prefs.start.title_mac"
    } else {
        "tray.prefs.start.title"
    };
    h.click(Some(Role::Switch), &tr.t(start));
    let rt = tokio::runtime::Runtime::new().unwrap();
    h.until("start in the tray is saved", |_| {
        rt.block_on(service.profile_set())
            .unwrap()
            .settings
            .start_in_tray
    });
    drop(h);

    // A start in the tray hides the window in its first frame ...
    let mut h = app(
        service.clone(),
        TrayOption::Simulated(Sink::default()),
        true,
    );
    h.settle();
    assert!(!h.visible());
    assert!(h.app().is_hidden_in_tray());
    drop(h);
    // ... unless there is no tray.
    let mut h = app(service, TrayOption::Off, true);
    h.settle();
    assert!(h.visible());
}

#[test]
fn a_second_start_shows_the_hidden_window() {
    let service = fake();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(service.save_settings(Settings {
        close_action: Some(CloseAction::Tray),
        ..Settings::default()
    }))
    .unwrap();
    let mut h = AppHarness::with_options(
        service,
        AppOptions {
            theme: Some(egui::Theme::Light),
            locale: Some(Locale::En),
            single_instance: true,
            tray: TrayOption::Simulated(Sink::default()),
            ..AppOptions::default()
        },
    );
    frames_until(&mut h, "the provider is known", |h| {
        menu_labels(h).iter().any(|l| l.starts_with("Krill · "))
    });
    h.close_window();
    assert!(!h.visible(), "hidden in the tray");
    // What a plain `superba` start does first.
    assert!(instance::forward(&a11y_harness::data_dir(), Request::Show).is_some());
    frames_until(&mut h, "the second start showed the window", |h| {
        h.visible()
    });
    assert_eq!(Request::parse("show"), Some(Request::Show));
    assert_eq!(Request::Show.as_str(), "show");
}

#[test]
fn the_tray_status_names_why_there_is_no_tray() {
    for why in [
        Unavailable::Off,
        Unavailable::NoHost,
        Unavailable::CannotHide,
        Unavailable::Failed("x".into()),
    ] {
        let status = TrayStatus::Unavailable(why.clone());
        assert!(!status.can_hide());
        assert_eq!(status.hide_support(), None);
        for locale in Locale::ALL {
            let text = Tr::new(locale).t(why.reason_key());
            assert_ne!(text, why.reason_key(), "{locale:?}: {why:?} is translated");
        }
    }
    assert!(TrayStatus::Shown(HideSupport::InPlace).can_hide());
    assert_eq!(
        TrayStatus::default(),
        TrayStatus::Unavailable(Unavailable::Off)
    );
}

#[test]
fn switching_from_the_tray_and_a_question_brings_the_hidden_window_back() {
    let tr = Tr::new(Locale::En);
    let service = fake();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(service.save_settings(Settings {
        close_action: Some(CloseAction::Tray),
        countdown_secs: 0,
        ..Settings::default()
    }))
    .unwrap();
    let sink = Sink::default();
    let mut h = app(service.clone(), TrayOption::Simulated(sink.clone()), false);
    frames_until(&mut h, "the provider is known", |h| {
        menu_labels(h).iter().any(|l| l.starts_with("Krill · "))
    });
    // A hot switch from the tray while hidden: no window needed.
    h.close_window();
    assert!(!h.visible());
    let line = |s: &FakeService| {
        rt.block_on(s.overview())
            .unwrap()
            .active
            .and_then(|a| a.line_id)
            .map(|l| l.to_string())
    };
    assert_eq!(line(&service).as_deref(), Some("overseas"));
    sink.push(TrayEvent::Action(TrayAction::Switch {
        profile: "krill".into(),
        line: Some("cdn".into()),
    }));
    frames_until(&mut h, "switched from the tray", |_| {
        line(&service).as_deref() == Some("cdn")
    });
    // The menu and tooltip follow the line in use.
    let cdn = tr.t("line.krill.cdn");
    frames_until(&mut h, "the menu names the new line", |h| {
        menu_labels(h).contains(&format!("Krill · {cdn}"))
    });
    assert_eq!(h.app().tray_tooltip(), format!("Superba · Krill · {cdn}"));
    assert!(!h.visible(), "a hot switch needs no window");

    // A line the plan refuses (China Express has no PAT endpoints): the
    // window comes back with the reason.
    sink.push(TrayEvent::Action(TrayAction::Switch {
        profile: "krill".into(),
        line: Some("cn-fast".into()),
    }));
    frames_until(&mut h, "the blocked plan shows the window", |h| h.visible());
    h.key(egui::Key::Escape);
    h.close_window();
    assert!(!h.visible());

    // ChatGPT opened elsewhere: a switch needs a confirmed full restart, so
    // the hidden window comes back with the question.
    service.open_app_elsewhere();
    sink.push(TrayEvent::Action(TrayAction::Switch {
        profile: "krill".into(),
        line: Some("overseas".into()),
    }));
    frames_until(&mut h, "the question shows the window", |h| h.visible());
    let t = h.settle();
    assert!(
        t.has(&tr.t("confirm.title")) || t.has(&tr.t("confirm.title_reopen")),
        "{}",
        t.dump()
    );
    h.key(egui::Key::Escape);

    // "Turn router on…" opens the Router page.
    h.close_window();
    assert!(!h.visible());
    sink.push(TrayEvent::Action(TrayAction::RouterPage));
    frames_until(&mut h, "the Router page shows", |h| h.visible());
    let t = h.settle();
    assert!(t.has(&tr.t("page.router.subtitle")), "{}", t.dump());
}
