//! pi, `@earendil-works/pi-coding-agent` (research: `harness-research/pi.md`).
//!
//! - **Install / update**: the standalone release archive for this platform
//!   from GitHub (`pi-<os>-<arch>.tar.gz` / `.zip`), checked against the
//!   release's `SHA256SUMS`, unpacked into `<data>/tools/pi/app` (the
//!   previous one is kept until the swap worked). The newest version comes
//!   from `pi.dev/api/latest-version` (what pi itself asks). No Node needed.
//! - The command: a launcher in `~/.local/bin` (Unix) or the folder on the
//!   user `PATH` (Windows).
//! - pi's own updater only shows a notice (off with `PI_SKIP_VERSION_CHECK`).
//! - Installs made by pi's installer (`~/.pi/agent/install`) or npm are
//!   updated with `pi update`, Homebrew with `brew upgrade`; removed with the
//!   steps of pi's installer / their own tool. `~/.pi/agent` (settings,
//!   sessions) is never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use superba_core::{
    Error, Notice, Result, Timestamp, ToolId, ToolInstall, ToolInstallKind, ToolOp, ToolReport,
    ToolStage, ToolUpdater,
};

use super::common::{self, install, slashed, text};
use crate::descriptor::pi_asset;
use crate::http::{self, Checksum};
use crate::platform::Os;
use crate::state::{InstalledRecord, ToolState};
use crate::{Ctx, Detected, Op, archive, fsx, version};

/// The npm package.
pub const PACKAGE: &str = "@earendil-works/pi-coding-agent";
/// The GitHub repository.
pub const REPO: &str = "earendil-works/pi";

fn app_dir(cx: &Ctx) -> PathBuf {
    cx.tool_dir(ToolId::Pi).join("app")
}

fn app_exe(cx: &Ctx) -> PathBuf {
    app_dir(cx).join(cx.platform.exe("pi"))
}

/// pi's own installer's folder (`~/.pi/agent/install`).
fn managed_dir(cx: &Ctx) -> PathBuf {
    cx.home.join(".pi").join("agent").join("install")
}

async fn classify(cx: &Ctx, path: &Path) -> ToolInstall {
    let os = cx.platform.os;
    let canon = slashed(&common::canonical(path), os);
    let managed = slashed(&managed_dir(cx), os);
    let launcher_dir = slashed(&cx.home.join(".pi").join("agent").join("bin"), os);
    if canon.starts_with(&managed) || slashed(path, os).starts_with(&launcher_dir) {
        let v = std::fs::read_to_string(managed_dir(cx).join("current-version"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let v = match v {
            Some(v) => Some(v),
            None => crate::probe_version(cx, path, &["--version"]).await,
        };
        return install(path.to_path_buf(), ToolInstallKind::Managed, v, false, true);
    }
    if let Some((dir, _)) = common::npm_package_of(path, PACKAGE, os) {
        return install(
            path.to_path_buf(),
            ToolInstallKind::NpmGlobal,
            common::package_version(&dir),
            false,
            true,
        );
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
    let v = crate::probe_version(cx, path, &["--version"]).await;
    install(path.to_path_buf(), ToolInstallKind::Other, v, false, true)
}

/// What is installed.
pub async fn detect(cx: &Ctx, st: &ToolState) -> Detected {
    let os = cx.platform.os;
    let mut det = Detected::default();
    let exe = app_exe(cx);
    let mut ours = None;
    if exe.is_file() {
        let v = match &st.installed {
            Some(r) if r.kind == ToolInstallKind::Standalone => Some(r.version.clone()),
            _ => crate::probe_version(cx, &exe, &["--version"]).await,
        };
        ours = Some(install(
            exe.clone(),
            ToolInstallKind::Standalone,
            v,
            true,
            false,
        ));
    }
    let mut installs = Vec::new();
    if let Some(f) = cx.which("pi") {
        let launcher = cx.local_bin().join("pi");
        let mine = match &ours {
            Some(o) => {
                common::same_file(&f, &o.path, os)
                    || (common::same_file(&f, &launcher, os) && fsx::is_own_launcher(&launcher))
            }
            None => false,
        };
        if mine {
            if let Some(o) = &mut ours {
                o.on_path = true;
            }
        } else {
            installs.push(classify(cx, &f).await);
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
    let kind = det.installs.first().map(|i| i.kind);
    det.updater = Some(if cx.flag("PI_SKIP_VERSION_CHECK") {
        ToolUpdater::Off {
            by: "PI_SKIP_VERSION_CHECK".into(),
        }
    } else if cx.flag("PI_OFFLINE") {
        ToolUpdater::Off {
            by: "PI_OFFLINE".into(),
        }
    } else if kind == Some(ToolInstallKind::Homebrew) {
        ToolUpdater::PackageManager
    } else {
        ToolUpdater::Notifies
    });
    let pi_ok = |k: ToolInstallKind| match k {
        ToolInstallKind::Standalone => true,
        ToolInstallKind::Managed | ToolInstallKind::NpmGlobal => true,
        ToolInstallKind::Homebrew => cx.which("brew").is_some(),
        _ => false,
    };
    det.can_update = kind.is_some_and(pi_ok);
    det.can_uninstall = match kind {
        Some(ToolInstallKind::NpmGlobal) => cx.which("npm").is_some(),
        Some(k) => pi_ok(k),
        None => false,
    };
    if det.installs.len() > 1 {
        det.notes.push(Notice::warning(
            text(
                "tools.pi.multiple",
                "Several pi installs were found; `pi` runs {path}.",
            )
            .arg(
                "path",
                superba_core::paths::display_path(&det.installs[0].path),
            ),
        ));
    }
    if (kind.is_none() || kind == Some(ToolInstallKind::Standalone)) && os != Os::Windows {
        det.path_hint = cx.path_hint(&cx.local_bin());
    }
    det
}

/// The newest version (`pi.dev/api/latest-version`).
pub async fn latest(cx: &Ctx) -> Result<String> {
    let url = format!("{}/api/latest-version", cx.sources.pi);
    let doc = http::get_json(cx.client()?, &url).await?;
    doc.get("version")
        .and_then(Value::as_str)
        .filter(|v| version::Version::parse(v).is_some())
        .map(str::to_owned)
        .ok_or_else(|| Error::parse(url, "no version"))
}

/// The sha256 of `asset` in a `SHA256SUMS` text.
pub fn sha_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let sha = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset && sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| sha.to_ascii_lowercase())
    })
}

/// Install, update or remove.
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    let kind = op.det.installs.first().map(|i| i.kind);
    match (op.req.op, kind) {
        (ToolOp::Install, _) | (ToolOp::Update, Some(ToolInstallKind::Standalone)) => {
            install_archive(op).await
        }
        (ToolOp::Update, Some(ToolInstallKind::Managed | ToolInstallKind::NpmGlobal)) => {
            pi_update(op).await
        }
        (ToolOp::Update | ToolOp::Uninstall, Some(ToolInstallKind::Homebrew)) => brew(op).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::Standalone)) => remove_ours(op).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::Managed)) => remove_managed(op).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::NpmGlobal)) => npm_uninstall(op).await,
        _ => Err(Error::Unsupported {
            what: "this pi install through Superba".into(),
        }),
    }
}

