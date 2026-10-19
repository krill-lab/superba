//! opencode (research: `harness-research/opencode.md`). Two products share
//! the name: OpenCode 2 (`@opencode/cli`, the default for new installs,
//! decisions.md) and OpenCode 1 (`opencode-ai`, channel `v1`); an install
//! keeps its major unless the user picks the other channel.
//!
//! - **Install / update**: the standalone archive for this platform
//!   (`-baseline` without AVX2, `-musl` on musl) into `~/.opencode/bin` —
//!   the official installer's place, so opencode's own method detection
//!   still says "curl". OpenCode 2: `opencode.ai/files/bin/<v>/…` with the
//!   sha256 and size of the update API (`/update/api/<channel>/cli`); a
//!   version that is not the channel's newest comes from its npm platform
//!   package, checked against the registry's sha512. OpenCode 1: the GitHub
//!   release asset with its sha256 digest (the npm platform package when the
//!   GitHub API is unavailable). The running program is renamed aside on
//!   Windows. After an OpenCode 2 update a running background service is
//!   stopped (the next opencode command starts the new one).
//! - **Removal**: only the program (and Superba's `PATH` entry). opencode's
//!   own `uninstall` is never run: OpenCode 1's deletes config and data. Its
//!   folders go only when the user asks (`remove_data`).
//! - **"Superba manages updates"**: `update: "disable"` (2) or
//!   `autoupdate: false` (1) in the global config, edited in place (JSONC).
//! - npm and Homebrew installs are updated / removed with their own tool.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use superba_core::{
    Error, Notice, Result, Timestamp, ToolId, ToolInstall, ToolInstallKind, ToolOp, ToolReport,
    ToolStage, ToolUpdater,
};

use super::common::{self, install, text};
use crate::descriptor::opencode_target;
use crate::http::{self, Checksum};
use crate::platform::Os;
use crate::state::{InstalledRecord, ManagedUpdates, State, ToolState};
use crate::{Ctx, Detected, Op, archive, fsx, jsonedit, version};

/// OpenCode 2's npm package.
pub const V2_PACKAGE: &str = "@opencode/cli";
/// OpenCode 1's npm package.
pub const V1_PACKAGE: &str = "opencode-ai";
/// OpenCode 1's GitHub repository.
pub const V1_REPO: &str = "anomalyco/opencode";

/// Even `--version` initializes opencode's XDG directories. Keep those
/// writes in a fresh Superba scratch directory, removed on every exit path.
async fn probe_version(cx: &Ctx, program: &Path) -> Option<String> {
    let tmp = cx.data.root().join("tmp");
    std::fs::create_dir_all(&tmp).ok()?;
    let scratch = tempfile::Builder::new()
        .prefix("opencode-probe-")
        .tempdir_in(&tmp)
        .ok()?;
    let cmd = cx
        .cmd(program)
        .args(["--version"])
        .env("XDG_DATA_HOME", scratch.path().join("data"))
        .env("XDG_STATE_HOME", scratch.path().join("state"))
        .env("XDG_CACHE_HOME", scratch.path().join("cache"));
    crate::probe_version_with(cmd).await
}

/// `~/.opencode/bin`.
pub fn bin_dir(cx: &Ctx) -> PathBuf {
    cx.home.join(".opencode").join("bin")
}

fn exe(cx: &Ctx) -> PathBuf {
    bin_dir(cx).join(cx.platform.exe("opencode"))
}

