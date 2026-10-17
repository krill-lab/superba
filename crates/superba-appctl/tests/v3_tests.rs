//! v0.3 app control: the `CODEX_CLI_PATH` pin at launch, the app-server's
//! process image, the app-server floor from `app.asar`, `codex debug models` in
//! a private Codex home and install kinds.
//!
//! Tests against the real desktop app and its bundled Codex run only when
//! `SUPERBA_TEST_APP_DIR` names an extracted app directory (`resources/app.asar`,
//! `resources/codex`); everything else uses fixtures.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use superba_appctl::DesktopAppController;
use superba_appctl::codexbin::{
    FLOOR_MARKER, asar_path, debug_models_args, floor_candidates, install_kind, scan_floor,
};
use superba_appctl::launch::{CdpPlan, build_spec, macos_open_argv};
use superba_core::codex::{APP_SERVER_FLOOR_FALLBACK, ENV_CODEX_CLI_PATH};
use superba_core::{
    AppController, AppInstall, AppVariant, CodexInstallKind, CodexVersion, Error, LaunchOptions,
    LocalContext,
};

fn spec(options: &LaunchOptions) -> superba_appctl::launch::LaunchSpec {
    build_spec(
        Path::new("/opt/chatgpt/ChatGPT"),
        options,
        &[],
        CdpPlan::Fixed(5555),
    )
}

// ---------------------------------------------------------------------------
// launch environment
// ---------------------------------------------------------------------------

#[test]
fn an_unpinned_launch_removes_an_inherited_codex_cli_path() {
    let s = spec(&LaunchOptions::default());
    assert!(!s.env_set.contains_key(ENV_CODEX_CLI_PATH));
    assert_eq!(
        s.env_remove
            .iter()
            .filter(|k| *k == ENV_CODEX_CLI_PATH)
            .count(),
        1
    );
    // Asked explicitly as well: still listed once.
    let s = spec(&LaunchOptions {
        env_remove: vec![ENV_CODEX_CLI_PATH.into()],
        ..Default::default()
    });
    assert_eq!(
        s.env_remove
            .iter()
            .filter(|k| *k == ENV_CODEX_CLI_PATH)
            .count(),
        1
    );
}

#[test]
fn a_pinned_launch_sets_codex_cli_path_and_does_not_remove_it() {
    let pin = "/home/user/.local/share/superba/codex/0.162.0-x86_64-unknown-linux-musl/bin/codex";
    let s = spec(&LaunchOptions {
        env: BTreeMap::from([(ENV_CODEX_CLI_PATH.to_owned(), pin.to_owned())]),
        env_remove: vec![ENV_CODEX_CLI_PATH.into()],
        ..Default::default()
    });
    assert_eq!(
        s.env_set.get(ENV_CODEX_CLI_PATH).map(String::as_str),
        Some(pin)
    );
    assert!(!s.env_remove.iter().any(|k| k == ENV_CODEX_CLI_PATH));
}

#[test]
fn macos_passes_the_pin_and_blanks_a_launchd_value_when_unpinned() {
    let bundle = Path::new("/Applications/ChatGPT.app");
    let unpinned = spec(&LaunchOptions::default());
    let argv = macos_open_argv(bundle, &unpinned.env_set, &unpinned.env_remove, &[]);
    assert!(
        argv.contains(&OsString::from("CODEX_CLI_PATH=")),
        "{argv:?}"
    );
    let pinned = spec(&LaunchOptions {
        env: BTreeMap::from([(ENV_CODEX_CLI_PATH.to_owned(), "/s/bin/codex".to_owned())]),
        ..Default::default()
    });
    let argv = macos_open_argv(bundle, &pinned.env_set, &pinned.env_remove, &[]);
    assert!(
        argv.contains(&OsString::from("CODEX_CLI_PATH=/s/bin/codex")),
        "{argv:?}"
    );
    assert!(!argv.contains(&OsString::from("CODEX_CLI_PATH=")));
}

