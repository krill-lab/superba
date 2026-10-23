//! dsh, the DeepSeek Harness (research: `harness-research/dsh.md`).
//!
//! - **npm (Node ≥ 22.19)**: `npm install -g --prefix <data>/tools/dsh/npm
//!   @deepseek-ai/dsh@<v> --allow-scripts=@deepseek-ai/dsh-subprocess-local,koffi,node-pty`
//!   (npm 12 blocks those install scripts otherwise; older npm ignores the
//!   flag); channels are the npm dist-tags (`latest`, `next`, `alpha`).
//! - **Node-free fallback**: the PyPI `deepseek-harness-runtime-bin` wheel
//!   for this platform (sha256 from PyPI), its `runtime/` folder unpacked
//!   into `<data>/tools/dsh/runtime` (a single executable with Node built in,
//!   plus its ripgrep / spawn-helper sidecars). PyPI only gets some releases:
//!   its channel is `pypi`, an npm version without a wheel is refused.
//! - The command: a launcher in `~/.local/bin` (Unix) or the folder on the
//!   user `PATH` (Windows). dsh has no updater of its own.
//! - A dsh installed with the user's own npm is updated / removed with that
//!   npm prefix. `~/.dsh` is never touched.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use superba_core::{
    Error, Notice, Result, Timestamp, ToolId, ToolInstallKind, ToolOp, ToolReport, ToolStage,
    ToolUpdater,
};

use super::common::{self, install, text};
use crate::descriptor::{Descriptor, Method, dsh_runtime_tag};
use crate::http::{self, Checksum};
use crate::platform::Os;
use crate::state::{InstalledRecord, ToolState};
use crate::{Ctx, Detected, Op, archive, fsx, version};

/// The npm package.
pub const PACKAGE: &str = "@deepseek-ai/dsh";

/// The PyPI project of the Node-free runtime.
pub const PYPI_PROJECT: &str = "deepseek-harness-runtime-bin";

/// The `--allow-scripts` list.
pub const ALLOW_SCRIPTS: &str = "@deepseek-ai/dsh-subprocess-local,koffi,node-pty";

/// The channel of the PyPI runtime.
pub const PYPI: &str = "pypi";

const MIN_NODE: (u64, u64) = (22, 19);

fn prefix(cx: &Ctx) -> PathBuf {
    cx.tool_dir(ToolId::Dsh).join("npm")
}

fn runtime_dir(cx: &Ctx) -> PathBuf {
    cx.tool_dir(ToolId::Dsh).join("runtime")
}

/// The runtime's executable in `dir`.
fn runtime_exe(cx: &Ctx, dir: &std::path::Path) -> Option<PathBuf> {
    let (tag, _) = dsh_runtime_tag(&cx.platform)?;
    Some(
        dir.join(
            cx.platform
                .exe(&format!("deepseek-harness-sdk-runtime-{tag}")),
        ),
    )
}

/// The program `dsh` of the install in `dir` that runs it (Windows: the
/// folder that goes on `PATH`).
fn path_dir_of(cx: &Ctx, kind: ToolInstallKind) -> PathBuf {
    match kind {
        ToolInstallKind::Runtime => runtime_dir(cx),
        _ => prefix(cx),
    }
}

/// Node and npm are usable for the npm method.
async fn node_usable(cx: &Ctx) -> (bool, Option<String>) {
    let v = common::node_version(cx).await;
    let shown = v.as_ref().map(ToString::to_string);
    (
        v.is_some_and(|v| common::node_ok(&v, MIN_NODE)) && cx.which("npm").is_some(),
        shown,
    )
}

/// DeepSeek's `dsh`: the chosen path, else the first `dsh` found that is not
/// Homebrew's `dsh` formula (Dancer's shell, a different program).
pub fn find(cx: &Ctx) -> Option<PathBuf> {
    let (found, dirs) = cx.locate("dsh");
    if let Some(l) = &found
        && l.source == crate::locate::Source::Override
    {
        return Some(l.path.clone());
    }
    dirs.find_all("dsh")
        .into_iter()
        .map(|l| l.path)
        .find(|p| !common::homebrew_of(p).is_some_and(|(name, cask, _)| name == "dsh" && !cask))
}

