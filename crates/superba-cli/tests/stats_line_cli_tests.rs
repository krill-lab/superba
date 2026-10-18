//! `spb router stats-line` (user bug 23): without arguments it shows the line's
//! settings with the default template resolved in the UI language (Superba's
//! language, not the CLI output's), `--template default` goes back to the
//! default marker, any other template is kept verbatim.

mod common;

use common::*;
use superba_core::*;

const ZH: &str = "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out";
const EN: &str = "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out";

#[tokio::test]
async fn stats_line_shows_the_default_in_the_ui_language() {
    let service = FakeService::new();
    // Superba's UI is Chinese; the CLI output stays English (SUPERBA_LOCALE=en).
    service.profiles.lock().unwrap().settings.locale = Some(Locale::ZhCn);
    let (code, stdout, stderr) = invoke(service.clone(), &["router", "stats-line"]).await;
    assert_eq!(code, 0, "{stderr}");
    for value in [
        "The stats line is off.",
        "Clients: Desktop app",
        "Style: Quote",
        "Template: Default (follows the UI language: 简体中文)",
        ZH,
    ] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    // Showing changes nothing.
    assert!(service.v3.lock().unwrap().stats_settings.is_empty());
    let (code, stdout, _) = invoke(service.clone(), &["router", "stats-line", "--json"]).await;
    assert_eq!(code, 0);
    let v = one_json(&stdout);
    assert_eq!(v["template"], "default");
    assert_eq!(v["resolved_template"], ZH);
    assert_eq!(v["locale"], "zh-CN");
    // In the Chinese CLI too.
    let (_, stdout, _) = invoke(
        service.clone(),
        &["--locale", "zh-CN", "router", "stats-line"],
    )
    .await;
    assert!(
        stdout.contains("默认（跟随界面语言：简体中文）") && stdout.contains(ZH),
        "{stdout}"
    );
    // English UI.
    service.profiles.lock().unwrap().settings.locale = Some(Locale::En);
    let (_, stdout, _) = invoke(service.clone(), &["router", "stats-line"]).await;
    assert!(
        stdout.contains("Default (follows the UI language: English)") && stdout.contains(EN),
        "{stdout}"
    );
}

#[tokio::test]
async fn stats_line_keeps_an_edited_template_and_resets_with_default() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &[
            "router",
            "stats-line",
            "on",
            "--template",
            "⚡ {tps} tok/s (mine)",
        ],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Template: Custom (kept as written)"),
        "{stdout}"
    );
    let s = service.v3.lock().unwrap().stats_settings[0].clone();
    assert!(s.line.enabled);
    assert_eq!(s.line.template.as_deref(), Some("⚡ {tps} tok/s (mine)"));
    // Only the template changes; the line stays on.
    let (code, stdout, _) = invoke(
        service.clone(),
        &["router", "stats-line", "--template", "default"],
    )
    .await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Template: Default (follows the UI language: English)"),
        "{stdout}"
    );
    let s = service.v3.lock().unwrap().stats_settings[1].clone();
    assert!(s.line.enabled);
    assert_eq!(s.line.template, None, "stored as the default marker");
    use clap::Parser;
    assert!(
        superba_cli::Cli::try_parse_from(["spb", "router", "stats-line", "--default-template"])
            .is_err()
    );
}

#[tokio::test]
async fn stats_line_harnesses_pick_the_tools_that_get_the_line() {
    let service = FakeService::new();
    let (code, stdout, _) = invoke(service.clone(), &["router", "stats-line"]).await;
    assert_eq!(code, 0);
    // Codex and Claude Code by default.
    assert!(stdout.contains("Tools: Codex, Claude Code"), "{stdout}");
    let (code, _, stderr) = invoke(
        service.clone(),
        &["router", "stats-line", "--harnesses", "codex"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let s = service.v3.lock().unwrap().stats_settings[0].clone();
    assert_eq!(s.line.harnesses, vec![RequestHarness::Codex]);
    let (code, _, _) = invoke(
        service.clone(),
        &["router", "stats-line", "--harnesses", "none"],
    )
    .await;
    assert_eq!(code, 0);
    let s = service.v3.lock().unwrap().stats_settings[1].clone();
    assert!(s.line.harnesses.is_empty());
}
