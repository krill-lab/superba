//! The text of a notification, rendered once per dispatch in the user's
//! language and shaped per channel by [`crate::channels`].
//!
//! ```text
//! ✅ Task finished · Codex
//! Project: superba
//! Thread: Fix the tray icon
//! Host: macbook
//! 4 min 12 s · in 120.3k (cached 98.0k) · out 3.2k · $0.42
//! Reply: Fixed: the icon now uses …
//! ```
//!
//! What a message carries besides the event follows [`NotifyContent`]; the
//! reply summary is opt-in and cut to [`NotifyContent::summary_chars`].

use superba_core::notify::harness;
use superba_core::{Catalog, EventKind, LocalizedText, NotifyContent, NotifyEvent, TurnTokens};

/// A rendered notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// What happened.
    pub kind: EventKind,
    /// First line (`✅ Task finished · Codex`).
    pub title: String,
    /// The other lines.
    pub lines: Vec<String>,
    /// Needs the user now (high priority where a channel has one).
    pub urgent: bool,
}

impl Message {
    /// Title and lines, one per line.
    pub fn text(&self) -> String {
        let mut s = self.title.clone();
        for line in &self.lines {
            s.push('\n');
            s.push_str(line);
        }
        s
    }

    /// The lines without the title.
    pub fn body(&self) -> String {
        self.lines.join("\n")
    }

    /// Title + lines with the title in bold (Markdown).
    pub fn markdown(&self) -> String {
        let mut s = format!("**{}**", escape_markdown(&self.title));
        for line in &self.lines {
            s.push_str("  \n");
            s.push_str(&escape_markdown(line));
        }
        s
    }
}

/// Emoji in front of each kind (the same in every language).
pub fn icon(kind: EventKind) -> &'static str {
    match kind {
        EventKind::TurnComplete => "✅",
        EventKind::NeedsApproval => "✋",
        EventKind::NeedsInput => "💬",
        EventKind::TurnFailed => "❌",
        EventKind::SwitchDone => "🔀",
        EventKind::SwitchFailed => "⚠️",
        EventKind::RouterMemberDown => "🔌",
        EventKind::UpdateAvailable => "⬆️",
        EventKind::Test => "🔔",
        _ => "📣",
    }
}

/// Render `event` for `catalog`'s language. `dropped` = messages of this
/// channel that the rate limit swallowed since the last one sent.
pub fn render(
    event: &NotifyEvent,
    content: &NotifyContent,
    catalog: &Catalog,
    dropped: u32,
) -> Message {
    let t = |text: LocalizedText| catalog.text(&text);
    let line = |key: &str, fallback: &str, value: &str| {
        t(LocalizedText::new(format!("notify.msg.{key}"), fallback).arg("value", value))
    };
    let title = t(
        LocalizedText::new("notify.msg.title", "{icon} {event} · {harness}")
            .arg("icon", icon(event.kind))
            .arg("event", t(event.kind.label()))
            .arg("harness", harness::display_name(&event.harness)),
    );
    let mut lines = Vec::new();
    if content.project {
        if let Some(project) = event.project() {
            lines.push(line("project", "Project: {value}", &one_line(&project, 80)));
        }
        if let Some(title) = event.title.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(line("thread", "Thread: {value}", &one_line(title, 80)));
        }
    }
    if content.host
        && let Some(host) = event.host.as_deref().filter(|s| !s.is_empty())
    {
        lines.push(line("host", "Host: {value}", host));
    }
    if let Some(detail) = event.detail.as_deref().filter(|s| !s.trim().is_empty()) {
        let detail = one_line(detail, 300);
        let (key, fallback) = match event.kind {
            EventKind::TurnFailed | EventKind::SwitchFailed => ("error", "Error: {value}"),
            EventKind::NeedsApproval | EventKind::NeedsInput => ("request", "Asks: {value}"),
            EventKind::SwitchDone => ("switch_to", "Now: {value}"),
            EventKind::RouterMemberDown => ("member", "Member: {value}"),
            EventKind::UpdateAvailable => ("version", "Version: {value}"),
            _ => ("", ""),
        };
        if key.is_empty() {
            lines.push(detail);
        } else {
            lines.push(line(key, fallback, &detail));
        }
    }
    if content.usage
        && let Some(usage) = usage_line(event, catalog)
    {
        lines.push(usage);
    }
    if content.summary
        && let Some(summary) = event.summary.as_deref().filter(|s| !s.trim().is_empty())
    {
        let summary = cut(
            &collapse_space(summary),
            content.summary_chars.max(20) as usize,
        );
        // For an approval the "summary" is the command / input asked for.
        if event.kind == EventKind::NeedsApproval {
            lines.push(line("command", "Command: {value}", &summary));
        } else {
            lines.push(line("reply", "Reply: {value}", &summary));
        }
    }
    if dropped > 0 {
        lines.push(t(LocalizedText::new(
            "notify.msg.dropped",
            "({n} earlier notifications were not sent: rate limit)",
        )
        .arg("n", dropped)));
    }
    Message {
        kind: event.kind,
        title,
        lines,
        urgent: event.kind.urgent(),
    }
}