async fn target_version(op: &Op<'_>) -> Result<String> {
    match &op.req.version {
        Some(v) => {
            let v = v.trim().trim_start_matches('v');
            version::Version::parse(v)
                .map(|_| v.to_owned())
                .ok_or_else(|| Error::invalid(format!("`{v}` is not a pi version")))
        }
        None => {
            op.stage(ToolStage::Resolving, Some(op.channel.clone()));
            latest(op.cx).await
        }
    }
}

async fn install_archive(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let v = target_version(op).await?;
    op.log.line(format!("version: {v}"));
    let asset = pi_asset(&cx.platform);
    let base = format!("{}/{REPO}/releases/download/v{v}", cx.sources.github);
    op.stage(ToolStage::Resolving, Some(asset.clone()));
    let sums_url = format!("{base}/SHA256SUMS");
    let sums = http::get_text(cx.client()?, &sums_url).await?;
    let sha = sha_for(&sums, &asset).ok_or_else(|| Error::NotFound {
        what: format!("{asset} in pi {v}'s SHA256SUMS"),
    })?;
    op.log.line(format!("SHA256SUMS: {asset} {sha}"));
    let dl = common::downloads(cx, ToolId::Pi).join(format!("{v}-{asset}"));
    op.download(
        &format!("{base}/{asset}"),
        &dl,
        Some(&Checksum::Sha256(sha)),
        None,
    )
    .await?;
    op.check_cancel()?;
    op.stage(ToolStage::Extracting, Some(asset.clone()));
    let staging = fsx::staging_dir(&cx.tool_dir(ToolId::Pi), "app")?;
    let zip = asset.ends_with(".zip");
    let (a, s) = (dl.clone(), staging.clone());
    tokio::task::spawn_blocking(move || {
        if zip {
            archive::extract_zip(&a, "", &s)
        } else {
            archive::extract_tar_gz(&a, "pi", &s)
        }
    })
    .await
    .map_err(|e| Error::other(e.to_string()))??;
    let staged_exe = staging.join(cx.platform.exe("pi"));
    if !staged_exe.is_file() {
        let _ = fsx::remove_dir(&staging);
        return Err(Error::Invalid {
            message: format!("{asset} has no pi program"),
        });
    }
    archive::make_executable(&staged_exe)?;
    common::swap_dir(&staging, &app_dir(cx), "pi")?;
    let _ = std::fs::remove_file(&dl);
    op.stage(ToolStage::Checking, None);
    crate::forget_versions();
    let exe = app_exe(cx);
    let got = crate::probe_version(cx, &exe, &["--version"]).await;
    op.log.line(format!(
        "--version: {}",
        got.as_deref().unwrap_or("no answer")
    ));
    if !got.as_deref().is_some_and(|g| version::same(g, &v)) {
        return Err(Error::Process {
            message: format!(
                "{} reports {} after installing {v}",
                exe.display(),
                got.as_deref().unwrap_or("nothing")
            ),
        });
    }
    let notes = common::expose(op, "pi", &exe, &app_dir(cx))?;
    op.record(|s| {
        s.installed = Some(InstalledRecord {
            kind: ToolInstallKind::Standalone,
            version: v.clone(),
            path: exe.clone(),
            at: Timestamp::now(),
        });
    })?;
    Ok(op.report(Some(v), Some(exe), notes))
}

