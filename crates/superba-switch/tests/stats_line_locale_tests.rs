//! User bug 23: the default stats line follows the UI language. The default is
//! stored as a marker and resolved in the UI language when the settings reach
//! the router (the language the GUI reports, else the chosen one, else
//! `SUPERBA_LOCALE`, else the system's); a change of the UI language pushes the
//! newly resolved default to a configured router; an edited template is never
//! re-resolved.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{Locale, MapEnv, RouterStatsSettings, SwitchService};
use superba_router::StatsConfig;

const ZH: &str = "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out";
const EN: &str = "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out";

/// A world whose environment names `env_locale` and whose router is configured.
fn world(env_locale: &str) -> World {
    let mut w = World::new();
    w.ctx.env = Arc::new(MapEnv::new().with("SUPERBA_LOCALE", env_locale));
    *w.router.port.lock().unwrap() = Some(18_431);
    w
}

fn line_on() -> RouterStatsSettings {
    let mut stats = RouterStatsSettings::default();
    stats.line.enabled = true;
    stats
}

/// The templates the router was given so far (as the router builds them).
fn sent(w: &World) -> Vec<String> {
    let configured = w.router.stats_configured.lock().unwrap().clone();
    let locales = w.router.stats_locales.lock().unwrap().clone();
    assert_eq!(configured.len(), locales.len());
    configured
        .into_iter()
        .zip(locales)
        .map(|((settings, prices), locale)| {
            StatsConfig::from_settings(&settings, prices, locale)
                .line
                .map(|l| l.template)
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test]
async fn the_default_line_reaches_the_router_in_the_ui_language() {
    let w = world("zh-CN");
    let sw = w.switcher();
    sw.router_set_stats(line_on()).await.unwrap();
    assert_eq!(sent(&w), [ZH]);
    // Stored as the marker, not as the text the router got.
    let stored = w.profiles.set.lock().unwrap().settings.router.stats.clone();
    assert_eq!(stored.line.template, None);
    // The language chosen in Preferences wins over the environment.
    w.profiles.set.lock().unwrap().settings.locale = Some(Locale::En);
    sw.router_set_stats(line_on()).await.unwrap();
    assert_eq!(sent(&w), [ZH, EN]);
    // The language the running UI reports wins over both.
    sw.set_ui_locale(Locale::ZhCn).await.unwrap();
    sw.router_set_stats(line_on()).await.unwrap();
    assert_eq!(sent(&w).last().map(String::as_str), Some(ZH));
}

#[tokio::test]
async fn a_ui_language_change_pushes_the_newly_resolved_default() {
    let w = world("en");
    w.profiles.set.lock().unwrap().settings.router.stats = line_on();
    let sw = w.switcher();
    // The GUI starts in the language the router already uses: nothing to push.
    sw.set_ui_locale(Locale::En).await.unwrap();
    assert!(sent(&w).is_empty());
    // The user switches the UI to Chinese: the running router follows.
    sw.set_ui_locale(Locale::ZhCn).await.unwrap();
    assert_eq!(sent(&w), [ZH]);
    // A repeated report changes nothing.
    sw.set_ui_locale(Locale::ZhCn).await.unwrap();
    assert_eq!(sent(&w), [ZH]);
    // And back.
    sw.set_ui_locale(Locale::En).await.unwrap();
    assert_eq!(sent(&w), [ZH, EN]);
    // The stored settings never change.
    let stored = w.profiles.set.lock().unwrap().settings.router.stats.clone();
    assert_eq!(stored, line_on());
}

#[tokio::test]
async fn an_edited_template_a_line_that_is_off_or_no_router_is_not_pushed() {
    // Edited template: the router keeps it verbatim in every language.
    let w = world("en");
    let mut edited = line_on();
    edited.line.set_template("⚡ {tps} tok/s (mine)");
    w.profiles.set.lock().unwrap().settings.router.stats = edited.clone();
    let sw = w.switcher();
    sw.set_ui_locale(Locale::ZhCn).await.unwrap();
    assert!(sent(&w).is_empty());
    sw.router_set_stats(edited.clone()).await.unwrap();
    assert_eq!(sent(&w), ["⚡ {tps} tok/s (mine)"]);
    sw.set_ui_locale(Locale::En).await.unwrap();
    assert_eq!(sent(&w).len(), 1);

    // The line is off: nothing language-dependent reaches the router.
    let w = world("en");
    let sw = w.switcher();
    sw.set_ui_locale(Locale::ZhCn).await.unwrap();
    assert!(sent(&w).is_empty());

    // No router configured: the next install picks the language up.
    let w = world("en");
    *w.router.port.lock().unwrap() = None;
    w.profiles.set.lock().unwrap().settings.router.stats = line_on();
    let sw = w.switcher();
    sw.set_ui_locale(Locale::ZhCn).await.unwrap();
    assert!(sent(&w).is_empty());
}
