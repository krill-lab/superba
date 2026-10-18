//! The sender end to end against local mock servers: every channel kind,
//! signatures on the wire, failures, redaction, rate limits and the log.

mod support;

use std::sync::Arc;

use pretty_assertions::assert_eq;
use superba_core::{
    Catalog, ChannelKind, ChannelOutcome, EventKind, Locale, NotifyChannel, NotifyEvent,
    NotifySecrets, NotifySettings, NotifySkip, Secret, SecretStore, Timestamp, TurnTokens,
};
use superba_notify::{Dispatch, Notifier, PolicyInput, secret_entry, secrets_to_secret, sign};
use support::{MemSecrets, MockServer};

const NOW_MS: i64 = 1_792_335_642_000;

fn event() -> NotifyEvent {
    let mut e = NotifyEvent::new(
        EventKind::TurnComplete,
        "codex",
        Timestamp::from_millis(NOW_MS),
    );
    e.cwd = Some("/home/me/src/superba".into());
    e.title = Some("Fix the tray icon".into());
    e.host = Some("devbox".into());
    e.turn_id = Some("turn-1".into());
    e.thread_id = Some("thread-1".into());
    e.duration_ms = Some(252_000);
    e.tokens = Some(TurnTokens {
        input: 120_300,
        cached: 98_000,
        output: 3_200,
    });
    e.summary = Some("The icon now uses the 22 px master.".into());
    e
}

async fn add_channel(
    settings: &mut NotifySettings,
    store: &MemSecrets,
    kind: ChannelKind,
    params: &[(&str, String)],
    secrets: &[(&str, String)],
) -> String {
    let id = format!("{}-{}", kind.id(), settings.channels.len());
    let mut c = NotifyChannel::new(id.clone(), kind, kind.id());
    for (k, v) in params {
        c.params.insert((*k).into(), v.clone());
    }
    let mut s = NotifySecrets::default();
    for (k, v) in secrets {
        s.set(*k, Secret::new(v.clone()));
    }
    c.key_ref = Some(
        store
            .put(&secret_entry(&id), &secrets_to_secret(&s))
            .await
            .unwrap(),
    );
    settings.channels.push(c);
    id
}

fn input(force: bool) -> PolicyInput {
    PolicyInput {
        now_ms: NOW_MS,
        local_minute: Some(12 * 60),
        only_channel: None,
        force,
    }
}

