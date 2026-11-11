//! The `superba://` URL handler on Linux (feature 140).
//!
//! Windows registers the scheme in the MSI and macOS in `Info.plist`; a
//! Linux tarball has no installer, so Superba registers itself whenever its
//! window starts: a hidden desktop entry
//! `$XDG_DATA_HOME/applications/superba-url-handler.desktop` (default
//! `~/.local/share/applications`) whose `Exec` is this installation's
//! `superba "%u"` and whose `MimeType` is `x-scheme-handler/superba`, then
//! `update-desktop-database` on that directory and, when no other program is
//! the default handler of the scheme, `xdg-mime default`. A default the user
//! (or the packaged `superba.desktop`) set is left alone. Everything is per
//! user; nothing here needs root, and a missing tool only skips its step.

use std::path::{Path, PathBuf};

use superba_core::{Error, LocalContext, Result};

use crate::fsx;

/// The desktop entry Superba writes.
pub const DESKTOP_FILE: &str = "superba-url-handler.desktop";
/// The MIME type of the scheme.
pub const MIME_TYPE: &str = "x-scheme-handler/superba";

/// The per-user applications directory: `$XDG_DATA_HOME/applications` when
/// that is absolute, else `~/.local/share/applications`.
pub fn applications_dir(ctx: &LocalContext) -> PathBuf {
    ctx.env
        .var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| ctx.home_dir.join(".local").join("share"))
        .join("applications")
}

/// The desktop entry for `executable`.
pub fn render(executable: &Path) -> Result<String> {
    let text = executable
        .to_str()
        .ok_or_else(|| Error::invalid("the Superba executable path is not UTF-8"))?;
    if text.chars().any(char::is_control) {
        return Err(Error::invalid(
            "the Superba executable path contains a control character",
        ));
    }
    // A plain path goes in as it is: the generic `xdg-open` of xdg-utils
    // (no desktop environment) takes the first word of `Exec` literally and
    // does not understand quotes. Paths that need quoting are quoted per the
    // desktop entry spec.
    let plain = !text.is_empty()
        && text.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b',' | b':' | b'@')
        });
    let exec = if plain {
        text.to_owned()
    } else {
        crate::autostart::desktop_exec(text)?
    };
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Superba\nComment=Opens superba:// import links from the Krill website\nExec={exec} %u\nIcon=superba\nTerminal=false\nNoDisplay=true\nMimeType={MIME_TYPE};\n"
    ))
}

/// The programs registration runs (`None`: not installed, step skipped).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    /// `update-desktop-database`.
    pub update_desktop_database: Option<PathBuf>,
    /// `xdg-mime`.
    pub xdg_mime: Option<PathBuf>,
}

impl Tools {
    /// The tools on `PATH` of `ctx`'s environment.
    pub fn on_path(ctx: &LocalContext) -> Self {
        let path = ctx.env.var_os("PATH").unwrap_or_default();
        let find = |name: &str| {
            std::env::split_paths(&path)
                .filter(|d| d.is_absolute())
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        };
        Self {
            update_desktop_database: find("update-desktop-database"),
            xdg_mime: find("xdg-mime"),
        }
    }
}

/// What [`register`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registration {
    /// The desktop entry.
    pub entry: PathBuf,
    /// It was written (new or changed).
    pub written: bool,
    /// `update-desktop-database` ran successfully.
    pub database_updated: bool,
    /// The scheme's default handler before (`xdg-mime query default`).
    pub previous_default: Option<String>,
    /// Superba's entry was made the default.
    pub default_set: bool,
}

/// Register Superba's handler for `executable` (see the module docs).
pub fn register_with(ctx: &LocalContext, executable: &Path, tools: &Tools) -> Result<Registration> {
    let dir = applications_dir(ctx);
    let entry = dir.join(DESKTOP_FILE);
    let rendered = render(executable)?;
    let written = std::fs::read_to_string(&entry).ok().as_deref() != Some(rendered.as_str());
    if written {
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, &e))?;
        fsx::write_atomic(&entry, rendered.as_bytes(), fsx::Mode::PreserveOrPublic)?;
    }
    let mut out = Registration {
        entry,
        written,
        ..Registration::default()
    };
    // The cache of MimeType= lines (mimeinfo.cache) that handler lookups read.
    if let Some(tool) = &tools.update_desktop_database {
        out.database_updated =
            run(ctx, tool, &["-q".as_ref(), dir.as_os_str()]).is_some_and(|o| o.status.success());
    }
    if let Some(tool) = &tools.xdg_mime {
        let current = run(
            ctx,
            tool,
            &["query".as_ref(), "default".as_ref(), MIME_TYPE.as_ref()],
        )
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty());
        out.previous_default = current.clone();
        if current.is_none() {
            out.default_set = run(
                ctx,
                tool,
                &[
                    "default".as_ref(),
                    DESKTOP_FILE.as_ref(),
                    MIME_TYPE.as_ref(),
                ],
            )
            .is_some_and(|o| o.status.success());
        }
    }
    Ok(out)
}

