#![allow(missing_docs)]

use std::collections::HashSet;
use std::ffi::OsString;

use pretty_assertions::assert_eq;
use superba_core::Locale;
use superba_gui::args::{GuiArgs, PAGE_NAMES, USAGE, parse_page};
use superba_gui::pages::Page;

fn parse(flags: &[&str]) -> Result<GuiArgs, String> {
    let args: Vec<OsString> = std::iter::once("superba")
        .chain(flags.iter().copied())
        .map(OsString::from)
        .collect();
    GuiArgs::parse(&args)
}

#[test]
fn defaults_leave_all_overrides_disabled() {
    let expected = GuiArgs {
        theme: None,
        locale: None,
        size: None,
        page: None,
        launch_chatgpt: false,
        start_in_tray: false,
        autostart: false,
        command: None,
    };
    assert_eq!(GuiArgs::default(), expected);
    assert_eq!(parse(&[]).unwrap(), expected);
    assert_eq!(
        GuiArgs::parse(&[OsString::from("/some/path/superba")]).unwrap(),
        expected
    );
}

#[test]
fn demo_flags_are_gone() {
    for flag in ["--demo", "--first-run", "--scene", "--wizard-step"] {
        let error = parse(&[flag, "overview"]).unwrap_err();
        assert!(error.contains("unknown argument"), "{flag}: {error}");
    }
    assert!(!USAGE.contains("demo"));
}

#[test]
fn page_opens_a_page_by_name() {
    assert_eq!(
        parse(&["--page", "models"]).unwrap(),
        GuiArgs {
            page: Some(Page::Models),
            ..GuiArgs::default()
        }
    );
    assert_eq!(
        parse(&["--page=PROVIDERS"]).unwrap().page,
        Some(Page::Providers)
    );
    let error = parse(&["--page", "lines"]).unwrap_err();
    assert!(error.contains("unknown page"), "{error}");
}

#[test]
fn theme_accepts_light_and_dark_in_separate_and_inline_forms() {
    for (flags, theme) in [
        (vec!["--theme", "light"], egui::Theme::Light),
        (vec!["--theme=dark"], egui::Theme::Dark),
        (vec!["--theme", "DARK"], egui::Theme::Dark),
        (vec!["--theme=LIGHT"], egui::Theme::Light),
    ] {
        assert_eq!(
            parse(&flags).unwrap(),
            GuiArgs {
                theme: Some(theme),
                ..GuiArgs::default()
            }
        );
    }
}

#[test]
fn invalid_theme_reports_the_bad_value() {
    let error = parse(&["--theme", "sepia"]).unwrap_err();
    assert!(
        error.contains("unknown theme") && error.contains("sepia"),
        "{error}"
    );
}

#[test]
fn language_accepts_chinese_english_and_locale_alias() {
    for (flags, locale) in [
        (vec!["--lang", "zh-CN"], Locale::ZhCn),
        (vec!["--lang=en"], Locale::En),
        (vec!["--locale", "zh_CN.UTF-8"], Locale::ZhCn),
    ] {
        assert_eq!(
            parse(&flags).unwrap(),
            GuiArgs {
                locale: Some(locale),
                ..GuiArgs::default()
            }
        );
    }
}

#[test]
fn invalid_language_reports_the_bad_value() {
    let error = parse(&["--lang=not-a-language"]).unwrap_err();
    assert!(
        error.contains("unknown language") && error.contains("not-a-language"),
        "{error}"
    );
}

#[test]
fn sizes_accept_lowercase_and_uppercase_separators() {
    assert_eq!(
        parse(&["--size", "1280x800"]).unwrap().size,
        Some([1280.0, 800.0])
    );
    assert_eq!(
        parse(&["--size=900X700"]).unwrap().size,
        Some([900.0, 700.0])
    );
    assert_eq!(
        parse(&["--size", "100x100"]).unwrap().size,
        Some([320.0, 240.0])
    );
}

#[test]
fn malformed_sizes_report_which_part_is_invalid() {
    for (value, message) in [
        ("1280", "expects WxH"),
        ("widex800", "bad width"),
        ("1280xtall", "bad height"),
        ("1280x800x600", "bad height"),
        ("x800", "bad width"),
        ("1280x", "bad height"),
    ] {
        let error = parse(&["--size", value]).unwrap_err();
        assert!(
            error.contains(message) && error.contains(value),
            "{value}: {error}"
        );
    }
}

#[test]
fn unknown_flag_is_an_error() {
    let error = parse(&["--unknown"]).unwrap_err();
    assert!(
        error.contains("unknown argument") && error.contains("--unknown"),
        "{error}"
    );
}

