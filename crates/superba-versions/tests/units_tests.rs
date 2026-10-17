//! Pure parts: compatibility tiers, list parsers, safe extraction.

mod support;

use pretty_assertions::assert_eq;
use superba_core::{CodexVersion, CompatTier, Error};
use superba_versions::compat_tier;
use superba_versions::extract::extract;
use superba_versions::sources::{
    Endpoints, Layout, merge_releases, npm_platform, override_base, parse_dist_tags,
    parse_latest_redirect, parse_npm_dist, parse_sha256sums, release_of,
};
use support::targz_raw_path;

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

#[test]
fn compatibility_tiers_follow_spike_b6() {
    let floor = v("0.141.0");
    let bundled = v("0.162.0-alpha.17.2");
    let rows = [
        ("0.162.0-alpha.17.2", CompatTier::Same),
        ("0.162.0", CompatTier::Compatible),
        ("0.162.1", CompatTier::Compatible),
        ("0.163.0", CompatTier::Compatible),
        ("0.163.0-alpha.10.2", CompatTier::Compatible),
        ("0.164.0", CompatTier::Newer),
        ("1.0.0", CompatTier::Newer),
        ("0.0.0", CompatTier::Newer),
        ("0.161.0", CompatTier::Older),
        ("0.150.1", CompatTier::Older),
        ("0.141.0", CompatTier::Older),
        ("0.140.0", CompatTier::BelowFloor),
    ];
    for (candidate, tier) in rows {
        assert_eq!(
            compat_tier(Some(&bundled), &floor, &v(candidate)),
            tier,
            "{candidate}"
        );
    }
    // App 26.1013 bundles 0.163.0-alpha.10.2: 0.162.0 is one minor older.
    assert_eq!(
        compat_tier(Some(&v("0.163.0-alpha.10.2")), &floor, &v("0.162.0")),
        CompatTier::Older
    );
    // Unknown bundled version: anything at or above the floor is compatible.
    assert_eq!(
        compat_tier(None, &floor, &v("0.150.0")),
        CompatTier::Compatible
    );
    assert_eq!(
        compat_tier(None, &floor, &v("0.140.0")),
        CompatTier::BelowFloor
    );
    assert!(CompatTier::BelowFloor.is_blocked());
    assert!(!CompatTier::Older.is_blocked());
}

#[test]
fn sha256sums_and_npm_documents() {
    let sums = parse_sha256sums(
        "4f573944c1d2059109d75a2f4d0cc9c03697288224a5e407717a9de98fc010c5  codex-package-x86_64-unknown-linux-musl.tar.gz\n\
         junk line\n\
         D47A5FA21E037A1B85729B88C238FF3DA8A956FC9FB5AD3976714428E7FB2BDA *codex-package-aarch64-unknown-linux-musl.tar.gz\n",
    );
    assert_eq!(sums.len(), 2);
    assert_eq!(
        sums["codex-package-aarch64-unknown-linux-musl.tar.gz"],
        "d47a5fa21e037a1b85729b88c238ff3da8a956fc9fb5ad3976714428e7fb2bda"
    );
    let dist = parse_npm_dist(
        r#"{"name":"@openai/codex","dist":{"tarball":"https://registry.npmjs.org/@openai/codex/-/codex-0.162.0-linux-x64.tgz","integrity":"sha512-qHWp6oKpf456PqjberGN8avs8zChv2oS+UuDiqVFYAuDOhq6vBc1drh8jEsnR7dBX+zcw03PN5VLtePGlMcC3w==","unpackedSize":452474258}}"#,
    )
    .unwrap();
    assert!(dist.integrity.unwrap().starts_with("sha512-"));
    assert_eq!(dist.unpacked_size, Some(452_474_258));
    assert_eq!(npm_platform("aarch64-apple-darwin"), Some("darwin-arm64"));
    assert_eq!(npm_platform("riscv64"), None);
}

