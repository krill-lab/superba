use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};

use pretty_assertions::assert_eq;
use superba_config::version::{
    AppVersion, Confidence, Gate, bundled_codex_for_app, check, min_version_table,
    parse_codex_version_output, required_capabilities,
};
use superba_core::Error;
use superba_core::codex::{CodexVersion, VersionCapability};

#[test]
fn codex_version_output_accepts_release_and_prerelease() {
    assert_eq!(
        parse_codex_version_output("codex-cli 0.162.0\n").unwrap(),
        CodexVersion::new(0, 162, 0)
    );
    let prerelease = parse_codex_version_output("codex 0.162.0-alpha.17.2").unwrap();
    assert_eq!(prerelease.triple(), (0, 162, 0));
    assert_eq!(prerelease.pre.as_deref(), Some("alpha.17.2"));
    assert_eq!(prerelease.to_string(), "0.162.0-alpha.17.2");
    assert_eq!(
        parse_codex_version_output("  v0.162.0  ").unwrap(),
        CodexVersion::new(0, 162, 0)
    );
}

#[test]
fn codex_version_output_prefers_codex_over_node_warning_versions() {
    let text = "\n(node:1234) Warning: Node.js v22.14.0 is deprecated\n  codex-cli 0.162.0-alpha.17.2\nNode v24.1.0\n";
    assert_eq!(
        parse_codex_version_output(text).unwrap(),
        CodexVersion {
            major: 0,
            minor: 162,
            patch: 0,
            pre: Some("alpha.17.2".into())
        }
    );
}

#[test]
fn garbage_codex_output_is_a_parse_error() {
    for text in [
        "",
        " \n\t",
        "codex-cli not-a-version",
        "codex-cli 1.2.3.4",
        "random diagnostic",
    ] {
        assert!(
            matches!(parse_codex_version_output(text), Err(Error::Parse { .. })),
            "{text:?}"
        );
    }
}

#[test]
fn app_versions_parse_and_display_with_optional_patch() {
    for (input, expected, display) in [
        (
            "26.1013",
            AppVersion {
                major: 26,
                minor: 1013,
                patch: None,
            },
            "26.1013",
        ),
        (
            "ChatGPT 26.1013.2",
            AppVersion {
                major: 26,
                minor: 1013,
                patch: Some(2),
            },
            "26.1013.2",
        ),
        (
            "v26.1007",
            AppVersion {
                major: 26,
                minor: 1007,
                patch: None,
            },
            "26.1007",
        ),
        (
            "(26.1013.1-beta)",
            AppVersion {
                major: 26,
                minor: 1013,
                patch: Some(1),
            },
            "26.1013.1",
        ),
    ] {
        let parsed = AppVersion::parse(input).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(parsed.to_string(), display);
        assert_eq!(AppVersion::parse(display).unwrap(), parsed);
    }
    for text in ["1.2.3.4", "ChatGPT 1.2.3.4", "26", "26..1", "garbage", ""] {
        assert_eq!(AppVersion::parse(text), None, "{text:?}");
    }
}

#[test]
fn app_versions_sort_numerically_by_major_minor_and_patch() {
    let mut versions: Vec<_> = ["27.1", "26.1013.10", "26.1007", "26.1013.2", "26.1013"]
        .into_iter()
        .map(|s| AppVersion::parse(s).unwrap())
        .collect();
    versions.sort();
    assert_eq!(
        versions.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["26.1007", "26.1013", "26.1013.2", "26.1013.10", "27.1"]
    );
}

#[test]
fn app_version_ordering_equality_and_hash_collections_agree_for_zero_patch() {
    let versions: Vec<_> = ["26.1013", "26.1013.0", "26.1013.1"]
        .into_iter()
        .map(|s| AppVersion::parse(s).unwrap())
        .collect();
    assert_eq!(versions[0], versions[1]);
    for a in &versions {
        for b in &versions {
            assert_eq!(
                a.cmp(b) == Ordering::Equal,
                a == b,
                "Ord and Eq disagree for {a} and {b}"
            );
            assert_eq!(a.partial_cmp(b), Some(a.cmp(b)));
        }
    }
    assert_eq!(
        versions.iter().collect::<BTreeSet<_>>().len(),
        versions.iter().collect::<HashSet<_>>().len()
    );
}

#[test]
fn known_app_bundles_include_patches_but_do_not_guess_unknown_builds() {
    for input in ["26.1007", "26.1013", "ChatGPT 26.1013.2"] {
        assert_eq!(
            bundled_codex_for_app(&AppVersion::parse(input).unwrap()),
            Some(CodexVersion::new(0, 162, 0))
        );
    }
    for input in ["26.1008", "27.1013", "25.1007"] {
        assert_eq!(
            bundled_codex_for_app(&AppVersion::parse(input).unwrap()),
            None
        );
    }
}

#[test]
fn capability_gates_cover_supported_too_old_unknown_and_prerelease_floor() {
    let required = CodexVersion::new(0, 160, 0);
    let old = CodexVersion::new(0, 159, 99);
    let alpha = CodexVersion::parse("0.160.0-alpha.1").unwrap();
    for capability in [
        VersionCapability::TopLevelOpenaiBaseUrl,
        VersionCapability::PersonalAccessToken,
        VersionCapability::WhamAccountsCheck,
    ] {
        for version in [&required, &alpha, &CodexVersion::new(0, 162, 0)] {
            let gate = check(capability, Some(version));
            assert_eq!(gate, Gate::Supported);
            assert!(gate.is_supported());
        }
        let gate = check(capability, Some(&old));
        assert_eq!(
            gate,
            Gate::TooOld {
                required: required.clone(),
                found: old.clone()
            }
        );
        assert!(!gate.is_supported());
        let unknown = check(capability, None);
        assert_eq!(
            unknown,
            Gate::Unknown {
                required: required.clone()
            }
        );
        assert!(!unknown.is_supported());
    }
    let table = min_version_table();
    assert_eq!(table.len(), 3);
    assert_eq!(
        table
            .iter()
            .find(|r| r.capability == VersionCapability::WhamAccountsCheck)
            .unwrap()
            .confidence,
        Confidence::Verified
    );
    assert!(
        table
            .iter()
            .filter(|r| r.capability != VersionCapability::WhamAccountsCheck)
            .all(|r| r.confidence == Confidence::Provisional)
    );
}

#[test]
fn token_capabilities_include_pat_only_requirements_exactly_once() {
    assert_eq!(
        required_capabilities(false),
        vec![VersionCapability::TopLevelOpenaiBaseUrl]
    );
    assert_eq!(
        required_capabilities(true),
        vec![
            VersionCapability::TopLevelOpenaiBaseUrl,
            VersionCapability::PersonalAccessToken,
            VersionCapability::WhamAccountsCheck
        ]
    );
}
