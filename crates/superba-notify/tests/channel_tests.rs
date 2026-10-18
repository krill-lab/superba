//! Channel payloads, signatures and answer checks (pure).

use pretty_assertions::assert_eq;
use superba_core::{
    ChannelKind, EventKind, NotifyChannel, NotifyEvent, NotifySecrets, Secret, Timestamp,
};
use superba_notify::channels::{self, BuildContext, allowed_url, serverchan_url};
use superba_notify::message::Message;
use superba_notify::sign;

fn msg(kind: EventKind) -> Message {
    Message {
        kind,
        title: "✅ Task finished · Codex".into(),
        lines: vec!["Project: superba".into(), "Host: mac <1> & co".into()],
        urgent: kind.urgent(),
    }
}

fn event() -> NotifyEvent {
    let mut e = NotifyEvent::new(EventKind::TurnComplete, "codex", Timestamp::from_millis(1));
    e.cwd = Some("/home/me/work/superba".into());
    e.summary = Some("secret reply text".into());
    e.turn_id = Some("t1".into());
    e
}

fn cx(e: &NotifyEvent) -> BuildContext<'_> {
    BuildContext {
        event: e,
        now_ms: 1_700_000_000_123,
        zh: false,
        include_summary: false,
    }
}

fn channel(kind: ChannelKind, params: &[(&str, &str)]) -> NotifyChannel {
    let mut c = NotifyChannel::new("c1", kind, "test");
    for (k, v) in params {
        c.params.insert((*k).into(), (*v).into());
    }
    c
}

fn secrets(pairs: &[(&str, &str)]) -> NotifySecrets {
    let mut s = NotifySecrets::default();
    for (k, v) in pairs {
        s.set(*k, Secret::new(*v));
    }
    s
}

