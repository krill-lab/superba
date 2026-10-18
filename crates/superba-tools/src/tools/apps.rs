//! Desktop apps (ZCode, WorkBuddy): the version of an installed app on each
//! OS, and the Windows per-user installer flow — a verified download, the
//! signature, the silent installer, waiting for its `Uninstall` entry,
//! closing what the installer opened, and the quiet uninstaller.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use superba_core::{Error, Result, ToolStage};

use super::common;
use crate::http::Checksum;
use crate::procs::{self, Proc};
use crate::winapps::{self, UninstallEntry};
use crate::{Ctx, Op};

/// `CFBundleShortVersionString` of a macOS app bundle.
pub async fn mac_bundle_version(cx: &Ctx, app: &Path) -> Option<String> {
    let plist = app.join("Contents").join("Info.plist");
    let bytes = std::fs::read(&plist).ok()?;
    if bytes.starts_with(b"bplist") {
        let cmd = cx
            .cmd("/usr/bin/plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
            .args([plist.as_os_str()]);
        let out = crate::run::output(&cmd).await.ok()?;
        return out.success().then(|| out.stdout.trim().to_owned());
    }
    plist_string(
        &String::from_utf8_lossy(&bytes),
        "CFBundleShortVersionString",
    )
}

/// The `<string>` after `<key>name</key>` in an XML property list.
pub fn plist_string(xml: &str, name: &str) -> Option<String> {
    let key = format!("<key>{name}</key>");
    let rest = &xml[xml.find(&key)? + key.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    let between = &rest[..rest.find("<string>")?];
    // Only whitespace between the key and its value.
    between
        .trim()
        .is_empty()
        .then(|| rest[start..start + end].trim().to_owned())
}

/// The version of a Linux system package (`dpkg-query`, else `rpm`).
pub async fn linux_package_version(cx: &Ctx, package: &str) -> Option<String> {
    for (prog, args) in [
        ("dpkg-query", vec!["-W", "-f=${Version}", package]),
        ("rpm", vec!["-q", "--qf", "%{VERSION}", package]),
    ] {
        let Some(p) = cx.which(prog) else { continue };
        let cmd = cx.cmd(p).args(args);
        if let Ok(o) = crate::run::output(&cmd).await
            && o.success()
            && !o.stdout.trim().is_empty()
        {
            return Some(o.stdout.trim().to_owned());
        }
    }
    None
}

/// The first of `bundles` in `/Applications` or `~/Applications`.
pub fn mac_app(cx: &Ctx, bundles: &[&str]) -> Option<PathBuf> {
    for dir in [PathBuf::from("/Applications"), cx.home.join("Applications")] {
        for b in bundles {
            let p = dir.join(b);
            if p.join("Contents").join("Info.plist").is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Check a downloaded Windows installer's signature: valid, and (when
/// `signer` is not empty) signed by a subject containing it.
pub async fn check_signature(op: &Op<'_>, path: &Path, signer: &str) -> Result<()> {
    op.stage(ToolStage::Verifying, Some("signature".into()));
    let subject = op.cx.host.verify_signature(path).await.map_err(|e| {
        op.log.line(format!("signature: {e}"));
        e
    })?;
    op.log.line(format!("signature valid: {subject}"));
    if !signer.is_empty()
        && !subject
            .to_ascii_lowercase()
            .contains(&signer.to_ascii_lowercase())
    {
        return Err(Error::Invalid {
            message: format!(
                "{} is signed by {subject}, not by {signer}: refused",
                path.display()
            ),
        });
    }
    Ok(())
}

/// Download (checksum when published) and check the signature.
pub async fn fetch_installer(
    op: &Op<'_>,
    url: &str,
    file: &str,
    checksum: Option<&Checksum>,
    size: Option<u64>,
    signer: &str,
) -> Result<PathBuf> {
    let dest = common::downloads(op.cx, op.tool()).join(file);
    op.download(url, &dest, checksum, size).await?;
    op.check_cancel()?;
    if let Err(e) = check_signature(op, &dest, signer).await {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    Ok(dest)
}

/// Processes of the app in `dir` that started at or after `since` (seconds
/// since the epoch; 0 = all).
pub fn app_processes(dir: &Path, since: u64) -> Vec<Proc> {
    procs::under(dir)
        .into_iter()
        .filter(|p| p.start + 1 >= since)
        .collect()
}

/// Close the app's processes in `dirs` (politely, then forced).
pub async fn close_app(op: &Op<'_>, dirs: &[PathBuf], since: u64) -> Result<usize> {
    let running: Vec<Proc> = dirs.iter().flat_map(|d| app_processes(d, since)).collect();
    if running.is_empty() {
        return Ok(0);
    }
    op.log.line(format!(
        "closing {} process(es): {}",
        running.len(),
        running
            .iter()
            .map(|p| format!("{} {}", p.pid, p.exe.display()))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let n = running.len();
    let left = tokio::task::spawn_blocking(move || procs::stop(&running, Duration::from_secs(8)))
        .await
        .unwrap_or_default();
    if !left.is_empty() {
        return Err(Error::Conflict {
            message: format!(
                "{} could not be closed; close it and try again",
                op.tool().display_name()
            ),
        });
    }
    Ok(n)
}

/// Seconds since the epoch now.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Wait up to `limit` for `done` to return an entry (polling the registry),
/// stopping on a cancel.
pub async fn wait_for(
    op: &Op<'_>,
    limit: Duration,
    mut done: impl FnMut() -> Option<UninstallEntry>,
) -> Result<Option<UninstallEntry>> {
    let end = Instant::now() + limit;
    loop {
        if let Some(e) = done() {
            return Ok(Some(e));
        }
        if Instant::now() >= end {
            return Ok(None);
        }
        op.check_cancel()?;
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

/// The quiet uninstall command of `entry`: its `QuietUninstallString`, else
/// `UninstallString` with `/S` (and `/currentuser` for a per-user
/// electron-builder install).
pub fn quiet_uninstall(entry: &UninstallEntry) -> Option<Vec<String>> {
    if let Some(q) = &entry.quiet_uninstall {
        let parts = winapps::split_command_line(q);
        if !parts.is_empty() {
            return Some(parts);
        }
    }
    let mut parts = winapps::split_command_line(entry.uninstall.as_deref()?);
    if parts.is_empty() {
        return None;
    }
    let has = |p: &[String], a: &str| p.iter().any(|x| x.eq_ignore_ascii_case(a));
    if entry.per_user && !has(&parts, "/currentuser") {
        parts.push("/currentuser".into());
    }
    if !has(&parts, "/S") {
        parts.push("/S".into());
    }
    Some(parts)
}

/// After a quiet uninstaller returned (an NSIS uninstaller hands the work to
/// a copy of itself in the temp folder and exits at once): wait up to
/// `limit` for `program` to go and for `dir` to empty, then remove `dir` if
/// nothing is left in it. Returns whether `dir` is gone.
pub async fn wait_removed(
    op: &Op<'_>,
    dir: &Path,
    program: &Path,
    limit: Duration,
) -> Result<bool> {
    let end = Instant::now() + limit;
    loop {
        let empty = std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none());
        if !program.exists() && empty {
            if dir.exists() {
                let _ = std::fs::remove_dir(dir);
            }
            return Ok(!dir.exists());
        }
        if Instant::now() >= end {
            op.log.line(format!("{} still holds files", dir.display()));
            return Ok(false);
        }
        op.check_cancel()?;
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}
