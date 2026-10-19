//! Claude Code (research: `harness-research/claude-code-install.md`).
//!
//! - **Install / update (native)**: the channel head
//!   (`claude-code-releases/{latest,stable}`) or an exact version →
//!   `<v>/manifest.json` → the platform's binary, sha256 and size checked →
//!   `$XDG_DATA_HOME/claude/versions/<v>` (default `~/.local/share`) → that
//!   binary's own `install <v>` (an exact version never touches
//!   `autoUpdatesChannel`), which makes the launcher `~/.local/bin/claude`.
//!   `claude install` also runs `npm uninstall -g @anthropic-ai/claude-code`,
//!   deletes `~/.claude/local` and old `claude` aliases: the row asks first
//!   when any of them exists. Refused while `DISABLE_UPDATES` is set (Claude
//!   Code then reports success without installing). The installer never
//!   changes `PATH`: on Windows Superba adds `~\.local\bin` to the user
//!   `PATH`; on Unix the row shows the line to add.
//! - **Other installs** are serviced with their own tool: npm
//!   (`--allow-scripts=@anthropic-ai/claude-code`, npm 12 blocks the
//!   postinstall otherwise), Homebrew (`brew upgrade|uninstall --cask`, never
//!   `--zap`), WinGet.
//! - **Removal (native)**: `claude daemon stop --any`, the launcher, the
//!   versions, its lock and staging folders. `~/.claude*` stay.
//! - **"Superba manages updates"**: `env.DISABLE_AUTOUPDATER = "1"` in the
//!   user `settings.json` (never `DISABLE_UPDATES`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use superba_core::{
    Error, Notice, Result, Timestamp, ToolInstall, ToolInstallKind, ToolOp, ToolReport, ToolStage,
    ToolUpdater,
};

use super::common::{self, canonical, install, slashed, text};
use crate::descriptor::claude_platform;
use crate::http::{self, Checksum};
use crate::platform::Os;
use crate::state::{InstalledRecord, ManagedUpdates, State, ToolState};
use crate::{Ctx, Detected, Op, fsx, jsonedit, version};

/// The npm package.
pub const PACKAGE: &str = "@anthropic-ai/claude-code";

/// The `settings.json` key Superba writes.
pub const MANAGED_KEY: &str = "env.DISABLE_AUTOUPDATER";