/// The global config folder (`$OPENCODE_CONFIG_DIR`, else
/// `$XDG_CONFIG_HOME/opencode`).
pub fn config_dir(cx: &Ctx) -> PathBuf {
    cx.env
        .var_non_empty("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| cx.xdg("XDG_CONFIG_HOME", ".config").join("opencode"))
}

/// The global config file opencode reads (`opencode.jsonc`, else
/// `opencode.json`; a new one is `opencode.json`).
pub fn config_file(cx: &Ctx) -> PathBuf {
    let dir = config_dir(cx);
    let jsonc = dir.join("opencode.jsonc");
    if jsonc.is_file() {
        return jsonc;
    }
    dir.join("opencode.json")
}

fn data_dirs(cx: &Ctx) -> Vec<PathBuf> {
    [
        config_dir(cx),
        cx.xdg("XDG_DATA_HOME", ".local/share").join("opencode"),
        cx.xdg("XDG_CACHE_HOME", ".cache").join("opencode"),
        cx.xdg("XDG_STATE_HOME", ".local/state").join("opencode"),
    ]
    .into_iter()
    .filter(|d| d.is_dir())
    .collect()
}

/// The major of a version (`2.0.32` → 2).
pub fn major(v: &str) -> Option<u64> {
    version::Version::parse(v).map(|v| v.major())
}

async fn classify(cx: &Ctx, path: &Path) -> ToolInstall {
    let os = cx.platform.os;
    for pkg in [V2_PACKAGE, V1_PACKAGE] {
        if let Some((dir, _)) = common::npm_package_of(path, pkg, os) {
            return install(
                path.to_path_buf(),
                ToolInstallKind::NpmGlobal,
                common::package_version(&dir),
                false,
                true,
            );
        }
    }
    if let Some((_, _, v)) = common::homebrew_of(path) {
        return install(
            path.to_path_buf(),
            ToolInstallKind::Homebrew,
            Some(v),
            false,
            true,
        );
    }
    let v = probe_version(cx, path).await;
    install(path.to_path_buf(), ToolInstallKind::Other, v, false, true)
}

/// The updater setting of the global config: `(state, key, value)`.
fn config_updater(cx: &Ctx, major: u64) -> Option<(String, String)> {
    let text = std::fs::read_to_string(config_file(cx)).ok()?;
    let obj = jsonedit::jsonc_object(&text, "opencode config").ok()?;
    if major >= 2
        && let Some(v) = obj.get("update").and_then(Value::as_str)
    {
        return Some(("update".into(), v.to_owned()));
    }
    obj.get("autoupdate")
        .map(|v| ("autoupdate".into(), v.to_string()))
}

/// What is installed.
pub async fn detect(cx: &Ctx, st: &ToolState) -> Detected {
    let os = cx.platform.os;
    let mut det = Detected::default();
    let standalone = exe(cx);
    let mut ours = None;
    if standalone.is_file() {
        let v = probe_version(cx, &standalone).await;
        let by = st
            .installed
            .as_ref()
            .is_some_and(|r| r.kind == ToolInstallKind::Standalone);
        ours = Some(install(
            standalone.clone(),
            ToolInstallKind::Standalone,
            v,
            by,
            false,
        ));
    }
    let mut installs = Vec::new();
    if let Some(f) = cx.which("opencode") {
        match &mut ours {
            Some(o) if common::same_file(&f, &o.path, os) => o.on_path = true,
            _ => installs.push(classify(cx, &f).await),
        }
    }
    if let Some(o) = ours {
        if o.on_path {
            installs.insert(0, o);
        } else {
            installs.push(o);
        }
    }
    det.installs = installs;
    let primary = det.installs.first().cloned();
    let maj = primary
        .as_ref()
        .and_then(|i| i.version.as_deref())
        .and_then(major)
        .unwrap_or(2);
    if maj == 1 {
        det.channel_hint = Some("v1".into());
    }
    det.updater = Some(
        match (primary.as_ref().map(|i| i.kind), config_updater(cx, maj)) {
            _ if cx.flag("OPENCODE_DISABLE_AUTOUPDATE") => ToolUpdater::Off {
                by: "OPENCODE_DISABLE_AUTOUPDATE".into(),
            },
            (_, Some((key, v))) if v == "\"disable\"" || v == "disable" || v == "false" => {
                if st.managed_updates.is_some() {
                    ToolUpdater::Superba
                } else {
                    ToolUpdater::Off { by: key }
                }
            }
            (_, Some((_, v))) if v == "auto" => ToolUpdater::Automatic,
            (Some(ToolInstallKind::Homebrew), _) => ToolUpdater::PackageManager,
            _ => ToolUpdater::Notifies,
        },
    );
    let kind = primary.as_ref().map(|i| i.kind);
    det.can_update = match kind {
        Some(ToolInstallKind::Standalone) => true,
        Some(ToolInstallKind::NpmGlobal) => cx.which("npm").is_some(),
        Some(ToolInstallKind::Homebrew) => cx.which("brew").is_some(),
        _ => false,
    };
    det.can_uninstall = det.can_update;
    det.data_dirs = data_dirs(cx);
    if kind == Some(ToolInstallKind::Standalone) {
        let running = crate::procs::under(&bin_dir(cx));
        if !running.is_empty() {
            let c = text(
                "tools.opencode.caution_running",
                "opencode is running ({n} process(es), its background service included): they are ended first.",
            )
            .arg("n", running.len());
            det.uninstall_cautions.push(c);
        }
    }
    if kind.is_none() || kind == Some(ToolInstallKind::Standalone) {
        if os == Os::Windows {
            if kind.is_some() && !common::dir_on_path(cx, &bin_dir(cx)) {
                det.notes.push(Notice::info(
                    text(
                        "tools.opencode.path_windows",
                        "{dir} is not on your PATH; updating through Superba adds it.",
                    )
                    .arg("dir", superba_core::paths::display_path(&bin_dir(cx))),
                ));
            }
        } else if kind.is_some() {
            det.path_hint = cx.path_hint(&bin_dir(cx));
        }
    }
    if det.installs.len() > 1 {
        det.notes.push(Notice::warning(
            text(
                "tools.opencode.multiple",
                "Several opencode installs were found; `opencode` runs {path}.",
            )
            .arg(
                "path",
                superba_core::paths::display_path(&det.installs[0].path),
            ),
        ));
    }
    det
}

/// The newest version on `channel` (`latest`, `beta`: OpenCode 2; `v1`).
pub async fn latest(cx: &Ctx, channel: &str) -> Result<String> {
    if channel == "v1" {
        let url = format!("{}/{V1_PACKAGE}/latest", cx.sources.npm);
        let doc = http::get_json(cx.client()?, &url).await?;
        return doc
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::parse(url, "no version"));
    }
    let url = format!("{}/update/api/{channel}/cli/npm", cx.sources.opencode);
    let doc = http::get_json(cx.client()?, &url).await?;
    doc.get("version")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::parse(url, "no version"))
}

