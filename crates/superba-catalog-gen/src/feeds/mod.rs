//! `superba-catalog-gen feeds ...`: the signed data feeds in `catalog/feeds/`
//! (model list, prompts, prices, Codex releases, desktop app builds).
//!
//! Generation reads a local `openai/codex` clone and index files the workflow
//! downloads (APT `Packages`, RPM `primary.xml.gz`, appcasts, the Windows Store
//! JSON, `codex-package_SHA256SUMS`); the generator itself makes no network
//! requests. `prices.json` and `models-extra.json` are written by people.

pub mod models;
pub mod parse;
pub mod versions;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use clap::Subcommand;
use ed25519_dalek::Signer as _;
use superba_catalog::FeedData;
use superba_catalog::feeds::FEEDS_SIGNATURE_CONTEXT;
use superba_catalog::feeds::format::{
    FEEDS_FORMAT, FeedFile, FeedIndex, INDEX_FILE, PRICES_FILE, PricesFeed, SIGNATURE_FILE,
    parse_feed, to_pretty_json,
};
use superba_catalog::format::{SIGNATURE_FORMAT, SignatureEntry, SignatureFile};
use superba_catalog::integrity::{
    REVOKED_KEY_IDS, SignatureStatus, TRUSTED_KEYS, key_id, sha256_hex, signed_message_with_context,
};
use superba_catalog::store::TrustPolicy;
use superba_core::CodexVersion;

use crate::dist::write_if_changed;
use crate::git::GitRepo;

/// Oldest Codex version the feeds describe (prompts, releases): below the desktop
/// app's minimum app-server version (0.141.0).
pub const DEFAULT_FLOOR: &str = "0.140.0";

#[derive(Subcommand)]
pub enum FeedsCmd {
    /// models.json (OpenAI entries of the selected releases + models-extra.json),
    /// prompts.json and prompts/ from a codex clone.
    Models {
        #[arg(long)]
        git: PathBuf,
        /// Oldest version whose prompt.md is published.
        #[arg(long, default_value = DEFAULT_FLOOR)]
        prompt_floor: String,
    },
    /// app-versions.json from downloaded index files.
    AppVersions {
        /// APT `Packages` files (`dists/stable/main/binary-<arch>/Packages`).
        #[arg(long)]
        apt: Vec<PathBuf>,
        /// RPM primary metadata as `<arch>=<file>` (`x86_64=..-primary.xml.gz`).
        #[arg(long)]
        rpm: Vec<String>,
        /// Sparkle appcasts (macOS).
        #[arg(long)]
        appcast: Vec<PathBuf>,
        /// `windows-store-update.json`.
        #[arg(long)]
        windows_store: Option<PathBuf>,
        /// `<app version>=<codex version>` the build bundles (from the package).
        #[arg(long)]
        bundled: Vec<String>,
    },
    /// codex-versions.json from the tags of a codex clone and downloaded
    /// `codex-package_SHA256SUMS` files (`<dir>/<tag>.SHA256SUMS`).
    CodexVersions {
        #[arg(long)]
        git: PathBuf,
        #[arg(long)]
        sums_dir: Option<PathBuf>,
        /// Oldest stable release listed.
        #[arg(long, default_value = DEFAULT_FLOOR)]
        floor: String,
    },
    /// Rewrite feeds/index.json (the serial increases when a file changes; a stale
    /// signature is removed).
    Index,
    /// Sign feeds/index.json. Keys: file paths or `env:NAME`.
    Sign {
        #[arg(long = "key", required = true)]
        keys: Vec<String>,
    },
    /// Verify the feeds (index, digests, formats, signature).
    Verify {
        #[arg(long)]
        require_signature: bool,
    },
    /// CI check: index up to date, prices valid, everything verifies.
    Check {
        #[arg(long)]
        require_signature: bool,
    },
}

