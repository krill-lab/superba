#![allow(missing_docs)]
//! Preferences → Notifications (GH#12): the channels with their masked
//! secrets and the third-party mark, the test button reaching `notify_send`
//! (forced, one channel) and its result under the row, the add dialog's check
//! in the user's language, the palette entries and the hooks block.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{ChannelKind, EventKind, Locale, NotifyChannel, Settings};
use superba_gui::pages::{Action, Page, notify, preferences};
use support::fake::{FakeOptions, FakeService};

fn settings() -> Settings {
    let mut s = Settings::default();
    s.notify.enabled = true;
    let mut tg = NotifyChannel::new("telegram-a1", ChannelKind::Telegram, "My phone");
    tg.params.insert("chat_id".into(), "518302047".into());
    tg.secret_hints
        .insert("bot_token".into(), "••••x9Qz".into());
    let mut wx = NotifyChannel::new("serverchan-b2", ChannelKind::ServerChan, "WeChat");
    wx.secret_hints.insert("send_key".into(), "••••okey".into());
    s.notify.channels = vec![tg, wx];
    s
}

#[test]
fn channels_test_button_and_hooks() {
    let service = Arc::new(FakeService::new(FakeOptions::default()));
    // Tall enough for the whole section below the scrolled-to title.
    let mut h = Harness::new([1280.0, 3600.0], Locale::ZhCn, service.clone(), false);
    let mut prefs = settings();
    let mut state = notify::State::default();
    // The palette's "Notifications" scrolls the section into view.
    h.frame(|_, cx, _| notify::command(&mut state, cx, "notify:show"));
    let f = h.until(
        |ui, cx, out| preferences::show(ui, cx, &mut prefs, &mut state, out),
        |f| f.has("My phone") && f.has("在 Codex 中信任"),
    );
    for want in [
        "通知",
        "发送通知",
        "My phone",
        "WeChat",
        "第三方",
        "chat_id=518302047",
        "bot_token=••••x9Qz",
        "发送哪些事件",
        "回复开头和待批准的命令",
        "Codex：任务完成",
        "已安装",
        "在 Codex 中信任",
        "复制钩子配置",
    ] {
        assert!(f.has(want), "{want} missing:\n{}", f.all_text());
    }
    // No secret value is ever drawn, only the masked hints.
    assert!(!f.has("x9Qzx"));

    // The first channel's test button: one forced send to that channel.
    let test = f
        .texts
        .iter()
        .filter(|(t, _)| t == "测试")
        .map(|(_, r)| *r)
        .next()
        .expect("a test button");
    h.click_at(test.center());
    let f = h.until(
        |ui, cx, out| preferences::show(ui, cx, &mut prefs, &mut state, out),
        |f| f.has("已送达（12 毫秒）"),
    );
    let sends = service.notify_sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0.kind, EventKind::Test);
    assert!(sends[0].1.force);
    assert_eq!(sends[0].1.channel.as_deref(), Some("telegram-a1"));
    assert!(f.actions.iter().any(|a| matches!(a, Action::Toast { .. })));
}

#[test]
fn the_add_dialog_checks_fields_in_the_users_language() {
    let service = Arc::new(FakeService::new(FakeOptions::default()));
    let mut h = Harness::new([1280.0, 900.0], Locale::ZhCn, service.clone(), false);
    let mut prefs = settings();
    let mut state = notify::State::default();
    h.frame(|_, cx, _| notify::command(&mut state, cx, "notify:add"));
    assert!(state.editor().is_some());
    let f = h.until(
        |ui, cx, out| preferences::show(ui, cx, &mut prefs, &mut state, out),
        |f| f.has("添加通知渠道"),
    );
    assert!(f.has("机器人 Token") && f.has("Chat ID") && f.has("@BotFather"));
    h.click_text(&f, "保存");
    let f = h.until(
        |ui, cx, out| preferences::show(ui, cx, &mut prefs, &mut state, out),
        |f| f.has("请填写机器人 Token"),
    );
    assert!(f.has("添加通知渠道"), "the dialog stays open");
    assert!(service.notify_saves().is_empty(), "nothing saved");
}

#[test]
fn palette_entries_open_the_section() {
    let service = Arc::new(FakeService::new(FakeOptions::default()));
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service, false);
    h.frame(|_, cx, _| {
        let items = notify::palette(cx);
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles[..3], ["Notifications", "Add channel", "Test all"]);
        assert!(titles.contains(&"Add a notification channel: WeChat via ServerChan"));
        assert_eq!(items.len(), 3 + ChannelKind::ALL.len());
        assert!(items.iter().all(|i| matches!(
            &i.action,
            Action::PageCommand {
                page: Page::Preferences,
                ..
            }
        )));
    });
}

#[test]
fn adding_a_kind_from_the_palette_opens_its_form() {
    let service = Arc::new(FakeService::new(FakeOptions::default()));
    let mut h = Harness::new([1280.0, 900.0], Locale::ZhCn, service, false);
    let mut prefs = settings();
    let mut state = notify::State::default();
    h.frame(|_, cx, _| notify::command(&mut state, cx, "notify:add:serverchan"));
    assert_eq!(
        state.editor().map(|e| e.kind),
        Some(ChannelKind::ServerChan)
    );
    let f = h.until(
        |ui, cx, out| preferences::show(ui, cx, &mut prefs, &mut state, out),
        |f| f.has("SendKey"),
    );
    // Said plainly: the message goes through a third party.
    assert!(f.has("第三方服务"), "{}", f.all_text());
    assert!(f.has("个人微信没有官方机器人接口"));
}
