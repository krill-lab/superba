//! Shared pieces of the tool modules: telling installs apart by where their
//! program lives, npm prefixes, Node's version, launchers and `PATH` entries,
//! text helpers.

use std::path::{Path, PathBuf};

use superba_core::{Error, LocalizedText, Notice, Result, ToolInstall, ToolInstallKind};

use crate::platform::Os;
use crate::version::Version;
use crate::{Ctx, Op, fsx, pathenv};

/// `p` with links resolved (unchanged when it cannot be).
pub fn canonical(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // Windows' verbatim prefix only gets in the way of comparisons.
    let s = c.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => c,
    }
}

/// `p` as `/`-separated text (lower case on Windows) for matching.
pub fn slashed(p: &Path, os: Os) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if os == Os::Windows {
        s.to_ascii_lowercase()
    } else {
        s
    }
}

/// An npm package's folder from a program inside it
/// (`<prefix>/lib/node_modules/@scope/name/...`, or a Windows npm shim
/// `<prefix>\name.cmd` next to `<prefix>\node_modules`): the package folder
/// and the npm prefix it belongs to.
pub fn npm_package_of(program: &Path, package: &str, os: Os) -> Option<(PathBuf, PathBuf)> {
    let canon = canonical(program);
    let raw = canon.to_string_lossy().into_owned();
    // Same byte positions as `raw` (ASCII lower-casing, `\` → `/`).
    let text = slashed(&canon, os);
    let pkg = if os == Os::Windows {
        package.to_ascii_lowercase()
    } else {
        package.to_owned()
    };
    let needle = format!("/node_modules/{pkg}/");
    if let Some(at) = text.find(&needle) {
        let pkg_dir = PathBuf::from(&raw[..at + needle.len() - 1]);
        let modules = PathBuf::from(&raw[..at + "/node_modules".len()]);
        let parent = modules.parent()?;
        let prefix = if parent.file_name().is_some_and(|n| n == "lib") {
            parent.parent()?
        } else {
            parent
        };
        return Some((pkg_dir, prefix.to_path_buf()));
    }
    // A Windows npm shim (`claude.cmd`) beside the prefix's node_modules.
    let dir = program.parent()?;
    let pkg_dir = package
        .split('/')
        .fold(dir.join("node_modules"), |p, part| p.join(part));
    pkg_dir
        .join("package.json")
        .is_file()
        .then(|| (pkg_dir, dir.to_path_buf()))
}

/// `version` of the `package.json` in `dir`.
pub fn package_version(dir: &Path) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("package.json")).ok()?).ok()?;
    v.get("version")?.as_str().map(str::to_owned)
}

/// `<prefix>/lib/node_modules/<package>` (Unix) or
/// `<prefix>/node_modules/<package>` (Windows).
pub fn npm_package_dir(prefix: &Path, package: &str, os: Os) -> PathBuf {
    let base = if os == Os::Windows {
        prefix.join("node_modules")
    } else {
        prefix.join("lib").join("node_modules")
    };
    package.split('/').fold(base, |p, part| p.join(part))
}

/// The program `bin` of a global npm install in `prefix`
/// (`<prefix>/bin/<bin>` on Unix, `<prefix>\<bin>.cmd` on Windows).
pub fn npm_bin(prefix: &Path, bin: &str, os: Os) -> PathBuf {
    if os == Os::Windows {
        prefix.join(format!("{bin}.cmd"))
    } else {
        prefix.join("bin").join(bin)
    }
}

/// Homebrew's install of a program (`.../Cellar/<formula>/<v>/...` or
/// `.../Caskroom/<cask>/<v>/...`): the formula or cask name and version.
pub fn homebrew_of(program: &Path) -> Option<(String, bool, String)> {
    let canon = canonical(program);
    let text = canon.to_string_lossy().replace('\\', "/");
    for (marker, cask) in [("/Caskroom/", true), ("/Cellar/", false)] {
        if let Some(at) = text.find(marker) {
            let mut parts = text[at + marker.len()..].split('/');
            let name = parts.next()?.to_owned();
            let version = parts.next()?.to_owned();
            return Some((name, cask, version));
        }
    }
    None
}

/// Where npm installs global packages for this user: `NPM_CONFIG_PREFIX`,
/// `prefix=` in `~/.npmrc`, else next to the `node` on `PATH`
/// (`<prefix>/bin/node`; Windows `%APPDATA%\npm`).
pub fn npm_global_prefixes(cx: &Ctx) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["NPM_CONFIG_PREFIX", "npm_config_prefix"] {
        if let Some(p) = cx.env.var_non_empty(var) {
            out.push(PathBuf::from(p));
        }
    }
    if let Ok(rc) = std::fs::read_to_string(cx.home.join(".npmrc")) {
        for line in rc.lines() {
            if let Some((k, v)) = line.split_once('=')
                && k.trim() == "prefix"
            {
                let v = v.trim();
                let v = v
                    .strip_prefix("~/")
                    .map_or_else(|| PathBuf::from(v), |r| cx.home.join(r));
                out.push(v);
            }
        }
    }
    if cx.platform.os == Os::Windows {
        if let Some(app) = cx.env.var_non_empty("APPDATA") {
            out.push(PathBuf::from(app).join("npm"));
        }
    } else if let Some(node) = cx.which("node") {
        let node = canonical(&node);
        if let Some(prefix) = node.parent().and_then(Path::parent) {
            out.push(prefix.to_path_buf());
        }
    }
    out.dedup();
    out
}

/// A folder this user can create files in.
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".superba-write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// The version of the `node` on `PATH`.
pub async fn node_version(cx: &Ctx) -> Option<Version> {
    let node = cx.which("node")?;
    Version::parse(&crate::probe_version(cx, &node, &["--version"]).await?)
}

