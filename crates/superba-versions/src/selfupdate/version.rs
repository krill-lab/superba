//! Semantic version handling for Superba's own releases.

use std::cmp::Ordering;

use semver::Version;

/// The version a release tag names. Only the tags the release workflow accepts:
/// `vX.Y.Z` or `vX.Y.Z-(alpha|beta|rc).N`, nothing else (the tag ends up in a
/// download URL).
pub fn parse_tag(tag: &str) -> Option<Version> {
    let v = Version::parse(tag.strip_prefix('v')?).ok()?;
    let pre_ok = if v.pre.is_empty() {
        true
    } else {
        let pre = v.pre.as_str();
        pre.split_once('.').is_some_and(|(kind, n)| {
            matches!(kind, "alpha" | "beta" | "rc")
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        })
    };
    (pre_ok && v.build.is_empty() && format!("v{v}") == tag).then_some(v)
}

/// A running version string (`CARGO_PKG_VERSION`), with or without a `v`.
pub fn parse_version(text: &str) -> Option<Version> {
    Version::parse(text.trim().trim_start_matches('v')).ok()
}

/// `candidate` is a newer release than `current` (semver precedence: a
/// pre-release is older than its release, `0.2.0-rc.1` is newer than `0.1.9`;
/// build metadata is ignored).
pub fn is_newer(candidate: &Version, current: &Version) -> bool {
    candidate.cmp_precedence(current) == Ordering::Greater
}

/// Precedence order of two versions.
pub fn cmp(a: &Version, b: &Version) -> Ordering {
    a.cmp_precedence(b)
}