/// Where Claude Code's files are.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `~/.local/bin/claude[.exe]` (always there, no `XDG_BIN_HOME`).
    pub launcher: PathBuf,
    /// `<data>/claude`.
    pub data: PathBuf,
    /// `<data>/claude/versions`.
    pub versions: PathBuf,
    /// `<state>/claude`.
    pub state: PathBuf,
    /// `<cache>/claude`.
    pub cache: PathBuf,
    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`.
    pub config: PathBuf,
    /// `<config>/settings.json`.
    pub settings: PathBuf,
}

/// Claude Code's paths for `cx`.
pub fn paths(cx: &Ctx) -> Paths {
    let data = cx.xdg("XDG_DATA_HOME", ".local/share").join("claude");
    let config = cx
        .env
        .var_non_empty("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| cx.home.join(".claude"));
    Paths {
        launcher: cx.local_bin().join(cx.platform.exe("claude")),
        versions: data.join("versions"),
        data,
        state: cx.xdg("XDG_STATE_HOME", ".local/state").join("claude"),
        cache: cx.xdg("XDG_CACHE_HOME", ".cache").join("claude"),
        settings: config.join("settings.json"),
        config,
    }
}

/// The managed-settings files an administrator may have written.
fn managed_settings(os: Os) -> Vec<PathBuf> {
    match os {
        Os::Linux => vec![PathBuf::from("/etc/claude-code/managed-settings.json")],
        Os::Macos => vec![PathBuf::from(
            "/Library/Application Support/ClaudeCode/managed-settings.json",
        )],
        Os::Windows => vec![
            PathBuf::from(r"C:\Program Files\ClaudeCode\managed-settings.json"),
            PathBuf::from(r"C:\ProgramData\ClaudeCode\managed-settings.json"),
        ],
    }
}

fn read_object(path: &Path) -> Option<serde_json::Map<String, Value>> {
    let bytes = std::fs::read(path).ok()?;
    jsonedit::parse_object(Some(&bytes), "settings.json").ok()
}

/// A variable as Claude Code sees it: the environment, the user
/// `settings.json` `env` block, or a managed `env` block; with where it came
/// from.
fn claude_var(
    cx: &Ctx,
    settings: Option<&serde_json::Map<String, Value>>,
    name: &str,
) -> Option<(String, String)> {
    if let Some(v) = cx.env.var(name) {
        return Some((v, name.to_owned()));
    }
    let from_env = |m: &serde_json::Map<String, Value>| -> Option<String> {
        match m.get("env")?.as_object()?.get(name)? {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        }
    };
    if let Some(v) = settings.and_then(from_env) {
        return Some((v, format!("settings.json env.{name}")));
    }
    for f in managed_settings(cx.platform.os) {
        if let Some(v) = read_object(&f).as_ref().and_then(from_env) {
            return Some((v, format!("managed settings env.{name}")));
        }
    }
    None
}

fn truthy(v: &str) -> bool {
    !matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no"
    )
}

/// The native install: the launcher and, on Unix, the version its link
/// points at.
async fn native(cx: &Ctx, p: &Paths) -> Option<ToolInstall> {
    let meta = std::fs::symlink_metadata(&p.launcher).ok()?;
    let version = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&p.launcher).ok()?;
        let target = if target.is_absolute() {
            target
        } else {
            p.launcher.parent()?.join(target)
        };
        if !target.exists() {
            return None;
        }
        if canonical(&target).starts_with(canonical(&p.versions)) {
            target.file_name().map(|n| n.to_string_lossy().into_owned())
        } else {
            crate::probe_version(cx, &p.launcher, &["--version"]).await
        }
    } else {
        crate::probe_version(cx, &p.launcher, &["--version"]).await
    };
    Some(install(
        p.launcher.clone(),
        ToolInstallKind::Native,
        version,
        false,
        false,
    ))
}

/// A Claude Code found on `PATH` that is not the native launcher.
async fn classify(cx: &Ctx, path: &Path) -> ToolInstall {
    let os = cx.platform.os;
    if let Some((dir, _prefix)) = common::npm_package_of(path, PACKAGE, os) {
        return install(
            path.to_path_buf(),
            ToolInstallKind::NpmGlobal,
            common::package_version(&dir),
            false,
            true,
        );
    }
    if let Some((_name, _cask, v)) = common::homebrew_of(path) {
        return install(
            path.to_path_buf(),
            ToolInstallKind::Homebrew,
            Some(v),
            false,
            true,
        );
    }
    let s = slashed(&canonical(path), os);
    let kind =
        if s.contains("/winget/packages/anthropic.claudecode_") || s.contains("/winget/links/") {
            ToolInstallKind::Winget
        } else if s.starts_with("/usr/") || s.starts_with("/opt/") {
            ToolInstallKind::SystemPackage
        } else {
            ToolInstallKind::Other
        };
    let v = crate::probe_version(cx, path, &["--version"]).await;
    install(path.to_path_buf(), kind, v, false, true)
}

/// npm installs that the native installer's `npm uninstall -g` would remove
/// (in npm's global prefixes, whether on `PATH` or not).
fn npm_installs(cx: &Ctx) -> Vec<PathBuf> {
    common::npm_global_prefixes(cx)
        .into_iter()
        .map(|prefix| common::npm_package_dir(&prefix, PACKAGE, cx.platform.os))
        .filter(|d| d.join("package.json").is_file())
        .collect()
}

/// Shell start-up files with a `claude` alias (the installer removes those
/// lines).
fn alias_files(cx: &Ctx) -> Vec<String> {
    if cx.platform.os == Os::Windows {
        return Vec::new();
    }
    [
        ".zshrc",
        ".bashrc",
        ".bash_profile",
        ".profile",
        ".config/fish/config.fish",
    ]
    .iter()
    .filter(|f| {
        std::fs::read_to_string(cx.home.join(f)).is_ok_and(|t| {
            t.lines().any(|l| {
                let l = l.trim_start();
                l.starts_with("alias claude=") || l.starts_with("alias claude ")
            })
        })
    })
    .map(|f| format!("~/{f}"))
    .collect()
}

/// What is installed.
pub async fn detect(cx: &Ctx, st: &ToolState) -> Detected {
    let p = paths(cx);
    let os = cx.platform.os;
    let mut det = Detected::default();
    let mut native = native(cx, &p).await;
    let on_path = cx.which("claude");
    let mut others = Vec::new();
    if let Some(found) = &on_path {
        match &mut native {
            Some(n) if common::same_file(found, &n.path, os) => n.on_path = true,
            _ => others.push(classify(cx, found).await),
        }
    }
    for dir in npm_installs(cx) {
        let seen = others.iter().any(|i| {
            common::npm_package_of(&i.path, PACKAGE, os)
                .is_some_and(|(d, _)| common::same_file(&d, &dir, os))
        });
        if !seen {
            let program = dir.join("bin").join(cx.platform.exe("claude"));
            others.push(install(
                program,
                ToolInstallKind::NpmGlobal,
                common::package_version(&dir),
                false,
                false,
            ));
        }
    }
    if let Some(n) = &mut native {
        n.by_superba = st
            .installed
            .as_ref()
            .is_some_and(|r| r.kind == ToolInstallKind::Native);
    }
    // In use first: the one on PATH, else the native one.
    let mut installs: Vec<ToolInstall> = Vec::new();
    if let Some(i) = others.iter().position(|i| i.on_path) {
        installs.push(others.remove(i));
    }
    if let Some(n) = native.clone() {
        if n.on_path {
            installs.insert(0, n);
        } else {
            installs.push(n);
        }
    }
    installs.extend(others);
    det.installs = installs;

    let settings = read_object(&p.settings);
    det.channel_hint = settings
        .as_ref()
        .and_then(|s| s.get("autoUpdatesChannel"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let primary_kind = det.installs.first().map(|i| i.kind);
    det.updater = Some(
        if let Some((_, by)) = claude_var(cx, settings.as_ref(), "DISABLE_UPDATES")
            .filter(|(v, _)| !v.trim().is_empty())
        {
            ToolUpdater::Blocked { by }
        } else if let Some((_, by)) =
            claude_var(cx, settings.as_ref(), "DISABLE_AUTOUPDATER").filter(|(v, _)| truthy(v))
        {
            if st.managed_updates.is_some() {
                ToolUpdater::Superba
            } else {
                ToolUpdater::Off { by }
            }
        } else if let Some((_, by)) = claude_var(
            cx,
            settings.as_ref(),
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
        ) {
            ToolUpdater::Off { by }
        } else if matches!(
            primary_kind,
            Some(ToolInstallKind::Homebrew | ToolInstallKind::Winget)
        ) && claude_var(
            cx,
            settings.as_ref(),
            "CLAUDE_CODE_PACKAGE_MANAGER_AUTO_UPDATE",
        )
        .is_none()
        {
            ToolUpdater::PackageManager
        } else {
            ToolUpdater::Automatic
        },
    );
    if let Some(ToolUpdater::Blocked { by }) = &det.updater {
        det.notes.push(Notice::warning(
            text(
                "tools.claude.updates_blocked",
                "{by} is set: Claude Code refuses every install and update (and reports success without installing). Superba does not install while it is set.",
            )
            .arg("by", by),
        ));
    }

    // What `claude install` cleans up, asked before a native install / update.
    let mut cautions = Vec::new();
    for dir in npm_installs(cx) {
        cautions.push(
            text(
                "tools.claude.caution_npm",
                "Claude Code's installer runs `npm uninstall -g @anthropic-ai/claude-code`: the npm install in {path} will be removed.",
            )
            .arg("path", superba_core::paths::display_path(&dir)),
        );
    }
    let local = p.config.join("local");
    if local.exists() {
        cautions.push(
            text(
                "tools.claude.caution_local",
                "It deletes the old local install in {path}.",
            )
            .arg("path", superba_core::paths::display_path(&local)),
        );
    }
    let aliases = alias_files(cx);
    if !aliases.is_empty() {
        cautions.push(
            text(
                "tools.claude.caution_alias",
                "It removes the old `claude` alias lines from {files}.",
            )
            .arg("files", aliases.join(", ")),
        );
    }
    let has_native = native.is_some();
    det.install_cautions = cautions.clone();
    if matches!(primary_kind, Some(ToolInstallKind::Native)) {
        det.update_cautions = cautions;
    }
    det.install_beside = !has_native;
    if !det.installs.is_empty() && !has_native {
        det.notes.push(Notice::info(text(
            "tools.claude.native_offer",
            "Install adds Claude Code's native build (its recommended method) beside this install.",
        )));
    }
    let tools_ok = |k: ToolInstallKind| match k {
        ToolInstallKind::Native => true,
        ToolInstallKind::NpmGlobal => cx.which("npm").is_some(),
        ToolInstallKind::Homebrew => cx.which("brew").is_some(),
        ToolInstallKind::Winget => cx.which("winget").is_some(),
        _ => false,
    };
    det.can_update = primary_kind.is_some_and(tools_ok);
    det.can_uninstall = det.can_update;
    if let Some(i) = det.installs.first()
        && i.kind == ToolInstallKind::NpmGlobal
        && let Some((dir, _)) = common::npm_package_of(&i.path, PACKAGE, os)
    {
        det.uninstall_cautions.push(
            text(
                "tools.claude.caution_npm_uninstall",
                "This removes the global npm Claude Code install in {path} with `npm uninstall -g @anthropic-ai/claude-code`.",
            )
            .arg("path", superba_core::paths::display_path(&dir)),
        );
        let exe = dir.join("bin").join("claude.exe");
        if std::fs::metadata(&exe).is_ok_and(|m| m.len() < 4096) {
            det.notes.push(Notice::warning(text(
                "tools.claude.npm_placeholder",
                "This npm install is broken: npm blocked its install script, so `claude` is only a placeholder. Update it (Superba allows the script) or install the native build.",
            )));
        }
    }
    if det.installs.len() > 1 {
        det.notes.push(Notice::warning(
            text(
                "tools.claude.multiple",
                "Several Claude Code installs were found; `claude` runs {path}.",
            )
            .arg(
                "path",
                det.installs
                    .iter()
                    .find(|i| i.on_path)
                    .map(|i| superba_core::paths::display_path(&i.path))
                    .unwrap_or_else(|| "none of them".into()),
            ),
        ));
    }
    let bin = cx.local_bin();
    if has_native || det.installs.is_empty() {
        if os == Os::Windows {
            if !common::dir_on_path(cx, &bin) {
                det.notes.push(Notice::info(
                    text(
                        "tools.claude.path_windows",
                        "{dir} is not on your PATH yet; Superba adds it when it installs Claude Code.",
                    )
                    .arg("dir", superba_core::paths::display_path(&bin)),
                ));
            }
        } else if has_native {
            det.path_hint = cx.path_hint(&bin);
        }
    }
    det
}

/// The newest version on `channel` (`latest` or `stable`).
pub async fn latest(cx: &Ctx, channel: &str) -> Result<String> {
    if !matches!(channel, "latest" | "stable") {
        return Err(Error::invalid(format!(
            "unknown Claude Code channel `{channel}`"
        )));
    }
    let url = format!("{}/{channel}", cx.sources.claude);
    let v = http::get_text(cx.client()?, &url).await?;
    let v = v.trim();
    version::Version::parse(v)
        .map(|_| v.to_owned())
        .ok_or_else(|| {
            Error::parse(
                url,
                "not a version (the download site may be blocked in this region)",
            )
        })
}

/// One platform's binary in `manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// File name (`claude`, `claude.exe`).
    pub binary: String,
    /// sha256.
    pub checksum: String,
    /// Size.
    pub size: u64,
}

