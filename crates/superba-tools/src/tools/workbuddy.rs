//! WorkBuddy (China build) and WorkBuddy AI (international build), Tencent's
//! desktop apps (research: `harness-research/workbuddy.md`).
//!
//! - **Newest version**: `https://www.workbuddy.{cn,ai}/v2/update?platform=<p>`
//!   (`{version, url, sha256hash}`; the hash is empty for some packages, the
//!   signature is checked in any case).
//! - **Windows (x64)**: the per-user NSIS installer run with `/S`. It opens
//!   the app when it is done and (international build) its process stays
//!   the app's parent, so Superba does not wait for it: it waits for the
//!   app's `Uninstall` entry with the new version, then closes what the
//!   installer opened. WorkBuddy forces its own updates: Superba shows the
//!   version and offers no update.
//! - **Removal**: the running app (and its Node helpers under
//!   `~/.workbuddy[-ai]/binaries`) is closed first — its uninstaller does not
//!   do that and leaves a half-deleted folder — then the quiet uninstaller,
//!   then the launch-at-login value it leaves behind. Data folders stay.
//! - **macOS, Linux**: a link (no per-user install verified; Linux packages
//!   need root).

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use superba_core::{
    Error, Notice, Result, Timestamp, ToolId, ToolInstallKind, ToolOp, ToolReport, ToolStage,
    ToolUpdater,
};

use super::apps;
use super::common::{install, text};
use crate::descriptor::{Descriptor, Method, workbuddy_platform};
use crate::http::{self, Checksum};
use crate::platform::Os;
use crate::state::InstalledRecord;
use crate::winapps::{self, UninstallEntry};
use crate::{Ctx, Detected, Op, version};

/// The facts that differ between the two builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build {
    /// Product name (`DisplayName`).
    pub name: &'static str,
    /// Program file.
    pub exe: &'static str,
    /// Install folder under `%LOCALAPPDATA%\Programs`.
    pub folder: &'static str,
    /// Its `Uninstall` key.
    pub key: &'static str,
    /// Its launch-at-login value under `HKCU\...\Run`.
    pub run_value: &'static str,
    /// Its data folder in the home.
    pub data: &'static str,
    /// macOS bundle.
    pub bundle: &'static str,
    /// Linux package.
    pub package: &'static str,
}

/// The build of `tool`.
pub fn build(tool: ToolId) -> Build {
    if tool == ToolId::WorkbuddyAi {
        Build {
            name: "WorkBuddy AI",
            exe: "WorkBuddyAI.exe",
            folder: "WorkBuddyAI",
            key: "C02C88CB-5DAE-4330-AEC1-572CFD048A59",
            run_value: "WorkBuddy.WorkBuddyAI",
            data: ".workbuddy-ai",
            bundle: "WorkBuddy AI.app",
            package: "workbuddy-ai",
        }
    } else {
        Build {
            name: "WorkBuddy",
            exe: "WorkBuddy.exe",
            folder: "WorkBuddy",
            key: "BFD312E9-1019-4F57-9F44-F86246833B50",
            run_value: "WorkBuddy.WorkBuddy",
            data: ".workbuddy",
            bundle: "WorkBuddy.app",
            package: "workbuddy",
        }
    }
}

/// The update API's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    /// Version as shown (`5.8.3`; the build number is dropped).
    pub version: String,
    /// Full version (`5.8.3.41334564`).
    pub full: String,
    /// Package URL.
    pub url: String,
    /// sha256 when published.
    pub sha256: Option<String>,
}

/// Parse the update API's JSON.
pub fn parse_update(doc: &Value) -> Option<Update> {
    let full = doc.get("version")?.as_str()?.trim().to_owned();
    let parts: Vec<&str> = full.split('.').collect();
    let version = parts[..parts.len().min(3)].join(".");
    version::Version::parse(&version)?;
    Some(Update {
        version,
        full,
        url: doc.get("url")?.as_str()?.to_owned(),
        sha256: doc
            .get("sha256hash")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase),
    })
}

/// The newest release of `tool`'s build for this platform.
pub async fn latest(cx: &Ctx, tool: ToolId) -> Result<Update> {
    let host = if tool == ToolId::WorkbuddyAi {
        &cx.sources.workbuddy_intl
    } else {
        &cx.sources.workbuddy_cn
    };
    let url = format!(
        "{host}/v2/update?platform={}",
        workbuddy_platform(&cx.platform)
    );
    let doc = http::get_json(cx.client()?, &url).await?;
    parse_update(&doc).ok_or_else(|| Error::parse(url, "not an update answer"))
}

fn entry(b: &Build) -> Option<UninstallEntry> {
    winapps::uninstall_entries(|e| e.key.to_ascii_uppercase().contains(b.key) || e.name == b.name)
        .into_iter()
        .next()
}