/// What is installed.
pub async fn detect(cx: &Ctx, desc: &Descriptor, st: &ToolState) -> Detected {
    let os = cx.platform.os;
    let mut det = Detected::default();
    let mut ours = Vec::new();
    let pkg = common::npm_package_dir(&prefix(cx), PACKAGE, os);
    if pkg.join("package.json").is_file() {
        ours.push(install(
            common::npm_bin(&prefix(cx), "dsh", os),
            ToolInstallKind::NpmPrefix,
            common::package_version(&pkg),
            true,
            false,
        ));
    }
    if let Some(exe) = runtime_exe(cx, &runtime_dir(cx))
        && exe.is_file()
    {
        let v = match &st.installed {
            Some(r) if r.kind == ToolInstallKind::Runtime => Some(r.version.clone()),
            _ => crate::probe_version(cx, &exe, &["--version"]).await,
        };
        ours.push(install(exe, ToolInstallKind::Runtime, v, true, false));
    }
    let found = find(cx);
    let mut external = None;
    if let Some(f) = &found {
        let launcher = cx.local_bin().join("dsh");
        let mine = ours.iter_mut().find(|i| {
            common::same_file(f, &i.path, os)
                || (common::same_file(f, &launcher, os) && fsx::is_own_launcher(&launcher))
                || (os == Os::Windows
                    && f.parent()
                        .is_some_and(|d| common::same_file(d, &path_dir_of(cx, i.kind), os)))
        });
        match mine {
            Some(i) => i.on_path = true,
            None => {
                external = Some(match common::npm_package_of(f, PACKAGE, os) {
                    Some((dir, _)) => install(
                        f.clone(),
                        ToolInstallKind::NpmGlobal,
                        common::package_version(&dir),
                        false,
                        true,
                    ),
                    None => install(
                        f.clone(),
                        ToolInstallKind::Other,
                        crate::probe_version(cx, f, &["--version"]).await,
                        false,
                        true,
                    ),
                });
            }
        }
    }
    ours.sort_by_key(|i| !i.on_path);
    det.installs = match external {
        Some(e) => std::iter::once(e).chain(ours).collect(),
        None => ours,
    };
    if det
        .installs
        .iter()
        .filter(|i| i.on_path || i.by_superba)
        .count()
        > 1
        && det.installs.first().is_some_and(|i| !i.by_superba)
    {
        det.notes.push(Notice::warning(
            text(
                "tools.dsh.shadowed",
                "`dsh` runs {path}, not the copy Superba installed.",
            )
            .arg(
                "path",
                superba_core::paths::display_path(&det.installs[0].path),
            ),
        ));
    }
    det.updater = Some(ToolUpdater::None);
    let primary = det.installs.first().map(|i| i.kind);
    let (node_ok, node_seen) = node_usable(cx).await;
    let runtime_possible = desc.fallback == Some(Method::DshRuntime);
    let use_runtime = match primary {
        Some(ToolInstallKind::Runtime) => true,
        Some(_) => false,
        None => !node_ok,
    };
    if use_runtime {
        det.channels = Some(vec![PYPI.into()]);
        if primary.is_none() {
            if runtime_possible {
                det.notes.push(Notice::info(
                    text(
                        "tools.dsh.no_node",
                        "Node.js {need} or newer with npm was not found ({found}): Superba installs dsh's Node-free build from PyPI, which is published for some releases only.",
                    )
                    .arg("need", "22.19")
                    .arg("found", node_seen.as_deref().unwrap_or("none")),
                ));
            } else {
                det.blocked = Some(text(
                    "tools.dsh.needs_node",
                    "dsh needs Node.js 22.19 or newer with npm on this computer (there is no Node-free build for it).",
                ));
            }
        }
    } else if primary == Some(ToolInstallKind::NpmPrefix) && !node_ok {
        det.notes.push(Notice::warning(text(
            "tools.dsh.node_gone",
            "Node.js 22.19 or newer is not on PATH any more: dsh cannot run or be updated until it is.",
        )));
    }
    det.can_update = match primary {
        Some(ToolInstallKind::NpmPrefix) => node_ok,
        Some(ToolInstallKind::Runtime) => true,
        Some(ToolInstallKind::NpmGlobal) => cx.which("npm").is_some(),
        _ => false,
    };
    det.can_uninstall = matches!(
        primary,
        Some(ToolInstallKind::NpmPrefix | ToolInstallKind::Runtime | ToolInstallKind::NpmGlobal)
    );
    if det.installs.iter().any(|i| i.by_superba) || det.installs.is_empty() {
        det.path_hint = cx.path_hint(&cx.local_bin());
    }
    det
}