// ---------------------------------------------------------------------------
// app-server process image
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn app_server_exe_is_the_process_image_of_the_newest_app_server_child() {
    use superba_appctl::procs::{ReadOpts, Snapshot, snapshot_os};

    let tmp = tempfile::tempdir().unwrap();
    // A native binary named `codex` started as `codex ... app-server`, child of
    // this test process (which plays the app main process).
    let fake = tmp.path().join("store/0.162.0/bin/codex");
    std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
    std::fs::copy("/bin/sh", &fake).unwrap();
    let mut child = std::process::Command::new(&fake)
        .args(["-c", "sleep 30; :", "app-server"])
        .spawn()
        .unwrap();
    let me = std::process::id();
    let snap = {
        let mut found = None;
        for _ in 0..50 {
            let snap =
                Snapshot::classify_entries(snapshot_os(ReadOpts::default()), &|e| e.pid == me);
            if !snap.app_servers_of(me).is_empty() {
                found = Some(snap);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        found
    };
    let exe = snap.and_then(|s| superba_appctl::codexbin::app_server_exe(&s, me));
    // A binary deleted while running reads as `<path> (deleted)`; the suffix goes.
    std::fs::remove_file(&fake).unwrap();
    let after = superba_appctl::codexbin::app_server_exe(
        &Snapshot::classify_entries(snapshot_os(ReadOpts::default()), &|e| e.pid == me),
        me,
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        exe,
        Some(
            std::fs::canonicalize(fake.parent().unwrap())
                .unwrap()
                .join("codex")
        )
    );
    assert_eq!(after.as_deref(), exe.as_deref());
    // No app-server child: nothing.
    let none = superba_appctl::codexbin::app_server_exe(
        &Snapshot::classify_entries(snapshot_os(ReadOpts::default()), &|e| e.pid == me),
        me,
    );
    assert_eq!(none, None);
}

// ---------------------------------------------------------------------------
// app-server floor
// ---------------------------------------------------------------------------

const BUNDLE_SNIPPET: &str = "require(\"node:perf_hooks\");var x=`0.141.0`,S=`codex-app-server-version-unsupported:`,C=`codex-app-server-version-restart-available:`";

#[test]
fn floor_is_the_constant_declared_before_the_marker() {
    assert_eq!(
        floor_candidates(BUNDLE_SNIPPET.as_bytes()),
        vec![CodexVersion::new(0, 141, 0)]
    );
    // Uses of the marker elsewhere (no declaration before it) are ignored.
    let other = "e.message.startsWith(`codex-app-server-version-unsupported:`)";
    assert!(floor_candidates(other.as_bytes()).is_empty());
    let junk = "var x=`hello`,S=`codex-app-server-version-unsupported:`";
    assert!(floor_candidates(junk.as_bytes()).is_empty());
    assert!(FLOOR_MARKER.starts_with(b"=`"));
}

#[test]
fn floor_scan_streams_across_chunk_boundaries() {
    let tmp = tempfile::tempdir().unwrap();
    let asar = tmp.path().join("app.asar");
    // Put the declaration right across the scanner's 8 MiB chunk boundary.
    let chunk = 8 * 1024 * 1024;
    let mut bytes = vec![b'a'; chunk - 30];
    bytes.extend_from_slice(BUNDLE_SNIPPET.as_bytes());
    bytes.extend(std::iter::repeat_n(b'z', 1024));
    std::fs::write(&asar, &bytes).unwrap();
    assert_eq!(scan_floor(&asar).unwrap(), CodexVersion::new(0, 141, 0));

    let none = tmp.path().join("none.asar");
    std::fs::write(&none, b"no marker here").unwrap();
    assert!(matches!(scan_floor(&none), Err(Error::NotFound { .. })));
}

fn linux_install(root: &Path) -> AppInstall {
    AppInstall {
        id: format!("path:{}", root.display()),
        variant: AppVariant::ChatGpt,
        path: root.to_path_buf(),
        executable: root.join("ChatGPT"),
        app_version: Some("26.1007.71326".into()),
        bundled_codex: Some(root.join("resources/codex")),
        bundled_codex_version: None,
    }
}

#[tokio::test]
async fn controller_floor_is_cached_on_disk_by_size_and_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("app");
    std::fs::create_dir_all(app.join("resources")).unwrap();
    std::fs::write(app.join("ChatGPT"), b"").unwrap();
    std::fs::write(app.join("resources/app.asar"), BUNDLE_SNIPPET).unwrap();
    let install = linux_install(&app);
    assert_eq!(asar_path(&install), Some(app.join("resources/app.asar")));
    let ctx = LocalContext::for_tests(&tmp.path().join("ctx"));
    let cache = ctx.data_dir.cache_dir().join("app-server-floor.json");
    let c = DesktopAppController::new(ctx.clone());
    assert_eq!(
        c.app_server_floor(&install).await.unwrap(),
        CodexVersion::new(0, 141, 0)
    );
    assert!(cache.is_file());
    // A fresh controller reads the disk cache (same size and mtime).
    let c2 = DesktopAppController::new(ctx);
    assert_eq!(
        c2.app_server_floor(&install).await.unwrap(),
        CodexVersion::new(0, 141, 0)
    );
    // No asar: an error the caller turns into the fallback.
    let missing = linux_install(&tmp.path().join("nowhere"));
    let err = c2.app_server_floor(&missing).await;
    assert!(matches!(err, Err(Error::NotFound { .. })));
    assert_eq!(
        superba_appctl::floor_or_fallback(err),
        APP_SERVER_FLOOR_FALLBACK
    );
}

fn real_app_dir() -> Option<PathBuf> {
    std::env::var_os("SUPERBA_TEST_APP_DIR")
        .map(PathBuf::from)
        .filter(|p| p.join("resources/app.asar").is_file())
}

#[tokio::test]
async fn real_app_floor_is_0_141_0() {
    let Some(dir) = real_app_dir() else {
        eprintln!("SUPERBA_TEST_APP_DIR not set: real app floor scan skipped");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let c = DesktopAppController::new(LocalContext::for_tests(tmp.path()));
    let started = std::time::Instant::now();
    let floor = c.app_server_floor(&linux_install(&dir)).await.unwrap();
    eprintln!("real app floor {floor} in {:?}", started.elapsed());
    assert_eq!(floor, CodexVersion::new(0, 141, 0));
}

// ---------------------------------------------------------------------------
// codex debug models
// ---------------------------------------------------------------------------

#[test]
fn debug_models_arguments() {
    assert_eq!(debug_models_args(None), ["debug", "models", "--bundled"]);
    assert_eq!(
        debug_models_args(Some(Path::new("/h/.codex/superba/models-ab\"c.json"))),
        [
            "debug",
            "models",
            "-c",
            "model_catalog_json=\"/h/.codex/superba/models-ab\\\"c.json\""
        ]
    );
}

#[cfg(unix)]
fn fake_codex(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    // Records its environment and arguments next to itself, fails on a catalog
    // named bad.json like Codex does on a schema error.
    let script = r#"#!/bin/sh
d=$(dirname "$0")
echo call >> "$d/calls"
{ echo "CODEX_HOME=$CODEX_HOME"; echo "HOME=$HOME"; echo "KEY=${OPENAI_API_KEY:-unset}"; echo "PIN=${CODEX_CLI_PATH:-unset}"; ls -A "$CODEX_HOME" | wc -l; echo "ARGS=$*"; } > "$d/last"
case "$*" in
  *bad.json*) echo "WARNING: proceeding" >&2; echo "Error: failed to parse model_catalog_json: missing field \`shell_type\`" >&2; exit 1 ;;
  *--bundled*) echo '{"models":[{"slug":"gpt-x"}]}' ;;
  *) echo '{"models":[]}' ;;