fn install_dir(cx: &Ctx, b: &Build, e: Option<&UninstallEntry>) -> PathBuf {
    let from_entry = e.and_then(|e| {
        e.location.clone().or_else(|| {
            let cmd = e.quiet_uninstall.as_deref().or(e.uninstall.as_deref())?;
            let exe = winapps::split_command_line(cmd).into_iter().next()?;
            PathBuf::from(exe).parent().map(PathBuf::from)
        })
    });
    from_entry.unwrap_or_else(|| {
        cx.env
            .var_non_empty("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| cx.home.join("AppData").join("Local"))
            .join("Programs")
            .join(b.folder)
    })
}

/// The folders whose processes belong to the app: its program folder and
/// its Node helpers.
fn app_dirs(cx: &Ctx, b: &Build, e: Option<&UninstallEntry>) -> Vec<PathBuf> {
    vec![install_dir(cx, b, e), cx.home.join(b.data).join("binaries")]
}

/// What is installed.
pub async fn detect(cx: &Ctx, desc: &Descriptor) -> Detected {
    let b = build(desc.tool);
    let mut det = Detected::default();
    match cx.platform.os {
        Os::Windows => {
            if let Some(e) = entry(&b) {
                let dir = install_dir(cx, &b, Some(&e));
                det.can_uninstall = e.per_user;
                let running = app_dirs(cx, &b, Some(&e))
                    .iter()
                    .map(|d| apps::app_processes(d, 0).len())
                    .sum::<usize>();
                if running > 0 {
                    det.uninstall_cautions.push(
                        text(
                            "tools.app.caution_close",
                            "{name} is running and will be closed first.",
                        )
                        .arg("name", b.name),
                    );
                }
                det.installs.push(install(
                    dir.join(b.exe),
                    ToolInstallKind::App,
                    e.version.clone(),
                    false,
                    true,
                ));
            }
        }
        Os::Macos => {
            if let Some(app) = apps::mac_app(cx, &[b.bundle]) {
                let v = apps::mac_bundle_version(cx, &app).await;
                det.installs
                    .push(install(app, ToolInstallKind::App, v, false, true));
            }
        }
        Os::Linux => {
            let opt = PathBuf::from("/opt").join(b.folder);
            if opt.is_dir() {
                let v = apps::linux_package_version(cx, b.package).await;
                det.installs
                    .push(install(opt, ToolInstallKind::SystemPackage, v, false, true));
            }
        }
    }
    det.updater = Some(ToolUpdater::Forced);
    if !det.installs.is_empty() {
        det.notes.push(Notice::info(
            text(
                "tools.workbuddy.self_update",
                "{name} updates itself and cannot be told not to; Superba shows its version only.",
            )
            .arg("name", b.name),
        ));
    }
    if desc.method == Method::Link {
        det.link = Some(desc.page.to_owned());
    }
    det
}

/// Install or remove (Windows).
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    let Method::WindowsInstaller { args, signer } = op.desc.method else {
        return Err(Error::Unsupported {
            what: format!(
                "installing {} on this OS through Superba",
                op.tool().display_name()
            ),
        });
    };
    match op.req.op {
        ToolOp::Install => install_windows(op, args, signer).await,
        ToolOp::Uninstall => uninstall_windows(op).await,
        _ => Err(Error::Unsupported {
            what: format!("updating {} (it updates itself)", op.tool().display_name()),
        }),
    }
}

