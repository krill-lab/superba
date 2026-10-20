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
use crate::{Ctx, Detected, Op, archive, fsx, run, version};

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
    let cmd = cx
        .cmd(&exe)
        .args(["--version"])
        .timeout(Duration::from_secs(20));
    verify_version(op, &cmd, &v).await?;
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

async fn verify_version(op: &Op<'_>, cmd: &run::Cmd, expected: &str) -> Result<()> {
    // A newly extracted Windows pi can take longer than 20 s to start, while
    // the next launch finishes in a few seconds. Retry only that timeout,
    // once. Keep each failure in the operation log and honor cancellation.
    // Installation must execute the program, even if detection cached a version.
    let attempts = if op.cx.platform.os == Os::Windows {
        2
    } else {
        1
    };
    for attempt in 1..=attempts {
        op.log.line(format!(
            "--version attempt {attempt}/{attempts} (deadline {} ms)",
            cmd.timeout.as_millis()
        ));
        let out = match op.run_ok(cmd, ToolStage::Checking).await {
            Err(Error::Timeout { .. }) if attempt < attempts => {
                op.log
                    .line("--version timed out; retrying once after the process tree ended");
                continue;
            }
            result => result?,
        };
        let got = version::from_output(&format!("{}\n{}", out.stdout, out.stderr));
        if !got.as_deref().is_some_and(|g| version::same(g, expected)) {
            return Err(Error::Process {
                message: format!(
                    "{} reports {} after installing {expected}: {}",
                    cmd.program.display(),
                    got.as_deref().unwrap_or("no parseable version"),
                    out.tail(6)
                ),
            });
        }
        return Ok(());
    }
    unreachable!("the last attempt returns its result")
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

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;

    use superba_core::{
        LocalContext, NetworkSettings, NoToolProgress, SharedToolProgress, ToolProgressSink,
        ToolRequest,
    };

    use super::*;
    use crate::log::OpLog;
    use crate::platform::{Arch, Platform};
    use crate::{NoHost, Sources, ToolManager};

    struct Check {
        root: tempfile::TempDir,
        manager: ToolManager,
        log: OpLog,
        cmd: run::Cmd,
    }

    impl Check {
        fn new(os: Os, script: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let ctx = LocalContext::for_tests(root.path());
            let manager = ToolManager::with(
                &ctx,
                NetworkSettings::default(),
                Arc::new(NoHost),
                Platform::new(os, Arch::X64),
                Sources::default(),
            );
            let log = OpLog::begin(&ctx.data_dir, ToolId::Pi, "verify test");
            let cmd = run::Cmd::new("/bin/sh")
                .args(["-c", &format!("echo attempt >> attempts; {script}")])
                .cwd(root.path())
                .timeout(Duration::from_millis(200));
            Self {
                root,
                manager,
                log,
                cmd,
            }
        }

        async fn verify(&self, sink: &dyn ToolProgressSink) -> Result<()> {
            let req = ToolRequest::new(ToolId::Pi, ToolOp::Install);
            let op = Op {
                cx: self.manager.ctx(),
                req: &req,
                det: &Detected::default(),
                desc: crate::descriptor::descriptor(ToolId::Pi, &self.manager.ctx().platform),
                channel: "latest".into(),
                log: &self.log,
                sink,
            };
            verify_version(&op, &self.cmd, "1.3.0").await
        }

        fn attempts(&self) -> usize {
            std::fs::read_to_string(self.root.path().join("attempts"))
                .unwrap_or_default()
                .lines()
                .count()
        }

        fn log(&self) -> String {
            std::fs::read_to_string(self.log.path()).unwrap()
        }
    }

    #[tokio::test]
    async fn windows_first_start_timeout_retries_and_logs_both_results() {
        let check = Check::new(
            Os::Windows,
            "if [ ! -f ready ]; then : > ready; exec /bin/sleep 30; fi; echo 1.3.0",
        );
        check.verify(&NoToolProgress).await.unwrap();
        assert_eq!(check.attempts(), 2);
        let log = check.log();
        assert!(log.contains("failed: timed out:"), "{log}");
        assert!(log.contains("retrying once"), "{log}");
        assert!(log.contains("| 1.3.0") && log.contains("exit: 0"), "{log}");
    }

    #[tokio::test]
    async fn persistent_timeout_is_bounded_and_non_windows_is_not_retried() {
        for (os, attempts) in [(Os::Windows, 2), (Os::Linux, 1)] {
            let check = Check::new(os, "exec /bin/sleep 30");
            let result =
                tokio::time::timeout(Duration::from_secs(5), check.verify(&NoToolProgress))
                    .await
                    .expect("verification must be bounded");
            assert!(matches!(result, Err(Error::Timeout { .. })), "{result:?}");
            assert_eq!(check.attempts(), attempts);
        }
    }

    #[tokio::test]
    async fn failed_exit_unparseable_and_wrong_versions_are_not_retried() {
        for (script, detail) in [
            ("echo 1.3.0; echo startup-failed >&2; exit 9", "exit code 9"),
            ("echo not-ready", "no parseable version"),
            ("echo 1.2.0", "reports 1.2.0 after installing 1.3.0"),
        ] {
            let check = Check::new(Os::Windows, script);
            let err = check.verify(&NoToolProgress).await.unwrap_err();
            assert!(err.to_string().contains(detail), "{err}");
            assert_eq!(check.attempts(), 1);
            assert!(!check.log().contains("retrying"));
        }
    }

    #[tokio::test]
    async fn wrong_version_after_timeout_still_fails() {
        let check = Check::new(
            Os::Windows,
            "if [ ! -f ready ]; then : > ready; exec /bin/sleep 30; fi; echo 1.2.0",
        );
        let err = check.verify(&NoToolProgress).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("reports 1.2.0 after installing 1.3.0")
        );
        assert_eq!(check.attempts(), 2);
    }

    #[tokio::test]
    async fn cancellation_ends_verification_without_retry() {
        let check = Check::new(Os::Windows, "echo waiting; exec /bin/sleep 30");
        let sink = SharedToolProgress::default();
        let cancel = async {
            loop {
                if sink
                    .latest()
                    .is_some_and(|p| p.detail.as_deref() == Some("waiting"))
                {
                    sink.cancel();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(check.verify(&sink), cancel)
        })
        .await
        .expect("cancellation must stop the command");
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert_eq!(check.attempts(), 1);
        assert!(!check.log().contains("retrying"));
    }
}
