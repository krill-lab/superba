//! Superba self-update: versions, assets, overrides, zip safety, file swaps.

mod support;

use std::path::PathBuf;

use superba_versions::selfupdate::apply::{self, InstallLayout};
use superba_versions::selfupdate::finish::FinishArgs;
use superba_versions::selfupdate::release::{Os, Platform, asset_name};
use superba_versions::selfupdate::version::{is_newer, parse_tag, parse_version};
use superba_versions::selfupdate::zipx::extract_zip;
use superba_versions::selfupdate::{
    DEFAULT_API, DEFAULT_DOWNLOADS, ENV_SUPERBA_SELF_UPDATE_API, ENV_SUPERBA_SELF_UPDATE_DOWNLOADS,
    Endpoints,
};
use support::{ZipFile, zip_bytes};

fn v(s: &str) -> semver::Version {
    parse_version(s).unwrap()
}

#[test]
fn newer_versions_by_semver() {
    assert!(is_newer(&v("0.2.0"), &v("0.1.0")));
    assert!(is_newer(&v("0.10.0"), &v("0.9.9")), "numeric, not textual");
    assert!(!is_newer(&v("0.1.0"), &v("0.1.0")));
    assert!(!is_newer(&v("0.1.0"), &v("0.2.0")));
}

#[test]
fn prereleases_order_before_their_release() {
    assert!(
        is_newer(&v("0.2.0"), &v("0.2.0-rc.1")),
        "release beats its rc"
    );
    assert!(!is_newer(&v("0.2.0-rc.1"), &v("0.2.0")));
    assert!(is_newer(&v("0.2.0-rc.1"), &v("0.1.9")));
    assert!(is_newer(&v("0.2.0-rc.2"), &v("0.2.0-rc.1")));
    assert!(
        is_newer(&v("0.2.0-rc.1"), &v("0.2.0-beta.3")),
        "rc after beta"
    );
    assert!(is_newer(&v("0.2.0-beta.1"), &v("0.2.0-alpha.9")));
    assert!(
        is_newer(&v("0.2.0-rc.10"), &v("0.2.0-rc.9")),
        "numeric parts"
    );
    assert!(
        !is_newer(&v("0.1.0"), &v("0.2.0-rc.1")),
        "stable 0.1.0 is not an update of rc 0.2"
    );
    assert!(
        !is_newer(&v("0.1.0+build5"), &v("0.1.0")),
        "build metadata is ignored"
    );
}

#[test]
fn only_release_workflow_tags_are_accepted() {
    for ok in [
        "v0.1.0",
        "v1.20.3",
        "v0.2.0-rc.1",
        "v0.2.0-alpha.12",
        "v0.2.0-beta.3",
    ] {
        assert!(parse_tag(ok).is_some(), "{ok}");
    }
    for bad in [
        "0.1.0",
        "v0.1",
        "v0.1.0-rc",
        "v0.1.0-nightly.1",
        "v0.1.0+x",
        "v0.1.0/../x",
        "v01.0.0",
        "v0.1.0-rc.1.2",
        "",
    ] {
        assert!(parse_tag(bad).is_none(), "{bad}");
    }
}

fn plat(os: Os, arch: &'static str, musl: bool) -> Platform {
    Platform { os, arch, musl }
}