/// The entry of `platform` in a `manifest.json`.
pub fn manifest_entry(manifest: &Value, version: &str, platform: &str) -> Result<ManifestEntry> {
    if manifest.get("version").and_then(Value::as_str) != Some(version) {
        return Err(Error::parse(
            "Claude Code manifest.json",
            format!("it is not the manifest of {version}"),
        ));
    }
    let e = manifest
        .get("platforms")
        .and_then(|p| p.get(platform))
        .ok_or_else(|| Error::Unsupported {
            what: format!("Claude Code {version} for {platform}"),
        })?;
    let checksum = e
        .get("checksum")
        .and_then(Value::as_str)
        .filter(|c| c.len() == 64 && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| Error::parse("Claude Code manifest.json", "no sha256 checksum"))?;
    Ok(ManifestEntry {
        binary: e
            .get("binary")
            .and_then(Value::as_str)
            .unwrap_or("claude")
            .to_owned(),
        checksum: checksum.to_ascii_lowercase(),
        size: e.get("size").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// Install, update or remove.
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    let kind = op.det.installs.first().map(|i| i.kind);
    match (op.req.op, kind) {
        (ToolOp::Install, _) | (ToolOp::Update, Some(ToolInstallKind::Native)) => {
            native_install(op).await
        }
        (ToolOp::Update, Some(ToolInstallKind::NpmGlobal)) => npm(op, false).await,
        (ToolOp::Update, Some(ToolInstallKind::Homebrew)) => brew(op, false).await,
        (ToolOp::Update, Some(ToolInstallKind::Winget)) => winget(op, false).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::Native)) => native_uninstall(op).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::NpmGlobal)) => npm(op, true).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::Homebrew)) => brew(op, true).await,
        (ToolOp::Uninstall, Some(ToolInstallKind::Winget)) => winget(op, true).await,
        _ => Err(Error::Unsupported {
            what: "this Claude Code install through Superba".into(),
        }),
    }
}

