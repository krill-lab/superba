//! Rendered text in both languages and the content options.

use pretty_assertions::assert_eq;
use superba_core::{Catalog, EventKind, Locale, NotifyContent, NotifyEvent, Timestamp, TurnTokens};
use superba_notify::message::{compact, cost_text, cut, cut_bytes, render};

fn event() -> NotifyEvent {
    let mut e = NotifyEvent::new(
        EventKind::TurnFailed,
        "claude-code",
        Timestamp::from_millis(0),
    );
    e.cwd = Some(r"C:\Users\me\src\web-app\".into());
    e.title = Some("Upgrade\nthe   router".into());
    e.host = Some("winbox".into());
    e.detail = Some("429 rate limited".into());
    e.duration_ms = Some(3_725_000);
    e.tokens = Some(TurnTokens {
        input: 950,
        cached: 0,
        output: 12,
    });
    e.cost_micro_usd = Some(4_200);
    e.model = Some("gpt-6.1-sol".into());
    e.summary = Some("private".into());
    e
}

#[test]
fn english_and_chinese() {
    let content = NotifyContent::default();
    let en = render(&event(), &content, &Catalog::new(Locale::En), 0);
    assert_eq!(en.title, "❌ Task failed · Claude Code");
    assert_eq!(
        en.lines,
        vec![
            "Project: web-app",
            "Thread: Upgrade the router",
            "Host: winbox",
            "Error: 429 rate limited",
            "1 h 2 min · in 950 · out 12 · $0.0042 · gpt-6.1-sol",
        ]
    );
    let zh = render(&event(), &content, &Catalog::new(Locale::ZhCn), 3);
    assert_eq!(zh.title, "❌ 任务出错 · Claude Code");
    assert_eq!(zh.lines[0], "项目：web-app");
    assert_eq!(zh.lines[3], "错误：429 rate limited");
    assert_eq!(
        zh.lines[4],
        "1 小时 2 分 · 输入 950 · 输出 12 · $0.0042 · gpt-6.1-sol"
    );
    assert_eq!(zh.lines[5], "（之前另有 3 条通知因频率限制未发送）");
    assert!(!zh.text().contains("private"));
}

#[test]
fn content_options_remove_lines() {
    let content = NotifyContent {
        host: false,
        project: false,
        usage: false,
        summary: true,
        summary_chars: 200,
    };
    let m = render(&event(), &content, &Catalog::new(Locale::En), 0);
    assert_eq!(m.lines, vec!["Error: 429 rate limited", "Reply: private"]);
    assert!(!m.urgent);
    let mut approval = event();
    approval.kind = EventKind::NeedsApproval;
    assert!(render(&approval, &content, &Catalog::new(Locale::En), 0).urgent);
}

#[test]
fn number_and_text_helpers() {
    assert_eq!(
        (compact(999), compact(12_345), compact(1_250_000)),
        ("999".into(), "12.3k".into(), "1.2M".into())
    );
    assert_eq!(
        (cost_text(420_000), cost_text(12_300_000), cost_text(31)),
        ("$0.420".into(), "$12.30".into(), "$0.0000".into())
    );
    assert_eq!(cut("abcdef", 4), "abc…");
    assert_eq!(cut("abc", 4), "abc");
    let s = cut_bytes("汉汉汉", 7);
    assert!(s.len() <= 7 && s.ends_with('…'), "{s}");
}