#[test]
fn asset_selection_per_os_and_architecture() {
    let a = |p| asset_name("0.2.0", &p);
    assert_eq!(
        a(plat(Os::Macos, "aarch64", false)).as_deref(),
        Some("superba-0.2.0-macos-universal.zip")
    );
    assert_eq!(
        a(plat(Os::Macos, "x86_64", false)).as_deref(),
        Some("superba-0.2.0-macos-universal.zip")
    );
    assert_eq!(
        a(plat(Os::Windows, "x86_64", false)).as_deref(),
        Some("superba-0.2.0-windows-x86_64.msi")
    );
    assert_eq!(
        a(plat(Os::Linux, "x86_64", false)).as_deref(),
        Some("superba-0.2.0-linux-x86_64.tar.gz")
    );
    assert_eq!(
        a(plat(Os::Linux, "aarch64", false)).as_deref(),
        Some("superba-0.2.0-linux-aarch64.tar.gz")
    );
    assert_eq!(
        a(plat(Os::Linux, "x86_64", true)).as_deref(),
        Some("spb-0.2.0-linux-x86_64-musl.tar.gz")
    );
    assert_eq!(a(plat(Os::Windows, "aarch64", false)), None);
    assert_eq!(a(plat(Os::Linux, "riscv64", false)), None);
    assert_eq!(a(plat(Os::Linux, "aarch64", true)), None);
}

#[test]
fn overrides_accept_only_https_or_loopback() {
    let with = |api: &str, dl: &str| {
        Endpoints::with_overrides(|k| match k {
            ENV_SUPERBA_SELF_UPDATE_API => Some(api.to_owned()),
            ENV_SUPERBA_SELF_UPDATE_DOWNLOADS => Some(dl.to_owned()),
            _ => None,
        })
    };
    let e = with("http://127.0.0.1:8080/api/", "https://mirror.example/dl");
    assert_eq!(e.api, "http://127.0.0.1:8080/api");
    assert_eq!(e.downloads, "https://mirror.example/dl");
    assert!(e.downloads_overridden);

    let e = with("http://localhost:1/x", "http://[::1]:2/y");
    assert_eq!(e.api, "http://localhost:1/x");
    assert_eq!(e.downloads, "http://[::1]:2/y");

    for bad in [
        "http://example.com/api",
        "ftp://127.0.0.1/x",
        "http://10.0.0.1/api",
        "https://user:pw@example.com/api",
        "https://example.com/api?x=1",
        "not a url",
    ] {
        let e = with(bad, bad);
        assert_eq!(e.api, DEFAULT_API, "{bad}");
        assert_eq!(e.downloads, DEFAULT_DOWNLOADS, "{bad}");
        assert!(!e.downloads_overridden, "{bad}");
    }
    let none = Endpoints::with_overrides(|_| None);
    assert_eq!(none, Endpoints::default());
}

#[test]
fn zip_extracts_files_modes_and_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = zip_bytes(&[
        ZipFile {
            name: "pkg/",
            data: b"",
            mode: 0o040_755,
            deflate: false,
        },
        ZipFile::file(
            "pkg/Superba.app/Contents/MacOS/superba",
            b"gui-binary",
            0o755,
        ),
        ZipFile::file("pkg/spb", &vec![b'x'; 5000], 0o755),
        ZipFile::file("pkg/README.md", b"hello", 0o644),
    ]);
    let archive = tmp.path().join("a.zip");
    std::fs::write(&archive, zip).unwrap();
    let dest = tmp.path().join("out");
    extract_zip(&archive, &dest).unwrap();
    assert_eq!(
        std::fs::read(dest.join("pkg/Superba.app/Contents/MacOS/superba")).unwrap(),
        b"gui-binary"
    );
    assert_eq!(std::fs::read(dest.join("pkg/spb")).unwrap().len(), 5000);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = |p: &str| {
            std::fs::metadata(dest.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(m("pkg/spb") & 0o111, 0o111, "executable bit kept");
        assert_eq!(m("pkg/README.md") & 0o111, 0, "no exec bit added");
    }
}