async fn target_version(op: &Op<'_>) -> Result<String> {
    match &op.req.version {
        Some(v) => {
            let v = v.trim().trim_start_matches('v');
            version::Version::parse(v)
                .map(|_| v.to_owned())
                .ok_or_else(|| Error::invalid(format!("`{v}` is not a Claude Code version")))
        }
        None => {
            op.stage(ToolStage::Resolving, Some(op.channel.clone()));
            latest(op.cx, &op.channel).await
        }
    }
}

async fn native_install(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    if let Some(ToolUpdater::Blocked { by }) = &op.det.updater {
        return Err(Error::Invalid {
            message: format!(
                "{by} is set: Claude Code refuses `claude install` while it is (and reports success without installing); remove it first"
            ),
        });
    }
    let p = paths(cx);
    let v = target_version(op).await?;
    op.log.line(format!("version: {v}"));
    op.check_cancel()?;
    let plat = claude_platform(&cx.platform);
    op.stage(ToolStage::Resolving, Some(format!("{v} {plat}")));
    let manifest_url = format!("{}/{v}/manifest.json", cx.sources.claude);
    let manifest = http::get_json(cx.client()?, &manifest_url).await?;
    let entry = manifest_entry(&manifest, &v, &plat)?;
    op.log.line(format!(
        "manifest: {manifest_url}: {plat} {} sha256 {} ({} bytes)",
        entry.binary, entry.checksum, entry.size
    ));
    let sum = Checksum::Sha256(entry.checksum.clone());
    let size = (entry.size > 0).then_some(entry.size);
    let dest = p.versions.join(&v);
    let present = dest.is_file() && http::verify_file(&dest, Some(&sum), size).is_ok();
    if present {
        op.log
            .line(format!("{} is already there and verified", dest.display()));
    } else {
        let dl_dir = common::downloads(cx, op.tool()).join(format!("{v}-{plat}"));
        let file = dl_dir.join(&entry.binary);
        let url = format!("{}/{v}/{plat}/{}", cx.sources.claude, entry.binary);
        op.download(&url, &file, Some(&sum), size).await?;
        op.check_cancel()?;
        common::move_file(&file, &dest)?;
        crate::archive::make_executable(&dest)?;
        let _ = fsx::remove_dir(&dl_dir);
        op.log.line(format!("placed {}", dest.display()));
    }
    op.check_cancel()?;
    // A fresh install runs the new binary's own `install <v>`. Over an
    // existing native install the current launcher installs it: with the
    // version already on disk it only relinks (the new binary's `install`
    // would see "already installed" and keep the old link). `--force` is
    // the fallback when that did not move the link.
    let existing = op
        .det
        .installs
        .iter()
        .any(|i| i.kind == ToolInstallKind::Native)
        && crate::run::is_executable(&p.launcher);
    let installer = if existing {
        p.launcher.clone()
    } else {
        dest.clone()
    };
    claude_install(op, &installer, &v, false).await?;
    op.stage(ToolStage::Checking, None);
    let got = match check_launcher(cx, &p, &v).await {
        Ok(got) => got,
        Err(e) => {
            op.log.line(format!(
                "{e}; running `{} install {v} --force`",
                dest.display()
            ));
            claude_install(op, &dest, &v, true).await?;
            check_launcher(cx, &p, &v).await?
        }
    };
    op.log
        .line(format!("launcher {} runs {got}", p.launcher.display()));
    let mut notes = Vec::new();
    common::add_user_path(op, &cx.local_bin(), &mut notes)?;
    if let Some(h) = cx.path_hint(&cx.local_bin()) {
        notes.push(common::path_note(&h));
    }
    op.record(|s| {
        s.installed = Some(InstalledRecord {
            kind: ToolInstallKind::Native,
            version: v.clone(),
            path: p.launcher.clone(),
            at: Timestamp::now(),
        });
    })?;
    Ok(op.report(Some(v), Some(p.launcher.clone()), notes))
}

