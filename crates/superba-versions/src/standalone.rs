//! The local official standalone layout (same protocol as the remote install
//! script in `superba-remote`):
//!
//! ```text
//! <codex_home>/packages/standalone/releases/<ver>-<target>/   a store entry, hard-linked or copied
//! <codex_home>/packages/standalone/current -> releases/<ver>-<target>
//! <bin_dir>/codex -> <root>/current/bin/codex
//! <root>/.superba-previous      release switched away from (rollback target)
//! <root>/.superba-installed     releases Superba installed (only these are pruned)
//! <root>/.superba-original*     what `<bin_dir>/codex` was before the first install
//! ```
//!
//! Links are replaced atomically (a new link renamed over the old one; `rename`
//! never follows a link to a directory). Unix only: Windows has no user-level
//! symbolic links by default and the standalone layout is not used there.

use std::fs;
use std::path::{Path, PathBuf};

use superba_core::{CodexVersion, Error, Result, StandaloneLayout};

use crate::store::nonce;

const PREVIOUS: &str = ".superba-previous";
const INSTALLED: &str = ".superba-installed";
const ORIGINAL: &str = ".superba-original";
const ORIGINAL_LINK: &str = ".superba-original-link";
const ORIGINAL_CODEX: &str = ".superba-original-codex";

/// Result of a local install or rollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// `<bin_dir>/codex`.
    pub visible: PathBuf,
    /// Version of the command before (when it ran).
    pub previous: Option<CodexVersion>,
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::io(path, &e)
}

/// `--version` of an executable (5 s, `None` when it does not run).
pub fn version_of(exe: &Path) -> Option<CodexVersion> {
    if !exe.exists() {
        return None;
    }
    let spawn = || {
        std::process::Command::new(exe)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    // A file this process just wrote can be "busy" (ETXTBSY) for a moment while
    // another thread's fork still holds the write descriptor: try again briefly.
    let mut child = None;
    for _ in 0..10 {
        match spawn() {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) if text_file_busy(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let mut child = child?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    CodexVersion::parse(&String::from_utf8_lossy(&out.stdout)).ok()
}

/// `ETXTBSY`: the executable is still open for writing somewhere.
fn text_file_busy(e: &std::io::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        e.raw_os_error() == Some(26)
    }
    #[cfg(target_os = "macos")]
    {
        e.raw_os_error() == Some(26)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = e;
        false
    }
}

/// Replace `link` with a symbolic link to `target` atomically.
#[cfg(unix)]
fn swap_link(link: &Path, target: &Path) -> Result<()> {
    let dir = link
        .parent()
        .ok_or_else(|| Error::invalid("link without a parent"))?;
    fs::create_dir_all(dir).map_err(io(dir))?;
    let tmp = dir.join(format!(".superba-link-{}", nonce()));
    std::os::unix::fs::symlink(target, &tmp).map_err(io(&tmp))?;
    fs::rename(&tmp, link).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::io(link, &e)
    })
}

#[cfg(not(unix))]
fn swap_link(link: &Path, _target: &Path) -> Result<()> {
    Err(Error::Unsupported {
        what: format!("the standalone Codex layout here ({})", link.display()),
    })
}

/// Hard-link (or, across file systems, copy) the tree `from` into the new
/// directory `to`, keeping modes and symbolic links.
fn link_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(io(to))?;
    for e in fs::read_dir(from).map_err(io(from))? {
        let e = e.map_err(io(from))?;
        let src = e.path();
        let dst = to.join(e.file_name());
        let meta = src.symlink_metadata().map_err(io(&src))?;
        if meta.is_dir() {
            link_tree(&src, &dst)?;
        } else if meta.file_type().is_symlink() {
            #[cfg(unix)]
            {
                let t = fs::read_link(&src).map_err(io(&src))?;
                std::os::unix::fs::symlink(t, &dst).map_err(io(&dst))?;
            }
        } else if fs::hard_link(&src, &dst).is_err() {
            fs::copy(&src, &dst).map_err(io(&dst))?;
        }
    }
    Ok(())
}

fn read_first_line(p: &Path) -> Option<String> {
    fs::read_to_string(p)
        .ok()?
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains('/') && *l != "." && *l != "..")
        .map(str::to_owned)
}

