//! `app-versions.json` and `codex-versions.json`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use superba_catalog::feeds::format::{
    APP_VERSIONS_FILE, AppVersionsFeed, CODEX_VERSIONS_FILE, CodexVersionsFeed, FEEDS_FORMAT,
    to_pretty_json,
};
use superba_core::{AppRelease, CodexRelease, CodexVersion, Timestamp};

use super::parse::Seen;
use crate::dist::{Sources, write_if_changed};
use crate::git::GitRepo;

/// Compare dotted numeric app versions (`26.1013.40934`).
fn app_version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

/// Merge newly seen builds and bundled Codex versions into `app-versions.json`.
/// Known builds keep what they had; a package of the same OS, kind and
/// architecture is replaced by the new one.
pub fn write_app(
    catalog: &Path,
    seen: Vec<Seen>,
    bundled: &[(String, CodexVersion)],
) -> Result<usize> {
    let path = catalog.join("feeds").join(APP_VERSIONS_FILE);
    let old: AppVersionsFeed = read_json(&path)?;
    let mut by_version: BTreeMap<String, AppRelease> = old
        .releases
        .into_iter()
        .map(|r| (r.version.clone(), r))
        .collect();
    for s in seen {
        if !s.version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            bail!("unexpected app version `{}`", s.version);
        }
        let r = by_version
            .entry(s.version.clone())
            .or_insert_with(|| AppRelease {
                version: s.version.clone(),
                date: None,
                bundled_codex: None,
                packages: Vec::new(),
                store_product_id: None,
            });
        if r.date.is_none() {
            r.date = s.date;
        }
        if let Some(p) = s.package {
            r.packages
                .retain(|q| !(q.os == p.os && q.kind == p.kind && q.arch == p.arch));
            r.packages.push(p);
        }
        if s.store_product_id.is_some() {
            r.store_product_id = s.store_product_id;
        }
    }
    for (app, codex) in bundled {
        let r = by_version.entry(app.clone()).or_insert_with(|| AppRelease {
            version: app.clone(),
            date: None,
            bundled_codex: None,
            packages: Vec::new(),
            store_product_id: None,
        });
        r.bundled_codex = Some(codex.clone());
    }
    let mut releases: Vec<AppRelease> = by_version.into_values().collect();
    for r in &mut releases {
        r.packages.sort_by(|a, b| {
            (format!("{:?}", a.os), format!("{:?}", a.kind), &a.arch).cmp(&(
                format!("{:?}", b.os),
                format!("{:?}", b.kind),
                &b.arch,
            ))
        });
    }
    releases.sort_by_key(|r| std::cmp::Reverse(app_version_key(&r.version)));
    let n = releases.len();
    let feed = AppVersionsFeed {
        format: FEEDS_FORMAT,
        releases,
    };
    std::fs::create_dir_all(path.parent().context("feeds dir")?)?;
    write_if_changed(&path, to_pretty_json(&feed)?.as_bytes())?;
    Ok(n)
}

/// `<sha256>  codex-package-<target>.tar.gz` lines -> target -> sha256 (Bazel
/// builds and other archives are skipped).
pub fn parse_sums(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(sha), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        let name = name.trim_start_matches('*');
        let Some(target) = name
            .strip_prefix("codex-package-")
            .and_then(|n| n.strip_suffix(".tar.gz"))
        else {
            continue;
        };
        if target.ends_with("-bazel")
            || sha.len() != 64
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            continue;
        }
        out.insert(target.to_owned(), sha.to_ascii_lowercase());
    }
    out
}

