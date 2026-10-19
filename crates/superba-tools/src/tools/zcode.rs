//! ZCode, Z.ai's desktop app (research: `harness-research/zcode.md`).
//!
//! - **Newest version**: the release manifest
//!   `zcode.z.ai/api/v1/releases/electron/manifest?platform=<os>-<arch>&channel=<1|3>`
//!   (electron-updater YAML: version, files with sha512 and size).
//! - **Windows**: the per-user NSIS installer (`ZCode-<v>-win-<arch>.exe`,
//!   sha512 from the manifest, signature checked) run with `/S /currentuser`;
//!   the `Uninstall` entry it writes gives the version and the quiet
//!   uninstaller. A running ZCode is closed first (the row asks).
//! - **macOS, Linux**: a link (the package for this OS from the manifest, or
//!   the download page): no per-user install was verified there.
//! - ZCode updates itself (electron-updater; `autoDownloadAndInstallUpdates`
//!   in `~/.zcode/v2/setting.json`; the server can force an update).
//!   `~/.zcode` is never touched.

use std::path::PathBuf;
use std::time::Duration;

use superba_core::{
    Error, Notice, Result, Timestamp, ToolInstallKind, ToolOp, ToolReport, ToolStage, ToolUpdater,
};

use super::apps;
use super::common::{install, text};
use crate::descriptor::{Descriptor, Method, zcode_platform};
use crate::http::{self, Checksum};
use crate::platform::Os;
use crate::state::InstalledRecord;
use crate::winapps::{self, UninstallEntry};
use crate::{Ctx, Detected, Op, version};

/// One file of a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseFile {
    /// URL.
    pub url: String,
    /// sha512 (base64).
    pub sha512: String,
    /// Size.
    pub size: u64,
}

/// A release from the manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Release {
    /// Version.
    pub version: String,
    /// Files.
    pub files: Vec<ReleaseFile>,
}

/// Parse the electron-updater YAML manifest (the keys Superba needs).
pub fn parse_manifest(yaml: &str) -> Option<Release> {
    let mut rel = Release::default();
    let mut in_files = false;
    let mut cur: Option<ReleaseFile> = None;
    for line in yaml.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if indent == 0 {
            if let Some(f) = cur.take() {
                rel.files.push(f);
            }
            in_files = t == "files:";
            if let Some(v) = t.strip_prefix("version:") {
                rel.version = unquote(v);
            }
            continue;
        }
        if !in_files {
            continue;
        }
        let item = t.strip_prefix("- ");
        if let Some(rest) = item {
            if let Some(f) = cur.take() {
                rel.files.push(f);
            }
            cur = Some(ReleaseFile {
                url: String::new(),
                sha512: String::new(),
                size: 0,
            });
            set_field(cur.as_mut()?, rest);
        } else if let Some(f) = cur.as_mut() {
            set_field(f, t);
        }
    }
    if let Some(f) = cur.take() {
        rel.files.push(f);
    }
    rel.files.retain(|f| !f.url.is_empty());
    (!rel.version.is_empty()).then_some(rel)
}

fn unquote(v: &str) -> String {
    v.trim().trim_matches(['"', '\'']).to_owned()
}

fn set_field(f: &mut ReleaseFile, kv: &str) {
    if let Some((k, v)) = kv.split_once(':') {
        let v = unquote(v);
        match k.trim() {
            // `split_once` cuts at the first colon: the scheme's stays.
            "url" => f.url = v,
            "sha512" => f.sha512 = v,
            "size" => f.size = v.parse().unwrap_or(0),
            _ => {}
        }
    }
}

fn channel_number(channel: &str) -> u32 {
    if channel == "preview" { 3 } else { 1 }
}

/// The release on `channel`.
pub async fn latest(cx: &Ctx, channel: &str) -> Result<Release> {
    let url = format!(
        "{}/api/v1/releases/electron/manifest?platform={}&channel={}",
        cx.sources.zcode,
        zcode_platform(&cx.platform),
        channel_number(channel)
    );
    let text = http::get_text(cx.client()?, &url).await?;
    parse_manifest(&text).ok_or_else(|| Error::parse(url, "not a release manifest"))
}