/// `4 min 12 s · in 120.3k (cached 98.0k) · out 3.2k · $0.42 · model`.
fn usage_line(event: &NotifyEvent, catalog: &Catalog) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ms) = event.duration_ms {
        parts.push(duration(ms, catalog));
    }
    if let Some(tokens) = event.tokens.filter(|t| t.input + t.output > 0) {
        parts.push(tokens_text(tokens, catalog));
    }
    if let Some(cost) = event.cost_micro_usd {
        parts.push(cost_text(cost));
    }
    if let Some(model) = event.model.as_deref().filter(|m| !m.is_empty()) {
        parts.push(model.to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `12 s`, `4 min 12 s`, `1 h 3 min`.
pub fn duration(ms: u64, catalog: &Catalog) -> String {
    let secs = ms.div_ceil(1000);
    let text = if secs < 60 {
        LocalizedText::new("notify.msg.dur_s", "{s} s").arg("s", secs)
    } else if secs < 3600 {
        LocalizedText::new("notify.msg.dur_ms", "{m} min {s} s")
            .arg("m", secs / 60)
            .arg("s", secs % 60)
    } else {
        LocalizedText::new("notify.msg.dur_hm", "{h} h {m} min")
            .arg("h", secs / 3600)
            .arg("m", (secs % 3600) / 60)
    };
    catalog.text(&text)
}

fn tokens_text(t: TurnTokens, catalog: &Catalog) -> String {
    let text = if t.cached > 0 {
        LocalizedText::new(
            "notify.msg.tokens",
            "in {input} (cached {cached}) · out {output}",
        )
        .arg("input", compact(t.input))
        .arg("cached", compact(t.cached))
        .arg("output", compact(t.output))
    } else {
        LocalizedText::new("notify.msg.tokens_plain", "in {input} · out {output}")
            .arg("input", compact(t.input))
            .arg("output", compact(t.output))
    };
    catalog.text(&text)
}

/// `$0.42`, `$0.0031`, `$12.30`.
pub fn cost_text(micro_usd: u64) -> String {
    let usd = micro_usd as f64 / 1_000_000.0;
    if usd >= 1.0 {
        format!("${usd:.2}")
    } else if usd >= 0.01 {
        format!("${usd:.3}")
    } else {
        format!("${usd:.4}")
    }
}

/// `950`, `12.3k`, `1.2M`.
pub fn compact(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Collapse runs of whitespace (newlines included) to one space.
pub fn collapse_space(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One line of at most `max` characters.
pub fn one_line(s: &str, max: usize) -> String {
    cut(&collapse_space(s), max)
}

/// At most `max` characters, with `…` when cut.
pub fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Cut to at most `max_bytes` UTF-8 bytes on a character boundary (WeCom and
/// DingTalk limit text by bytes).
pub fn cut_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes.saturating_sub('…'.len_utf8());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Escape the characters Markdown renderers treat specially.
pub fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape for Telegram's HTML parse mode.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
