//! `superba-catalog-gen`: maintains Superba's Codex settings catalog (`catalog/`).
//!
//! Typical flows (see `catalog/CONTRIBUTING.md`):
//!
//! ```text
//! # new upstream release
//! superba-catalog-gen build --git ../codex --only-new   # dist/ + changes + index
//! superba-catalog-gen overlay-sync                      # official English into overlays
//! superba-catalog-gen index && superba-catalog-gen coverage --write
//! superba-catalog-gen sign --key .secrets/catalog-signing.key
//!
//! # after editing overlays or curation.toml by hand
//! superba-catalog-gen index && superba-catalog-gen coverage --write
//! ```
//!
//! The generator never writes Chinese text.

mod diff;
mod dist;
mod extract;
mod features;
mod feeds;
mod git;
mod overlay_sync;
mod platform;
mod schema;
mod sign;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use superba_catalog::format::{INDEX_PATH, to_pretty_json};
use superba_catalog::integrity::{SignatureStatus, TRUSTED_KEYS, key_id};
use superba_catalog::store::{CatalogStore, TrustPolicy};
use superba_catalog::{Coverage, Os, Overlay};
use superba_core::CodexVersion;

#[derive(Parser)]
#[command(
    name = "superba-catalog-gen",
    version,
    about = "Maintain Superba's Codex settings catalog"
)]
struct Cli {
    /// Catalog directory.
    #[arg(long, global = true, default_value = "catalog")]
    catalog: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the release tags of a codex repository and whether the catalog has them.
    Tags {
        /// Path of an openai/codex clone (a blobless clone is enough).
        #[arg(
            long,
            required_unless_present = "ls_remote",
            conflicts_with = "ls_remote"
        )]
        git: Option<PathBuf>,
        /// Output of `git ls-remote --tags https://github.com/openai/codex` instead of
        /// a clone.
        #[arg(long)]
        ls_remote: Option<PathBuf>,
        /// Only print selected versions missing from dist/ (one per line).
        #[arg(long)]
        missing: bool,
    },
    /// Extract one version's catalog to stdout or a file.
    Extract {
        /// openai/codex clone to read the tag from.
        #[arg(long, conflicts_with = "src")]
        git: Option<PathBuf>,
        /// A checked-out openai/codex tree instead of --git.
        #[arg(long)]
        src: Option<PathBuf>,
        /// Release tag (`rust-v0.163.0`).
        #[arg(long)]
        tag: String,
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Regenerate dist/ (version files, change summaries, index) from a clone.
    Build {
        #[arg(long)]
        git: PathBuf,
        /// Skip versions whose file already comes from the same commit.
        #[arg(long)]
        only_new: bool,
    },
    /// Rewrite dist/index.json from the files on disk (after hand edits).
    Index,
    /// Create a new signing key file (refuses to overwrite).
    Keygen {
        #[arg(long)]
        out: PathBuf,
    },
    /// Sign dist/index.json. Keys: file paths or `env:NAME`.
    Sign {
        #[arg(long = "key", required = true)]
        keys: Vec<String>,
    },
    /// Verify the manifest and the signature.
    Verify {
        /// Fail when the index is not signed by a trusted key.
        #[arg(long)]
        require_signature: bool,
    },
    /// Copy official English text into the overlays; add and retire entries.
    OverlaySync {
        /// Report what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Text coverage report.
    Coverage {
        /// Write catalog/COVERAGE.md.
        #[arg(long)]
        write: bool,
        /// Fail if catalog/COVERAGE.md is out of date.
        #[arg(long)]
        check: bool,
        /// List entries without official English text.
        #[arg(long)]
        list_missing: bool,
    },
    /// Markdown diff of two version files.
    Diff { from: PathBuf, to: PathBuf },
    /// CI check: index, overlays and coverage are up to date; optionally re-extract
    /// every version from a clone and compare byte for byte.
    Check {
        #[arg(long)]
        git: Option<PathBuf>,
        #[arg(long)]
        require_signature: bool,
    },
    /// Pull request body: versions added since an older index, with their changes.
    Summary {
        /// The index before the update (missing file = everything is new).
        #[arg(long)]
        old_index: PathBuf,
    },
    /// Compare `codex features list` output with a catalog version.
    FeaturesCheck {
        /// Catalog version.
        #[arg(long)]
        version: String,
        /// File with the output of `codex features list`.
        #[arg(long)]
        list: PathBuf,
        /// OS the list comes from.
        #[arg(long, default_value = "linux")]
        os: String,
        /// The config.toml the list was produced with (default: an empty config, so
        /// effective states must equal the catalog defaults).
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Digest of all human-written overlay text (the update workflow checks it does
    /// not change).
    HumanDigest,
    /// The signed data feeds in catalog/feeds/ (models, prompts, prices, Codex and
    /// app versions).
    Feeds {
        #[command(subcommand)]
        cmd: feeds::FeedsCmd,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let cat = cli.catalog.as_path();
    match cli.cmd {
        Cmd::Tags {
            git,
            ls_remote,
            missing,
        } => {
            let tags = match (git, ls_remote) {
                (Some(g), _) => git::GitRepo::new(g).release_tags()?,
                (None, Some(f)) => git::tags_from_ls_remote(&std::fs::read_to_string(f)?),
                (None, None) => bail!("pass --git or --ls-remote"),
            };
            let versions: Vec<CodexVersion> = tags.iter().map(|(v, _)| v.clone()).collect();
            let sources = dist::Sources::load(cat)?.resolve(&versions)?;
            let have: Vec<CodexVersion> = dist::dist_versions(cat)?
                .into_iter()
                .map(|(v, _)| v)
                .collect();
            for (v, tag) in tags {
                let selected = sources.selects(&v)?;
                let present = have.contains(&v);
                if missing {
                    if selected && !present {
                        println!("{v}");
                    }
                } else {
                    let state = match (selected, present) {
                        (true, true) => "in catalog",
                        (true, false) => "MISSING",
                        (false, true) => "in catalog, not selected",
                        (false, false) => "",
                    };
                    println!("{tag:<32} {state}");
                }
            }
        }
        Cmd::Extract { git, src, tag, out } => {
            let v = git::version_of_tag(&tag)
                .with_context(|| format!("`{tag}` is not a release tag"))?;
            let files = match (git, src) {
                (Some(g), None) => git::GitRepo::new(g).read_sources(&tag)?,
                (None, Some(s)) => git::read_checkout(&s, &tag)?,
                _ => bail!("pass --git or --src"),
            };
            let vc = extract::extract(&v, &files)?;
            let text = to_pretty_json(&vc)?;
            match out {
                Some(p) => std::fs::write(&p, text)?,
                None => print!("{text}"),
            }
        }
        Cmd::Build { git, only_new } => {
            let r = dist::build(cat, &git::GitRepo::new(git), only_new)?;
            println!(
                "written: {}\nunchanged: {}\nremoved: {}",
                list(&r.written),
                r.unchanged.len(),
                list(&r.removed)
            );
            if dist::write_index(cat)? {
                println!("index updated");
            }
        }
        Cmd::Index => {
            dist::write_changes(cat)?;
            if dist::write_index(cat)? {
                println!("index updated");
            } else {
                println!("index unchanged");
            }
        }
        Cmd::Keygen { out } => {
            let pk = sign::keygen(&out)?;
            println!("key id:     {}", key_id(&pk));
            println!(
                "public key: {}",
                pk.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            println!(
                "Add the public key to TRUSTED_KEYS in crates/superba-catalog/src/integrity.rs."
            );
        }
        Cmd::Sign { keys } => {
            let keys = keys
                .iter()
                .map(|k| sign::load_key(k))
                .collect::<Result<Vec<_>>>()?;
            let ids = sign::sign(cat, &keys)?;
            for id in &ids {
                if !TRUSTED_KEYS.iter().any(|k| k.id == id) {
                    eprintln!(
                        "warning: key {id} is not in TRUSTED_KEYS; clients will not accept it"
                    );
                }
            }
            println!("signed with {}", ids.join(", "));
        }
        Cmd::Verify { require_signature } => verify(cat, require_signature)?,
        Cmd::OverlaySync { dry_run } => {
            let r = overlay_sync::sync(cat, dry_run)?;
            println!(
                "added {}, updated {}, retired {}, dropped {}; files: {}",
                r.added,
                r.updated,
                r.retired,
                r.dropped,
                list(&r.files_written)
            );
        }
        Cmd::Coverage {
            write,
            check,
            list_missing,
        } => {
            let cov = coverage(cat)?;
            let md = cov.to_markdown();
            let path = cat.join("COVERAGE.md");
            if check && std::fs::read_to_string(&path).ok().as_deref() != Some(md.as_str()) {
                bail!(
                    "{} is out of date: run `superba-catalog-gen coverage --write`",
                    path.display()
                );
            }
            if write {
                dist::write_if_changed(&path, md.as_bytes())?;
            }
            let t = cov.total;
            println!(
                "entries {}: English official {} / missing {} (with note {}); Chinese current {} / stale {} / missing {}",
                t.total,
                t.en_official,
                t.en_missing,
                t.en_note,
                t.zh_current,
                t.zh_stale,
                t.zh_missing
            );
            if list_missing {
                for id in &cov.en_missing_ids {
                    println!("{id}");
                }
            }
        }
        Cmd::Diff { from, to } => {
            let (a, b) = (dist::load_version(&from)?, dist::load_version(&to)?);
            let title = format!("Codex {} → {}", a.codex_version, b.codex_version);
            print!("{}", diff::markdown(&diff::diff(&a, &b), &title));
        }
        Cmd::Check {
            git,
            require_signature,
        } => check(cat, git.as_deref(), require_signature)?,
        Cmd::Summary { old_index } => summary(cat, &old_index)?,
        Cmd::FeaturesCheck {
            version,
            list,
            os,
            config,
        } => {
            use superba_core::CatalogSource as _;
            let os: Os = os.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let store = CatalogStore::from_dir(cat, TrustPolicy::ManifestOnly)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let v = CodexVersion::parse(&version).map_err(|e| anyhow::anyhow!("{e}"))?;
            if store
                .exact(&v)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .is_none()
            {
                bail!("the catalog has no version {v}");
            }
            let snap = superba_catalog::SnapshotSource::new(std::sync::Arc::new(store), os)
                .snapshot(Some(&v))
                .context("no snapshot")?;
            let listed = superba_core::FeaturesList::parse(&std::fs::read_to_string(&list)?);
            let consumer = superba_core::SettingsConsumer::new(
                superba_core::ConsumerKind::CliProcesses,
                "codex features list",
                Some(v),
            );
            let config = config.map(std::fs::read).transpose()?;
            let cc = superba_config::settings::cross_check_features(
                &consumer,
                &listed,
                Some(&snap),
                config.as_deref(),
            );
            for m in &cc.mismatches {
                println!("{m:?}");
            }
            println!(
                "{} rows, {} catalog features, {} differences",
                listed.entries.len(),
                snap.features.len(),
                cc.mismatches.len()
            );
            if !cc.mismatches.is_empty() {
                std::process::exit(2);
            }
        }
        Cmd::HumanDigest => {
            println!(
                "{}",
                overlay_sync::human_digest(&overlay_sync::read_overlays(cat)?)
            );
        }
        Cmd::Feeds { cmd } => feeds::run(cat, cmd)?,
    }
    Ok(())
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(", ")
    }
}

fn coverage(cat: &Path) -> Result<Coverage> {
    let files = overlay_sync::read_overlays(cat)?;
    let texts: Vec<(String, String)> = files
        .keys()
        .map(|n| {
            let p = cat.join(superba_catalog::format::OVERLAY_DIR).join(n);
            std::fs::read_to_string(&p).map(|t| (format!("overlay/{n}"), t))
        })
        .collect::<std::io::Result<_>>()?;
    let overlay = Overlay::from_files(texts.iter().map(|(n, t)| (n.as_str(), t.as_str())))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let versions = dist::dist_versions(cat)?;
    let latest = versions
        .iter()
        .rev()
        .find(|(v, _)| v.pre.is_none())
        .or(versions.last())
        .map(|(_, p)| dist::load_version(p))
        .transpose()?;
    Ok(Coverage::compute(&overlay, latest.as_ref(), Os::Linux))
}

fn verify(cat: &Path, require_signature: bool) -> Result<()> {
    let policy = if require_signature {
        TrustPolicy::RequireSignature
    } else {
        TrustPolicy::ManifestOnly
    };
    let store = CatalogStore::from_dir(cat, policy).map_err(|e| anyhow::anyhow!("{e}"))?;
    for v in store.versions().cloned().collect::<Vec<_>>() {
        store.exact(&v).map_err(|e| anyhow::anyhow!("{v}: {e}"))?;
    }
    println!(
        "manifest ok: serial {}, {} versions (latest {}), {} files, {} overlay entries",
        store.index().serial,
        store.index().versions.len(),
        store.index().latest,
        store.index().files.len(),
        store.overlay().len()
    );
    match store.signature() {
        SignatureStatus::Valid { key_id } => println!("signature ok: key {key_id}"),
        SignatureStatus::Unsigned => println!("signature: none (run `superba-catalog-gen sign`)"),
        SignatureStatus::Invalid { reason } => println!("signature INVALID: {reason}"),
    }
    Ok(())
}

fn check(cat: &Path, git: Option<&Path>, require_signature: bool) -> Result<()> {
    let mut problems = Vec::new();
    if let Some(g) = git {
        let repo = git::GitRepo::new(g);
        for (v, path) in dist::dist_versions(cat)? {
            let old = dist::load_version(&path)?;
            let src = repo.read_sources(&old.source.tag)?;
            let fresh = to_pretty_json(&extract::extract(&v, &src)?)?;
            if std::fs::read_to_string(&path)? != fresh {
                problems.push(format!(
                    "{} differs from a fresh extraction",
                    path.display()
                ));
            }
        }
    }
    let r = overlay_sync::sync(cat, true)?;
    if !r.files_written.is_empty() {
        problems.push(format!(
            "overlays are out of date ({}): run `superba-catalog-gen overlay-sync`",
            r.files_written.join(", ")
        ));
    }
    let previous = dist::read_index(cat)?;
    let index = dist::compute_index(cat, previous.as_ref())?;
    if std::fs::read_to_string(cat.join(INDEX_PATH)).ok() != Some(to_pretty_json(&index)?) {
        problems.push("dist/index.json is out of date: run `superba-catalog-gen index`".into());
    }
    problems.extend(common_problems(cat)?);
    let md = coverage(cat)?.to_markdown();
    if std::fs::read_to_string(cat.join("COVERAGE.md"))
        .ok()
        .as_deref()
        != Some(md.as_str())
    {
        problems
            .push("COVERAGE.md is out of date: run `superba-catalog-gen coverage --write`".into());
    }
    if problems.is_empty() {
        verify(cat, require_signature)?;
        println!("catalog is up to date");
        Ok(())
    } else {
        for p in &problems {
            eprintln!("- {p}");
        }
        bail!("{} problem(s)", problems.len())
    }
}

/// `[common]` paths of `curation.toml` that no catalog version knows.
fn common_problems(cat: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(cat.join(superba_catalog::format::CURATION_PATH))?;
    let curation = superba_catalog::Curation::parse(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
    let versions = dist::dist_versions(cat)?
        .into_iter()
        .map(|(_, p)| dist::load_version(&p))
        .collect::<Result<Vec<_>>>()?;
    let mut problems = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for path in curation.common() {
        if !seen.insert(path.as_str()) {
            problems.push(format!("curation.toml: [common] lists `{path}` twice"));
            continue;
        }
        let known = versions
            .iter()
            .any(|v| match path.strip_prefix("features.") {
                Some(key) => v.features.iter().any(|f| f.key == key),
                None => v.config_keys.iter().any(|k| k.path == *path),
            });
        if !known {
            problems.push(format!(
                "curation.toml: [common] path `{path}` is in no catalog version"
            ));
        }
    }
    Ok(problems)
}

fn summary(cat: &Path, old_index: &Path) -> Result<()> {
    let old: Option<superba_catalog::Index> = match std::fs::read(old_index) {
        Ok(b) => {
            Some(superba_catalog::integrity::parse_index(&b).map_err(|e| anyhow::anyhow!("{e}"))?)
        }
        Err(_) => None,
    };
    let new = dist::read_index(cat)?.context("no dist/index.json")?;
    let added: Vec<_> = new
        .versions
        .iter()
        .filter(|v| {
            old.as_ref()
                .is_none_or(|o| !o.versions.iter().any(|x| x.version == v.version))
        })
        .collect();
    let changed: Vec<_> = new
        .versions
        .iter()
        .filter(|v| {
            old.as_ref().is_some_and(|o| {
                o.versions.iter().any(|x| x.version == v.version)
                    && o.file(&v.path).map(|f| &f.sha256) != new.file(&v.path).map(|f| &f.sha256)
            })
        })
        .collect();
    println!("## Codex settings catalog update\n");
    println!(
        "Generated by `superba-catalog-gen` from openai/codex release tags. The generator \
         never writes Chinese text: new entries arrive with empty `zh_name` / `zh` fields, \
         which maintainers write entry by entry from the codex source before the release \
         (catalog/CONTRIBUTING.md).\n"
    );
    println!(
        "- Index serial: {} → {}",
        old.as_ref().map_or(0, |o| o.serial),
        new.serial
    );
    println!("- Latest stable: {}", new.latest);
    let names = |v: &[&superba_catalog::format::IndexVersion]| {
        v.iter().map(|x| x.version.clone()).collect::<Vec<_>>()
    };
    println!("- Versions added: {}", list(&names(&added)));
    println!("- Versions regenerated: {}\n", list(&names(&changed)));
    for v in added.iter().chain(changed.iter()) {
        let p = cat.join(&v.changes);
        if let Ok(text) = std::fs::read_to_string(&p) {
            let body = text.split_once("-->\n\n").map_or(text.as_str(), |(_, b)| b);
            println!("{body}");
        }
    }
    let t = coverage(cat)?.total;
    println!(
        "### Text coverage\n\n{} entries: official English {} / missing {}; Chinese current {} / stale {} / missing {}.",
        t.total, t.en_official, t.en_missing, t.zh_current, t.zh_stale, t.zh_missing
    );
    let sig = std::fs::read(cat.join(superba_catalog::format::SIGNATURE_PATH)).ok();
    let idx = std::fs::read(cat.join(INDEX_PATH))?;
    let status = superba_catalog::integrity::verify_signature(
        &idx,
        sig.as_deref(),
        TRUSTED_KEYS,
        superba_catalog::integrity::REVOKED_KEY_IDS,
    );
    if !status.is_valid() {
        println!(
            "\n> The index is not signed yet: a maintainer runs `superba-catalog-gen sign` before merging."
        );
    }
    Ok(())
}