/// The file of `target.ext` in the update API's `opencode` artifact:
/// `(version, url, sha256, size)`.
pub fn api_file(doc: &Value, file: &str) -> Option<(String, String, String, u64)> {
    let a = doc
        .get("artifacts")?
        .as_array()?
        .iter()
        .find(|a| a.get("distribution").and_then(Value::as_str) == Some("opencode"))?;
    let v = a.get("version")?.as_str()?.to_owned();
    let f = a.pointer("/metadata/files")?.get(file)?;
    Some((
        v,
        f.get("url")?.as_str()?.to_owned(),
        f.get("sha256")?.as_str()?.to_owned(),
        f.get("size").and_then(Value::as_u64).unwrap_or(0),
    ))
}

/// Where to get one archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// URL.
    pub url: String,
    /// Checksum.
    pub checksum: Checksum,
    /// Size (0: unknown).
    pub size: u64,
    /// Archive kind (`tar.gz`, `zip`, `tgz` = npm package).
    pub kind: &'static str,
    /// Folder of the program inside the archive.
    pub prefix: &'static str,
}

/// An npm package version's tarball and sha512.
pub async fn npm_tarball(cx: &Ctx, package: &str, v: &str) -> Result<(String, Checksum)> {
    let url = format!("{}/{}/{v}", cx.sources.npm, package.replacen('/', "%2f", 1));
    let doc = http::get_json(cx.client()?, &url).await?;
    let tarball = doc
        .pointer("/dist/tarball")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::parse(&url, "no dist.tarball"))?;
    let sum = doc
        .pointer("/dist/integrity")
        .and_then(Value::as_str)
        .and_then(Checksum::from_integrity)
        .ok_or_else(|| Error::parse(&url, "no sha512 integrity"))?;
    Ok((tarball.to_owned(), sum))
}

