//! Reading upstream sources: a git repository at a tag (works with blobless
//! clones; blobs are fetched on demand) or a plain checkout directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use superba_catalog::format::sources;
use superba_core::CodexVersion;

/// Tag prefix of Codex releases.
pub const TAG_PREFIX: &str = "rust-v";

/// The Codex version of a release tag, or `None` for tags that are not releases.
/// Accepts `rust-vX.Y.Z` and `rust-vX.Y.Z-(alpha|beta|rc).N[.M...]`; rejects junk
/// such as `rust-vrust-v0.140.0-alpha.6`, `rust-v.0.0.2504291921` or
/// `rust-v0.11.0-alpha`.
pub fn version_of_tag(tag: &str) -> Option<CodexVersion> {
    let rest = tag.strip_prefix(TAG_PREFIX)?;
    let (core, pre) = match rest.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (rest, None),
    };
    let numeric = |s: &str| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| numeric(p)) {
        return None;
    }
    if let Some(pre) = pre {
        let mut ids = pre.split('.');
        let label = ids.next()?;
        if !matches!(label, "alpha" | "beta" | "rc") {
            return None;
        }
        let nums: Vec<&str> = ids.collect();
        if nums.is_empty() || !nums.iter().all(|n| numeric(n)) {
            return None;
        }
    }
    let v = CodexVersion::parse(rest).ok()?;
    (v.to_string() == rest).then_some(v)
}

/// Release tags in `git ls-remote --tags` output (`<sha>\trefs/tags/<tag>[^{}]`),
/// with their versions, ascending and without duplicates.
pub fn tags_from_ls_remote(text: &str) -> Vec<(CodexVersion, String)> {
    let mut tags: Vec<(CodexVersion, String)> = text
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(|r| r.strip_prefix("refs/tags/"))
        .map(|t| t.trim_end_matches("^{}"))
        .filter_map(|t| version_of_tag(t).map(|v| (v, t.to_owned())))
        .collect();
    tags.sort();
    tags.dedup_by(|a, b| a.0 == b.0);
    tags
}

/// The three source files of one version plus their provenance.
#[derive(Clone, Debug)]
pub struct SourceFiles {
    pub tag: String,
    pub commit: String,
    pub schema: String,
    pub lib_rs: String,
    pub legacy_rs: String,
    /// path -> git blob id.
    pub blobs: Vec<(String, String)>,
}

/// A git repository of `openai/codex`.
#[derive(Clone, Debug)]
pub struct GitRepo {
    pub path: PathBuf,
}

impl GitRepo {
    pub fn new(path: impl Into<PathBuf>) -> GitRepo {
        GitRepo { path: path.into() }
    }

    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        // No automatic `git gc` from on-demand blob fetches in a shared clone.
        let out = superba_core::process::command("git")
            .args(["-c", "gc.auto=0", "-C"])
            .arg(&self.path)
            .args(args)
            .output()
            .with_context(|| format!("run git {}", args.join(" ")))?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    fn git_text(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8(self.git(args)?)
            .context("git output is not UTF-8")?
            .trim()
            .to_owned())
    }

    /// Release tags in the repository with their versions, ascending.
    pub fn release_tags(&self) -> Result<Vec<(CodexVersion, String)>> {
        let text = self.git_text(&["tag", "--list", &format!("{TAG_PREFIX}*")])?;
        let mut tags: Vec<(CodexVersion, String)> = text
            .lines()
            .filter_map(|t| version_of_tag(t.trim()).map(|v| (v, t.trim().to_owned())))
            .collect();
        tags.sort();
        tags.dedup_by(|a, b| a.0 == b.0);
        Ok(tags)
    }

    /// Commit an (annotated) tag points to.
    pub fn peel(&self, tag: &str) -> Result<String> {
        self.git_text(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}^{{commit}}"),
        ])
        .with_context(|| format!("tag {tag} not found"))
    }

    /// A file at `tag` (`None` when the tag has no such file).
    pub fn read_file(&self, tag: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let commit = self.peel(tag)?;
        let spec = format!("{commit}:{path}");
        let Ok(blob) = self.git_text(&["rev-parse", "--verify", "--quiet", &spec]) else {
            return Ok(None);
        };
        Ok(Some(self.git(&["cat-file", "blob", &blob])?))
    }

    /// Creation date of a tag (annotated: tagger date; lightweight: commit date),
    /// Unix seconds.
    pub fn tag_date(&self, tag: &str) -> Result<Option<i64>> {
        let text = self.git_text(&[
            "for-each-ref",
            "--format=%(creatordate:unix)",
            &format!("refs/tags/{tag}"),
        ])?;
        Ok(text.lines().next().and_then(|l| l.trim().parse().ok()))
    }

    /// Read the source files at `tag`.
    pub fn read_sources(&self, tag: &str) -> Result<SourceFiles> {
        let commit = self.peel(tag)?;
        let mut texts = Vec::new();
        let mut blobs = Vec::new();
        for path in sources::ALL {
            let spec = format!("{commit}:{path}");
            let blob = self
                .git_text(&["rev-parse", "--verify", "--quiet", &spec])
                .with_context(|| format!("{path} does not exist at {tag}"))?;
            let bytes = self.git(&["cat-file", "blob", &blob])?;
            texts.push(
                String::from_utf8(bytes)
                    .with_context(|| format!("{path} at {tag} is not UTF-8"))?,
            );
            blobs.push((path.to_owned(), blob));
        }
        let [schema, lib_rs, legacy_rs]: [String; 3] = texts
            .try_into()
            .map_err(|_| anyhow::anyhow!("internal: three sources"))?;
        Ok(SourceFiles {
            tag: tag.to_owned(),
            commit,
            schema,
            lib_rs,
            legacy_rs,
            blobs,
        })
    }
}

