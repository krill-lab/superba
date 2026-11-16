#![allow(missing_docs)]

//! Saved ChatGPT accounts (PR #20): the shell's limits timer (one read at a
//! time, never overlapping, backoff after failures, the last limits kept), and
//! the official login's detail on the Providers page (rows, quota columns that
//! stay aligned for a weekly-only account, switching through the ordinary
//! flow) in both locales.

mod support;

use std::sync::Arc;
use std::time::Duration;

use superba_core::{
    ChatGptAccount, ChatGptQuota, ChatGptWindow, Error, Locale, ProfileId, RestoreTarget,
    SwitchService, Timestamp,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Cx, Data, providers};
use superba_gui::quota::{BACKOFF_MAX, BUSY_RETRY, QUOTA_EVERY, QuotaBoard, backoff};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

const T0: Timestamp = Timestamp::from_millis(1_800_000_000_000);

fn at(secs: u64) -> Timestamp {
    T0.saturating_add(Duration::from_secs(secs))
}

fn window(remaining: f64, minutes: u64) -> ChatGptWindow {
    ChatGptWindow {
        remaining,
        minutes,
        resets_at: Some(at(3 * 3600)),
    }
}

fn quota(id: &str, five: Option<f64>, weekly: Option<f64>) -> ChatGptQuota {
    ChatGptQuota {
        account_id: id.into(),
        plan_type: Some("plus".into()),
        five_hour: five.map(|r| window(r, 300)),
        weekly: weekly.map(|r| window(r, 10080)),
        fetched_at: T0,
    }
}

#[test]
fn backoff_doubles_from_the_interval_up_to_its_cap() {
    assert_eq!(backoff(1), QUOTA_EVERY);
    assert_eq!(backoff(2), QUOTA_EVERY * 2);
    assert_eq!(backoff(3), QUOTA_EVERY * 4);
    assert_eq!(backoff(4), QUOTA_EVERY * 8);
    assert_eq!(backoff(5), BACKOFF_MAX);
    assert_eq!(backoff(40), BACKOFF_MAX);
}

#[test]
fn one_read_runs_at_a_time_and_each_account_comes_back_every_minute() {
    let mut board = QuotaBoard::default();
    board.sync(["a", "b"]);
    let first = board.start_due(T0, false).unwrap();
    assert!(board.get(&first).unwrap().in_flight);
    assert_eq!(board.start_due(T0, false), None, "never two reads at once");
    board.finish(&first, Ok(quota(&first, Some(50.0), Some(80.0))), T0);
    let second = board.start_due(T0, false).unwrap();
    assert_ne!(first, second);
    board.finish(&second, Ok(quota(&second, None, Some(10.0))), T0);
    assert_eq!(board.start_due(at(59), false), None);
    assert_eq!(board.wake_in(at(59)), Some(Duration::from_secs(1)));
    assert!(board.start_due(at(60), false).is_some());
    // A switch or a sign-in pauses reads.
    let mut paused = QuotaBoard::default();
    paused.sync(["a"]);
    assert_eq!(paused.start_due(T0, true), None);
    assert_eq!(paused.start_due(T0, false).as_deref(), Some("a"));
    // A manual refresh while the read runs does not start another one.
    paused.refresh_now("a");
    assert_eq!(paused.start_due(T0, false), None);
}

#[test]
fn a_failed_read_keeps_the_last_limits_and_backs_off() {
    let mut board = QuotaBoard::default();
    board.sync(["a"]);
    board.start_due(T0, false).unwrap();
    board.finish("a", Ok(quota("a", Some(40.0), Some(70.0))), T0);
    let mut now = at(60);
    for failures in 1..=6u32 {
        assert_eq!(board.start_due(now, false).as_deref(), Some("a"));
        board.finish("a", Err(Error::other("network")), now);
        let row = board.get("a").unwrap();
        assert_eq!(row.failures, failures);
        assert!(row.error.is_some());
        let kept = row.value.as_ref().unwrap();
        assert_eq!(
            kept.fetched_at, T0,
            "the last good limits and their time stay"
        );
        assert_eq!(kept.weekly.as_ref().unwrap().remaining, 70.0);
        let due = row.next_due.unwrap();
        assert_eq!(due.duration_since(now), Some(backoff(failures)));
        assert_eq!(
            board.start_due(due.saturating_sub(Duration::from_secs(1)), false),
            None
        );
        now = due;
    }
    // A postponed read (a switch runs) is no failure and comes back soon.
    assert_eq!(board.start_due(now, false).as_deref(), Some("a"));
    board.finish(
        "a",
        Err(Error::Locked {
            what: "switch".into(),
        }),
        now,
    );
    let row = board.get("a").unwrap();
    assert_eq!(row.failures, 6);
    assert_eq!(row.next_due.unwrap().duration_since(now), Some(BUSY_RETRY));
    // The user's refresh resets the pause and runs next.
    board.refresh_now("a");
    assert_eq!(board.get("a").unwrap().failures, 0);
    assert_eq!(board.start_due(now, false).as_deref(), Some("a"));
    board.finish("a", Ok(quota("a", Some(90.0), Some(95.0))), now);
    let row = board.get("a").unwrap();
    assert!(row.error.is_none() && row.failures == 0);
    assert_eq!(row.next_due.unwrap().duration_since(now), Some(QUOTA_EVERY));
    // An account that is no longer saved goes.
    board.sync(["b"]);
    assert!(board.get("a").is_none() && board.get("b").is_some());
}

