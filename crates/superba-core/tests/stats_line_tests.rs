#![allow(missing_docs)]
//! The default stats line template is a marker, resolved in the UI language
//! (user bug 23); an edited template is kept as written.

use pretty_assertions::assert_eq;
use superba_core::{Locale, RouterStatsSettings, StatsLineSettings, StatsMetric};

const ZH: &str = "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out";
const EN: &str = "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out";

#[test]
fn the_default_template_resolves_in_each_ui_language() {
    let line = StatsLineSettings::default();
    assert_eq!(line.template, None);
    assert_eq!(line.custom_template(), None);
    assert_eq!(line.resolved_template(Locale::ZhCn), ZH);
    assert_eq!(line.resolved_template(Locale::En), EN);
    // The metric checkboxes build the default in the same language.
    let mut more = line.clone();
    more.metrics.push(StatsMetric::Cached);
    assert_eq!(
        more.resolved_template(Locale::ZhCn),
        "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out · 缓存 {cached}"
    );
    assert_eq!(
        more.resolved_template(Locale::En),
        "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out · cached {cached}"
    );
}

#[test]
fn the_default_is_stored_as_a_marker_and_an_edited_template_verbatim() {
    let mut line = StatsLineSettings {
        enabled: true,
        ..StatsLineSettings::default()
    };
    // Nothing materialised: the stored settings carry no template text.
    let json = serde_json::to_value(&line).unwrap();
    assert!(json.get("template").is_none(), "{json}");
    for input in [
        "",
        "  ",
        "default",
        " Default ",
        "⚡",
        "⚡ ",
        "⚡\u{2060} default",
    ] {
        line.template = Some("⚡ {tps}".into());
        line.set_template(input);
        assert_eq!(line.template, None, "{input:?}");
    }
    let edited = "⚡ {tps} tok/s · 首字 {ttft}  (mine)";
    line.set_template(edited);
    assert_eq!(line.template.as_deref(), Some(edited));
    assert_eq!(line.custom_template(), Some(edited));
    for locale in Locale::ALL {
        assert_eq!(line.resolved_template(locale), edited);
    }
    // A stored `default` (e.g. a hand-edited settings file) is the marker too.
    line.template = Some("default".into());
    assert_eq!(line.custom_template(), None);
    assert_eq!(line.resolved_template(Locale::ZhCn), ZH);
}

#[test]
fn stored_stats_settings_round_trip_with_the_marker() {
    let mut stats = RouterStatsSettings::default();
    stats.line.enabled = true;
    let back: RouterStatsSettings =
        serde_json::from_str(&serde_json::to_string(&stats).unwrap()).unwrap();
    assert_eq!(back, stats);
    assert_eq!(back.line.template, None);
}
