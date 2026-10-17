//! Against the real servers, only when asked: `SUPERBA_ONLINE_WINAPP=<path of a
//! downloaded ChatGPT-x64.msix of the current build>` and
//! `SUPERBA_ONLINE_WINAPP_DIR=<scratch directory on the same filesystem>`.
//!
//! The package is hard-linked in as a finished partial download, so both
//! sources run their whole flow (release JSON / store.rg-adguard.net lookup,
//! the resume request answered with 416 by OpenAI's server and Microsoft's
//! CDN, size, SHA-1, manifest identity) without transferring the ~1 GB again.

use std::sync::Arc;

use superba_core::{
    AppOs, AppPackageSource, DataDir, NetworkSettings, NoDownloadProgress, VersionStore,
    WindowsAppRequest, WindowsAppSource,
};
use superba_versions::HttpVersionStore;

fn setup() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let real = std::env::var_os("SUPERBA_ONLINE_WINAPP")?;
    let dir = std::env::var_os("SUPERBA_ONLINE_WINAPP_DIR")?;
    Some((real.into(), dir.into()))
}

fn seed(real: &std::path::Path, dest: &std::path::Path, validator: &str, sha1: Option<&str>) {
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let _ = std::fs::remove_file(&part);
    std::fs::hard_link(real, &part).unwrap();
    let total = std::fs::metadata(real).unwrap().len();
    let mut meta = dest.as_os_str().to_owned();
    meta.push(".part.json");
    std::fs::write(
        meta,
        serde_json::json!({"validator": validator, "total": total, "sha1": sha1}).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn both_real_sources_resolve_and_verify_the_current_package() {
    let Some((real, dir)) = setup() else {
        return;
    };
    let version = "26.1013.4894.0";
    let data = DataDir::new(dir.join("data"));
    let store = |source| {
        HttpVersionStore::new(
            &data,
            NetworkSettings {
                windows_app_source: source,
                ..NetworkSettings::default()
            },
        )
        .with_platform(None, AppOs::Windows, "x86_64")
    };

    // Official: the validator OpenAI's server sends (Last-Modified / ETag).
    let official = data
        .downloads_dir()
        .join(format!("ChatGPT-{version}-x64.msix"));
    seed(&real, &official, "\"0x8DF2C24054871D5\"", None);
    let r = store(WindowsAppSource::OfficialOnly)
        .download_windows_app(
            WindowsAppRequest {
                source: WindowsAppSource::OfficialOnly,
                installed: Vec::new(),
            },
            Arc::new(NoDownloadProgress),
        )
        .await
        .unwrap();
    let w = r.windows.unwrap();
    eprintln!(
        "official: {:?} {:?} {}",
        w.package.identity,
        w.release_version,
        r.path.display()
    );
    assert_eq!(w.source, AppPackageSource::Official);
    assert_eq!(w.package.identity.version, version);

    // Store mirror: the CDN's ETag is the base64 SHA-1 the resolver lists.
    let mirror = data
        .downloads_dir()
        .join(format!("OpenAI.Codex_{version}_x64__2p2nqsd0c76g0.msix"));
    seed(
        &real,
        &mirror,
        "\"Kfa1/lLyeMsKmDcziCBEPRU97bY=\"",
        Some("29f6b5fe52f278cb0a9837338820443d153dedb6"),
    );
    let r = store(WindowsAppSource::StoreMirrorOnly)
        .download_windows_app(
            WindowsAppRequest {
                source: WindowsAppSource::StoreMirrorOnly,
                installed: Vec::new(),
            },
            Arc::new(NoDownloadProgress),
        )
        .await
        .unwrap();
    let w = r.windows.unwrap();
    eprintln!(
        "mirror: {:?} sha1 {:?} url {}",
        w.package.identity, w.package.sha1, r.package.url
    );
    assert_eq!(w.source, AppPackageSource::StoreMirror);
    assert_eq!(
        w.package.sha1.as_deref(),
        Some("29f6b5fe52f278cb0a9837338820443d153dedb6")
    );
    assert!(!official.exists(), "the older copy is pruned");
}
