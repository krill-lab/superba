//! Archive extraction: prefixes, modes, and refusal of unsafe entries.

mod support;

use pretty_assertions::assert_eq;
use superba_tools::archive::{clean_name, extract_tar_gz, extract_zip, zip_names};
use support::{ZipFile, targz, targz_raw_path, write, zip_bytes};

#[test]
fn tar_keeps_the_entries_under_the_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("pi.tgz");
    write(
        &a,
        &targz(
            &[
                ("pi/pi", b"#!/bin/sh\necho 1.3.0\n", 0o755),
                ("pi/theme/dark.json", b"{}", 0o644),
                ("README.md", b"outside", 0o644),
            ],
            &[],
        ),
    );
    let out = dir.path().join("out");
    extract_tar_gz(&a, "pi", &out).unwrap();
    assert!(out.join("pi").is_file());
    assert!(out.join("theme/dark.json").is_file());
    assert!(!out.join("README.md").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("pi"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn tar_refuses_links_and_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("l.tgz");
    write(
        &a,
        &targz(&[("x", b"x", 0o644)], &[("evil", "/etc/passwd")]),
    );
    let err = extract_tar_gz(&a, "", &dir.path().join("o1")).unwrap_err();
    assert!(err.to_string().contains("is a link"), "{err}");
    let b = dir.path().join("t.tgz");
    write(&b, &targz_raw_path(b"../escape", b"x"));
    let err = extract_tar_gz(&b, "", &dir.path().join("o2")).unwrap_err();
    assert!(err.to_string().contains("unsafe path"), "{err}");
    assert!(!dir.path().join("escape").exists());
}

#[test]
fn zip_extracts_a_prefix_with_modes() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("w.whl");
    write(
        &a,
        &zip_bytes(&[
            ZipFile::file("pkg/__init__.py", b"print()", 0o644),
            ZipFile::file("pkg/runtime/exe", b"\x7fELF", 0o755),
            ZipFile::file("pkg/runtime/linux-x64/downloads.json", b"{}", 0o644),
            ZipFile::file("pkg-1.0.dist-info/RECORD", b"", 0o644),
        ]),
    );
    let out = dir.path().join("out");
    extract_zip(&a, "pkg/runtime", &out).unwrap();
    assert!(out.join("exe").is_file());
    assert!(out.join("linux-x64/downloads.json").is_file());
    assert!(!out.join("__init__.py").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("exe"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    let names = zip_names(&a).unwrap();
    assert_eq!(names.len(), 4);
}

#[test]
fn zip_refuses_traversal_and_links() {
    let dir = tempfile::tempdir().unwrap();
    for (i, files) in [
        vec![ZipFile::file("../up", b"x", 0o644)],
        vec![ZipFile::file("C:/abs", b"x", 0o644)],
        vec![ZipFile {
            name: "link",
            data: b"/etc/passwd",
            mode: 0o120_777,
            deflate: false,
        }],
    ]
    .into_iter()
    .enumerate()
    {
        let a = dir.path().join(format!("{i}.zip"));
        write(&a, &zip_bytes(&files));
        assert!(
            extract_zip(&a, "", &dir.path().join(format!("o{i}"))).is_err(),
            "case {i}"
        );
    }
    assert!(!dir.path().join("up").exists());
}

#[test]
fn names_are_cleaned() {
    assert_eq!(
        clean_name("a/./b").unwrap(),
        std::path::Path::new("a").join("b")
    );
    assert_eq!(
        clean_name("a\\b").unwrap(),
        std::path::Path::new("a").join("b")
    );
    for bad in ["/abs", "../x", "a/../../x", "c:/x", "x:stream", ""] {
        assert!(clean_name(bad).is_none(), "{bad}");
    }
}