esac
"#;
    let p = dir.join("codex");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
#[tokio::test]
async fn debug_models_runs_in_a_private_empty_home_and_is_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_codex(tmp.path());
    let c = DesktopAppController::new(LocalContext::for_tests(&tmp.path().join("ctx")));
    let out = c.codex_debug_models(&bin, None).await.unwrap();
    assert!(out.contains("gpt-x"));
    let last = std::fs::read_to_string(tmp.path().join("last")).unwrap();
    let lines: Vec<&str> = last.lines().collect();
    let home = lines[0].strip_prefix("CODEX_HOME=").unwrap();
    assert_eq!(lines[1], format!("HOME={home}"));
    assert_eq!(lines[2], "KEY=unset");
    assert_eq!(lines[3], "PIN=unset");
    assert_eq!(lines[4].trim(), "0", "the private home starts empty");
    assert!(!Path::new(home).exists(), "the private home is removed");
    assert!(lines[5].ends_with("debug models --bundled"));
    // Same binary, same catalog: served from the cache.
    c.codex_debug_models(&bin, None).await.unwrap();
    let calls = std::fs::read_to_string(tmp.path().join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 1);

    let good = tmp.path().join("good.json");
    std::fs::write(&good, "{}").unwrap();
    c.codex_debug_models(&bin, Some(&good)).await.unwrap();
    let last = std::fs::read_to_string(tmp.path().join("last")).unwrap();
    assert!(last.contains("-c model_catalog_json="), "{last}");

    // Different bytes than good.json (the cache is keyed by content).
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "{\"models\":[{}]}").unwrap();
    match c.codex_debug_models(&bin, Some(&bad)).await {
        Err(Error::Parse { what, message }) => {
            assert_eq!(what, "model_catalog_json");
            assert!(message.contains("missing field"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn debug_models_with_the_real_bundled_codex() {
    let Some(dir) = real_app_dir() else {
        eprintln!("SUPERBA_TEST_APP_DIR not set: real codex debug models skipped");
        return;
    };
    let codex = dir.join("resources/codex");
    let tmp = tempfile::tempdir().unwrap();
    let c = DesktopAppController::new(LocalContext::for_tests(tmp.path()));
    let bundled = c.codex_debug_models(&codex, None).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&bundled).unwrap();
    let models = v["models"].as_array().unwrap();
    assert!(!models.is_empty());
    // The bundled list round-trips as a catalog file.
    let ok = tmp.path().join("models-ok.json");
    std::fs::write(&ok, &bundled).unwrap();
    let again = c.codex_debug_models(&codex, Some(&ok)).await.unwrap();
    assert!(again.contains(models[0]["slug"].as_str().unwrap()));
    // A catalog missing a required field is refused with Codex's message.
    let mut broken = v.clone();
    for m in broken["models"].as_array_mut().unwrap() {
        m.as_object_mut().unwrap().remove("shell_type");
    }
    let bad = tmp.path().join("models-bad.json");
    std::fs::write(&bad, serde_json::to_vec(&broken).unwrap()).unwrap();
    match c.codex_debug_models(&codex, Some(&bad)).await {
        Err(Error::Parse { message, .. }) => {
            eprintln!("codex refused: {message}");
            assert!(message.contains("shell_type") || message.contains("missing field"));
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// install kinds
// ---------------------------------------------------------------------------

#[test]
fn install_kinds_of_local_binaries() {
    let tmp = tempfile::tempdir().unwrap();
    let mk = |rel: &str| {
        let p = tmp.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        p
    };
    let store = tmp.path().join("data/codex");
    let store_bin = mk("data/codex/0.162.0-x86_64-unknown-linux-musl/bin/codex");
    let app = tmp.path().join("app");
    let bundled = mk("app/resources/codex");
    let npm = mk(
        "npm/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex",
    );
    let standalone = mk("home/.codex/packages/standalone/releases/0.162.0-x/bin/codex");
    let other = mk("usr/local/bin/codex");
    let installs = vec![linux_install(&app)];
    assert_eq!(bundled.parent().unwrap(), app.join("resources"));
    let kind = |p: &Path| install_kind(p, &store, &installs);
    assert_eq!(kind(&store_bin), CodexInstallKind::SuperbaStore);
    assert_eq!(kind(&bundled), CodexInstallKind::AppBundled);
    assert_eq!(kind(&npm), CodexInstallKind::Npm);
    assert_eq!(kind(&standalone), CodexInstallKind::Standalone);
    assert_eq!(kind(&other), CodexInstallKind::Other);
    #[cfg(unix)]
    {
        // A symlink in ~/.local/bin is classified by what it points at.
        let link = tmp.path().join("home/.local/bin/codex");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&standalone, &link).unwrap();
        assert_eq!(kind(&link), CodexInstallKind::Standalone);
    }
}