async fn resolve(op: &Op<'_>, v: &str, maj: u64) -> Result<Artifact> {
    let cx = op.cx;
    let (target, ext) = opencode_target(&cx.platform);
    let file = format!("opencode-{target}.{ext}");
    let prefix = "";
    if maj >= 2 {
        let channel = if op.channel == "v1" {
            "latest"
        } else {
            op.channel.as_str()
        };
        let url = format!("{}/update/api/{channel}/cli", cx.sources.opencode);
        if let Ok(doc) = http::get_json(cx.client()?, &url).await
            && let Some((av, aurl, sha, size)) = api_file(&doc, &file)
            && version::same(&av, v)
        {
            op.log.line(format!("update API: {file} {av} sha256 {sha}"));
            return Ok(Artifact {
                url: aurl,
                checksum: Checksum::Sha256(sha),
                size,
                kind: if ext == "zip" { "zip" } else { "tar.gz" },
                prefix,
            });
        }
        let (url, sum) = npm_tarball(cx, &format!("{V2_PACKAGE}-{target}"), v).await?;
        op.log.line(format!("npm platform package: {url}"));
        return Ok(Artifact {
            url,
            checksum: sum,
            size: 0,
            kind: "tgz",
            prefix: "package/bin",
        });
    }
    let api = format!(
        "{}/repos/{V1_REPO}/releases/tags/v{v}",
        cx.sources.github_api
    );
    match http::get_json(cx.client()?, &api).await {
        Ok(doc) => {
            let asset = doc
                .get("assets")
                .and_then(Value::as_array)
                .and_then(|a| {
                    a.iter()
                        .find(|x| x.get("name").and_then(Value::as_str) == Some(file.as_str()))
                })
                .ok_or_else(|| Error::NotFound {
                    what: format!("{file} in OpenCode {v}"),
                })?;
            let digest = asset
                .get("digest")
                .and_then(Value::as_str)
                .and_then(|d| d.strip_prefix("sha256:"))
                .ok_or_else(|| Error::parse(&api, "no sha256 digest"))?;
            Ok(Artifact {
                url: format!(
                    "{}/{V1_REPO}/releases/download/v{v}/{file}",
                    cx.sources.github
                ),
                checksum: Checksum::Sha256(digest.to_owned()),
                size: asset.get("size").and_then(Value::as_u64).unwrap_or(0),
                kind: if ext == "zip" { "zip" } else { "tar.gz" },
                prefix,
            })
        }
        Err(e) => {
            op.log.line(format!(
                "GitHub API unavailable ({e}); using the npm platform package"
            ));
            let (url, sum) = npm_tarball(cx, &format!("opencode-{target}"), v).await?;
            Ok(Artifact {
                url,
                checksum: sum,
                size: 0,
                kind: "tgz",
                prefix: "package/bin",
            })
        }
    }
}

/// Install, update or remove.
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    let kind = op.det.installs.first().map(|i| i.kind);
    match (op.req.op, kind) {
        (ToolOp::Install, _) | (ToolOp::Update, Some(ToolInstallKind::Standalone)) => {
            install_archive(op).await
        }
        (ToolOp::Update | ToolOp::Uninstall, Some(ToolInstallKind::NpmGlobal)) => {
            external_npm(op).await
        }
        (ToolOp::Update | ToolOp::Uninstall, Some(ToolInstallKind::Homebrew)) => brew(op).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::Standalone)) => remove_standalone(op).await,
        _ => Err(Error::Unsupported {
            what: "this opencode install through Superba".into(),
        }),
    }
}