fn version(s: &str) -> Result<CodexVersion> {
    CodexVersion::parse(s).map_err(|e| anyhow::anyhow!("{s}: {e}"))
}

pub fn run(catalog: &Path, cmd: FeedsCmd) -> Result<()> {
    match cmd {
        FeedsCmd::Models { git, prompt_floor } => {
            let r = models::write(catalog, &GitRepo::new(git), &version(&prompt_floor)?)?;
            println!(
                "models.json: {} models (newest release {}); prompts: {} versions, {} files",
                r.models, r.codex_version, r.prompts, r.prompt_files
            );
        }
        FeedsCmd::AppVersions {
            apt,
            rpm,
            appcast,
            windows_store,
            bundled,
        } => {
            let mut seen = Vec::new();
            for p in &apt {
                seen.extend(parse::apt_packages(&read_text(p)?)?);
            }
            for spec in &rpm {
                let (arch, file) = spec
                    .split_once('=')
                    .with_context(|| format!("--rpm {spec}: expected <arch>=<file>"))?;
                if !arch.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    bail!("--rpm {spec}: bad architecture");
                }
                seen.extend(parse::rpm_primary(&std::fs::read(file)?, arch)?);
            }
            for p in &appcast {
                seen.extend(parse::appcast(&read_text(p)?)?);
            }
            if let Some(p) = &windows_store {
                seen.extend(parse::windows_store(&read_text(p)?)?);
            }
            let bundled = bundled
                .iter()
                .map(|b| {
                    let (a, c) = b
                        .split_once('=')
                        .with_context(|| format!("--bundled {b}: expected <app>=<codex>"))?;
                    Ok((a.trim().to_owned(), version(c.trim())?))
                })
                .collect::<Result<Vec<_>>>()?;
            let n = versions::write_app(catalog, seen, &bundled)?;
            println!("app-versions.json: {n} builds");
        }
        FeedsCmd::CodexVersions {
            git,
            sums_dir,
            floor,
        } => {
            let (n, sums) = versions::write_codex(
                catalog,
                &GitRepo::new(git),
                &version(&floor)?,
                sums_dir.as_deref(),
            )?;
            println!("codex-versions.json: {n} releases ({sums} with package checksums)");
        }
        FeedsCmd::Index => {
            if write_index(catalog)? {
                println!("feeds index updated");
            } else {
                println!("feeds index unchanged");
            }
        }
        FeedsCmd::Sign { keys } => {
            let keys = keys
                .iter()
                .map(|k| crate::sign::load_key(k))
                .collect::<Result<Vec<_>>>()?;
            let ids = sign(catalog, &keys)?;
            for id in &ids {
                if !TRUSTED_KEYS.iter().any(|k| k.id == id) {
                    eprintln!(
                        "warning: key {id} is not in TRUSTED_KEYS; clients will not accept it"
                    );
                }
            }
            println!("feeds signed with {}", ids.join(", "));
        }
        FeedsCmd::Verify { require_signature } => verify(catalog, require_signature)?,
        FeedsCmd::Check { require_signature } => check(catalog, require_signature)?,
    }
    Ok(())
}

fn read_text(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))
}

fn feeds_dir(catalog: &Path) -> PathBuf {
    catalog.join("feeds")
}

/// Every published file below `feeds/` except the index and its signature.
fn listed(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if e.file_type()?.is_dir() {
            listed(&e.path(), &rel, out)?;
        } else if rel != INDEX_FILE && rel != SIGNATURE_FILE {
            out.push(rel);
        }
    }
    Ok(())
}

