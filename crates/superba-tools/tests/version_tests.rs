//! Version strings of the tools: parsing, ordering, `--version` output,
//! PyPI ↔ npm spellings.

use pretty_assertions::assert_eq;
use superba_tools::version::{self, Version};

#[test]
fn parses_the_shapes_the_tools_use() {
    for (text, nums, pre) in [
        ("2.1.302", vec![2, 1, 302], vec![]),
        ("v2.0.32", vec![2, 0, 32], vec![]),
        ("0.2.1-rc.2", vec![0, 2, 1], vec!["rc", "2"]),
        ("0.2.1-alpha.5", vec![0, 2, 1], vec!["alpha", "5"]),
        ("5.8.3.41334564", vec![5, 8, 3, 41_334_564], vec![]),
        ("3.15", vec![3, 15], vec![]),
        ("1.3.0+build.7", vec![1, 3, 0], vec![]),
    ] {
        let v = Version::parse(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(v.nums, nums, "{text}");
        assert_eq!(
            v.pre,
            pre.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "{text}"
        );
    }
    for bad in ["", "2", "latest", "1..2", "1.2.x", "1.2-", "1.2-r c"] {
        assert!(Version::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn orders_releases_and_pre_releases() {
    let v = |s| Version::parse(s).unwrap();
    assert!(v("2.1.292") < v("2.1.302"));
    assert!(v("0.2.1-rc.2") < v("0.2.1"));
    assert!(v("0.2.1-alpha.5") < v("0.2.1-rc.2"));
    assert!(
        v("0.2.1-rc.2") < v("0.2.1-rc.10"),
        "numeric parts compare as numbers"
    );
    assert!(v("0.2.0-rc.2") < v("0.2.1-alpha.5"));
    assert!(v("5.8.3") < v("5.8.3.41334564"));
    assert!(v("1.19.3") < v("2.0.1"));
    assert_eq!(v("2.0"), v("2.0.0"));
    assert!(version::is_older("2.1.292", "2.1.302"));
    assert!(!version::is_older("2.1.302", "2.1.302"));
    assert!(!version::is_older("2.1.303", "2.1.302"));
    assert!(
        version::is_older("weird", "2.1.302"),
        "an unreadable version can be updated"
    );
}

#[test]
fn reads_versions_from_program_output() {
    for (out, want) in [
        ("2.1.302 (Claude Code)", Some("2.1.302")),
        ("opencode v2.0.32\n", Some("2.0.32")),
        ("1.19.3", Some("1.19.3")),
        ("0.2.1-rc.2\n", Some("0.2.1-rc.2")),
        ("zcode 0.16.9", Some("0.16.9")),
        ("pi 1.3.0.", Some("1.3.0")),
        ("Error: claude native binary not installed", None),
        ("", None),
    ] {
        assert_eq!(version::from_output(out).as_deref(), want, "{out:?}");
    }
}

#[test]
fn maps_pypi_and_npm_spellings() {
    for (pep, npm) in [
        ("0.2.1rc2", "0.2.1-rc.2"),
        ("0.2.1a5", "0.2.1-alpha.5"),
        ("0.1.2b1", "0.1.2-beta.1"),
        ("0.1.5", "0.1.5"),
    ] {
        assert_eq!(version::pep440_to_semver(pep), npm);
        assert_eq!(version::semver_to_pep440(npm).as_deref(), Some(pep));
    }
    assert_eq!(version::pep440_to_semver("0.0.0.dev0"), "0.0.0-dev.0");
    assert!(version::same("0.2.1rc2", "0.2.1-rc.2"));
    assert!(version::same("v2.0.32", "2.0.32"));
    assert!(!version::same("2.0.31", "2.0.32"));
    assert_eq!(version::semver_to_pep440("1.0.0-nightly.3"), None);
}