async fn target_version(op: &Op<'_>) -> Result<String> {
    match &op.req.version {
        Some(v) => {
            let v = v.trim().trim_start_matches('v');
            version::Version::parse(v)
                .map(|_| v.to_owned())
                .ok_or_else(|| Error::invalid(format!("`{v}` is not an opencode version")))
        }
        None => {
            op.stage(ToolStage::Resolving, Some(op.channel.clone()));
            latest(op.cx, &op.channel).await
        }
    }
}

async fn install_archive(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let v = target_version(op).await?;
    let maj = major(&v).unwrap_or(2);
    op.log.line(format!("version: {v} (OpenCode {maj})"));
    op.stage(ToolStage::Resolving, Some(v.clone()));
    let art = resolve(op, &v, maj).await?;
    let ext = match art.kind {
        "tgz" => "tgz",
        "zip" => "zip",
        _ => "tar.gz",
    };
    let dl = common::downloads(cx, ToolId::Opencode).join(format!("opencode-{v}.{ext}"));
    op.download(
        &art.url,
        &dl,
        Some(&art.checksum),
        (art.size > 0).then_some(art.size),
    )
    .await?;
    op.check_cancel()?;
    op.stage(
        ToolStage::Extracting,
        dl.file_name().map(|n| n.to_string_lossy().into_owned()),
    );
    let staging = fsx::staging_dir(&cx.tool_dir(ToolId::Opencode), "extract")?;
    let (a, s, k, pre) = (dl.clone(), staging.clone(), art.kind, art.prefix);
    tokio::task::spawn_blocking(move || match k {
        "zip" => archive::extract_zip(&a, pre, &s),
        _ => archive::extract_tar_gz(&a, pre, &s),
    })
    .await
    .map_err(|e| Error::other(e.to_string()))??;
    let name = cx.platform.exe("opencode");
    let new = staging.join(&name);
    if !new.is_file() {
        let _ = fsx::remove_dir(&staging);
        return Err(Error::Invalid {
            message: format!("the archive has no {name}"),
        });
    }
    archive::make_executable(&new)?;
    let target = exe(cx);
    let next = fsx::sibling(&target, ".superba-new");
    common::move_file(&new, &next)?;
    let _ = fsx::remove_dir(&staging);
    let _ = std::fs::remove_file(&dl);
    fsx::replace_program(&next, &target)?;
    op.log.line(format!("installed {}", target.display()));
    op.stage(ToolStage::Checking, None);
    crate::forget_versions();
    let got = probe_version(cx, &target).await;
    op.log.line(format!(
        "--version: {}",
        got.as_deref().unwrap_or("no answer")
    ));
    if !got.as_deref().is_some_and(|g| version::same(g, &v)) {
        return Err(Error::Process {
            message: format!(
                "{} reports {} after installing {v}",
                target.display(),
                got.as_deref().unwrap_or("nothing")
            ),
        });
    }
    let mut notes = Vec::new();
    if maj >= 2 && op.req.op == ToolOp::Update {
        stop_service(op, &target).await;
    }
    common::add_user_path(op, &bin_dir(cx), &mut notes)?;
    if let Some(h) = cx.path_hint(&bin_dir(cx)) {
        notes.push(common::path_note(&h));
    }
    op.record(|s| {
        s.installed = Some(InstalledRecord {
            kind: ToolInstallKind::Standalone,
            version: v.clone(),
            path: target.clone(),
            at: Timestamp::now(),
        });
    })?;
    Ok(op.report(Some(v), Some(target), notes))
}

/// What `opencode service status` printed means a service runs: its URL
/// (`http://127.0.0.1:49374`, seen with 2.0.32) or "running"; "stopped" when
/// none does.
pub fn service_running(success: bool, text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    success
        && !t.contains("stopped")
        && !t.contains("not running")
        && (t.contains("http://") || t.contains("https://") || t.contains("running"))
}