/// Regenerate `codex-versions.json`: stable releases from `floor`, the catalog's
/// extra versions and every version an app build bundles; dates from the tags,
/// package checksums from `<sums_dir>/<tag>.SHA256SUMS` (kept from the previous
/// file when absent), `bundled_by` from `app-versions.json`.
pub fn write_codex(
    catalog: &Path,
    repo: &GitRepo,
    floor: &CodexVersion,
    sums_dir: Option<&Path>,
) -> Result<(usize, usize)> {
    let feeds = catalog.join("feeds");
    let path = feeds.join(CODEX_VERSIONS_FILE);
    let old: CodexVersionsFeed = read_json(&path)?;
    let apps: AppVersionsFeed = read_json(&feeds.join(APP_VERSIONS_FILE))?;
    let sources = Sources::load(catalog)?;
    let mut bundled_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &apps.releases {
        if let Some(c) = &a.bundled_codex {
            bundled_by
                .entry(c.to_string())
                .or_default()
                .push(a.version.clone());
        }
    }
    let mut releases = Vec::new();
    let mut with_sums = 0;
    for (v, tag) in repo.release_tags()? {
        let s = v.to_string();
        if sources.exclude_versions.contains(&s) {
            continue;
        }
        let wanted = (v.pre.is_none() && v >= *floor)
            || sources.extra_versions.contains(&s)
            || bundled_by.contains_key(&s);
        if !wanted {
            continue;
        }
        let mut sha256 = sums_dir
            .map(|d| d.join(format!("{tag}.SHA256SUMS")))
            .filter(|p| p.is_file())
            .map(|p| std::fs::read_to_string(&p).map(|t| parse_sums(&t)))
            .transpose()?
            .unwrap_or_default();
        if sha256.is_empty()
            && let Some(prev) = old.releases.iter().find(|r| r.version == v)
        {
            sha256 = prev.sha256.clone();
        }
        if !sha256.is_empty() {
            with_sums += 1;
        }
        let mut by = bundled_by.get(&s).cloned().unwrap_or_default();
        by.sort_by_key(|a| std::cmp::Reverse(app_version_key(a)));
        releases.push(CodexRelease {
            date: repo.tag_date(&tag)?.map(Timestamp::from_unix_secs),
            prerelease: v.pre.is_some(),
            version: v,
            tag,
            bundled_by: by,
            sha256,
        });
    }
    for v in bundled_by.keys() {
        if !releases.iter().any(|r| r.version.to_string() == *v) {
            bail!("an app bundles Codex {v}, which has no release tag");
        }
    }
    releases.sort_by(|a, b| b.version.cmp(&a.version));
    let n = releases.len();
    let feed = CodexVersionsFeed {
        format: FEEDS_FORMAT,
        releases,
    };
    std::fs::create_dir_all(&feeds)?;
    write_if_changed(&path, to_pretty_json(&feed)?.as_bytes())?;
    Ok((n, with_sums))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_keep_only_codex_packages() {
        let t = "aa  codex-package-x86_64-unknown-linux-musl.tar.gz\n\
                 0000000000000000000000000000000000000000000000000000000000000001  codex-package-x86_64-unknown-linux-musl.tar.gz\n\
                 0000000000000000000000000000000000000000000000000000000000000002  codex-package-x86_64-unknown-linux-musl-bazel.tar.gz\n\
                 0000000000000000000000000000000000000000000000000000000000000003  codex-app-server-package-x86_64-apple-darwin.tar.gz\n\
                 00000000000000000000000000000000000000000000000000000000000000AB *codex-package-aarch64-apple-darwin.tar.gz\n";
        let s = parse_sums(t);
        assert_eq!(s.len(), 2);
        assert!(s["x86_64-unknown-linux-musl"].ends_with('1'));
        assert!(s["aarch64-apple-darwin"].ends_with("ab"));
    }

    #[test]
    fn app_versions_sort_numerically() {
        let mut v = vec![
            "26.1007.71326",
            "26.1013.40934",
            "26.1013.31256",
            "26.715.1",
        ];
        v.sort_by_key(|a| std::cmp::Reverse(app_version_key(a)));
        assert_eq!(
            v,
            [
                "26.1013.40934",
                "26.1013.31256",
                "26.1007.71326",
                "26.715.1"
            ]
        );
    }
}