#[test]
fn every_value_flag_reports_a_missing_value() {
    for flag in ["--page", "--theme", "--lang", "--locale", "--size"] {
        let error = parse(&[flag]).unwrap_err();
        let reported_flag = if flag == "--locale" { "--lang" } else { flag };
        assert!(
            error.contains(reported_flag) && error.contains("needs a value"),
            "{flag}: {error}"
        );
    }
}

#[test]
fn page_names_round_trip_cover_every_page_and_appear_in_usage() {
    let mut names = HashSet::new();
    let words: HashSet<_> = USAGE.split_whitespace().collect();
    for (page, name) in PAGE_NAMES {
        assert!(names.insert(name), "duplicate page name {name}");
        assert_eq!(parse_page(name), Some(page));
        assert_eq!(parse_page(&name.to_ascii_uppercase()), Some(page));
        assert!(words.contains(name), "USAGE omits page {name}");
    }
    assert_eq!(names.len(), Page::DESTINATIONS.len());
    // Names of pages that no longer exist are unknown.
    for name in ["connection", "sites", "targets", "preferences", "more"] {
        assert_eq!(parse_page(name), None, "{name}");
        let error = parse(&["--page", name]).unwrap_err();
        assert!(error.contains("unknown page"), "{name}: {error}");
    }
    for page in Page::DESTINATIONS {
        assert!(
            PAGE_NAMES.iter().any(|(p, _)| *p == page),
            "{page:?} has no name"
        );
    }
}

#[test]
fn flags_compose_and_later_values_override_earlier_ones() {
    let args = parse(&[
        "--page",
        "hosts",
        "--theme",
        "light",
        "--theme=dark",
        "--lang=zh-CN",
        "--size",
        "1280x800",
    ])
    .unwrap();
    assert_eq!(
        args,
        GuiArgs {
            theme: Some(egui::Theme::Dark),
            locale: Some(Locale::ZhCn),
            size: Some([1280.0, 800.0]),
            page: Some(Page::Hosts),
            launch_chatgpt: false,
            start_in_tray: false,
            autostart: false,
            command: None,
        }
    );
}

#[test]
fn launch_chatgpt_opens_chatgpt_through_superba() {
    // GH#7: the "ChatGPT (Superba)" shortcut and the Linux desktop action.
    assert_eq!(
        parse(&["--launch-chatgpt"]).unwrap(),
        GuiArgs {
            launch_chatgpt: true,
            ..GuiArgs::default()
        }
    );
    // With other GUI flags (a window that opens anyway uses them).
    let args = parse(&["--lang", "zh-CN", "--launch-chatgpt"]).unwrap();
    assert!(args.launch_chatgpt);
    assert_eq!(args.locale, Some(Locale::ZhCn));
    // It takes no value.
    let error = parse(&["--launch-chatgpt=yes"]).unwrap_err();
    assert!(error.contains("unknown argument"), "{error}");
    // `superba --gui-help` documents it.
    assert!(USAGE.contains("--launch-chatgpt"));
    // Not a subcommand: `superba --launch-chatgpt` reaches the GUI's parser.
    let argv: Vec<OsString> = ["superba", "--launch-chatgpt"]
        .into_iter()
        .map(OsString::from)
        .collect();
    assert!(!superba_cli::is_cli_invocation(&argv));
}

#[test]
fn start_in_tray_opens_hidden_in_the_tray() {
    // Item 70: Superba starts itself so after a Wayland window was closed into
    // the tray; also usable from an autostart entry.
    assert_eq!(
        parse(&["--start-in-tray"]).unwrap(),
        GuiArgs {
            start_in_tray: true,
            ..GuiArgs::default()
        }
    );
    let args = parse(&["--start-in-tray", "--lang", "en"]).unwrap();
    assert!(args.start_in_tray);
    assert_eq!(args.locale, Some(Locale::En));
    let error = parse(&["--start-in-tray=1"]).unwrap_err();
    assert!(error.contains("unknown argument"), "{error}");
    assert!(USAGE.contains("--start-in-tray"));
    let argv: Vec<OsString> = ["superba", "--start-in-tray"]
        .into_iter()
        .map(OsString::from)
        .collect();
    assert!(!superba_cli::is_cli_invocation(&argv));
}

#[test]
fn login_start_is_distinct_from_forcing_the_tray() {
    let args = parse(&["--autostart"]).unwrap();
    assert!(args.autostart);
    assert!(!args.start_in_tray);
    assert!(parse(&["--autostart=true"]).is_err());
}

#[test]
fn command_runs_a_page_command_after_the_page() {
    let args = parse(&["--page", "providers", "--command", "import"]).unwrap();
    assert_eq!(args.command.as_deref(), Some("import"));
    assert!(parse(&["--command"]).is_err());
}