fn read_index(catalog: &Path) -> Result<Option<FeedIndex>> {
    match std::fs::read(feeds_dir(catalog).join(INDEX_FILE)) {
        Ok(b) => Ok(Some(
            superba_catalog::feeds::format::parse_index(&b).map_err(|e| anyhow::anyhow!("{e}"))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The index of the files on disk; the serial increases when they differ from
/// `previous` (the date is informational and only changes with the serial).
pub fn compute_index(
    catalog: &Path,
    previous: Option<&FeedIndex>,
    today: &str,
) -> Result<FeedIndex> {
    let dir = feeds_dir(catalog);
    let mut names = Vec::new();
    listed(&dir, "", &mut names)?;
    let mut files = Vec::new();
    for rel in names {
        let bytes = std::fs::read(dir.join(&rel))?;
        files.push(FeedFile {
            sha256: sha256_hex(&bytes),
            size: bytes.len() as u64,
            path: rel,
        });
    }
    let mut index = FeedIndex {
        format: FEEDS_FORMAT,
        serial: previous.map_or(1, |p| p.serial),
        generated: previous
            .and_then(|p| p.generated.clone())
            .or_else(|| Some(today.to_owned())),
        files,
    };
    if let Some(p) = previous
        && p.files != index.files
    {
        index.serial = p.serial + 1;
        index.generated = Some(today.to_owned());
    }
    Ok(index)
}

fn today() -> String {
    superba_core::Timestamp::now().utc_date_string()
}

/// Rewrite `feeds/index.json`; a now-stale signature is removed.
pub fn write_index(catalog: &Path) -> Result<bool> {
    let previous = read_index(catalog)?;
    let index = compute_index(catalog, previous.as_ref(), &today())?;
    let dir = feeds_dir(catalog);
    let changed = write_if_changed(&dir.join(INDEX_FILE), to_pretty_json(&index)?.as_bytes())?;
    let sig = dir.join(SIGNATURE_FILE);
    if changed && sig.exists() {
        std::fs::remove_file(&sig)?;
        eprintln!("note: feeds/index.json changed; removed the stale signature (run `feeds sign`)");
    }
    Ok(changed)
}

/// Sign `feeds/index.json` with every key (feeds signature context).
pub fn sign(catalog: &Path, keys: &[ed25519_dalek::SigningKey]) -> Result<Vec<String>> {
    if keys.is_empty() {
        bail!("no signing key");
    }
    let dir = feeds_dir(catalog);
    let index = std::fs::read(dir.join(INDEX_FILE)).context("read feeds/index.json")?;
    let message = signed_message_with_context(FEEDS_SIGNATURE_CONTEXT, &index);
    let mut ids = Vec::new();
    let signatures = keys
        .iter()
        .map(|k| {
            let id = key_id(&k.verifying_key().to_bytes());
            ids.push(id.clone());
            SignatureEntry {
                key_id: id,
                algorithm: "ed25519".into(),
                signature: base64::engine::general_purpose::STANDARD
                    .encode(k.sign(&message).to_bytes()),
            }
        })
        .collect();
    let file = SignatureFile {
        format: SIGNATURE_FORMAT,
        signatures,
    };
    std::fs::write(
        dir.join(SIGNATURE_FILE),
        superba_catalog::format::to_pretty_json(&file)?,
    )?;
    Ok(ids)
}

/// Every file of the feeds directory (for verification).
fn all_files(catalog: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let dir = feeds_dir(catalog);
    let mut names = Vec::new();
    listed(&dir, "", &mut names)?;
    let mut files = BTreeMap::new();
    for rel in names {
        files.insert(rel.clone(), std::fs::read(dir.join(&rel))?);
    }
    files.insert(INDEX_FILE.into(), std::fs::read(dir.join(INDEX_FILE))?);
    if let Ok(sig) = std::fs::read(dir.join(SIGNATURE_FILE)) {
        files.insert(SIGNATURE_FILE.into(), sig);
    }
    Ok(files)
}

pub fn verify(catalog: &Path, require_signature: bool) -> Result<()> {
    let policy = if require_signature {
        TrustPolicy::RequireSignature
    } else {
        TrustPolicy::ManifestOnly
    };
    let data = FeedData::from_files(all_files(catalog)?, policy, TRUSTED_KEYS, REVOKED_KEY_IDS)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let snap = data.snapshot(superba_core::FeedOrigin::Builtin, None);
    println!(
        "feeds ok: serial {}, {} files, {} models, {} prices, {} Codex releases, {} app builds",
        data.serial(),
        data.index().files.len(),
        snap.models.len(),
        snap.prices.entries.len(),
        snap.codex_versions.len(),
        snap.app_versions.len()
    );
    match data.signature() {
        SignatureStatus::Valid { key_id } => println!("signature ok: key {key_id}"),
        SignatureStatus::Unsigned => println!("signature: none (run `feeds sign`)"),
        SignatureStatus::Invalid { reason } => println!("signature INVALID: {reason}"),
    }
    Ok(())
}

/// Service tiers `tier_prices` may name (`priority` is Fast; `fast` is accepted
/// in responses but stored as `priority`).
const PRICE_TIERS: [&str; 3] = ["priority", "flex", "ultrafast"];

/// Price entries must name a public https source page and the date they were
/// read (`rev`, `YYYY-MM-DD`); tier prices use known tier names; long-context
/// bands ascend; cached input is not above input.
fn check_prices(catalog: &Path) -> Result<Vec<String>> {
    let path = feeds_dir(catalog).join(PRICES_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(Vec::new());
    };
    let prices: PricesFeed = parse_feed(PRICES_FILE, &bytes, |p: &PricesFeed| p.format)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut problems = Vec::new();
    let https = |s: Option<&str>| s.is_some_and(|s| s.starts_with("https://") && !s.contains(' '));
    for (model, e) in &prices.entries {
        if !https(e.source.as_deref()) {
            problems.push(format!("prices.json: {model} has no https source page"));
        }
        match e.rev.as_deref() {
            None | Some("") => problems.push(format!("prices.json: {model} has no rev")),
            Some(r) if !is_date(r) => {
                problems.push(format!("prices.json: {model} rev {r:?} is not YYYY-MM-DD"));
            }
            Some(_) => {}
        }
        let mut sets = vec![(
            String::new(),
            e.input,
            e.cached_input,
            e.long_context.as_slice(),
        )];
        for (tier, t) in &e.tier_prices {
            if !PRICE_TIERS.contains(&tier.as_str()) {
                problems.push(format!(
                    "prices.json: {model} tier_prices.{tier} is not one of {}",
                    PRICE_TIERS.join(", ")
                ));
            }
            if t.source.is_some() && !https(t.source.as_deref()) {
                problems.push(format!(
                    "prices.json: {model} tier_prices.{tier} source is not an https page"
                ));
            }
            sets.push((
                format!(" tier_prices.{tier}"),
                t.input,
                t.cached_input,
                t.long_context.as_slice(),
            ));
        }
        for (what, input, cached, bands) in sets {
            if cached.is_some_and(|c| c > input) {
                problems.push(format!(
                    "prices.json: {model}{what} cached_input is above input"
                ));
            }
            let mut last = 0;
            for t in bands {
                if t.above_input_tokens <= last && last != 0 {
                    problems.push(format!(
                        "prices.json: {model}{what} long_context is not ascending"
                    ));
                }
                last = t.above_input_tokens;
            }
        }
    }
    Ok(problems)
}

/// `YYYY-MM-DD`.
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

pub fn check(catalog: &Path, require_signature: bool) -> Result<()> {
    let mut problems = check_prices(catalog)?;
    let previous = read_index(catalog)?;
    let fresh = compute_index(catalog, previous.as_ref(), &today())?;
    if previous.as_ref() != Some(&fresh) {
        problems
            .push("feeds/index.json is out of date: run `superba-catalog-gen feeds index`".into());
    }
    if problems.is_empty() {
        verify(catalog, require_signature)?;
        println!("feeds are up to date");
        Ok(())
    } else {
        for p in &problems {
            eprintln!("- {p}");
        }
        bail!("{} problem(s)", problems.len())
    }
}