/// `<installer> install <v> [--force]`; refused output of `DISABLE_UPDATES`
/// (exit code 0) is an error.
async fn claude_install(op: &Op<'_>, installer: &Path, v: &str, force: bool) -> Result<()> {
    let mut args = vec!["install", v];
    if force {
        args.push("--force");
    }
    let cmd = op
        .cx
        .cmd(installer)
        .args(args)
        .timeout(Duration::from_secs(15 * 60));
    let out = op.run_ok(&cmd, ToolStage::Installing).await?;
    let all = format!("{}\n{}", out.stdout, out.stderr);
    if all.contains("Updates are disabled") {
        return Err(Error::Invalid {
            message: "Claude Code refused to install: updates are disabled by an administrator setting (DISABLE_UPDATES)".into(),
        });
    }
    Ok(())
}

/// The launcher runs `v` (Unix: its link points at `versions/<v>`).
async fn check_launcher(cx: &Ctx, p: &Paths, v: &str) -> Result<String> {
    if cx.platform.os != Os::Windows
        && let Ok(target) = std::fs::read_link(&p.launcher)
    {
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if version::same(&name, v) {
            return Ok(name);
        }
    }
    crate::forget_versions();
    match crate::probe_version(cx, &p.launcher, &["--version"]).await {
        Some(got) if version::same(&got, v) => Ok(got),
        got => Err(Error::Process {
            message: format!(
                "`claude install {v}` finished but {} runs {}",
                p.launcher.display(),
                got.as_deref().unwrap_or("nothing")
            ),
        }),
    }
}