/// The newest version on `channel` (an npm dist-tag, or `pypi`).
pub async fn latest(cx: &Ctx, channel: &str, _det: &Detected) -> Result<String> {
    if channel == PYPI {
        let url = format!("{}/{PYPI_PROJECT}/json", cx.sources.pypi);
        let doc = http::get_json(cx.client()?, &url).await?;
        let v = doc
            .pointer("/info/version")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::parse(&url, "no info.version"))?;
        return Ok(version::pep440_to_semver(v));
    }
    npm_dist_tag(cx, PACKAGE, channel).await
}

/// `dist-tags.<tag>` of `package`.
pub async fn npm_dist_tag(cx: &Ctx, package: &str, tag: &str) -> Result<String> {
    let url = format!("{}/-/package/{package}/dist-tags", cx.sources.npm);
    let doc = http::get_json(cx.client()?, &url).await?;
    doc.get(tag)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::NotFound {
            what: format!("npm dist-tag `{tag}` of {package}"),
        })
}

/// Install, update or remove.
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    let primary = op.det.installs.first().map(|i| i.kind);
    match (op.req.op, primary) {
        (ToolOp::Uninstall, Some(ToolInstallKind::NpmPrefix | ToolInstallKind::Runtime)) => {
            remove_ours(op).await
        }
        (ToolOp::Uninstall, Some(ToolInstallKind::NpmGlobal)) => external_npm(op, true).await,
        (ToolOp::Update, Some(ToolInstallKind::NpmGlobal)) => external_npm(op, false).await,
        (ToolOp::Install | ToolOp::Update, _) if op.channel == PYPI => install_runtime(op).await,
        (ToolOp::Install | ToolOp::Update, _) => install_npm(op).await,
        _ => Err(Error::Unsupported {
            what: "this dsh install through Superba".into(),
        }),
    }
}

async fn target_version(op: &Op<'_>) -> Result<String> {
    match &op.req.version {
        Some(v) => {
            let v = version::pep440_to_semver(v.trim().trim_start_matches('v'));
            version::Version::parse(&v)
                .map(|_| v.clone())
                .ok_or_else(|| Error::invalid(format!("`{v}` is not a dsh version")))
        }
        None => {
            op.stage(ToolStage::Resolving, Some(op.channel.clone()));
            latest(op.cx, &op.channel, op.det).await
        }
    }
}