fn entry(cx: &Ctx) -> Option<UninstallEntry> {
    let _ = cx;
    winapps::uninstall_entries(|e| e.name == "ZCode" || e.name.starts_with("ZCode "))
        .into_iter()
        .next()
}

/// The app's folder: `InstallLocation`, else the folder of its uninstaller
/// (ZCode's per-user entry leaves `InstallLocation` empty).
fn install_dir(e: &UninstallEntry) -> Option<PathBuf> {
    e.location.clone().or_else(|| {
        let cmd = e.quiet_uninstall.as_deref().or(e.uninstall.as_deref())?;
        let exe = winapps::split_command_line(cmd).into_iter().next()?;
        PathBuf::from(exe).parent().map(PathBuf::from)
    })
}

fn program_of(e: &UninstallEntry) -> Option<PathBuf> {
    install_dir(e)
        .map(|l| l.join("ZCode.exe"))
        .filter(|p| p.is_file())
        .or_else(|| {
            e.icon
                .as_deref()
                .map(winapps::icon_path)
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")))
        })
}

/// What is installed.
pub async fn detect(cx: &Ctx, desc: &Descriptor) -> Detected {
    let mut det = Detected::default();
    match cx.platform.os {
        Os::Windows => {
            if let Some(e) = entry(cx) {
                let path = program_of(&e)
                    .or_else(|| install_dir(&e))
                    .unwrap_or_default();
                det.can_update = e.per_user;
                det.can_uninstall = e.per_user;
                if !e.per_user {
                    det.notes.push(Notice::info(text(
                        "tools.app.machine_wide",
                        "This copy is installed for all users: changing it needs administrator rights, so Superba leaves it to its own installer.",
                    )));
                }
                if let Some(loc) = install_dir(&e)
                    && !apps::app_processes(&loc, 0).is_empty()
                {
                    let c = text(
                        "tools.app.caution_close",
                        "{name} is running and will be closed first.",
                    )
                    .arg("name", "ZCode");
                    det.update_cautions.push(c.clone());
                    det.uninstall_cautions.push(c);
                }
                det.installs.push(install(
                    path,
                    ToolInstallKind::App,
                    e.version.clone(),
                    false,
                    true,
                ));
            }
        }
        Os::Macos => {
            if let Some(app) = apps::mac_app(cx, &["ZCode.app"]) {
                let v = apps::mac_bundle_version(cx, &app).await;
                det.installs
                    .push(install(app, ToolInstallKind::App, v, false, true));
            }
        }
        Os::Linux => {
            let opt = PathBuf::from("/opt/ZCode/zcode");
            if opt.is_file() {
                let v = apps::linux_package_version(cx, "zcode").await;
                det.installs
                    .push(install(opt, ToolInstallKind::SystemPackage, v, false, true));
            }
        }
    }
    let setting = cx.home.join(".zcode").join("v2").join("setting.json");
    let auto = std::fs::read(&setting)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.get("autoDownloadAndInstallUpdates")
                .and_then(serde_json::Value::as_bool)
        });
    det.updater = Some(if auto == Some(true) {
        ToolUpdater::Automatic
    } else {
        ToolUpdater::Notifies
    });
    if !det.installs.is_empty() {
        det.notes.push(Notice::info(text(
            "tools.zcode.self_update",
            "ZCode also updates itself (Help → Check for Updates); its server can require an update.",
        )));
    }
    if desc.method == Method::Link {
        det.link = Some(desc.page.to_owned());
    }
    det
}

/// Install, update or remove (Windows).
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    let Method::WindowsInstaller { args, signer } = op.desc.method else {
        return Err(Error::Unsupported {
            what: "installing ZCode on this OS through Superba".into(),
        });
    };
    match op.req.op {
        ToolOp::Install | ToolOp::Update => install_windows(op, args, signer).await,
        ToolOp::Uninstall => uninstall_windows(op).await,
        _ => Err(Error::Unsupported {
            what: "this operation".into(),
        }),
    }
}

