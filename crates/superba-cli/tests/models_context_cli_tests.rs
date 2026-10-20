//! `spb models` and per-model context windows (user bug 7): the table shows
//! what Codex uses, with its source and whether it is applied - the numbers the
//! Models page shows (both use `superba_core::model_limits`); `models set`
//! checks the page's bounds and takes percentages of the window the model ends
//! up with; a personal access token refuses per-model values.

mod common;

use std::collections::BTreeMap;

use common::*;
use superba_core::*;

/// The fixture view: gpt-6-astra with its own 400K / 300K, every other model
/// at the all-models 1M (capped at 872K); Codex reads astra's values already,
/// luna's not yet.
fn view_with_overrides(per_model: bool) -> ModelsView {
    let mut view = FakeService::new().v3.lock().unwrap().models.clone();
    view.per_model = per_model;
    view.defaults.context_window = Some(1_000_000);
    view.overrides = BTreeMap::from([(
        "gpt-6-astra".to_owned(),
        ModelOverride {
            context_window: Some(400_000),
            auto_compact: Some(300_000),
            ..ModelOverride::default()
        },
    )]);
    view.applied = Some(AppliedModels {
        catalog: Some("/home/u/.codex/superba/models-0a1b2c3d.json".into()),
        models: BTreeMap::from([
            (
                "gpt-6-astra".to_owned(),
                AppliedModel {
                    context_window: Some(400_000),
                    max_context_window: Some(872_000),
                    auto_compact: Some(300_000),
                },
            ),
            (
                "gpt-6-luna".to_owned(),
                AppliedModel {
                    context_window: Some(272_000),
                    max_context_window: Some(872_000),
                    auto_compact: None,
                },
            ),
        ]),
        ..AppliedModels::default()
    });
    view
}

fn service(view: ModelsView) -> std::sync::Arc<FakeService> {
    let s = FakeService::new();
    s.v3.lock().unwrap().models = view;
    s
}