async fn install_npm(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let os = cx.platform.os;
    let (ok, seen) = node_usable(cx).await;
    if !ok {
        return Err(Error::Invalid {
            message: format!(
                "dsh's npm package needs Node.js 22.19 or newer with npm (found: {}); choose the `pypi` channel for the Node-free build",
                seen.as_deref().unwrap_or("none")
            ),
        });
    }
    let npm = cx
        .which("npm")
        .ok_or_else(|| Error::invalid("npm is not on PATH"))?;
    let v = target_version(op).await?;
    op.log.line(format!("version: {v}"));
    let prefix = prefix(cx);
    std::fs::create_dir_all(&prefix).map_err(|e| Error::io(&prefix, &e))?;
    let cmd = cx
        .cmd(&npm)
        .args([
            "install".to_owned(),
            "-g".into(),
            "--prefix".into(),
            prefix.display().to_string(),
            format!("{PACKAGE}@{v}"),
            format!("--allow-scripts={ALLOW_SCRIPTS}"),
            "--no-fund".into(),
            "--no-audit".into(),
        ])
        .timeout(Duration::from_secs(30 * 60));
    op.run_ok(&cmd, ToolStage::Installing).await?;
    op.stage(ToolStage::Checking, None);
    let pkg = common::npm_package_dir(&prefix, PACKAGE, os);
    let got = common::package_version(&pkg);
    if !got.as_deref().is_some_and(|g| version::same(g, &v)) {
        return Err(Error::Process {
            message: format!(
                "npm finished but {} holds {}",
                pkg.display(),
                got.as_deref().unwrap_or("nothing")
            ),
        });
    }
    let bin = common::npm_bin(&prefix, "dsh", os);
    crate::forget_versions();
    match crate::probe_version(cx, &bin, &["--version"]).await {
        Some(r) if version::same(&r, &v) => {
            op.log.line(format!("{} --version: {r}", bin.display()))
        }
        r => op.log.line(format!(
            "{} --version answered {} (installed {v})",
            bin.display(),
            r.as_deref().unwrap_or("nothing")
        )),
    }
    let notes = common::expose(op, "dsh", &bin, &prefix)?;
    if fsx::remove_dir(&runtime_dir(cx))? {
        op.log.line("removed the earlier Node-free runtime");
    }
    op.record(|s| {
        s.installed = Some(InstalledRecord {
            kind: ToolInstallKind::NpmPrefix,
            version: v.clone(),
            path: bin.clone(),
            at: Timestamp::now(),
        });
    })?;
    Ok(op.report(Some(v), Some(bin), notes))
}

/// The wheel of `version` (PEP 440) for `tag` in PyPI's JSON.
pub fn pick_wheel(doc: &Value, wheel_tag: &str) -> Option<(String, String, String, u64)> {
    doc.get("urls")?.as_array()?.iter().find_map(|u| {
        let name = u.get("filename")?.as_str()?;
        if !(name.ends_with(".whl") && name.contains(wheel_tag)) {
            return None;
        }
        Some((
            name.to_owned(),
            u.get("url")?.as_str()?.to_owned(),
            u.pointer("/digests/sha256")?.as_str()?.to_owned(),
            u.get("size").and_then(Value::as_u64).unwrap_or(0),
        ))
    })
}

async fn install_runtime(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let (tag, wheel_tag) = dsh_runtime_tag(&cx.platform).ok_or_else(|| Error::Unsupported {
        what: "dsh's Node-free build on this platform".into(),
    })?;
    let v = target_version(op).await?;
    let pep = version::semver_to_pep440(&v)
        .ok_or_else(|| Error::invalid(format!("dsh {v} has no PyPI spelling")))?;
    op.log.line(format!("version: {v} (PyPI {pep})"));
    let url = format!("{}/{PYPI_PROJECT}/{pep}/json", cx.sources.pypi);
    let doc = http::get_optional(cx.client()?, &url)
        .await?
        .ok_or_else(|| Error::NotFound {
            what: format!(
                "a Node-free build of dsh {v} on PyPI (it is published for some releases only; install Node.js 22.19+ to use npm)"
            ),
        })?;
    let doc: Value = serde_json::from_slice(&doc).map_err(|e| Error::parse(&url, e.to_string()))?;
    let (file, wheel_url, sha, size) =
        pick_wheel(&doc, wheel_tag).ok_or_else(|| Error::NotFound {
            what: format!("a Node-free build of dsh {v} for {tag} on PyPI"),
        })?;
    let dl = common::downloads(cx, ToolId::Dsh).join(&file);
    op.download(
        &wheel_url,
        &dl,
        Some(&Checksum::Sha256(sha)),
        (size > 0).then_some(size),
    )
    .await?;
    op.check_cancel()?;
    op.stage(ToolStage::Extracting, Some(file.clone()));
    let base = cx.tool_dir(ToolId::Dsh);
    let staging = fsx::staging_dir(&base, "runtime")?;
    let (dl2, st2) = (dl.clone(), staging.clone());
    tokio::task::spawn_blocking(move || {
        archive::extract_zip(&dl2, "deepseek_harness_runtime/runtime", &st2)
    })
    .await
    .map_err(|e| Error::other(e.to_string()))??;
    let exe =
        runtime_exe(cx, &staging).ok_or_else(|| Error::invalid("no runtime for this platform"))?;
    if !exe.is_file() {
        let _ = fsx::remove_dir(&staging);
        return Err(Error::Invalid {
            message: format!("{file} has no {}", exe.display()),
        });
    }
    archive::make_executable(&exe)?;
    let final_dir = runtime_dir(cx);
    let final_exe = runtime_exe(cx, &final_dir).ok_or_else(|| Error::invalid("no runtime"))?;
    if cx.platform.os == Os::Windows {
        fsx::write_atomic(
            &staging.join("dsh.cmd"),
            fsx::launcher_cmd(&final_exe).as_bytes(),
        )?;
    }
    common::swap_dir(&staging, &final_dir, "dsh")?;
    let _ = std::fs::remove_file(&dl);
    op.stage(ToolStage::Checking, None);
    crate::forget_versions();
    let got = crate::probe_version(cx, &final_exe, &["--version"]).await;
    op.log.line(format!(
        "{} --version: {}",
        final_exe.display(),
        got.as_deref().unwrap_or("no answer")
    ));
    if let Some(g) = &got
        && !version::same(g, &v)
    {
        return Err(Error::Process {
            message: format!("the unpacked runtime reports {g}, not {v}"),
        });
    }
    let notes = common::expose(op, "dsh", &final_exe, &final_dir)?;
    if fsx::remove_dir(&prefix(cx))? {
        op.log.line("removed the earlier npm install");
    }
    op.record(|s| {
        s.installed = Some(InstalledRecord {
            kind: ToolInstallKind::Runtime,
            version: v.clone(),
            path: final_exe.clone(),
            at: Timestamp::now(),
        });
    })?;
    Ok(op.report(Some(v), Some(final_exe), notes))
}