#[tokio::test]
async fn every_channel_kind_delivers_to_its_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemSecrets::default();
    let tg = MockServer::start(200, r#"{"ok":true,"result":{"message_id":5}}"#).await;
    let feishu = MockServer::start(200, r#"{"code":0,"msg":"success"}"#).await;
    let wecom = MockServer::start(200, r#"{"errcode":0,"errmsg":"ok"}"#).await;
    let ding = MockServer::start(200, r#"{"errcode":0,"errmsg":"ok"}"#).await;
    let sc = MockServer::start(200, r#"{"code":0,"message":"","data":{"pushid":"1"}}"#).await;
    let pp = MockServer::start(200, r#"{"code":200,"msg":"请求成功","data":"x"}"#).await;
    let bark = MockServer::start(200, r#"{"code":200,"message":"success"}"#).await;
    let ntfy = MockServer::start(200, r#"{"id":"abc","event":"message"}"#).await;
    let hook = MockServer::start(204, "").await;

    let mut settings = NotifySettings {
        enabled: true,
        ..Default::default()
    };
    let s = |v: &str| v.to_owned();
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Telegram,
        &[("chat_id", s("4242")), ("api_base", tg.base.clone())],
        &[("bot_token", s("123:TOKEN"))],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Feishu,
        &[],
        &[
            (
                "webhook",
                format!("{}/open-apis/bot/v2/hook/fs-hook", feishu.base),
            ),
            ("sign_secret", s("fs-secret")),
        ],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::WeCom,
        &[],
        &[(
            "webhook",
            format!("{}/cgi-bin/webhook/send?key=wc-key", wecom.base),
        )],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::DingTalk,
        &[],
        &[
            (
                "webhook",
                format!("{}/robot/send?access_token=dt-token", ding.base),
            ),
            ("sign_secret", s("SECdt")),
        ],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::ServerChan,
        &[("api_base", sc.base.clone())],
        &[("send_key", s("SCTsendkey"))],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::PushPlus,
        &[("api_base", pp.base.clone())],
        &[("token", s("pp-token"))],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Bark,
        &[("server", bark.base.clone())],
        &[("device_key", s("bark-device"))],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Ntfy,
        &[("server", ntfy.base.clone())],
        &[("topic", s("sb-topic")), ("token", s("tk_ntfy"))],
    )
    .await;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Webhook,
        &[],
        &[
            ("url", format!("{}/hooks/in", hook.base)),
            ("sign_secret", s("wh-secret")),
        ],
    )
    .await;

    let catalog = Catalog::new(Locale::ZhCn);
    let notifier = Notifier::new(dir.path());
    let report = notifier
        .dispatch(
            &event(),
            Dispatch {
                settings: &settings,
                catalog: &catalog,
                policy: input(false),
                secrets: &store,
            },
        )
        .await;
    for c in &report.channels {
        assert!(matches!(c.outcome, ChannelOutcome::Sent { .. }), "{c:?}");
    }
    assert_eq!(report.sent(), 9);

    let r = &tg.taken()[0];
    assert_eq!(
        (r.method.as_str(), r.target.as_str()),
        ("POST", "/bot123:TOKEN/sendMessage")
    );
    let b = r.json();
    assert_eq!(b["chat_id"], "4242");
    let text = b["text"].as_str().unwrap();
    assert!(text.starts_with("<b>✅ 任务完成 · Codex</b>"), "{text}");
    assert!(text.contains("项目：superba"));
    assert!(text.contains("会话：Fix the tray icon"));
    assert!(
        text.contains("4 分 12 秒 · 输入 120.3k（缓存 98.0k）· 输出 3.2k"),
        "{text}"
    );
    // The reply summary is off by default.
    assert!(!text.contains("22 px"));

    let r = &feishu.taken()[0];
    assert_eq!(r.target, "/open-apis/bot/v2/hook/fs-hook");
    let b = r.json();
    let ts: i64 = b["timestamp"].as_str().unwrap().parse().unwrap();
    assert_eq!(b["sign"], sign::feishu("fs-secret", ts));
    assert_eq!(
        b["content"]["post"]["zh_cn"]["title"],
        "✅ 任务完成 · Codex"
    );

    let r = &wecom.taken()[0];
    assert_eq!(r.target, "/cgi-bin/webhook/send?key=wc-key");
    assert_eq!(r.json()["msgtype"], "text");

    let r = &ding.taken()[0];
    let u = url::Url::parse(&format!("http://x{}", r.target)).unwrap();
    let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
    let ts: i64 = q["timestamp"].parse().unwrap();
    assert_eq!(q["sign"], sign::dingtalk("SECdt", ts));
    assert_eq!(q["access_token"], "dt-token");

    assert_eq!(sc.taken()[0].target, "/SCTsendkey.send");
    assert_eq!(pp.taken()[0].target, "/send");
    assert_eq!(pp.taken()[0].json()["token"], "pp-token");
    assert_eq!(bark.taken()[0].target, "/push");
    assert_eq!(bark.taken()[0].json()["device_key"], "bark-device");
    let r = &ntfy.taken()[0];
    assert_eq!(r.header("authorization"), Some("Bearer tk_ntfy"));
    assert_eq!(r.json()["topic"], "sb-topic");

    let r = &hook.taken()[0];
    assert_eq!(r.target, "/hooks/in");
    let ts: i64 = r.header("x-superba-timestamp").unwrap().parse().unwrap();
    assert_eq!(
        r.header("x-superba-signature").unwrap(),
        sign::webhook("wh-secret", ts, &r.body)
    );
    assert_eq!(r.json()["data"]["project"], "superba");

    // The log names channels and outcomes, never tokens or text.
    let log = std::fs::read_to_string(dir.path().join("log.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 9);
    for secret in [
        "123:TOKEN",
        "fs-secret",
        "wc-key",
        "dt-token",
        "SCTsendkey",
        "pp-token",
        "bark-device",
        "sb-topic",
        "tk_ntfy",
        "wh-secret",
        "tray icon",
    ] {
        assert!(!log.contains(secret), "{secret} in the log");
    }
}

#[tokio::test]
async fn failures_are_reported_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemSecrets::default();
    // A server that echoes the URL it was called with in its error.
    let echo = MockServer::start_with(|r| {
        (
            400,
            format!(
                r#"{{"ok":false,"description":"bad token in {}"}}"#,
                r.target
            ),
        )
    })
    .await;
    let mut settings = NotifySettings {
        enabled: true,
        ..Default::default()
    };
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Telegram,
        &[("chat_id", "1".into()), ("api_base", echo.base.clone())],
        &[("bot_token", "999:SECRETTOKEN".into())],
    )
    .await;
    // Nothing listens on this port.
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Webhook,
        &[],
        &[("url", "http://127.0.0.1:9/hook?token=URLSECRET".into())],
    )
    .await;
    let catalog = Catalog::new(Locale::En);
    let report = Notifier::new(dir.path())
        .dispatch(
            &event(),
            Dispatch {
                settings: &settings,
                catalog: &catalog,
                policy: input(false),
                secrets: &store,
            },
        )
        .await;
    assert_eq!(report.failed(), 2);
    for c in &report.channels {
        let ChannelOutcome::Failed { error } = &c.outcome else {
            panic!("{c:?}");
        };
        assert!(
            !error.contains("SECRETTOKEN") && !error.contains("URLSECRET"),
            "{error}"
        );
    }
    let ChannelOutcome::Failed { error } = &report.channels[0].outcome else {
        unreachable!()
    };
    assert!(
        error.contains("400 bad token in /bot••••/sendMessage"),
        "{error}"
    );
}

#[tokio::test]
async fn rate_limit_drops_and_counts_and_the_test_bypasses_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemSecrets::default();
    let tg = MockServer::start(200, r#"{"ok":true}"#).await;
    let mut settings = NotifySettings {
        enabled: true,
        ..Default::default()
    };
    settings.rate_limit.per_minute = 2;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Telegram,
        &[("chat_id", "1".into()), ("api_base", tg.base.clone())],
        &[("bot_token", "t:1".into())],
    )
    .await;
    let catalog = Catalog::new(Locale::En);
    let notifier = Notifier::new(dir.path());
    let mut outcomes = Vec::new();
    for i in 0..4 {
        let mut e = event();
        e.turn_id = Some(format!("turn-{i}"));
        let report = notifier
            .dispatch(
                &e,
                Dispatch {
                    settings: &settings,
                    catalog: &catalog,
                    policy: input(false),
                    secrets: &store,
                },
            )
            .await;
        outcomes.push(report.channels[0].outcome.clone());
    }
    assert!(matches!(outcomes[0], ChannelOutcome::Sent { .. }));
    assert!(matches!(outcomes[1], ChannelOutcome::Sent { .. }));
    assert_eq!(
        outcomes[2],
        ChannelOutcome::Skipped {
            reason: NotifySkip::RateLimited
        }
    );
    assert_eq!(
        outcomes[3],
        ChannelOutcome::Skipped {
            reason: NotifySkip::RateLimited
        }
    );
    // The same turn again is a duplicate.
    let mut again = event();
    again.turn_id = Some("turn-0".into());
    let report = notifier
        .dispatch(
            &again,
            Dispatch {
                settings: &settings,
                catalog: &catalog,
                policy: input(false),
                secrets: &store,
            },
        )
        .await;
    assert_eq!(report.skipped, Some(NotifySkip::Duplicate));
    // A minute later the next message says how many were dropped.
    let mut later = input(false);
    later.now_ms += 61_000;
    let mut e = event();
    e.turn_id = Some("turn-9".into());
    notifier
        .dispatch(
            &e,
            Dispatch {
                settings: &settings,
                catalog: &catalog,
                policy: later,
                secrets: &store,
            },
        )
        .await;
    let last = tg.taken().last().unwrap().json();
    assert!(
        last["text"]
            .as_str()
            .unwrap()
            .contains("(2 earlier notifications were not sent: rate limit)")
    );
    // The test button ignores the limit and the master switch.
    settings.enabled = false;
    let mut t = NotifyEvent::new(EventKind::Test, "superba", Timestamp::from_millis(NOW_MS));
    t.detail = None;
    let report = notifier
        .dispatch(
            &t,
            Dispatch {
                settings: &settings,
                catalog: &catalog,
                policy: input(true),
                secrets: &store,
            },
        )
        .await;
    assert_eq!(report.sent(), 1);
}

#[tokio::test]
async fn summary_is_sent_only_when_turned_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MemSecrets::default());
    let tg = MockServer::start(200, r#"{"ok":true}"#).await;
    let mut settings = NotifySettings {
        enabled: true,
        ..Default::default()
    };
    settings.content.summary = true;
    settings.content.summary_chars = 30;
    add_channel(
        &mut settings,
        &store,
        ChannelKind::Telegram,
        &[("chat_id", "1".into()), ("api_base", tg.base.clone())],
        &[("bot_token", "t:1".into())],
    )
    .await;
    let catalog = Catalog::new(Locale::En);
    let mut e = event();
    e.summary = Some("Line one\n\nline two of a fairly long reply that goes on".into());
    Notifier::new(dir.path())
        .dispatch(
            &e,
            Dispatch {
                settings: &settings,
                catalog: &catalog,
                policy: input(false),
                secrets: store.as_ref(),
            },
        )
        .await;
    let text = tg.taken()[0].json()["text"].as_str().unwrap().to_owned();
    assert!(
        text.contains("Reply: Line one line two of a fairly…"),
        "{text}"
    );
}