/// Node `v` satisfies `min` (major, minor): `>= min`, also any newer major.
pub fn node_ok(v: &Version, min: (u64, u64)) -> bool {
    let (maj, min_minor) = min;
    let major = v.nums[0];
    let minor = v.nums.get(1).copied().unwrap_or(0);
    major > maj || (major == maj && minor >= min_minor)
}

/// An install record from detection.
pub fn install(
    path: PathBuf,
    kind: ToolInstallKind,
    version: Option<String>,
    by_superba: bool,
    on_path: bool,
) -> ToolInstall {
    ToolInstall {
        version,
        path,
        kind,
        by_superba,
        on_path,
    }
}

/// `a` and `b` are the same file (after resolving links).
pub fn same_file(a: &Path, b: &Path, os: Os) -> bool {
    slashed(&canonical(a), os) == slashed(&canonical(b), os)
}

/// Make the command `name` (running `target`) reachable: Unix — a launcher
/// script in `~/.local/bin` (refused when another program has the name);
/// Windows — `dir` added to the user `PATH`. Recorded so a removal takes it
/// back. Returns notes for the report.
pub fn expose(op: &Op<'_>, name: &str, target: &Path, dir: &Path) -> Result<Vec<Notice>> {
    let cx = op.cx;
    let mut notes = Vec::new();
    if cx.platform.os == Os::Windows {
        add_user_path(op, dir, &mut notes)?;
        return Ok(notes);
    }
    let launcher = cx.local_bin().join(name);
    fsx::write_launcher(&launcher, target)?;
    op.log.line(format!(
        "launcher: {} -> {}",
        launcher.display(),
        target.display()
    ));
    op.record(|s| {
        if !s.launchers.contains(&launcher) {
            s.launchers.push(launcher.clone());
        }
    })?;
    if let Some(h) = cx.path_hint(&cx.local_bin()) {
        notes.push(path_note(&h));
    }
    Ok(notes)
}

/// Windows: add `dir` to the user `PATH` (recorded); elsewhere a no-op.
pub fn add_user_path(op: &Op<'_>, dir: &Path, notes: &mut Vec<Notice>) -> Result<()> {
    #[cfg(windows)]
    {
        let text = dir.display().to_string();
        if crate::pathenv::user::add(&text)? {
            op.log.line(format!("user PATH: added {text}"));
            op.record(|s| {
                if !s.path_entries.contains(&text) {
                    s.path_entries.push(text.clone());
                }
            })?;
            notes.push(Notice::info(
                LocalizedText::new(
                    "tools.note.path_added",
                    "{dir} was added to your PATH; terminals opened from now on find the program.",
                )
                .arg("dir", superba_core::paths::display_path(dir)),
            ));
        }
    }
    #[cfg(not(windows))]
    let _ = (op, dir, notes);
    Ok(())
}

/// Take back the launchers and `PATH` entries Superba recorded for the tool
/// (a launcher only while it is still Superba's).
pub fn unexpose(op: &Op<'_>) -> Result<()> {
    let st = crate::state::State::load(&op.cx.data).tool(op.tool());
    for l in &st.launchers {
        if fsx::is_own_launcher(l) && fsx::remove_file(l)? {
            op.log.line(format!("removed launcher {}", l.display()));
        }
    }
    #[cfg(windows)]
    for e in &st.path_entries {
        if crate::pathenv::user::remove(e)? {
            op.log.line(format!("user PATH: removed {e}"));
        }
    }
    op.record(|s| {
        s.launchers.clear();
        s.path_entries.clear();
    })
}

/// The note for a `PATH` hint.
pub fn path_note(h: &superba_core::PathHint) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "tools.note.not_on_path",
            "{dir} is not on your PATH: add `{line}` to {file}, or run the program by its full path.",
        )
        .arg("dir", superba_core::paths::display_path(&h.dir))
        .arg("line", &h.line)
        .arg("file", &h.file),
    )
}

/// Whether `dir` is on `PATH` now (Windows: also the user `PATH` in the
/// registry, which new terminals get).
pub fn dir_on_path(cx: &Ctx, dir: &Path) -> bool {
    if pathenv::contains(&cx.path_dirs(), dir, cx.platform.os == Os::Windows) {
        return true;
    }
    #[cfg(windows)]
    {
        crate::pathenv::user::has(&dir.display().to_string())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Move `from` to `to` (rename, or copy then delete across file systems).
pub fn move_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| Error::io(d, &e))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| Error::io(to, &e))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

/// Put the folder `staged` in place of `target` (the previous one is kept
/// as `<target>.old` until the swap worked). Windows refuses while a program
/// inside runs: the error says to close it.
pub fn swap_dir(staged: &Path, target: &Path, what: &str) -> Result<()> {
    let old = fsx::sibling(target, ".old");
    let _ = fsx::remove_dir(&old);
    if target.exists() {
        std::fs::rename(target, &old).map_err(|e| Error::Conflict {
            message: format!("{} is in use (close {what} first): {e}", target.display()),
        })?;
    }
    if let Err(e) = std::fs::rename(staged, target) {
        if old.exists() {
            let _ = std::fs::rename(&old, target);
        }
        return Err(Error::io(target, &e));
    }
    let _ = fsx::remove_dir(&old);
    Ok(())
}

/// A caution / note text.
pub fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

/// A downloads folder for the tool under Superba's data dir.
pub fn downloads(cx: &Ctx, tool: superba_core::ToolId) -> PathBuf {
    cx.tool_dir(tool).join("downloads")
}
