#![allow(missing_docs)]

//! The Codex settings page source with the real ChatGPT app for Windows (the
//! MSIX package) installed (user bug 41: the page could not load). Ignored by
//! default; the `winapp` CI job installs the official package and runs, with
//! `SUPERBA_DATA_DIR` pointing at a temporary directory:
//!
//! ```text
//! SUPERBA_WINAPP_IT=1 cargo test -p superba-gui --test windows_settings_it -- --ignored --nocapture
//! ```
//!
//! It builds the overview through the production service (which detects the
//! package and its bundled Codex) and loads the settings model from it, the way
//! the page does, printing the targets, versions and notes.

#![cfg(windows)]

use superba_gui::pages::settings::source::{self, Input};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "the real MSIX app on a disposable Windows runner (winapp CI job)"]
async fn the_settings_source_loads_with_the_packaged_app() {
    if std::env::var("SUPERBA_WINAPP_IT").as_deref() != Ok("1") {
        return;
    }
    assert!(
        std::env::var_os("SUPERBA_DATA_DIR").is_some(),
        "run with a temporary SUPERBA_DATA_DIR"
    );
    let ctx = superba_switch::detect_context().unwrap();
    let service = superba_switch::default_service(ctx);
    let overview = service.overview().await.expect("overview");
    for i in &overview.app.installs {
        println!(
            "[winapp] settings: install {} at {} (bundled Codex {:?} {:?})",
            i.id,
            i.path.display(),
            i.bundled_codex,
            i.bundled_codex_version
        );
    }
    assert!(
        overview
            .app
            .installs
            .iter()
            .any(|i| i.id == "OpenAI.Codex_2p2nqsd0c76g0"),
        "the package is in the overview"
    );
    let input = Input {
        overview: Some(overview),
        profiles: service.profile_set().await.ok(),
        profile: None,
    };
    let model = tokio::task::spawn_blocking(move || source::load(input))
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("the settings source does not load: {e}"));
    for t in &model.targets {
        println!(
            "[winapp] settings: target {} version {:?} approximate {} outdated {}",
            t.label, t.version, t.approximate, t.catalog_outdated
        );
    }
    for n in &model.notes {
        println!("[winapp] settings: note {}", n.render_fallback());
    }
    println!(
        "[winapp] settings: {} entries from catalog {:?}",
        model.entries.len(),
        model.catalog_version
    );
    assert!(!model.entries.is_empty());
    assert!(model.targets.iter().any(|t| t.label == "App"));
}