/// OpenCode 2 runs a background service that keeps the old program: stop it
/// (best effort); the next opencode command starts it again with the new
/// version. Superba does not start it itself, so no long-lived process
/// inherits its handles.
async fn stop_service(op: &Op<'_>, program: &Path) {
    let cx = op.cx;
    let status = cx
        .cmd(program)
        .args(["service", "status"])
        .timeout(Duration::from_secs(15));
    let Ok(out) = crate::run::output(&status).await else {
        return;
    };
    let text = format!("{}\n{}", out.stdout, out.stderr);
    op.log.line(format!("service status: {}", text.trim()));
    if service_running(out.success(), &text) {
        let stop = cx
            .cmd(program)
            .args(["service", "stop"])
            .timeout(Duration::from_secs(30));
        let _ = op.run(&stop, ToolStage::Checking).await;
    }
}

async fn remove_standalone(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    op.stage(ToolStage::Removing, None);
    let running = crate::procs::under(&bin_dir(cx));
    if !running.is_empty() {
        op.log
            .line(format!("ending {} opencode process(es)", running.len()));
        let left = tokio::task::spawn_blocking(move || {
            crate::procs::stop(&running, Duration::from_secs(5))
        })
        .await
        .unwrap_or_default();
        if !left.is_empty() {
            return Err(Error::Conflict {
                message: "opencode is still running; close it and try again".into(),
            });
        }
    }
    let dir = bin_dir(cx);
    for name in ["opencode", "opencode2"] {
        for p in [
            dir.join(cx.platform.exe(name)),
            fsx::sibling(&dir.join(cx.platform.exe(name)), ".old"),
        ] {
            if fsx::remove_file(&p)? {
                op.log.line(format!("removed {}", p.display()));
            }
        }
    }
    fsx::remove_empty_dirs(&dir, &cx.home);
    common::unexpose(op)?;
    let mut notes = Vec::new();
    if op.req.remove_data {
        for d in data_dirs(cx) {
            fsx::remove_dir(&d)?;
            op.log.line(format!("removed (asked) {}", d.display()));
        }
        notes.push(Notice::info(text(
            "tools.opencode.data_removed",
            "opencode's settings and data were removed too.",
        )));
    } else {
        notes.push(Notice::info(text(
            "tools.opencode.kept",
            "opencode's settings and sessions (~/.config/opencode, ~/.local/share/opencode) were kept.",
        )));
    }
    let _ = fsx::remove_dir(&common::downloads(cx, ToolId::Opencode));
    fsx::remove_empty_dirs(
        &cx.tool_dir(ToolId::Opencode),
        &crate::state::tools_dir(&cx.data),
    );
    op.record(|s| s.installed = None)?;
    Ok(op.report(None, None, notes))
}

async fn external_npm(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let os = cx.platform.os;
    let remove = op.req.op == ToolOp::Uninstall;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let (pkg, dir, prefix) = [V2_PACKAGE, V1_PACKAGE]
        .iter()
        .find_map(|p| common::npm_package_of(&i.path, p, os).map(|(d, pre)| (*p, d, pre)))
        .ok_or_else(|| Error::invalid("cannot tell this npm install's package"))?;
    let npm = cx
        .which("npm")
        .ok_or_else(|| Error::invalid("npm is not on PATH"))?;
    let mut args = vec![
        if remove { "uninstall" } else { "install" }.to_owned(),
        "-g".into(),
        "--prefix".into(),
        prefix.display().to_string(),
    ];
    let v = if remove {
        None
    } else {
        // An npm install keeps its package (its major).
        let v = match &op.req.version {
            Some(v) => v.trim().trim_start_matches('v').to_owned(),
            None if pkg == V1_PACKAGE => latest(cx, "v1").await?,
            None => {
                latest(
                    cx,
                    if op.channel == "v1" {
                        "latest"
                    } else {
                        &op.channel
                    },
                )
                .await?
            }
        };
        args.push(format!("{pkg}@{v}"));
        args.push(format!("--allow-scripts={pkg}"));
        Some(v)
    };
    if remove {
        args.push(pkg.into());
    }
    let modules = dir.parent().unwrap_or(&prefix).to_path_buf();
    if !common::writable(&modules) {
        return Err(Error::Unsupported {
            what: format!(
                "changing the npm install in {} (it needs administrator rights; run `npm {}` yourself)",
                prefix.display(),
                args.join(" ")
            ),
        });
    }
    let cmd = cx
        .cmd(&npm)
        .args(args)
        .timeout(Duration::from_secs(20 * 60));
    op.run_ok(
        &cmd,
        if remove {
            ToolStage::Removing
        } else {
            ToolStage::Installing
        },
    )
    .await?;
    let now = if remove {
        None
    } else {
        v.and(common::package_version(&dir))
    };
    Ok(op.report(now, (!remove).then(|| i.path.clone()), Vec::new()))
}

