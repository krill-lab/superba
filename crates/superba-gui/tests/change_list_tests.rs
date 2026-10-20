#![allow(missing_docs)]

//! The "write settings.json?" plans show every change whole (user report,
//! alpha.8: long `env.ANTHROPIC_*` keys took the row and the values were cut to
//! four characters, 「http…」 「••••n…」 「（无…」). In the Claude Code plan and
//! the Tools review, at 960x640 and 1280x800, in Chinese and English, with the
//! longest keys and values a relay produces: no text is elided, no text leaves
//! the dialog sideways, every value is drawn in full (masked keys as
//! `••••a3f9`), a removal ends in the full "(none)" placeholder, and the list
//! takes the dialog's width.

mod router_harness;
mod support;

use std::sync::Arc;

use egui::{Rect, pos2};
use router_harness::{Frame, Harness};
use superba_core::Locale;
use superba_core::harness::{HarnessChange, HarnessId, HarnessPlan, HarnessPlanKind};
use superba_gui::design::widgets::DIALOG_WINDOW_MARGIN;
use superba_gui::design::widgets::change_list::ARROW;
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::{claude, tools};
use support::fake::{FakeOptions, FakeService};

const SIZES: [[f32; 2]; 2] = [[960.0, 640.0], [1280.0, 800.0]];
const LOCALES: [Locale; 2] = [Locale::ZhCn, Locale::En];

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn change(slot: &str, before: Option<&str>, after: Option<&str>, secret: bool) -> HarnessChange {
    HarnessChange {
        file: "settings".into(),
        slot: slot.into(),
        before: before.map(Into::into),
        after: after.map(Into::into),
        drift: false,
        secret,
    }
}

/// The rows of the user's screenshot, with long values.
fn changes() -> Vec<HarnessChange> {
    let mut drifted = change(
        "env.ANTHROPIC_DEFAULT_SONNET_MODEL",
        Some("orca/claude-sonnet-4-6-20260214-thinking-extended"),
        Some("orca/claude-sonnet-4-7-20260901-thinking-extended-1m"),
        false,
    );
    drifted.drift = true;
    vec![
        change(
            "env.ANTHROPIC_BASE_URL",
            Some("https://relay.internal.example.com/v1/anthropic/old-gateway"),
            Some(
                "https://relay.internal.example.com/claude/v2/anthropic-compatible/messages-endpoint",
            ),
            false,
        ),
        change(
            "env.ANTHROPIC_AUTH_TOKEN",
            Some("••••9c1e"),
            Some("••••a3f9"),
            true,
        ),
        change(
            "env.ANTHROPIC_DEFAULT_OPUS_MODEL",
            None,
            Some("orca/claude-opus-4-7-20260901-thinking-extended-context-1m"),
            false,
        ),
        change(
            "env.ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
            Some("effort,thinking,interleaved-thinking,context-1m"),
            None,
            false,
        ),
        drifted,
        change(
            "env.ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
            None,
            Some("effort,thinking"),
            false,
        ),
    ]
}

fn plan(harness: HarnessId) -> HarnessPlan {
    HarnessPlan {
        kind: HarnessPlanKind::Apply,
        harness,
        provider: None,
        protocol: None,
        files: Vec::new(),
        changes: changes(),
        notes: Vec::new(),
        desired: None,
    }
}

/// The value text each change draws.
fn value_text(c: &HarnessChange, none: &str) -> String {
    match (&c.before, &c.after) {
        (Some(b), Some(a)) => format!("{b}{ARROW}{a}"),
        (Some(b), None) => format!("{b}{ARROW}{none}"),
        (None, Some(a)) => a.clone(),
        (None, None) => none.to_owned(),
    }
}