async fn install_windows(op: &Op<'_>, args: &[&str], signer: &str) -> Result<ToolReport> {
    let cx = op.cx;
    let b = build(op.tool());
    op.stage(ToolStage::Resolving, Some(op.channel.clone()));
    let up = latest(cx, op.tool()).await?;
    op.log.line(format!(
        "update API: {} {} sha256 {}",
        up.full,
        up.url,
        up.sha256.as_deref().unwrap_or("(none)")
    ));
    let name = up
        .url
        .rsplit('/')
        .next()
        .unwrap_or("WorkBuddy-setup.exe")
        .to_owned();
    let sum = up.sha256.clone().map(Checksum::Sha256);
    let setup = apps::fetch_installer(op, &up.url, &name, sum.as_ref(), None, signer).await?;
    let began = apps::now_secs();
    let cmd = cx.cmd(&setup).args(args.iter().copied());
    op.stage(ToolStage::Installing, Some(name.clone()));
    op.log.line(format!("start: {}", cmd.display()));
    let mut child = crate::run::spawn(&cmd)?;
    let want = up.version.clone();
    let dir = install_dir(cx, &b, None);
    // The installer opens the app as its last step (and the international
    // build's setup stays as the app's parent), so "done" is the setup
    // exiting or the app starting, never the Uninstall entry alone (it is
    // written before the files are all there).
    let end = std::time::Instant::now() + Duration::from_secs(25 * 60);
    let opened = loop {
        if op.sink.cancel_requested() {
            crate::run::end(&mut child).await;
            op.log.line("cancelled: installer ended");
            return Err(Error::Cancelled);
        }
        if let Ok(Some(status)) = child.try_wait() {
            op.log.line(format!("installer exited: {status}"));
            break false;
        }
        if !apps::app_processes(&dir, began).is_empty() {
            op.log
                .line("the installer opened the app: installation finished");
            break true;
        }
        if std::time::Instant::now() >= end {
            crate::run::end(&mut child).await;
            return Err(Error::Timeout {
                what: format!("{} installer", b.name),
            });
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    op.stage(ToolStage::Checking, None);
    let found = apps::wait_for(op, Duration::from_secs(90), || {
        entry(&b).filter(|e| {
            e.version
                .as_deref()
                .is_some_and(|v| version::same(&trim3(v), &want))
                && install_dir(cx, &b, Some(e)).join(b.exe).is_file()
        })
    })
    .await?
    .ok_or_else(|| Error::Process {
        message: format!(
            "the {} installer finished but {} {want} is not registered",
            b.name, b.name
        ),
    })?;
    op.log.line(format!(
        "registered: {} {}",
        found.name,
        found.version.as_deref().unwrap_or("?")
    ));
    let mut notes = Vec::new();
    if opened {
        // Let it settle, then close what the installer opened; the setup
        // that stays as its parent exits with it (else it is ended).
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let closed = apps::close_app(op, &app_dirs(cx, &b, Some(&found)), began).await?;
    for _ in 0..30 {
        if !matches!(child.try_wait(), Ok(None)) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if let Ok(None) = child.try_wait() {
        op.log
            .line("the installer is still running after the app closed; ending it");
        crate::run::end(&mut child).await;
    }
    if closed > 0 {
        notes.push(Notice::info(
            text(
                "tools.app.closed_after_install",
                "{name} opened itself after installing; Superba closed it. Start it from the Start menu.",
            )
            .arg("name", b.name),
        ));
    }
    if winapps::run_value(b.run_value).is_some() {
        notes.push(Notice::info(
            text(
                "tools.workbuddy.login_item",
                "{name} starts when you sign in to Windows (its own setting, on by default).",
            )
            .arg("name", b.name),
        ));
    }
    let _ = std::fs::remove_file(&setup);
    let path = install_dir(cx, &b, Some(&found)).join(b.exe);
    op.record(|s| {
        s.installed = Some(InstalledRecord {
            kind: ToolInstallKind::App,
            version: want.clone(),
            path: path.clone(),
            at: Timestamp::now(),
        });
    })?;
    Ok(op.report(Some(want), Some(path), notes))
}

fn trim3(v: &str) -> String {
    let parts: Vec<&str> = v.trim().split('.').collect();
    parts[..parts.len().min(3)].join(".")
}

async fn uninstall_windows(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let b = build(op.tool());
    let e = entry(&b).ok_or_else(|| Error::invalid(format!("{} is not installed", b.name)))?;
    if !e.per_user {
        return Err(Error::Unsupported {
            what: format!("removing a {} installed for all users", b.name),
        });
    }
    op.stage(ToolStage::Removing, None);
    let dir = install_dir(cx, &b, Some(&e));
    apps::close_app(op, &app_dirs(cx, &b, Some(&e)), 0).await?;
    let parts = apps::quiet_uninstall(&e).ok_or_else(|| Error::NotFound {
        what: format!("{}'s uninstaller", b.name),
    })?;
    let cmd = cx
        .cmd(&parts[0])
        .args(parts[1..].iter().cloned())
        .timeout(Duration::from_secs(10 * 60));
    op.run_ok(&cmd, ToolStage::Removing).await?;
    let gone = apps::wait_for(op, Duration::from_secs(120), || match entry(&b) {
        None => Some(UninstallEntry::default()),
        Some(_) => None,
    })
    .await?;
    if gone.is_none() {
        return Err(Error::Process {
            message: format!(
                "{}'s uninstaller finished but it is still registered",
                b.name
            ),
        });
    }
    // The launch-at-login value the uninstaller leaves behind.
    if let Some(v) = winapps::run_value(b.run_value) {
        let target = winapps::split_command_line(&v)
            .into_iter()
            .next()
            .map(PathBuf::from);
        let stale = target
            .as_ref()
            .is_none_or(|t| !t.exists() || t.starts_with(&dir));
        if stale && winapps::remove_run_value(b.run_value)? {
            op.log
                .line(format!("removed launch-at-login value {}", b.run_value));
        }
    }
    // The uninstaller's copy keeps deleting after it returned.
    if !apps::wait_removed(op, &dir, &dir.join(b.exe), Duration::from_secs(180)).await?
        && dir.ends_with(b.folder)
    {
        match crate::fsx::remove_dir(&dir) {
            Ok(_) => op.log.line(format!("removed leftover {}", dir.display())),
            Err(e) => op
                .log
                .line(format!("leftover {} stays: {e}", dir.display())),
        }
    }
    op.record(|s| s.installed = None)?;
    Ok(op.report(
        None,
        None,
        vec![Notice::info(
            text(
                "tools.workbuddy.kept",
                "{name}'s settings and data in ~/{data} were kept.",
            )
            .arg("name", b.name)
            .arg("data", b.data),
        )],
    ))
}