async fn native_uninstall(op: &Op<'_>) -> Result<ToolReport> {
    let cx = op.cx;
    let p = paths(cx);
    if crate::run::is_executable(&p.launcher) {
        // The background-session supervisor (best effort).
        let cmd = cx
            .cmd(&p.launcher)
            .args(["daemon", "stop", "--any"])
            .timeout(Duration::from_secs(20));
        let _ = op.run(&cmd, ToolStage::Removing).await;
    }
    op.stage(ToolStage::Removing, None);
    if cx.platform.os == Os::Windows {
        let running: Vec<_> = crate::procs::under(&p.data)
            .into_iter()
            .chain(
                crate::procs::under(&cx.local_bin())
                    .into_iter()
                    .filter(|q| common::same_file(&q.exe, &p.launcher, Os::Windows)),
            )
            .collect();
        if !running.is_empty() {
            return Err(Error::Conflict {
                message: format!(
                    "Claude Code is running ({} process(es)); close it first",
                    running.len()
                ),
            });
        }
    }
    match std::fs::symlink_metadata(&p.launcher) {
        Ok(m) if m.file_type().is_symlink() => {
            fsx::remove_file(&p.launcher)?;
            op.log.line(format!("removed {}", p.launcher.display()));
        }
        Ok(_) if cx.platform.os == Os::Windows => {
            fsx::remove_file(&p.launcher)?;
            op.log.line(format!("removed {}", p.launcher.display()));
            if let Ok(d) = std::fs::read_dir(cx.local_bin()) {
                for e in d.flatten() {
                    let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                    if n.starts_with("claude.exe.old.") {
                        let _ = fsx::remove_file(&e.path());
                    }
                }
            }
        }
        Ok(_) => op.log.line(format!(
            "{} is not the native launcher link; left alone",
            p.launcher.display()
        )),
        Err(_) => {}
    }
    if fsx::remove_dir(&p.versions)? {
        op.log.line(format!("removed {}", p.versions.display()));
    }
    for (dir, stop) in [
        (p.state.join("locks"), p.state.parent()),
        (p.cache.join("staging"), p.cache.parent()),
    ] {
        if fsx::remove_dir(&dir)? {
            op.log.line(format!("removed {}", dir.display()));
        }
        if let Some(stop) = stop {
            fsx::remove_empty_dirs(dir.parent().unwrap_or(&dir), stop);
        }
    }
    if let Some(stop) = p.data.parent() {
        fsx::remove_empty_dirs(&p.data, stop);
    }
    common::unexpose(op)?;
    op.record(|s| s.installed = None)?;
    let notes = vec![Notice::info(
        text(
            "tools.claude.kept",
            "Claude Code's settings and history ({config}, ~/.claude.json) were kept.",
        )
        .arg("config", superba_core::paths::display_path(&p.config)),
    )];
    Ok(op.report(None, None, notes))
}