/// [`register_with`] for this installation's GUI and the tools on `PATH`.
/// Linux only (a no-op elsewhere: the installers register the scheme).
pub fn register(ctx: &LocalContext) -> Result<Option<Registration>> {
    if !cfg!(target_os = "linux") {
        return Ok(None);
    }
    let current = std::env::current_exe().map_err(|e| Error::other(e.to_string()))?;
    let executable = crate::autostart::gui_executable(&current, ctx)?;
    if !executable.is_absolute() || !executable.is_file() {
        return Err(Error::invalid(format!(
            "Superba GUI executable not found: {}",
            executable.display()
        )));
    }
    register_with(ctx, &executable, &Tools::on_path(ctx)).map(Some)
}

/// Run `tool` with the context's home and XDG directories, quietly, with a
/// time limit (a desktop helper that hangs must not hold Superba up).
fn run(ctx: &LocalContext, tool: &Path, args: &[&std::ffi::OsStr]) -> Option<std::process::Output> {
    let mut command = superba_core::process::command(tool);
    command
        .args(args)
        .env("HOME", &ctx.home_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for var in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_DATA_DIRS"] {
        match ctx.env.var_os(var) {
            Some(v) => command.env(var, v),
            None => command.env_remove(var),
        };
    }
    if let Some(path) = ctx.env.var_os("PATH") {
        command.env("PATH", path);
    }
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> LocalContext {
        let mut ctx = LocalContext::for_tests(dir);
        ctx.env = std::sync::Arc::new(superba_core::MapEnv::new());
        ctx
    }

    #[test]
    fn renders_a_hidden_handler_entry_with_a_quoted_exec() {
        let entry = render(Path::new("/opt/My Apps/superba")).unwrap();
        assert!(
            entry.contains("\nExec=\"/opt/My Apps/superba\" %u\n"),
            "{entry}"
        );
        assert!(entry.contains("\nMimeType=x-scheme-handler/superba;\n"));
        assert!(entry.contains("\nNoDisplay=true\n"));
        assert!(render(Path::new("/opt/a=b/superba")).is_err());
        assert!(render(Path::new("/opt/a\nb/superba")).is_err());
        let percent = render(Path::new("/opt/100%/superba")).unwrap();
        assert!(
            percent.contains("Exec=\"/opt/100%%/superba\" %u"),
            "{percent}"
        );
    }

    #[test]
    fn applications_dir_follows_an_absolute_xdg_data_home() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path());
        assert_eq!(
            applications_dir(&c),
            c.home_dir.join(".local/share/applications")
        );
        c.env = std::sync::Arc::new(
            superba_core::MapEnv::new().with("XDG_DATA_HOME", tmp.path().join("data")),
        );
        assert_eq!(applications_dir(&c), tmp.path().join("data/applications"));
        c.env = std::sync::Arc::new(superba_core::MapEnv::new().with("XDG_DATA_HOME", "rel"));
        assert_eq!(
            applications_dir(&c),
            c.home_dir.join(".local/share/applications")
        );
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn registers_once_and_never_takes_over_another_default() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let log = tmp.path().join("log");
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let state = tmp.path().join("default");
        let tools = Tools {
            update_desktop_database: Some(script(
                &bin,
                "update-desktop-database",
                &format!("echo \"udb $*\" >> {}", log.display()),
            )),
            xdg_mime: Some(script(
                &bin,
                "xdg-mime",
                &format!(
                    "echo \"xdg-mime $* HOME=$HOME\" >> {log}\nif [ \"$1\" = query ]; then cat {state} 2>/dev/null; exit 0; fi\necho \"$2\" > {state}",
                    log = log.display(),
                    state = state.display()
                ),
            )),
        };
        let exe = tmp.path().join("superba");
        std::fs::write(&exe, "").unwrap();
        let first = register_with(&c, &exe, &tools).unwrap();
        assert!(first.written && first.database_updated && first.default_set);
        assert_eq!(first.previous_default, None);
        assert_eq!(
            std::fs::read_to_string(&first.entry).unwrap(),
            render(&exe).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(&state).unwrap().trim(),
            DESKTOP_FILE
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains(&format!("HOME={}", c.home_dir.display())),
            "{calls}"
        );
        // Again: nothing to write, the default is already Superba's.
        let again = register_with(&c, &exe, &tools).unwrap();
        assert!(!again.written && !again.default_set);
        assert_eq!(again.previous_default.as_deref(), Some(DESKTOP_FILE));
        // Another program is the default: left alone.
        std::fs::write(&state, "other.desktop\n").unwrap();
        let other = register_with(&c, &exe, &tools).unwrap();
        assert!(!other.default_set);
        assert_eq!(
            std::fs::read_to_string(&state).unwrap().trim(),
            "other.desktop"
        );
        // A moved installation rewrites the entry.
        let moved = tmp.path().join("moved-superba");
        std::fs::write(&moved, "").unwrap();
        assert!(register_with(&c, &moved, &tools).unwrap().written);
        // Without the tools only the entry is written.
        let bare = register_with(&c, &exe, &Tools::default()).unwrap();
        assert!(bare.written && !bare.database_updated && !bare.default_set);
    }
}