async fn brew(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let remove = op.req.op == ToolOp::Uninstall;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let (formula, cask, _) =
        common::homebrew_of(&i.path).ok_or_else(|| Error::invalid("cannot tell the formula"))?;
    let brew = cx
        .which("brew")
        .ok_or_else(|| Error::invalid("brew is not on PATH"))?;
    let mut args = vec![if remove { "uninstall" } else { "upgrade" }.to_owned()];
    if cask {
        args.push("--cask".into());
    }
    args.push(formula);
    let cmd = cx
        .cmd(&brew)
        .args(args)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .timeout(Duration::from_secs(20 * 60));
    op.run_ok(
        &cmd,
        if remove {
            ToolStage::Removing
        } else {
            ToolStage::Installing
        },
    )
    .await?;
    crate::forget_versions();
    let now = if remove {
        None
    } else {
        probe_version(cx, &i.path).await
    };
    Ok(op.report(now, (!remove).then(|| i.path.clone()), Vec::new()))
}

/// "Superba manages updates": `update: "disable"` (OpenCode 2) /
/// `autoupdate: false` (OpenCode 1) in the global config; off puts back what
/// was there.
pub async fn manage_updates(cx: &Ctx, on: bool) -> Result<()> {
    let st = State::load(&cx.data).tool(ToolId::Opencode);
    let det = detect(cx, &st).await;
    let maj = det
        .installs
        .first()
        .and_then(|i| i.version.as_deref())
        .and_then(major)
        .unwrap_or(2);
    let file = config_file(cx);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let what = file.display().to_string();
    if on {
        let (key, value) = if maj >= 2 {
            ("update", "\"disable\"")
        } else {
            ("autoupdate", "false")
        };
        let previous = jsonedit::jsonc_get(&text, key);
        let existed = file.exists();
        let new = jsonedit::jsonc_set(&text, key, value, &what)?;
        super::claude::backup(cx, &file, "opencode-config")?;
        fsx::write_atomic(&file, new.as_bytes())?;
        let record = match st.managed_updates {
            Some(m) => m,
            None => ManagedUpdates {
                file: file.clone(),
                key: key.into(),
                previous,
                created: !existed,
            },
        };
        State::update(&cx.data, ToolId::Opencode, |s| {
            s.managed_updates = Some(record)
        })
    } else {
        let (key, previous, created) = match st.managed_updates {
            Some(m) => (m.key, m.previous, m.created),
            None => (
                if maj >= 2 { "update" } else { "autoupdate" }.to_owned(),
                None,
                false,
            ),
        };
        let new = match previous.as_deref() {
            Some(p) if p != "\"disable\"" && p != "false" => {
                jsonedit::jsonc_set(&text, &key, p, &what)?
            }
            _ => jsonedit::jsonc_remove(&text, &key, &what)?,
        };
        if new != text {
            super::claude::backup(cx, &file, "opencode-config")?;
            let empty = jsonedit::jsonc_object(&new, &what).is_ok_and(|o| o.is_empty())
                && !new.contains("//")
                && !new.contains("/*");
            if created && empty {
                fsx::remove_file(&file)?;
            } else {
                fsx::write_atomic(&file, new.as_bytes())?;
            }
        }
        State::update(&cx.data, ToolId::Opencode, |s| s.managed_updates = None)
    }
}
