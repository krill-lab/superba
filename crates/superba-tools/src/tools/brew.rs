//! Homebrew on macOS (user bug 110): the default install method of every
//! tool that has a formula or cask, and the update / removal of a tool
//! Homebrew installed. Names checked on formulae.brew.sh (2026-10-21):
//!
//! | tool         | package                 |
//! |--------------|-------------------------|
//! | Claude Code  | cask `claude-code`      |
//! | opencode     | formula `opencode`      |
//! | pi           | formula `pi-coding-agent` |
//! | ZCode        | cask `zcode`            |
//! | WorkBuddy    | cask `workbuddy-cn`     |
//! | WorkBuddy AI | cask `workbuddy-ai`     |
//! | dsh          | none: the `dsh` formula is Dancer's shell, the `deepseek-harness` cask is the Desktop app |
//!
//! brew runs without questions (`NONINTERACTIVE=1`); installs and removals
//! skip its auto-update (`HOMEBREW_NO_AUTO_UPDATE=1`, the API is fresh
//! enough) and run `brew update` once when brew does not know the package
//! yet; an upgrade lets brew update itself so it sees the new version.
//! Another brew holding its lock is waited for (three tries).

use std::path::{Path, PathBuf};
use std::time::Duration;

use superba_core::{Error, Result, ToolId, ToolOp, ToolReport, ToolStage};

use super::common;
use crate::{Ctx, Op, run};

/// A formula or cask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Package {
    /// Its name.
    pub name: &'static str,
    /// A cask (`--cask`).
    pub cask: bool,
}

/// The Homebrew package of `tool`.
pub fn package(tool: ToolId) -> Option<Package> {
    let (name, cask) = match tool {
        ToolId::ClaudeCode => ("claude-code", true),
        ToolId::Opencode => ("opencode", false),
        ToolId::Pi => ("pi-coding-agent", false),
        ToolId::Zcode => ("zcode", true),
        ToolId::Workbuddy => ("workbuddy-cn", true),
        ToolId::WorkbuddyAi => ("workbuddy-ai", true),
        _ => return None,
    };
    Some(Package { name, cask })
}

/// `brew`, when it is installed.
pub fn brew(cx: &Ctx) -> Option<PathBuf> {
    cx.which("brew")
}

/// Homebrew's prefix (the folder above `bin/brew`).
pub fn prefix(brew: &Path) -> Option<PathBuf> {
    let c = common::canonical(brew);
    let bin = c.parent()?;
    // `/opt/homebrew/bin/brew` → `/opt/homebrew`; Intel links
    // `/usr/local/bin/brew` → `/usr/local/Homebrew/bin/brew`.
    let p = bin.parent()?;
    if p.file_name().is_some_and(|n| n == "Homebrew")
        && let Some(up) = p.parent()
        && (up.join("Caskroom").is_dir() || up.join("Cellar").is_dir())
    {
        return Some(up.to_path_buf());
    }
    Some(p.to_path_buf())
}

/// `pkg` is installed in this Homebrew (its `Cellar` / `Caskroom` folder).
pub fn installed(brew: &Path, pkg: Package) -> bool {
    prefix(brew).is_some_and(|p| {
        p.join(if pkg.cask { "Caskroom" } else { "Cellar" })
            .join(pkg.name)
            .is_dir()
    })
}

fn args(verb: &str, pkg: Package) -> Vec<String> {
    let mut a = vec![verb.to_owned()];
    if pkg.cask {
        a.push("--cask".into());
    }
    a.push(pkg.name.into());
    a
}

fn locked(out: &run::Outcome) -> bool {
    let t = format!("{}\n{}", out.stdout, out.stderr).to_ascii_lowercase();
    t.contains("has already locked") || t.contains("another active homebrew")
}

fn unknown(out: &run::Outcome) -> bool {
    let t = format!("{}\n{}", out.stdout, out.stderr).to_ascii_lowercase();
    t.contains("no available formula") || t.contains("no cask with this name")
        || t.contains("no available cask")
}

/// Run `brew <verb> [--cask] <name>`, waiting for a lock and updating brew
/// once when it does not know the package.
async fn brew_run(op: &Op<'_>, brew: &Path, verb: &str, pkg: Package, stage: ToolStage) -> Result<()> {
    let cx = op.cx;
    let mut cmd = cx
        .cmd(brew)
        .args(args(verb, pkg))
        .env("NONINTERACTIVE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .env("HOMEBREW_NO_COLOR", "1")
        .timeout(Duration::from_secs(30 * 60));
    if verb != "upgrade" {
        cmd = cmd.env("HOMEBREW_NO_AUTO_UPDATE", "1");
    }
    let mut updated = false;
    for attempt in 0..4 {
        let out = op.run(&cmd, stage).await?;
        if out.success() {
            return Ok(());
        }
        if locked(&out) && attempt < 3 {
            op.log.line("brew is busy (another brew holds its lock); waiting");
            tokio::time::sleep(Duration::from_secs(if cfg!(test) { 0 } else { 10 })).await;
            continue;
        }
        if unknown(&out) && !updated {
            updated = true;
            let up = cx
                .cmd(brew)
                .args(["update"])
                .env("NONINTERACTIVE", "1")
                .timeout(Duration::from_secs(10 * 60));
            op.run(&up, ToolStage::Resolving).await?;
            continue;
        }
        return Err(Error::Process {
            message: format!(
                "{} ended with {}: {}",
                cmd.display(),
                out.code.map_or("a signal".into(), |c| format!("exit code {c}")),
                out.tail(6)
            ),
        });
    }
    Err(Error::Process {
        message: "brew stayed locked by another brew; try again when it is done".into(),
    })
}

/// Install `pkg` with Homebrew, upgrade or remove the Homebrew install.
pub async fn run(op: &Op<'_>, pkg: Package) -> Result<ToolReport> {
    let cx = op.cx;
    let brew = brew(cx).ok_or_else(|| Error::invalid("Homebrew (brew) is not installed"))?;
    let (verb, stage) = match op.req.op {
        ToolOp::Install => ("install", ToolStage::Installing),
        ToolOp::Update => ("upgrade", ToolStage::Installing),
        ToolOp::Uninstall => ("uninstall", ToolStage::Removing),
        _ => return Err(Error::not_implemented("this Homebrew operation")),
    };
    if op.req.op == ToolOp::Install
        && let Some(v) = &op.req.version
    {
        op.log
            .line(format!("Homebrew installs its current version (asked for {v})"));
    }
    // Never `--zap`: it trashes the tool's settings.
    brew_run(op, &brew, verb, pkg, stage).await?;
    crate::forget_versions();
    crate::locate::forget();
    if op.req.op == ToolOp::Uninstall {
        return Ok(op.report(None, None, Vec::new()));
    }
    let det = super::detect(cx, &op.desc, &crate::state::ToolState::default()).await;
    let found = det.installs.first();
    Ok(op.report(
        found.and_then(|i| i.version.clone()),
        found.map(|i| i.path.clone()),
        Vec::new(),
    ))
}