fn zip_refused(files: &[ZipFile<'_>]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("a.zip");
    std::fs::write(&archive, zip_bytes(files)).unwrap();
    let dest = tmp.path().join("out");
    let err = extract_zip(&archive, &dest).unwrap_err().to_string();
    // Nothing escaped the destination.
    assert!(!tmp.path().join("evil").exists());
    assert!(!tmp.path().parent().unwrap().join("evil").exists());
    err
}

#[test]
fn zip_refuses_traversal_links_and_odd_entries() {
    for name in [
        "../evil",
        "pkg/../../evil",
        "/etc/evil",
        "pkg\\..\\..\\evil",
        "C:\\evil",
        "C:evil",
        "pkg/a:stream",
    ] {
        let e = zip_refused(&[ZipFile::file(name, b"x", 0o644)]);
        assert!(e.contains("unsafe path"), "{name}: {e}");
    }
    let e = zip_refused(&[ZipFile {
        name: "pkg/link",
        data: b"/etc/passwd",
        mode: 0o120_777,
        deflate: false,
    }]);
    assert!(e.contains("is a link"), "{e}");
    let e = zip_refused(&[ZipFile {
        name: "pkg/fifo",
        data: b"",
        mode: 0o010_644,
        deflate: false,
    }]);
    assert!(e.contains("unsupported entry type"), "{e}");
    let e = zip_refused(&[
        ZipFile::file("pkg/a", b"1", 0o644),
        ZipFile::file("pkg/a", b"2", 0o644),
    ]);
    assert!(e.contains("appears twice"), "{e}");
}

#[test]
fn zip_accepts_windows_backslash_names_inside_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("a.zip");
    std::fs::write(
        &archive,
        zip_bytes(&[
            ZipFile {
                name: "sub\\file.txt",
                data: b"win",
                mode: 0,
                deflate: true,
            },
            ZipFile {
                name: "superba.exe",
                data: b"MZ",
                mode: 0,
                deflate: false,
            },
        ]),
    )
    .unwrap();
    let dest = tmp.path().join("out");
    extract_zip(&archive, &dest).unwrap();
    assert_eq!(std::fs::read(dest.join("sub/file.txt")).unwrap(), b"win");
    assert!(dest.join("superba.exe").is_file());
}

#[test]
fn zip_refuses_corruption_and_non_zip_files() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("a.zip");
    let mut bytes = zip_bytes(&[ZipFile::file("pkg/a", b"hello world", 0o644)]);
    // Flip a byte of the (stored-size, deflated) data: CRC or inflate must fail.
    bytes[34 + 5] ^= 0xff;
    std::fs::write(&archive, &bytes).unwrap();
    assert!(extract_zip(&archive, &tmp.path().join("o1")).is_err());
    std::fs::write(&archive, b"this is not a zip file at all").unwrap();
    let e = extract_zip(&archive, &tmp.path().join("o2")).unwrap_err();
    assert!(e.to_string().contains("not a zip"), "{e}");
}

#[test]
fn zip_refuses_an_entry_larger_than_declared() {
    // A zip bomb shape: the declared size is smaller than the real content.
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("a.zip");
    let mut bytes = zip_bytes(&[ZipFile::file("pkg/a", &vec![b'a'; 4096], 0o644)]);
    // Central directory "uncompressed size" field: declare 10 bytes.
    let cd = bytes
        .windows(4)
        .position(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    bytes[cd + 24..cd + 28].copy_from_slice(&10_u32.to_le_bytes());
    std::fs::write(&archive, &bytes).unwrap();
    let e = extract_zip(&archive, &tmp.path().join("o")).unwrap_err();
    assert!(e.to_string().contains("larger than declared"), "{e}");
}

fn write(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

#[test]
fn replace_files_swaps_all_and_leaves_no_leftovers() {
    let tmp = tempfile::tempdir().unwrap();
    let (new, old) = (tmp.path().join("new"), tmp.path().join("old"));
    std::fs::create_dir_all(&new).unwrap();
    std::fs::create_dir_all(&old).unwrap();
    let pairs: Vec<_> = ["superba", "spb"]
        .iter()
        .map(|n| {
            write(&old, n, "old");
            (write(&new, n, "new"), old.join(n))
        })
        .collect();
    let replaced = apply::replace_files(&pairs).unwrap();
    assert_eq!(replaced.len(), 2);
    for n in ["superba", "spb"] {
        assert_eq!(std::fs::read_to_string(old.join(n)).unwrap(), "new");
    }
    let names: Vec<_> = std::fs::read_dir(&old)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        2,
        "no .superba-new / .superba-old left: {names:?}"
    );
}

#[test]
fn replace_files_failure_keeps_the_old_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (new, old) = (tmp.path().join("new"), tmp.path().join("old"));
    std::fs::create_dir_all(&new).unwrap();
    std::fs::create_dir_all(&old).unwrap();
    write(&old, "superba", "old-superba");
    write(&old, "spb", "old-spb");
    let good = write(&new, "superba", "new-superba");
    let missing = new.join("spb-missing");
    let pairs = vec![(good, old.join("superba")), (missing, old.join("spb"))];
    assert!(apply::replace_files(&pairs).is_err());
    assert_eq!(
        std::fs::read_to_string(old.join("superba")).unwrap(),
        "old-superba"
    );
    assert_eq!(std::fs::read_to_string(old.join("spb")).unwrap(), "old-spb");
    let n = std::fs::read_dir(&old).unwrap().count();
    assert_eq!(n, 2, "temporary copies removed");
}