#[test]
fn dist_tags_keep_plain_versions_newest_first() {
    let json = r#"{"beta":"0.1.2505172116","native":"0.1.2505291658",
        "release-0.162.0-alpha.17.1-alpha-linux-arm64":"0.162.0-alpha.17.1-linux-arm64",
        "release-0.162.0-alpha.17.1-alpha":"0.162.0-alpha.17.1",
        "latest":"0.163.0","alpha":"0.164.0-alpha.1","linux-x64":"0.163.0-linux-x64"}"#;
    let list = parse_dist_tags(json);
    let versions: Vec<String> = list.iter().map(|r| r.version.to_string()).collect();
    assert_eq!(
        versions,
        vec!["0.164.0-alpha.1", "0.163.0", "0.162.0-alpha.17.1"]
    );
    assert!(list[0].prerelease);
    assert!(!list[1].prerelease);
    assert_eq!(list[1].tag, "rust-v0.163.0");
    assert!(parse_dist_tags("<html>").is_empty());
}

#[test]
fn github_latest_redirect_names_the_version() {
    assert_eq!(
        parse_latest_redirect("https://github.com/openai/codex/releases/tag/rust-v0.163.0"),
        Some(v("0.163.0"))
    );
    assert_eq!(parse_latest_redirect("https://github.com/login"), None);
}

#[test]
fn merged_lists_prefer_the_first_source_and_fill_gaps() {
    let mut feed = release_of(v("0.162.0"), "feed");
    feed.sha256
        .insert("x86_64-unknown-linux-musl".into(), "ab".repeat(32));
    feed.bundled_by.push("26.1007.71326".into());
    let merged = merge_releases(&[
        vec![feed],
        vec![
            release_of(v("0.163.0"), "npm"),
            release_of(v("0.162.0"), "npm"),
        ],
    ]);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].version, v("0.163.0"));
    assert_eq!(merged[1].sha256.len(), 1);
    assert_eq!(merged[1].bundled_by, vec!["26.1007.71326".to_owned()]);
}

#[test]
fn endpoint_overrides_make_the_package_cache_the_only_source() {
    let e = Endpoints::with_overrides(|k| match k {
        "SUPERBA_CODEX_PACKAGES" => Some("http://127.0.0.1:9/pkgs/".into()),
        "SUPERBA_APP_DOWNLOADS" => Some("http://127.0.0.1:9/app".into()),
        _ => None,
    });
    assert!(e.packages_only_from_releases);
    assert_eq!(
        e.release_asset(&v("0.162.0"), "codex-package_SHA256SUMS"),
        "http://127.0.0.1:9/pkgs/rust-v0.162.0/codex-package_SHA256SUMS"
    );
    assert_eq!(e.app, "http://127.0.0.1:9/app");
    let d = Endpoints::with_overrides(|_| None);
    assert_eq!(d, Endpoints::default());
}

#[test]
fn endpoint_overrides_must_be_https_or_loopback() {
    // What the e2e run and the screenshot tool set: https on loopback.
    let e = Endpoints::with_overrides(|k| match k {
        "SUPERBA_CODEX_PACKAGES" => Some("https://127.0.0.1:4443".into()),
        "SUPERBA_APP_DOWNLOADS" => Some("https://127.0.0.1:4444/".into()),
        _ => None,
    });
    assert!(e.packages_only_from_releases);
    assert_eq!(e.codex_releases, "https://127.0.0.1:4443");
    assert_eq!(e.app, "https://127.0.0.1:4444");
    for ok in [
        "https://packages.example.com/codex",
        "http://localhost:9",
        "http://[::1]:9/app",
    ] {
        assert_eq!(
            override_base(ok).as_deref(),
            Some(ok.trim_end_matches('/')),
            "{ok}"
        );
    }
    for bad in [
        "http://packages.example.com/codex",
        "http://10.0.0.1/app",
        "ftp://127.0.0.1/pkgs",
        "file:///tmp/pkgs",
        "https://user:secret@packages.example.com/",
        "https://packages.example.com/?x=1",
        "https://packages.example.com/#x",
        "not a url",
    ] {
        assert_eq!(override_base(bad), None, "{bad}");
    }
    // A refused value is ignored: the official sources stay in use.
    let d = Endpoints::with_overrides(|k| match k {
        "SUPERBA_CODEX_PACKAGES" => Some("http://packages.example.com".into()),
        "SUPERBA_APP_DOWNLOADS" => Some("file:///tmp/app".into()),
        _ => None,
    });
    assert_eq!(d, Endpoints::default());
}