async fn npm(op: &Op<'_>, remove: bool) -> Result<ToolReport> {
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
    let modules = dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&prefix)
        .to_path_buf();
    let v = if remove {
        None
    } else {
        Some(target_version(op).await?)
    };
    let mut args: Vec<String> = vec![
        if remove { "uninstall" } else { "install" }.into(),
        "-g".into(),
        "--prefix".into(),
        prefix.display().to_string(),
    ];
    match &v {
        Some(v) => {
            args.push(format!("{PACKAGE}@{v}"));
            args.push(format!("--allow-scripts={PACKAGE}"));
            args.push("--no-fund".into());
            args.push("--no-audit".into());
        }
        None => args.push(PACKAGE.into()),
    }
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
        .timeout(Duration::from_secs(15 * 60));
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
        common::package_version(&dir)
    };
    Ok(op.report(now, (!remove).then(|| i.path.clone()), Vec::new()))
}

async fn brew(op: &Op<'_>, remove: bool) -> Result<ToolReport> {
    let cx = op.cx;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let (cask, is_cask, _) = common::homebrew_of(&i.path)
        .ok_or_else(|| Error::invalid("cannot tell the Homebrew cask"))?;
    let brew = cx
        .which("brew")
        .ok_or_else(|| Error::invalid("brew is not on PATH"))?;
    // Never `--zap`: it trashes ~/.claude.json and the other settings.
    let mut args = vec![if remove { "uninstall" } else { "upgrade" }.to_owned()];
    if is_cask {
        args.push("--cask".into());
    }
    args.push(cask);
    let cmd = cx
        .cmd(&brew)
        .args(args)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .timeout(Duration::from_secs(15 * 60));
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

async fn winget(op: &Op<'_>, remove: bool) -> Result<ToolReport> {
    let cx = op.cx;
    let i = op
        .det
        .installs
        .first()
        .ok_or_else(|| Error::invalid("not installed"))?;
    let winget = cx
        .which("winget")
        .ok_or_else(|| Error::invalid("winget is not on PATH"))?;
    let mut args = vec![
        if remove { "uninstall" } else { "upgrade" },
        "--id",
        "Anthropic.ClaudeCode",
        "-e",
        "--source",
        "winget",
        "--disable-interactivity",
    ];
    if !remove {
        args.extend(["--accept-source-agreements", "--accept-package-agreements"]);
    }
    let cmd = cx
        .cmd(&winget)
        .args(args)
        .timeout(Duration::from_secs(15 * 60));
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

/// Back up `file` into `<data>/tools/backups` before Superba changes it
/// (the ten newest copies are kept).
pub fn backup(cx: &Ctx, file: &Path, label: &str) -> Result<Option<PathBuf>> {
    let Ok(bytes) = std::fs::read(file) else {
        return Ok(None);
    };
    let dir = crate::state::tools_dir(&cx.data).join("backups");
    let path = dir.join(format!("{label}.{}.bak", Timestamp::now().as_millis()));
    fsx::write_atomic(&path, &bytes)?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&format!("{label}.")))
                })
                .collect()
        })
        .unwrap_or_default();
    old.sort();
    while old.len() > 10 {
        let _ = std::fs::remove_file(old.remove(0));
    }
    Ok(Some(path))
}