async fn remove_ours(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    op.stage(ToolStage::Removing, None);
    common::unexpose(op)?;
    for dir in [
        prefix(cx),
        runtime_dir(cx),
        common::downloads(cx, ToolId::Dsh),
    ] {
        match fsx::remove_dir(&dir) {
            Ok(true) => op.log.line(format!("removed {}", dir.display())),
            Ok(false) => {}
            Err(e) => {
                return Err(Error::Conflict {
                    message: format!("{} cannot be removed (close dsh first): {e}", dir.display()),
                });
            }
        }
    }
    fsx::remove_empty_dirs(
        &cx.tool_dir(ToolId::Dsh),
        &crate::state::tools_dir(&cx.data),
    );
    op.record(|s| s.installed = None)?;
    Ok(op.report(None, None, vec![kept_note()]))
}

fn kept_note() -> Notice {
    Notice::info(text(
        "tools.dsh.kept",
        "dsh's settings and sessions in ~/.dsh were kept.",
    ))
}

async fn external_npm(op: &Op<'_>, remove: bool) -> Result<ToolReport> {
    let cx = op.cx;
    let os = cx.platform.os;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let (dir, prefix) = common::npm_package_of(&i.path, PACKAGE, os)
        .ok_or_else(|| Error::invalid("cannot tell this npm install's prefix"))?;
    let npm = cx
        .which("npm")
        .ok_or_else(|| Error::invalid("npm is not on PATH"))?;
    let v = if remove {
        None
    } else {
        Some(target_version(op).await?)
    };
    let mut args = vec![
        if remove { "uninstall" } else { "install" }.to_owned(),
        "-g".into(),
        "--prefix".into(),
        prefix.display().to_string(),
    ];
    match &v {
        Some(v) => {
            args.push(format!("{PACKAGE}@{v}"));
            args.push(format!("--allow-scripts={ALLOW_SCRIPTS}"));
            args.push("--no-fund".into());
            args.push("--no-audit".into());
        }
        None => args.push(PACKAGE.into()),
    }
    let modules = dir
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(&prefix)
        .to_path_buf();
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
        .timeout(Duration::from_secs(30 * 60));
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
        common::package_version(&dir)
    };
    let notes = if remove {
        vec![kept_note()]
    } else {
        Vec::new()
    };
    Ok(op.report(now, (!remove).then(|| i.path.clone()), notes))
}
