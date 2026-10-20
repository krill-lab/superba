#![allow(missing_docs)]

//! Plan notices in dialogs (UI spec rule 8, user bug 103): exact duplicates
//! dropped, notices of one kind grouped into one row with a count, rows
//! sorted blocking first, then warnings, then information, planner order kept
//! within a severity.

mod support;

use pretty_assertions::assert_eq;
use superba_core::{Locale, LocalizedText, Notice, Severity, Timestamp};
use superba_gui::design::{fonts, tokens::Palette};
use superba_gui::i18n::Tr;
use superba_gui::pages::shared::notices::{Details, NoticeGroup, details, group, summary};
use superba_gui::pages::{Cx, Data};

fn generic(model: &str) -> Notice {
    Notice::info(
        LocalizedText::new(
            "models_note.generic_template",
            "{model}: not in the built-in catalog of this Codex version; Codex's settings for unknown models are used",
        )
        .arg("model", model),
    )
}

fn text(key: &str) -> LocalizedText {
    LocalizedText::new(key, key)
}

fn keys(groups: &[NoticeGroup]) -> Vec<(Severity, &str, usize)> {
    groups
        .iter()
        .map(|g| (g.severity, g.key.as_str(), g.items.len()))
        .collect()
}

#[test]
fn notices_of_one_kind_become_one_row_sorted_by_severity() {
    let notices = vec![
        Notice::info(text("switch.settings.no_catalog")),
        generic("gpt-5"),
        Notice::warning(text("switch.warn.model_missing")),
        generic("gpt-5.1"),
        Notice::danger(text("test.blocking")),
        generic("gpt-5.2"),
        Notice::info(text("switch.models.catalog_written")),
        Notice::warning(text("switch.warn.compaction_unknown")),
        Notice::danger(text("test.blocking.other")),
    ];
    let groups = group(&notices);
    assert_eq!(
        keys(&groups),
        vec![
            (Severity::Danger, "test.blocking", 1),
            (Severity::Danger, "test.blocking.other", 1),
            (Severity::Warning, "switch.warn.model_missing", 1),
            (Severity::Warning, "switch.warn.compaction_unknown", 1),
            (Severity::Info, "switch.settings.no_catalog", 1),
            (Severity::Info, "models_note.generic_template", 3),
            (Severity::Info, "switch.models.catalog_written", 1),
        ]
    );
    // The group keeps the planner's order of its items.
    let models: Vec<&str> = groups[5]
        .items
        .iter()
        .map(|t| t.args[0].1.as_str())
        .collect();
    assert_eq!(models, ["gpt-5", "gpt-5.1", "gpt-5.2"]);
}

#[test]
fn exact_duplicates_are_dropped_and_severities_never_mix() {
    let same = Notice::warning(text("switch.warn.cli_processes"));
    let notices = vec![
        same.clone(),
        same.clone(),
        generic("gpt-5"),
        generic("gpt-5"),
        // The same key at another severity is another row.
        Notice::warning(text("models_note.generic_template").arg("model", "gpt-6")),
    ];
    let groups = group(&notices);
    assert_eq!(
        keys(&groups),
        vec![
            (Severity::Warning, "switch.warn.cli_processes", 1),
            (Severity::Warning, "models_note.generic_template", 1),
            (Severity::Info, "models_note.generic_template", 1),
        ]
    );
}

#[test]
fn notices_without_a_key_are_not_merged() {
    let a = Notice::info(LocalizedText::new("", "first"));
    let b = Notice::info(LocalizedText::new("", "second"));
    let groups = group(&[a.clone(), b, a]);
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|g| g.items.len() == 1));
}

#[test]
fn the_varying_argument_is_found_only_when_it_is_the_only_difference() {
    let groups = group(&[generic("gpt-5"), generic("o3")]);
    assert_eq!(
        groups[0].varying_arg(),
        Some(("model", vec!["gpt-5", "o3"]))
    );
    let two = |m: &str, v: &str| {
        Notice::info(
            LocalizedText::new("models_note.filled_fields", "")
                .arg("model", m)
                .arg("version", v),
        )
    };
    // Two arguments differ: every notice is listed in full instead.
    assert_eq!(
        group(&[two("a", "1"), two("b", "2")])[0].varying_arg(),
        None
    );
    // One argument differs, the other is shared.
    assert_eq!(
        group(&[two("a", "1"), two("b", "1")])[0].varying_arg(),
        Some(("model", vec!["a", "b"]))
    );
    assert_eq!(group(&[generic("gpt-5")])[0].varying_arg(), None);
}

#[test]
fn a_group_reads_as_a_count_in_both_languages_and_lists_its_models() {
    let notices: Vec<Notice> = (0..32).map(|i| generic(&format!("gpt-5.{i}"))).collect();
    let groups = group(&notices);
    let service = support::idle_service();
    let fonts = fonts::FontReport::default();
    let data = Data::default();
    for (locale, expected) in [
        (
            Locale::En,
            "32 models are not in the built-in catalog of this Codex version; Codex's settings for unknown models are used",
        ),
        (
            Locale::ZhCn,
            "32 个模型不在当前 Codex 版本的内置目录中，使用 Codex 对未知模型的通用设置",
        ),
    ] {
        let tr = Tr::new(locale);
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(&egui::Context::default()),
            data: &data,
            busy: false,
            now: Timestamp::EPOCH,
            fonts: &fonts,
            service: &service,
            log: &[],
        };
        assert_eq!(summary(&cx, &groups[0]), expected);
        let Details::Values(values) = details(&cx, &groups[0]) else {
            panic!("model names");
        };
        assert!(values.starts_with("gpt-5.0, gpt-5.1, gpt-5.2"), "{values}");
        assert!(values.ends_with("gpt-5.31"), "{values}");
        // One notice reads as itself.
        let one = group(&[generic("o3")]);
        assert!(summary(&cx, &one[0]).starts_with("o3"));
        // A kind without its own count sentence: the first notice, "and n more".
        let other = group(&[
            Notice::info(text("switch.warn.compaction_unknown").arg("site", "A")),
            Notice::info(text("switch.warn.compaction_unknown").arg("site", "B")),
        ]);
        let line = summary(&cx, &other[0]);
        assert!(line.contains('A') && !line.contains('B'), "{line}");
        assert!(
            line.contains(if locale == Locale::En {
                "(and 1 more like it)"
            } else {
                "（另有 1 条同类）"
            }),
            "{line}"
        );
    }
}
