//! The Windows packaged-model-query fallback, tested without real installs,
//! registry entries, credentials or changes to the process environment.

use std::fs;
use std::io::{Error as IoError, ErrorKind};

use super::{Caches, packaged_spawn_denied, relocated_codex_copies};
use superba_core::Error;

#[test]
fn relocation_requires_a_packaged_executable_spawn_denial() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp
        .path()
        .join("WindowsApps/package/app/resources/codex.exe");
    let denied = Error::io(&binary, &IoError::from(ErrorKind::PermissionDenied));
    assert!(packaged_spawn_denied(&binary, &denied));

    let ordinary = temp.path().join("bin/codex.exe");
    let denied = Error::io(&ordinary, &IoError::from(ErrorKind::PermissionDenied));
    assert!(!packaged_spawn_denied(&ordinary, &denied));

    let scratch = temp.path().join("scratch");
    let denied = Error::io(&scratch, &IoError::from(ErrorKind::PermissionDenied));
    assert!(!packaged_spawn_denied(&binary, &denied));
    for kind in [
        ErrorKind::NotFound,
        ErrorKind::InvalidInput,
        ErrorKind::ExecutableFileBusy,
    ] {
        assert!(!packaged_spawn_denied(
            &binary,
            &Error::io(&binary, &IoError::from(kind))
        ));
    }
    assert!(!packaged_spawn_denied(
        &binary,
        &Error::Parse {
            what: "model_catalog_json".into(),
            message: "invalid catalog".into(),
        }
    ));
    assert!(!packaged_spawn_denied(
        &binary,
        &Error::Timeout {
            what: "codex debug models".into(),
        }
    ));
}

#[test]
fn relocated_copy_must_match_content_even_when_size_and_version_could_match() {
    let temp = tempfile::tempdir().unwrap();
    let binary = temp
        .path()
        .join("WindowsApps/package/app/resources/codex.exe");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, b"exact packaged binary").unwrap();
    let local = temp.path().join("local");
    let bin = local.join("OpenAI/Codex/bin");
    let matching = bin.join("a159911ff70742e9/codex.exe");
    fs::create_dir_all(matching.parent().unwrap()).unwrap();
    fs::copy(&binary, &matching).unwrap();
    let other = bin.join("other-build/codex.exe");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, b"wrong packaged binary").unwrap();
    assert_eq!(
        fs::metadata(&binary).unwrap().len(),
        fs::metadata(&other).unwrap().len()
    );
    // A plain bin/codex.exe is also a supported app relocation layout.
    let plain = bin.join("codex.exe");
    fs::copy(&binary, &plain).unwrap();

    let caches = Caches::default();
    let sha = caches.binary_sha(&binary).unwrap();
    let copies = relocated_codex_copies(&binary, &local, &sha, &caches);
    assert_eq!(copies.len(), 2);
    assert!(copies.contains(&matching));
    assert!(copies.contains(&plain));
    assert!(!copies.contains(&other));

    // Equal-size changes must invalidate the hash cache, not just the size
    // prefilter. Advance the mtime explicitly to avoid filesystem timing races.
    let previous_mtime = fs::metadata(&matching).unwrap().modified().unwrap();
    fs::write(&matching, b"wrong packaged binary").unwrap();
    fs::File::options()
        .write(true)
        .open(&matching)
        .unwrap()
        .set_modified(previous_mtime + std::time::Duration::from_secs(2))
        .unwrap();
    let copies = relocated_codex_copies(&binary, &local, &sha, &caches);
    assert_eq!(copies, vec![plain]);
    assert!(relocated_codex_copies(&binary, &temp.path().join("absent"), &sha, &caches).is_empty());
    assert!(relocated_codex_copies(&other, &local, &sha, &caches).is_empty());
    assert!(relocated_codex_copies(&binary, &local, "wrong hash", &caches).is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn denied_packaged_probe_reaches_an_identical_copy_and_preserves_its_parse_error() {
    use std::sync::Arc;
    use superba_core::{AppController, LocalContext, MapEnv};

    let temp = tempfile::tempdir().unwrap();
    let binary = temp
        .path()
        .join("WindowsApps/package/app/resources/codex.exe");
    let local = temp.path().join("local");
    let copy = local.join("OpenAI/Codex/bin/a159911ff70742e9/codex.exe");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    // The test harness is a real executable. With Codex's arguments it exits
    // with a parse error; reaching that error proves the copy was started.
    let helper = std::env::current_exe().unwrap();
    fs::copy(&helper, &binary).unwrap();
    fs::copy(&helper, &copy).unwrap();
    let acl = superba_core::process::command("icacls.exe")
        .arg(&binary)
        .args(["/deny", "*S-1-1-0:(X)"])
        .output()
        .unwrap();
    assert!(
        acl.status.success(),
        "{}",
        String::from_utf8_lossy(&acl.stderr)
    );

    let ctx = LocalContext::for_tests(temp.path());
    let controller = crate::DesktopAppController::new(ctx.clone());
    let error = controller
        .codex_debug_models(&binary, None)
        .await
        .unwrap_err();
    assert!(packaged_spawn_denied(&binary, &error));

    let mut ctx = ctx;
    ctx.env = Arc::new(MapEnv::new().with("LOCALAPPDATA", local.into_os_string()));
    let controller = crate::DesktopAppController::new(ctx);
    let error = controller
        .codex_debug_models(&binary, None)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Parse { .. }), "{error:?}");
}