/// The table row of `slug`, split into cells.
fn row(stdout: &str, slug: &str) -> Vec<String> {
    stdout
        .lines()
        .find(|l| l.starts_with(&format!("{slug} |")))
        .unwrap_or_else(|| panic!("no row {slug}: {stdout}"))
        .split(" | ")
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn models_lists_the_values_codex_uses_with_their_source_and_state() {
    let view = view_with_overrides(true);
    let service = service(view.clone());
    let (code, stdout, stderr) = invoke(service.clone(), &["models"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Context window (all models): 1,000,000 (custom)"),
        "{stdout}"
    );
    assert_eq!(
        row(&stdout, "gpt-6-astra")[2..4],
        ["400,000 (custom)", "300,000 (custom)"]
    );
    // The all-models 1M is capped at luna's 872K maximum; compaction at 90 %.
    assert_eq!(
        row(&stdout, "gpt-6-luna")[2..4],
        ["872,000 (all models)", "784,800 (default)"]
    );
    assert_eq!(row(&stdout, "gpt-6-astra")[5], "applied");
    assert_eq!(row(&stdout, "gpt-6-luna")[5], "saved, not applied yet");
    // The same numbers as the Models page computes.
    for m in &view.list.models {
        let l = model_limits(m, view.overrides.get(&m.slug), &view.defaults);
        let cells = row(&stdout, &m.slug);
        let n = |v: Option<u64>| {
            let s = v.unwrap().to_string();
            let mut out = String::new();
            for (i, c) in s.chars().enumerate() {
                if i > 0 && (s.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(c);
            }
            out
        };
        assert!(cells[2].starts_with(&n(l.window)), "{cells:?}");
        assert!(cells[3].starts_with(&n(l.compact)), "{cells:?}");
    }
    // Chinese.
    let (_, stdout, _) = invoke(service, &["--locale", "zh-CN", "models"]).await;
    assert_eq!(
        row(&stdout, "gpt-6-astra")[2..4],
        ["400,000（自定义）", "300,000（自定义）"]
    );
    assert_eq!(row(&stdout, "gpt-6-luna")[5], "已保存，待应用");
}

#[tokio::test]
async fn models_set_checks_the_bounds_and_takes_shares_of_the_final_window() {
    let service = service(view_with_overrides(true));
    // Compaction not below the window.
    let (code, _, stderr) = invoke(
        service.clone(),
        &[
            "models",
            "set",
            "gpt-6-astra",
            "--context",
            "300k",
            "--compact",
            "300k",
        ],
    )
    .await;
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains(
            "gpt-6-astra: the auto-compact limit must be below the context window (300,000)."
        ),
        "{stderr}"
    );
    // Above the model's maximum.
    let (code, _, stderr) = invoke(
        service.clone(),
        &["models", "set", "gpt-6-luna", "--context", "900k"],
    )
    .await;
    assert_eq!(code, 2);
    assert!(
        stderr.contains("gpt-6-luna: the context window must be at most 872,000."),
        "{stderr}"
    );
    assert!(service.v3.lock().unwrap().model_edits.is_empty());
    // A share of the window the model ends up with: luna has no window of its
    // own, so 50 % of the all-models 1M capped at 872K.
    let (code, _, stderr) = invoke(
        service.clone(),
        &["models", "set", "gpt-6-luna", "--compact", "50%", "--yes"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    let edit = service.v3.lock().unwrap().model_edits[0].clone();
    assert_eq!(edit.set["gpt-6-luna"].auto_compact, Some(436_000));
    // All models: a percentage needs the all-models window.
    let service = self::service(view_with_overrides(true));
    {
        let mut v3 = service.v3.lock().unwrap();
        v3.models.defaults.context_window = None;
    }
    let (code, _, stderr) =
        invoke(service.clone(), &["models", "default", "--compact", "80%"]).await;
    assert_eq!(code, 2, "{stderr}");
    let (code, _, stderr) = invoke(
        service.clone(),
        &[
            "models",
            "default",
            "--context",
            "200k",
            "--compact",
            "200k",
        ],
    )
    .await;
    assert_eq!(code, 2);
    assert!(
        stderr.contains(
            "all models: the auto-compact limit must be below the context window (200,000)."
        ),
        "{stderr}"
    );
}

#[tokio::test]
async fn a_profile_without_catalog_support_refuses_per_model_values() {
    let service = service(view_with_overrides(false));
    let (code, stdout, _) = invoke(service.clone(), &["models"]).await;
    assert_eq!(code, 0);
    // Codex uses the all-models value, not astra's saved 400K.
    assert_eq!(row(&stdout, "gpt-6-astra")[2], "872,000 (all models)");
    assert_eq!(
        row(&stdout, "gpt-6-astra")[5],
        "not used (profile does not support per-model settings)"
    );
    let (code, _, stderr) = invoke(
        service.clone(),
        &["models", "set", "gpt-6-astra", "--context", "200k"],
    )
    .await;
    assert_eq!(code, 2);
    assert!(
        stderr.contains("Per-model settings are unavailable"),
        "{stderr}"
    );
    assert!(service.v3.lock().unwrap().model_edits.is_empty());
    // Removing the unused value works.
    let (code, _, stderr) = invoke(
        service.clone(),
        &["models", "reset", "gpt-6-astra", "--yes"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        service.v3.lock().unwrap().model_edits[0].reset,
        vec!["gpt-6-astra".to_owned()]
    );
}

#[tokio::test]
async fn router_model_list_mode_is_shown_set_and_dry_run() {
    use superba_core::CatalogMode;
    let service = FakeService::new();
    let mode = |s: &FakeService| s.profiles.lock().unwrap().settings.router.catalog.mode;
    let (code, stdout, stderr) = invoke(service.clone(), &["models", "mode"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("codex"), "{stdout}");
    for (arg, expected) in [
        ("extended", CatalogMode::Extended),
        ("prefixed", CatalogMode::Prefixed),
        ("codex", CatalogMode::Codex),
    ] {
        let (code, stdout, stderr) = invoke(service.clone(), &["models", "mode", arg]).await;
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.contains(arg), "{stdout}");
        assert_eq!(mode(&service), expected);
    }
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["models", "mode", "prefixed", "--dry-run"],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("prefixed"), "{stdout}");
    assert_eq!(
        mode(&service),
        CatalogMode::Codex,
        "a dry run stores nothing"
    );
}