#[test]
fn bundle_swap_keeps_a_backup_and_restores_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("Superba.app");
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    std::fs::write(app.join("Contents/v"), "old").unwrap();
    let fresh = tmp.path().join("stage/Superba.app");
    std::fs::create_dir_all(fresh.join("Contents")).unwrap();
    std::fs::write(fresh.join("Contents/v"), "new").unwrap();
    let backup = apply::swap_bundle(&app, &fresh).unwrap();
    assert_eq!(
        std::fs::read_to_string(app.join("Contents/v")).unwrap(),
        "new"
    );
    assert_eq!(
        std::fs::read_to_string(backup.join("Contents/v")).unwrap(),
        "old"
    );
    // A missing replacement puts the current bundle back.
    let e = apply::swap_bundle(&app, &tmp.path().join("nope.app"));
    assert!(e.is_err());
    assert_eq!(
        std::fs::read_to_string(app.join("Contents/v")).unwrap(),
        "new"
    );
}

#[test]
fn install_layouts_are_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    write(dir, "spb", "x");
    write(dir, "superba", "x");
    match apply::detect_layout(&dir.join("spb"), Os::Linux) {
        Some(InstallLayout::Files { names, .. }) => assert_eq!(names, ["superba", "spb"]),
        other => panic!("{other:?}"),
    }
    let app = dir.join("Apps/Superba.app/Contents/MacOS");
    std::fs::create_dir_all(&app).unwrap();
    write(&app, "superba", "x");
    write(&dir.join("Apps"), "spb", "x");
    match apply::detect_layout(&app.join("superba"), Os::Macos) {
        Some(InstallLayout::Bundle { app, spb }) => {
            assert!(app.ends_with("Superba.app"));
            assert_eq!(spb, Some(dir.join("Apps/spb")));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        apply::detect_layout(&dir.join("empty/none"), Os::Linux),
        None
    );
}

#[test]
fn finish_arguments_round_trip() {
    let args = FinishArgs {
        wait_pid: Some(4242),
        msi: Some("/tmp/superba-0.2.0-windows-x86_64.msi".into()),
        restart_router: true,
        relaunch: Some("/opt/superba/superba".into()),
        open_app: None,
        log: Some("/tmp/log".into()),
    };
    let mut argv = vec![std::ffi::OsString::from("superba")];
    argv.extend(args.to_args());
    assert!(superba_versions::selfupdate::finish::is_finish_invocation(
        &argv
    ));
    assert_eq!(FinishArgs::parse(&argv).unwrap(), args);
    assert!(
        FinishArgs::parse(&[
            "superba".into(),
            "--finish-update".into(),
            "--bogus".into(),
            "1".into()
        ])
        .is_err()
    );
}
