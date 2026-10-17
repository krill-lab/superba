//! A generation restore never names a model catalog file that no longer exists
//! (review platform-security #0): Codex refuses a `config.toml` whose
//! `model_catalog_json` cannot be read, and Superba keeps only the current and
//! the previous catalog file.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use superba_core::codex::config_keys as k;
use superba_core::{ConfigEngine, ResolvedMode};

/// Apply an API-key switch that names a catalog file (created first, as the
/// switch executor does), then one without a catalog.
async fn two_generations(env: &Env) -> std::path::PathBuf {
    let file = env.home.model_catalog_file("0a1b2c3d");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"{\"models\":[]}").unwrap();
    let mut with_catalog = apikey(CDN);
    with_catalog.model_catalog = Some(file.clone());
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &with_catalog).await.unwrap())
        .await
        .unwrap();
    assert!(env.config().contains(k::MODEL_CATALOG_JSON));
    e.apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    assert!(!env.config().contains(k::MODEL_CATALOG_JSON));
    file
}

#[tokio::test]
async fn restoring_a_generation_whose_catalog_file_is_gone_drops_the_key_with_a_note() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    let file = two_generations(&env).await;
    std::fs::remove_file(&file).unwrap();

    let e = &env.engine;
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Generation { generation: 1 }),
        )
        .await
        .unwrap();
    let note = plan
        .notes
        .iter()
        .find(|n| n.text.key == "config.note.generation_catalog_missing")
        .expect("a note names the missing file");
    assert!(
        note.text
            .render_fallback()
            .contains(&file.display().to_string()),
        "{}",
        note.text.render_fallback()
    );
    assert!(
        plan.key_changes
            .iter()
            .all(|c| c.path != k::MODEL_CATALOG_JSON || c.after.is_none()),
        "{:?}",
        plan.key_changes
    );
    e.apply(&plan).await.unwrap();
    let cfg = env.config();
    assert!(!cfg.contains(k::MODEL_CATALOG_JSON), "{cfg}");
    // The rest of generation 1 is back (its line).
    assert!(cfg.contains(CDN), "{cfg}");
}

#[tokio::test]
async fn restoring_a_generation_whose_catalog_file_exists_names_it_again() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    let file = two_generations(&env).await;

    let e = &env.engine;
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Generation { generation: 1 }),
        )
        .await
        .unwrap();
    assert!(
        plan.notes
            .iter()
            .all(|n| n.text.key != "config.note.generation_catalog_missing")
    );
    e.apply(&plan).await.unwrap();
    let cfg = env.config();
    assert!(
        cfg.contains(&format!(
            "{} = \"{}\"",
            k::MODEL_CATALOG_JSON,
            // A TOML basic string: Windows backslashes are escaped.
            file.display().to_string().replace('\\', "\\\\")
        )),
        "{cfg}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"{\"models\":[]}");
}