/// Write `obj` to `path` keeping its permissions (a new file: owner only,
/// as it may hold keys).
pub fn write_settings(path: &Path, obj: &serde_json::Map<String, Value>) -> Result<()> {
    let existing = std::fs::metadata(path).ok().map(|m| m.permissions());
    fsx::write_atomic(path, &jsonedit::print_object(obj))?;
    if let Some(perm) = existing {
        let _ = std::fs::set_permissions(path, perm);
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

/// "Superba manages updates": `env.DISABLE_AUTOUPDATER = "1"` on; off puts
/// back what was there (or removes the key).
pub fn manage_updates(cx: &Ctx, on: bool) -> Result<()> {
    let p = paths(cx);
    let bytes = std::fs::read(&p.settings).ok();
    let mut obj = jsonedit::parse_object(bytes.as_deref(), &p.settings.display().to_string())?;
    let st = State::load(&cx.data).tool(superba_core::ToolId::ClaudeCode);
    if on {
        let previous = jsonedit::get_path(&obj, MANAGED_KEY).map(Value::to_string);
        jsonedit::set_path(&mut obj, MANAGED_KEY, Value::String("1".into()))?;
        backup(cx, &p.settings, "claude-settings.json")?;
        write_settings(&p.settings, &obj)?;
        let record = match st.managed_updates {
            Some(m) => m,
            None => ManagedUpdates {
                file: p.settings.clone(),
                key: MANAGED_KEY.into(),
                previous,
                created: bytes.is_none(),
            },
        };
        State::update(&cx.data, superba_core::ToolId::ClaudeCode, |s| {
            s.managed_updates = Some(record);
        })
    } else {
        let created = st.managed_updates.as_ref().is_some_and(|m| m.created);
        let previous = st.managed_updates.and_then(|m| m.previous);
        match previous.as_deref().map(serde_json::from_str::<Value>) {
            Some(Ok(v)) if !matches!(&v, Value::String(s) if truthy(s)) => {
                jsonedit::set_path(&mut obj, MANAGED_KEY, v)?;
            }
            _ => {
                jsonedit::remove_path(&mut obj, MANAGED_KEY, true);
            }
        }
        if bytes.is_some() {
            backup(cx, &p.settings, "claude-settings.json")?;
            if created && obj.is_empty() {
                fsx::remove_file(&p.settings)?;
            } else {
                write_settings(&p.settings, &obj)?;
            }
        }
        State::update(&cx.data, superba_core::ToolId::ClaudeCode, |s| {
            s.managed_updates = None;
        })
    }
}
