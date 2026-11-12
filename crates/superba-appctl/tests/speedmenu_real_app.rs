//! Checks against a real desktop app bundle (not in CI: needs the app).
//!
//! ```sh
//! SUPERBA_TEST_APP_ASAR=/path/to/resources/app.asar \
//!   cargo test -p superba-appctl --test speedmenu_real_app -- --ignored --nocapture
//! ```
//!
//! `SUPERBA_TEST_FACTORY_LIST=<file>` also writes one JSON line per module
//! factory of every chunk (`{"file","id","len","fnv"}`, len in UTF-16 units) to
//! compare with what V8 reports for the same chunks.

use std::io::Write;
use std::path::PathBuf;

use superba_appctl::speedmenu::{asar::Archive, plan};

fn asar() -> Option<PathBuf> {
    std::env::var_os("SUPERBA_TEST_APP_ASAR").map(PathBuf::from)
}

#[test]
#[ignore = "needs a desktop app bundle (SUPERBA_TEST_APP_ASAR)"]
fn plans_the_real_bundle() {
    let Some(path) = asar() else { return };
    let started = std::time::Instant::now();
    let outcome = plan::plan_for_asar(&path).unwrap();
    eprintln!("planned in {:?}", started.elapsed());
    let plan = outcome.unwrap_or_else(|n| panic!("no plan: {}", n.reason));
    for m in &plan.modules {
        eprintln!(
            "{} gates={:?} len={} patched_len={}",
            m.id,
            m.gates,
            m.original_len,
            m.source.len()
        );
    }
    let script = superba_appctl::speedmenu::document_script(&plan);
    if let Some(out) = std::env::var_os("SUPERBA_TEST_SCRIPT_OUT") {
        std::fs::write(out, &script).unwrap();
    }
}

#[test]
#[ignore = "needs a desktop app bundle (SUPERBA_TEST_APP_ASAR)"]
fn lists_every_factory() {
    let (Some(path), Some(out)) = (asar(), std::env::var_os("SUPERBA_TEST_FACTORY_LIST")) else {
        return;
    };
    let mut archive = Archive::open(&path).unwrap();
    let entries: Vec<_> = archive
        .entries()
        .iter()
        .filter(|e| e.path.starts_with("webview/assets/") && e.path.ends_with(".js"))
        .cloned()
        .collect();
    let mut file = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    let (mut chunks, mut unsplit) = (0, 0);
    let mut buf = Vec::new();
    for e in entries {
        archive.read_into(&e, &mut buf).unwrap();
        let Ok(text) = std::str::from_utf8(&buf) else {
            continue;
        };
        if !text.contains("__webpack_modules__={") {
            continue;
        }
        chunks += 1;
        let Some(factories) = plan::chunk_factories(text) else {
            unsplit += 1;
            writeln!(
                file,
                "{}",
                serde_json::json!({"file": e.path, "error": "unsplit"})
            )
            .unwrap();
            continue;
        };
        for f in factories {
            writeln!(
                file,
                "{}",
                serde_json::json!({
                    "file": e.path,
                    "id": f.id(),
                    "len": f.source.encode_utf16().count(),
                    "fnv": plan::fnv1a_utf16(f.source),
                })
            )
            .unwrap();
        }
    }
    eprintln!("{chunks} chunks, {unsplit} not split");
}

/// Start the real app in a prepared home and register the speed menu through
/// the debugging port as a launch does.
///
/// `SUPERBA_TEST_APP_EXE` (the app executable next to `resources/app.asar`),
/// `SUPERBA_TEST_APP_HOME` (a HOME whose `.codex` has an API-key sign-in and
/// the onboarding done) and `DISPLAY`. The app is killed afterwards.
#[test]
#[ignore = "needs a desktop app (SUPERBA_TEST_APP_EXE, SUPERBA_TEST_APP_HOME)"]
fn registers_in_the_real_window() {
    let (Some(exe), Some(home)) = (
        std::env::var_os("SUPERBA_TEST_APP_EXE").map(PathBuf::from),
        std::env::var_os("SUPERBA_TEST_APP_HOME").map(PathBuf::from),
    ) else {
        return;
    };
    let asar = exe.parent().unwrap().join("resources/app.asar");
    let port_file = home.join(".config/Codex/DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let r = plan::plan_for_asar(&asar).unwrap().map(|p| {
                let s = superba_appctl::speedmenu::document_script(&p);
                (p, s)
            });
            let _ = tx.send(r);
        });
        let launched_at = superba_core::Timestamp::now();
        let mut child = std::process::Command::new(&exe)
            .args([
                "--no-sandbox",
                "--disable-gpu",
                "--password-store=basic",
                "--remote-debugging-port=0",
            ])
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env_remove("CODEX_HOME")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let outcome = superba_appctl::speedmenu::attach_after_launch(
            child.id(),
            superba_appctl::launch::CdpPlan::ActivePortFile(port_file),
            launched_at,
            rx,
            std::time::Duration::from_secs(20),
        )
        .await;
        let _ = child.kill();
        let _ = child.wait();
        let outcome = outcome.expect("attached");
        eprintln!("{outcome:?}");
        assert!(!outcome.late, "{outcome:?}");
        assert!(outcome.mismatched.is_empty(), "{outcome:?}");
        for id in ["ipV4", "PAzX", "K6lt"] {
            assert!(outcome.patched.contains_key(id), "{id}: {outcome:?}");
        }
    });
}