// ---------------------------------------------------------------------------
// extraction
// ---------------------------------------------------------------------------

const T: &str = "x86_64-unknown-linux-musl";

fn extract_bytes(bytes: &[u8], layout: Layout) -> (tempfile::TempDir, superba_core::Result<()>) {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("pkg.tar.gz");
    std::fs::write(&archive, bytes).unwrap();
    let r = extract(&archive, layout, T, &tmp.path().join("out"));
    (tmp, r)
}

fn refused(r: superba_core::Result<()>) -> bool {
    matches!(r, Err(Error::Invalid { .. }))
}

#[test]
fn traversal_and_absolute_paths_are_refused() {
    let (tmp, r) = extract_bytes(&targz_raw_path(b"../evil", b"x"), Layout::Package);
    assert!(refused(r));
    assert!(!tmp.path().join("evil").exists());
    let (_t, r) = extract_bytes(&targz_raw_path(b"/tmp/superba-evil", b"x"), Layout::Package);
    assert!(refused(r));
    let (_t, r) = extract_bytes(&targz_raw_path(b"bin/../../evil", b"x"), Layout::Package);
    assert!(refused(r));
}

#[test]
fn symbolic_and_hard_links_are_refused() {
    // Even a link that points inside the package: real packages have none.
    let (tmp, r) = extract_bytes(
        &support::targz(
            &[("codex-path/rg", b"rg", 0o755)],
            &[("bin/rg", "../codex-path/rg")],
        ),
        Layout::Package,
    );
    assert!(refused(r));
    assert!(std::fs::symlink_metadata(tmp.path().join("out/bin/rg")).is_err());
    let (_t, r) = extract_bytes(
        &support::targz(&[], &[("bin/out", "../../outside")]),
        Layout::Package,
    );
    assert!(refused(r));
    // Chained links (`a -> .`, then `b -> a/..`) never get a second entry.
    let (tmp, r) = extract_bytes(
        &support::targz(&[], &[("a", "."), ("b", "a/..")]),
        Layout::Package,
    );
    assert!(refused(r));
    assert!(std::fs::symlink_metadata(tmp.path().join("out/a")).is_err());
    // A hard link to a file of the package.
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Regular);
    h.set_size(2);
    h.set_mode(0o755);
    h.set_cksum();
    b.append_data(&mut h, "codex-path/rg", &b"rg"[..]).unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Link);
    h.set_size(0);
    h.set_mode(0o755);
    b.append_link(&mut h, "bin/rg", "codex-path/rg").unwrap();
    let raw = b.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &raw).unwrap();
    let (tmp, r) = extract_bytes(&gz.finish().unwrap(), Layout::Package);
    assert!(refused(r));
    assert!(!tmp.path().join("out/bin/rg").exists());
}

#[test]
fn npm_tarballs_are_rooted_at_the_vendor_directory() {
    let bytes = support::package("0.162.0", T, true);
    let (tmp, r) = extract_bytes(&bytes, Layout::Npm);
    r.unwrap();
    let out = tmp.path().join("out");
    assert!(out.join("bin/codex").is_file());
    assert!(out.join("codex-package.json").is_file());
    assert!(!out.join("package").exists());
    assert!(!out.join("README.md").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("bin/codex"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn truncated_and_non_gzip_archives_are_refused() {
    let bytes = support::package("0.162.0", T, false);
    let (_t, r) = extract_bytes(&bytes[..bytes.len() / 2], Layout::Package);
    assert!(r.is_err());
    let (_t, r) = extract_bytes(b"<html>landing page</html>", Layout::Package);
    assert!(r.is_err());
}