fn current_name(root: &Path) -> Option<String> {
    fs::read_link(root.join("current"))
        .ok()?
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Install the store entry `entry` (`<ver>-<target>` directory) as `name` into
/// `layout`.
pub fn install(entry: &Path, name: &str, layout: &StandaloneLayout) -> Result<Outcome> {
    if cfg!(not(unix)) {
        return Err(Error::Unsupported {
            what: "installing the standalone Codex layout on this platform".into(),
        });
    }
    let root = layout.root();
    let rel = root.join("releases");
    let visible = layout.command();
    fs::create_dir_all(&rel).map_err(io(&rel))?;
    let previous = version_of(&visible);
    // What the command was before Superba's first install.
    if !root.join(ORIGINAL).exists() {
        match fs::read_link(&visible) {
            Ok(t) if !t.starts_with(&root) => {
                fs::write(root.join(ORIGINAL_LINK), t.to_string_lossy().as_bytes())
                    .map_err(io(&root))?;
            }
            Ok(_) => {}
            Err(_) if visible.is_file() => {
                fs::copy(&visible, root.join(ORIGINAL_CODEX)).map_err(io(&root))?;
            }
            Err(_) => {}
        }
        fs::write(root.join(ORIGINAL), b"").map_err(io(&root))?;
    }
    let prev_name = current_name(&root);
    let dest = rel.join(name);
    let tmp = rel.join(format!(".superba-install-{}", nonce()));
    if let Err(e) = link_tree(entry, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let old = rel.join(format!(".superba-old-{}", nonce()));
    if dest.exists() {
        fs::rename(&dest, &old).map_err(io(&dest))?;
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
        if old.exists() {
            let _ = fs::rename(&old, &dest);
        }
        let _ = fs::remove_dir_all(&tmp);
        return Err(Error::io(&dest, &e));
    }
    let _ = fs::remove_dir_all(&old);
    swap_link(&root.join("current"), &dest)?;
    let _ = fs::remove_file(root.join("auto-update-version"));
    swap_link(
        &visible,
        &root.join("current").join("bin").join(command_name()),
    )?;
    if cfg!(target_os = "macos") && dest.join("bin/codex-code-mode-host").is_file() {
        swap_link(
            &layout.bin_dir.join("codex-code-mode-host"),
            &root.join("current/bin/codex-code-mode-host"),
        )?;
    }
    if let Some(p) = prev_name.as_deref().filter(|p| *p != name) {
        fs::write(root.join(PREVIOUS), format!("{p}\n")).map_err(io(&root))?;
    }
    let keep = read_first_line(&root.join(PREVIOUS));
    let installed: Vec<String> = fs::read_to_string(root.join(INSTALLED))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains('/') && *l != "." && *l != "..")
        .map(str::to_owned)
        .collect();
    for n in &installed {
        if n != name && Some(n) != keep.as_ref() {
            let _ = fs::remove_dir_all(rel.join(n));
        }
    }
    let mut list = vec![name.to_owned()];
    if let Some(k) = keep.filter(|k| installed.contains(k)) {
        list.push(k);
    }
    fs::write(root.join(INSTALLED), list.join("\n") + "\n").map_err(io(&root))?;
    Ok(Outcome { visible, previous })
}

/// Switch back to the release kept by the last install, or restore what the
/// command was before the first one.
pub fn rollback(layout: &StandaloneLayout) -> Result<Outcome> {
    if cfg!(not(unix)) {
        return Err(Error::Unsupported {
            what: "the standalone Codex layout on this platform".into(),
        });
    }
    let root = layout.root();
    let visible = layout.command();
    let previous = version_of(&visible);
    let prev = read_first_line(&root.join(PREVIOUS)).filter(|p| {
        root.join("releases")
            .join(p)
            .join("bin")
            .join(command_name())
            .is_file()
    });
    if let Some(p) = prev {
        let cur = current_name(&root);
        swap_link(&root.join("current"), &root.join("releases").join(&p))?;
        swap_link(
            &visible,
            &root.join("current").join("bin").join(command_name()),
        )?;
        if let Some(c) = cur {
            fs::write(root.join(PREVIOUS), format!("{c}\n")).map_err(io(&root))?;
        }
    } else if let Ok(t) = fs::read_to_string(root.join(ORIGINAL_LINK)) {
        swap_link(&visible, Path::new(t.trim()))?;
    } else if root.join(ORIGINAL_CODEX).is_file() {
        let tmp = layout.bin_dir.join(format!(".superba-codex-{}", nonce()));
        fs::copy(root.join(ORIGINAL_CODEX), &tmp).map_err(io(&tmp))?;
        fs::rename(&tmp, &visible).map_err(io(&visible))?;
    } else {
        return Err(Error::NotFound {
            what: "an earlier Codex release or the original codex command to return to".into(),
        });
    }
    Ok(Outcome { visible, previous })
}

fn command_name() -> &'static str {
    if cfg!(windows) { "codex.exe" } else { "codex" }
}