async fn install_windows(op: &Op<'_>, args: &[&str], signer: &str) -> Result<ToolReport> {
    let cx = op.cx;
    op.stage(ToolStage::Resolving, Some(op.channel.clone()));
    let rel = latest(cx, &op.channel).await?;
    if let Some(v) = &op.req.version
        && !version::same(v, &rel.version)
    {
        return Err(Error::Unsupported {
            what: format!(
                "installing ZCode {v}: only the newest release ({}) is offered",
                rel.version
            ),
        });
    }
    let f = rel
        .files
        .iter()
        .find(|f| f.url.ends_with(".exe"))
        .ok_or_else(|| Error::NotFound {
            what: format!("a Windows installer of ZCode {}", rel.version),
        })?;
    let name = f
        .url
        .rsplit('/')
        .next()
        .unwrap_or("ZCode-setup.exe")
        .to_owned();
    let sum = (!f.sha512.is_empty()).then(|| Checksum::Sha512(f.sha512.clone()));
    let setup = apps::fetch_installer(
        op,
        &f.url,
        &name,
        sum.as_ref(),
        (f.size > 0).then_some(f.size),
        signer,
    )
    .await?;
    if let Some(e) = entry(cx)
        && let Some(loc) = install_dir(&e)
    {
        apps::close_app(op, &[loc], 0).await?;
    }
    let began = apps::now_secs();
    let cmd = cx
        .cmd(&setup)
        .gui_program()
        .args(args.iter().copied())
        .timeout(Duration::from_secs(15 * 60));
    op.run_ok(&cmd, ToolStage::Installing).await?;
    op.stage(ToolStage::Checking, None);
    let want = rel.version.clone();
    let found = apps::wait_for(op, Duration::from_secs(90), || {
        entry(cx).filter(|e| {
            e.version
                .as_deref()
                .is_some_and(|v| version::same(v, &want))
        })
    })
    .await?
    .ok_or_else(|| Error::Process {
        message: format!("the ZCode installer finished but ZCode {want} is not registered"),
    })?;
    let mut notes = Vec::new();
    if let Some(loc) = install_dir(&found)
        && apps::close_app(op, &[loc], began).await? > 0
    {
        notes.push(Notice::info(text(
            "tools.app.closed_after_install",
            "{name} opened itself after installing; Superba closed it. Start it from the Start menu.",
        ).arg("name", "ZCode")));
    }
    let _ = std::fs::remove_file(&setup);
    let path = program_of(&found).unwrap_or_default();
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

async fn uninstall_windows(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let e = entry(cx).ok_or_else(|| Error::invalid("ZCode is not installed"))?;
    if !e.per_user {
        return Err(Error::Unsupported {
            what: "removing a ZCode installed for all users (needs administrator rights)".into(),
        });
    }
    op.stage(ToolStage::Removing, None);
    if let Some(loc) = install_dir(&e) {
        apps::close_app(op, &[loc], 0).await?;
    }
    let parts = apps::quiet_uninstall(&e).ok_or_else(|| Error::NotFound {
        what: "ZCode's uninstaller".into(),
    })?;
    let cmd = cx
        .cmd(&parts[0])
        .gui_program()
        .args(parts[1..].iter().cloned())
        .timeout(Duration::from_secs(10 * 60));
    op.run_ok(&cmd, ToolStage::Removing).await?;
    let gone = apps::wait_for(op, Duration::from_secs(90), || match entry(cx) {
        None => Some(UninstallEntry::default()),
        Some(_) => None,
    })
    .await?;
    if gone.is_none() {
        return Err(Error::Process {
            message: "ZCode's uninstaller finished but ZCode is still registered".into(),
        });
    }
    if let Some(dir) = install_dir(&e) {
        apps::wait_removed(op, &dir, &dir.join("ZCode.exe"), Duration::from_secs(120)).await?;
    }
    op.record(|s| s.installed = None)?;
    Ok(op.report(
        None,
        None,
        vec![Notice::info(text(
            "tools.zcode.kept",
            "ZCode's settings and sessions in ~/.zcode were kept.",
        ))],
    ))
}