fn check(
    f: &Frame,
    size: [f32; 2],
    width: f32,
    at: &str,
    key: impl Fn(&HarnessChange) -> String,
    none: &str,
) {
    let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(size[0], size[1]));
    assert!(f.elided.is_empty(), "{at}: elided {:?}", f.elided);
    // Nothing leaves its clip rectangle or the window sideways (the body
    // scrolls vertically, so rows below the fold are clipped from below).
    for (t, r, clip) in &f.clipped_texts {
        if t.trim().is_empty() {
            continue;
        }
        assert!(
            r.min.x >= clip.min.x - 0.5 && r.max.x <= clip.max.x + 0.5,
            "{at}: {t:?} overflows its clip {r:?} / {clip:?}"
        );
        assert!(
            r.min.x >= screen.min.x && r.max.x <= screen.max.x,
            "{at}: {t:?} leaves the window"
        );
    }
    let mut list_left = f32::MAX;
    let mut list_right = f32::MIN;
    for c in &plan(HarnessId::ClaudeCode).changes {
        let k = key(c);
        let v = value_text(c, none);
        let (_, kr, _) = f
            .clipped_texts
            .iter()
            .find(|(t, _, _)| *t == k)
            .unwrap_or_else(|| panic!("{at}: key {k:?} not drawn whole:\n{}", f.all_text()));
        let (_, vr, _) = f
            .clipped_texts
            .iter()
            .find(|(t, _, _)| *t == v)
            .unwrap_or_else(|| panic!("{at}: value {v:?} not drawn whole:\n{}", f.all_text()));
        list_left = list_left.min(kr.min.x);
        list_right = list_right.max(vr.max.x).max(kr.max.x);
    }
    // The first row is on screen.
    let first = value_text(&changes()[0], none);
    let (_, r, clip) = f
        .clipped_texts
        .iter()
        .find(|(t, _, _)| *t == first)
        .unwrap();
    assert!(
        clip.expand(0.5).contains_rect(*r),
        "{at}: first value hidden"
    );
    // The list uses the dialog's width: the long URL wraps near its right edge.
    let span = width.min(size[0] - 2.0 * (DIALOG_WINDOW_MARGIN + 24.0));
    assert!(
        list_right - list_left > 0.8 * span,
        "{at}: the list ends early ({list_left}..{list_right} of {span})"
    );
}

#[test]
fn the_claude_plan_shows_every_key_and_value_whole() {
    for locale in LOCALES {
        let tr = Tr::new(locale);
        let none = tr.t("claude.plan.absent");
        for size in SIZES {
            let mut h =
                Harness::new(size, locale, fake(), true).min_height(Page::ClaudeCode.min_height());
            let mut state = claude::State::default();
            h.until(
                |ui, cx, out| claude::show(ui, cx, &mut state, Page::ClaudeCode, out),
                |f| f.has(&tr.t("claude.slots.title")),
            );
            state.connect.plan = Some(plan(HarnessId::ClaudeCode));
            let f = h.settle(|ui, cx, out| claude::show(ui, cx, &mut state, Page::ClaudeCode, out));
            let at = format!("claude {locale:?} {size:?}");
            assert!(f.has(&tr.t("claude.plan.title")), "{at}");
            assert!(f.has(&tr.t("claude.plan.removed")), "{at}: removal badge");
            assert!(f.has(&tr.t("claude.plan.drift")), "{at}: drift badge");
            check(&f, size, 760.0, &at, |c| c.slot.clone(), &none);
        }
    }
}

#[test]
fn the_tools_review_shows_every_key_and_value_whole() {
    for locale in LOCALES {
        let tr = Tr::new(locale);
        let none = tr.t("tools.absent");
        for size in SIZES {
            let mut h = Harness::new(size, locale, fake(), true);
            let mut state = tools::State::default();
            state.plan = Some(plan(HarnessId::Gemini));
            let f = h.settle(|ui, cx, _| tools::detail::review(ui, cx, &mut state));
            let at = format!("tools {locale:?} {size:?}");
            assert!(f.has(&tr.t("tools.review_apply")), "{at}");
            assert!(f.has(&tr.t("tools.removed")), "{at}: removal badge");
            check(
                &f,
                size,
                720.0,
                &at,
                |c| format!("{} · {}", c.file, c.slot),
                &none,
            );
        }
    }
}