/// Read the source files from a checked-out tree (`git checkout <tag>`); provenance
/// comes from `git` when the tree is a repository.
pub fn read_checkout(root: &Path, tag: &str) -> Result<SourceFiles> {
    let mut texts = Vec::new();
    let mut blobs = Vec::new();
    for path in sources::ALL {
        let p = root.join(path);
        let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let blob = superba_core::process::command("git")
            .args(["hash-object", "--no-filters"])
            .arg(&p)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        texts.push(text);
        if !blob.is_empty() {
            blobs.push((path.to_owned(), blob));
        }
    }
    let commit = superba_core::process::command("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let [schema, lib_rs, legacy_rs]: [String; 3] = texts
        .try_into()
        .map_err(|_| anyhow::anyhow!("internal: three sources"))?;
    Ok(SourceFiles {
        tag: tag.to_owned(),
        commit,
        schema,
        lib_rs,
        legacy_rs,
        blobs,
    })
}

#[cfg(test)]
mod tests {
    use super::{tags_from_ls_remote, version_of_tag};

    #[test]
    fn ls_remote_output() {
        let text = "a1\trefs/tags/rust-v0.163.0\nb2\trefs/tags/rust-v0.163.0^{}\n\
                    c3\trefs/tags/rust-v0.162.0-alpha.17.2^{}\nd4\trefs/tags/rust-vrust-v1.0.0\n";
        let tags: Vec<String> = tags_from_ls_remote(text)
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(tags, ["rust-v0.162.0-alpha.17.2", "rust-v0.163.0"]);
    }

    #[test]
    fn release_tags_are_parsed_strictly() {
        for (tag, want) in [
            ("rust-v0.163.0", Some("0.163.0")),
            ("rust-v0.162.0-alpha.17.2", Some("0.162.0-alpha.17.2")),
            ("rust-v0.153.0-alpha.5.1", Some("0.153.0-alpha.5.1")),
            ("rust-v1.0.0-beta.2", Some("1.0.0-beta.2")),
            ("rust-vrust-v0.140.0-alpha.6", None),
            ("rust-vv0.99.0-alpha.8", None),
            ("rust-v.0.0.2504291921", None),
            ("rust-v0.9.0-beta", None),
            ("rust-v0.11.0-alpha", None),
            ("rust-v0.11.0-preview.1", None),
            ("rust-v01.2.3", None),
            ("v0.163.0", None),
            ("rust-v0.163", None),
        ] {
            assert_eq!(
                version_of_tag(tag).map(|v| v.to_string()).as_deref(),
                want,
                "{tag}"
            );
        }
    }
}