fn account(id: &str, email: &str, active: bool) -> ChatGptAccount {
    ChatGptAccount {
        id: id.into(),
        email: email.into(),
        plan_type: Some("pro".into()),
        added_at: T0,
        last_refresh: None,
        active,
        latest: active,
    }
}

/// Data with the official login applied and two saved accounts: `a` in use
/// with both windows read, `b` weekly-only.
async fn official_data(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    let o = data.overview.value.as_mut().unwrap();
    if let Some(active) = o.active.as_mut() {
        active.profile_id = Some(ProfileId::new("official"));
        active.site_id = Some(superba_core::SiteId::new("official"));
        active.line_id = None;
    }
    o.chatgpt_accounts = vec![
        account("acct-a", "a@example.com", true),
        account("acct-b", "b@example.com", false),
    ];
    data.chatgpt_quota.sync(["acct-a", "acct-b"]);
    for (id, q) in [
        ("acct-a", quota("acct-a", Some(62.0), Some(81.0))),
        ("acct-b", quota("acct-b", None, Some(7.0))),
    ] {
        assert_eq!(data.chatgpt_quota.start_due(T0, false).as_deref(), Some(id));
        data.chatgpt_quota.finish(id, Ok(q), T0);
    }
    data
}

fn draw(
    h: &Harness,
    data: &Data,
    service: &Arc<dyn SwitchService>,
    state: &mut providers::State,
    locale: Locale,
    events: Vec<egui::Event>,
    out: &mut Vec<Action>,
) -> Vec<(String, egui::Rect)> {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    h.frame(events, &mut |ui| {
        let cx = Cx {
            tr: &tr,
            pal: superba_gui::design::Palette::of(ui.ctx()),
            data,
            busy: false,
            now: T0,
            fonts: &fonts,
            service,
            log: &[],
        };
        providers::show(ui, &cx, state, out);
    })
}

fn official_state(data: &Data, service: &Arc<dyn SwitchService>) -> providers::State {
    let mut state = providers::State::default();
    let tr = Tr::new(Locale::En);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: T0,
        fonts: &fonts,
        service,
        log: &[],
    };
    providers::command(&mut state, &cx, "profile:official", &mut Vec::new());
    state
}

#[tokio::test]
async fn the_official_detail_lists_accounts_with_aligned_quota_columns() {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let data = official_data(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    for (locale, weekly, five, in_use, use_it) in [
        (
            Locale::En,
            "Weekly",
            "5 hours",
            "In use",
            "Use this account",
        ),
        (Locale::ZhCn, "每周", "5 小时", "使用中", "使用此账号"),
    ] {
        let h = Harness::new([1280.0, 860.0], egui::Theme::Light);
        let mut state = official_state(&data, &svc);
        let mut out = Vec::new();
        draw(&h, &data, &svc, &mut state, locale, vec![], &mut out);
        let texts = draw(&h, &data, &svc, &mut state, locale, vec![], &mut out);
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        // The plan the limits service reported wins over the saved claim (pro).
        for want in ["a@example.com", "b@example.com", in_use, use_it, "Plus"] {
            assert!(all.contains(&want), "{locale:?}: {want} missing in {all:?}");
        }
        let weekly_x: Vec<f32> = texts
            .iter()
            .filter(|(t, _)| t == weekly)
            .map(|(_, r)| r.min.x)
            .collect();
        assert_eq!(weekly_x.len(), 2, "{locale:?}: {all:?}");
        assert!(
            (weekly_x[0] - weekly_x[1]).abs() < 0.5,
            "{locale:?}: the weekly column stays aligned for a weekly-only account: {weekly_x:?}"
        );
        assert_eq!(
            texts.iter().filter(|(t, _)| t == five).count(),
            1,
            "{locale:?}: a window that is not reported is not drawn"
        );
        // "Use this account" plans the ordinary switch to that account.
        let rect = texts.iter().find(|(t, _)| t == use_it).unwrap().1;
        let press = |pressed| egui::Event::PointerButton {
            pos: rect.center(),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let mut clicked = Vec::new();
        draw(
            &h,
            &data,
            &svc,
            &mut state,
            locale,
            vec![egui::Event::PointerMoved(rect.center()), press(true)],
            &mut clicked,
        );
        draw(
            &h,
            &data,
            &svc,
            &mut state,
            locale,
            vec![press(false)],
            &mut clicked,
        );
        assert!(
            clicked.iter().any(|a| matches!(
                a,
                Action::Restore(RestoreTarget::Account { identity }) if identity == "acct-b"
            )),
            "{locale:?}: {clicked:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_read_says_which_limits_are_shown() {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let mut data = official_data(&service).await;
    data.chatgpt_quota.refresh_now("acct-b");
    assert_eq!(
        data.chatgpt_quota.start_due(at(120), false).as_deref(),
        Some("acct-b"),
        "a manual refresh runs next"
    );
    data.chatgpt_quota
        .finish("acct-b", Err(Error::other("offline")), at(120));
    let svc: Arc<dyn SwitchService> = service.clone();
    let h = Harness::new([1280.0, 860.0], egui::Theme::Dark);
    let mut state = official_state(&data, &svc);
    let mut out = Vec::new();
    draw(&h, &data, &svc, &mut state, Locale::En, vec![], &mut out);
    let texts = draw(&h, &data, &svc, &mut state, Locale::En, vec![], &mut out);
    let joined: String = texts
        .iter()
        .map(|(t, _)| t.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Could not read the limits"), "{joined}");
    assert!(
        joined.contains("7% left"),
        "the weekly-only account keeps its limits: {joined}"
    );
}