#[test]
fn signatures_match_reference_values() {
    // Reference values computed independently (Python hmac/hashlib).
    assert_eq!(
        sign::feishu("demo-secret", 1_599_360_473),
        "3/MaVZ8JLIy4TUG+7KSFJqvUkTKd+HWY8g+56DZWq8s="
    );
    assert_eq!(
        sign::dingtalk("SEC0123456789abcdef", 1_700_000_000_000),
        "TSZbRFUuvaSQaRKUpF970OPCb2/LcQAP3wOvwZIzBZk="
    );
    assert_eq!(
        sign::webhook("whsec", 1_700_000_000, br#"{"a":1}"#),
        "sha256=8ad37ba156048ae0e0a5533c75cdf26fee88b07f93cb57ee4c80adb053012032"
    );
}

#[test]
fn telegram_uses_the_bot_api_with_escaped_html() {
    let e = event();
    let c = channel(
        ChannelKind::Telegram,
        &[("chat_id", "12345"), ("thread_id", "7")],
    );
    let r = channels::build(
        &c,
        &secrets(&[("bot_token", "123:ABC")]),
        &msg(e.kind),
        cx(&e),
    )
    .unwrap();
    assert_eq!(r.url, "https://api.telegram.org/bot123:ABC/sendMessage");
    let b = r.json_body().unwrap();
    assert_eq!(b["chat_id"], "12345");
    assert_eq!(b["parse_mode"], "HTML");
    assert_eq!(b["message_thread_id"], 7);
    assert_eq!(
        b["text"],
        "<b>✅ Task finished · Codex</b>\nProject: superba\nHost: mac &lt;1&gt; &amp; co"
    );
    // The token never shows in Debug output.
    assert!(!format!("{r:?}").contains("ABC"));
}

#[test]
fn feishu_post_is_signed_when_a_secret_is_set() {
    let e = event();
    let c = channel(ChannelKind::Feishu, &[]);
    let url = "https://open.feishu.cn/open-apis/bot/v2/hook/abc";
    let plain = channels::build(&c, &secrets(&[("webhook", url)]), &msg(e.kind), cx(&e)).unwrap();
    let b = plain.json_body().unwrap();
    assert_eq!(plain.url, url);
    assert_eq!(b["msg_type"], "post");
    assert_eq!(
        b["content"]["post"]["en_us"]["title"],
        "✅ Task finished · Codex"
    );
    assert_eq!(
        b["content"]["post"]["en_us"]["content"][0][0]["text"],
        "Project: superba"
    );
    assert!(b.get("sign").is_none());
    let signed = channels::build(
        &c,
        &secrets(&[("webhook", url), ("sign_secret", "demo-secret")]),
        &msg(e.kind),
        BuildContext {
            now_ms: 1_599_360_473_000,
            zh: true,
            ..cx(&e)
        },
    )
    .unwrap();
    let b = signed.json_body().unwrap();
    assert_eq!(b["timestamp"], "1599360473");
    assert_eq!(b["sign"], "3/MaVZ8JLIy4TUG+7KSFJqvUkTKd+HWY8g+56DZWq8s=");
    assert!(b["content"]["post"].get("zh_cn").is_some());
}

#[test]
fn wecom_sends_text_cut_to_2048_bytes() {
    let e = event();
    let c = channel(ChannelKind::WeCom, &[]);
    let url = "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=k";
    let mut long = msg(e.kind);
    long.lines = vec!["汉".repeat(2000)];
    let r = channels::build(&c, &secrets(&[("webhook", url)]), &long, cx(&e)).unwrap();
    let b = r.json_body().unwrap();
    assert_eq!(b["msgtype"], "text");
    let content = b["text"]["content"].as_str().unwrap();
    assert!(content.len() <= 2048, "{}", content.len());
    assert!(content.ends_with('…'));
}

#[test]
fn dingtalk_signs_in_the_query_and_adds_the_keyword() {
    let e = event();
    let c = channel(ChannelKind::DingTalk, &[("keyword", "Superba通知")]);
    let url = "https://oapi.dingtalk.com/robot/send?access_token=tok";
    let r = channels::build(
        &c,
        &secrets(&[("webhook", url), ("sign_secret", "SEC0123456789abcdef")]),
        &msg(e.kind),
        BuildContext {
            now_ms: 1_700_000_000_000,
            ..cx(&e)
        },
    )
    .unwrap();
    let u = url::Url::parse(&r.url).unwrap();
    let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
    assert_eq!(q["access_token"], "tok");
    assert_eq!(q["timestamp"], "1700000000000");
    assert_eq!(q["sign"], "TSZbRFUuvaSQaRKUpF970OPCb2/LcQAP3wOvwZIzBZk=");
    let b = r.json_body().unwrap();
    assert_eq!(b["msgtype"], "markdown");
    assert!(
        b["markdown"]["text"]
            .as_str()
            .unwrap()
            .starts_with("Superba通知")
    );
    assert!(
        b["markdown"]["title"]
            .as_str()
            .unwrap()
            .starts_with("Superba通知")
    );
}

#[test]
fn serverchan_picks_the_endpoint_from_the_key() {
    assert_eq!(
        serverchan_url("SCT12345abc"),
        "https://sctapi.ftqq.com/SCT12345abc.send"
    );
    assert_eq!(
        serverchan_url("sctp1234tABCdef"),
        "https://1234.push.ft07.com/send/sctp1234tABCdef.send"
    );
    let e = event();
    let c = channel(ChannelKind::ServerChan, &[]);
    let r = channels::build(
        &c,
        &secrets(&[("send_key", "SCTkey")]),
        &msg(e.kind),
        cx(&e),
    )
    .unwrap();
    assert_eq!(r.url, "https://sctapi.ftqq.com/SCTkey.send");
    let b = r.json_body().unwrap();
    assert_eq!(b["title"], "✅ Task finished · Codex");
    assert_eq!(b["desp"], "Project: superba\n\nHost: mac <1> & co");
    // A key with URL characters is refused instead of building a strange URL.
    assert!(channels::build(&c, &secrets(&[("send_key", "a/b?c")]), &msg(e.kind), cx(&e)).is_err());
}

#[test]
fn pushplus_bark_and_ntfy_payloads() {
    let e = event();
    let p = channels::build(
        &channel(ChannelKind::PushPlus, &[("topic", "team")]),
        &secrets(&[("token", "pp")]),
        &msg(e.kind),
        cx(&e),
    )
    .unwrap();
    assert_eq!(p.url, "https://www.pushplus.plus/send");
    let b = p.json_body().unwrap();
    assert_eq!(
        (
            b["token"].as_str(),
            b["template"].as_str(),
            b["topic"].as_str()
        ),
        (Some("pp"), Some("txt"), Some("team"))
    );

    let mut urgent = msg(EventKind::NeedsApproval);
    urgent.title = "✋ Needs approval · Claude Code".into();
    let bark = channels::build(
        &channel(ChannelKind::Bark, &[]),
        &secrets(&[("device_key", "dk")]),
        &urgent,
        cx(&e),
    )
    .unwrap();
    assert_eq!(bark.url, "https://api.day.app/push");
    let b = bark.json_body().unwrap();
    assert_eq!(b["device_key"], "dk");
    assert_eq!(b["group"], "Superba");
    assert_eq!(b["level"], "timeSensitive");

    let ntfy = channels::build(
        &channel(
            ChannelKind::Ntfy,
            &[("server", "https://ntfy.example.org/")],
        ),
        &secrets(&[("topic", "my-secret-topic"), ("token", "tk_1")]),
        &urgent,
        cx(&e),
    )
    .unwrap();
    assert_eq!(ntfy.url, "https://ntfy.example.org/");
    assert_eq!(ntfy.header("authorization"), Some("Bearer tk_1"));
    let b = ntfy.json_body().unwrap();
    assert_eq!(b["topic"], "my-secret-topic");
    assert_eq!(b["priority"], 4);
    assert_eq!(b["tags"][0], "raised_hand");
}

#[test]
fn webhook_sends_the_event_without_the_summary_unless_asked() {
    let e = event();
    let c = channel(ChannelKind::Webhook, &[]);
    let s = secrets(&[
        ("url", "https://hooks.example.org/x"),
        ("sign_secret", "whsec"),
    ]);
    let r = channels::build(&c, &s, &msg(e.kind), cx(&e)).unwrap();
    let b = r.json_body().unwrap();
    assert_eq!(b["source"], "superba");
    assert_eq!(b["event"], "turn_complete");
    assert_eq!(b["data"]["project"], "superba");
    assert!(b["data"].get("summary").is_none());
    assert!(b["data"].get("cwd").is_none());
    let ts = r
        .header("x-superba-timestamp")
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert_eq!(ts, 1_700_000_000);
    assert_eq!(
        r.header("x-superba-signature").unwrap(),
        sign::webhook("whsec", ts, &r.body)
    );
    let with = channels::build(
        &c,
        &s,
        &msg(e.kind),
        BuildContext {
            include_summary: true,
            ..cx(&e)
        },
    )
    .unwrap();
    assert_eq!(
        with.json_body().unwrap()["data"]["summary"],
        "secret reply text"
    );
}

#[test]
fn urls_must_be_https_or_loopback_http() {
    assert!(allowed_url("https://open.feishu.cn/x"));
    assert!(allowed_url("http://127.0.0.1:8080/x"));
    assert!(allowed_url("http://localhost/x"));
    assert!(allowed_url("http://[::1]:9/x"));
    assert!(!allowed_url("http://example.org/x"));
    assert!(!allowed_url("ftp://example.org"));
    assert!(!allowed_url("not a url"));
    let e = event();
    let err = channels::build(
        &channel(ChannelKind::WeCom, &[]),
        &secrets(&[("webhook", "http://qyapi.weixin.qq.com/x")]),
        &msg(e.kind),
        cx(&e),
    )
    .unwrap_err();
    assert_eq!(err.0.key, "notify.error.bad_url");
    let missing = channels::build(
        &channel(ChannelKind::Telegram, &[]),
        &secrets(&[("bot_token", "t")]),
        &msg(e.kind),
        cx(&e),
    )
    .unwrap_err();
    assert_eq!(missing.0.key, "notify.error.missing_field");
}

#[test]
fn validate_names_the_first_missing_field() {
    let c = channel(ChannelKind::Telegram, &[]);
    let err = channels::validate(&c, &secrets(&[("bot_token", "x")])).unwrap_err();
    assert_eq!(err.0.key, "notify.error.missing_field");
    assert!(
        channels::validate(
            &channel(ChannelKind::Telegram, &[("chat_id", "1")]),
            &secrets(&[("bot_token", "x")])
        )
        .is_ok()
    );
    let bad = channel(ChannelKind::Bark, &[("server", "http://bark.example.org")]);
    assert_eq!(
        channels::validate(&bad, &secrets(&[("device_key", "k")]))
            .unwrap_err()
            .0
            .key,
        "notify.error.bad_url"
    );
}

#[test]
fn answers_with_error_codes_are_failures() {
    use ChannelKind as K;
    let ok = |k, s, b: &str| channels::check(k, s, b.as_bytes()).is_ok();
    assert!(ok(K::Telegram, 200, r#"{"ok":true,"result":{}}"#));
    assert!(!ok(
        K::Telegram,
        400,
        r#"{"ok":false,"description":"Bad Request: chat not found"}"#
    ));
    assert!(ok(K::Feishu, 200, r#"{"code":0,"msg":"success"}"#));
    assert!(ok(
        K::Feishu,
        200,
        r#"{"StatusCode":0,"StatusMessage":"success"}"#
    ));
    let e =
        channels::check(K::Feishu, 200, br#"{"code":19021,"msg":"sign match fail"}"#).unwrap_err();
    assert_eq!(e.key, "notify.error.refused");
    assert!(e.render_fallback().contains("19021 sign match fail"));
    assert!(ok(K::WeCom, 200, r#"{"errcode":0,"errmsg":"ok"}"#));
    assert!(!ok(
        K::WeCom,
        200,
        r#"{"errcode":93000,"errmsg":"invalid webhook url"}"#
    ));
    assert!(!ok(
        K::DingTalk,
        200,
        r#"{"errcode":310000,"errmsg":"sign not match"}"#
    ));
    assert!(ok(
        K::ServerChan,
        200,
        r#"{"code":0,"message":"","data":{}}"#
    ));
    assert!(!ok(
        K::ServerChan,
        200,
        r#"{"code":40001,"message":"bad pushkey"}"#
    ));
    assert!(ok(K::PushPlus, 200, r#"{"code":200,"msg":"请求成功"}"#));
    assert!(!ok(
        K::PushPlus,
        200,
        r#"{"code":903,"msg":"无效的用户token"}"#
    ));
    assert!(ok(K::Bark, 200, r#"{"code":200,"message":"success"}"#));
    assert!(!ok(
        K::Bark,
        200,
        r#"{"code":400,"message":"failed to get device token"}"#
    ));
    assert!(ok(K::Ntfy, 200, r#"{"id":"x","event":"message"}"#));
    assert!(!ok(
        K::Ntfy,
        403,
        r#"{"code":40301,"http":403,"error":"forbidden"}"#
    ));
    assert!(ok(K::Webhook, 204, ""));
    assert!(!ok(K::Webhook, 500, "oops"));
}