async fn remove_ours(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    op.stage(ToolStage::Removing, None);
    common::unexpose(op)?;
    for dir in [app_dir(cx), common::downloads(cx, ToolId::Pi)] {
        if let Err(e) = fsx::remove_dir(&dir) {
            return Err(Error::Conflict {
                message: format!("{} cannot be removed (close pi first): {e}", dir.display()),
            });
        }
    }
    fsx::remove_empty_dirs(&cx.tool_dir(ToolId::Pi), &crate::state::tools_dir(&cx.data));
    op.log.line("removed the pi program");
    op.record(|s| s.installed = None)?;
    Ok(op.report(None, None, vec![kept()]))
}

fn kept() -> Notice {
    Notice::info(text(
        "tools.pi.kept",
        "pi's settings and sessions in ~/.pi/agent were kept.",
    ))
}

/// `pi update`: pi's own update of an install made by its installer or npm.
async fn pi_update(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let cmd = cx
        .cmd(&i.path)
        .args(["update"])
        .env("PI_TELEMETRY", "0")
        .timeout(Duration::from_secs(20 * 60));
    op.run_ok(&cmd, ToolStage::Installing).await?;
    crate::forget_versions();
    let now = crate::probe_version(cx, &i.path, &["--version"]).await;
    Ok(op.report(now, Some(i.path.clone()), Vec::new()))
}

async fn brew(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let remove = op.req.op == ToolOp::Uninstall;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let brew = cx
        .which("brew")
        .ok_or_else(|| Error::invalid("brew is not on PATH"))?;
    let cmd = cx
        .cmd(&brew)
        .args([
            if remove { "uninstall" } else { "upgrade" },
            "pi-coding-agent",
        ])
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
        crate::probe_version(cx, &i.path, &["--version"]).await
    };
    Ok(op.report(now, (!remove).then(|| i.path.clone()), Vec::new()))
}

async fn npm_uninstall(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let (dir, prefix) = common::npm_package_of(&i.path, PACKAGE, cx.platform.os)
        .ok_or_else(|| Error::invalid("cannot tell this npm install's prefix"))?;
    let npm = cx
        .which("npm")
        .ok_or_else(|| Error::invalid("npm is not on PATH"))?;
    let args = [
        "uninstall".to_owned(),
        "-g".into(),
        "--prefix".into(),
        prefix.display().to_string(),
        PACKAGE.into(),
    ];
    let modules = dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&prefix)
        .to_path_buf();
    if !common::writable(&modules) {
        return Err(Error::Unsupported {
            what: format!(
                "removing the npm install in {} (it needs administrator rights; run `npm {}` yourself)",
                prefix.display(),
                args.join(" ")
            ),
        });
    }
    let cmd = cx
        .cmd(&npm)
        .args(args)
        .timeout(Duration::from_secs(10 * 60));
    op.run_ok(&cmd, ToolStage::Removing).await?;
    Ok(op.report(None, None, vec![kept()]))
}

/// What pi's installer does for "uninstall": its releases folder, its
/// launchers in `~/.pi/agent/bin` and the link it made on `PATH`
/// (`managed-install.json` `entrypoint`).
async fn remove_managed(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    op.stage(ToolStage::Removing, None);
    let install = managed_dir(cx);
    let marker = install.join("managed-install.json");
    let entry = std::fs::read(&marker)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            ["entrypoint", "entrypointPath", "symlink", "link"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str).map(PathBuf::from))
        });
    let agent_bin = cx.home.join(".pi").join("agent").join("bin");
    if let Some(link) = entry
        && let Ok(target) = std::fs::read_link(&link)
        && common::canonical(&link.parent().unwrap_or(&link).join(&target))
            .starts_with(common::canonical(&cx.home.join(".pi")))
    {
        fsx::remove_file(&link)?;
        op.log.line(format!("removed {}", link.display()));
    }
    for name in ["pi", "pi.cmd", "pi.ps1"] {
        let p = agent_bin.join(name);
        if fsx::remove_file(&p)? {
            op.log.line(format!("removed {}", p.display()));
        }
    }
    if fsx::remove_dir(&install)? {
        op.log.line(format!("removed {}", install.display()));
    }
    fsx::remove_empty_dirs(&agent_bin, &cx.home.join(".pi").join("agent"));
    Ok(op.report(None, None, vec![kept()]))
}
