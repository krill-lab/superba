//! "Open through Superba" launcher entry (plan 6.1).
//!
//! Linux: a `.desktop` file with the same name as the app's system entry in
//! `<XDG_DATA_HOME or ~/.local/share>/applications` shadows it; its `Exec` runs
//! `superba open-app`. The file carries `X-Superba-Managed=true` and is only ever
//! removed when that marker is present. macOS (Dock menu / login item) and Windows
//! (Start-menu shortcut) are not implemented yet and report `Unsupported`.

use std::path::{Path, PathBuf};

use superba_core::EnvSource;

/// Marker line in managed entries.
pub const MANAGED_MARKER: &str = "X-Superba-Managed=true";

/// Default entry name when no system entry is found (`CHROME_DESKTOP` of the app).
pub const DEFAULT_ENTRY_NAME: &str = "chatgpt.desktop";

/// `<XDG_DATA_HOME or home/.local/share>/applications`.
pub fn user_applications_dir(env: &dyn EnvSource, home: &Path) -> PathBuf {
    env.var_non_empty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("applications")
}

/// Quote one Exec argument per the desktop entry spec.
pub fn quote_exec_arg(a: &str) -> String {
    let needs = a.is_empty()
        || a.chars()
            .any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c));
    if !needs {
        return a.replace('%', "%%");
    }
    let mut s = String::from("\"");
    for c in a.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                s.push('\\');
                s.push(c);
            }
            '%' => s.push_str("%%"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Render the managed entry. `base` is the system entry to copy (Name, Icon, ...)
/// when available; `exec` is the argv to run.
pub fn render_entry(base: Option<&str>, exec: &[String]) -> String {
    let exec_line = format!(
        "Exec={}",
        exec.iter()
            .map(|a| quote_exec_arg(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut out = String::new();
    let mut in_main = false;
    let mut wrote_exec = false;
    if let Some(base) = base {
        for line in base.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                if in_main && !wrote_exec {
                    out.push_str(&exec_line);
                    out.push('\n');
                    wrote_exec = true;
                }
                in_main = t == "[Desktop Entry]";
                out.push_str(line);
                out.push('\n');
                if in_main {
                    out.push_str(MANAGED_MARKER);
                    out.push('\n');
                }
                continue;
            }
            if in_main {
                if t.starts_with("Exec=") {
                    out.push_str(&exec_line);
                    out.push('\n');
                    wrote_exec = true;
                    continue;
                }
                if t.starts_with("TryExec=")
                    || t.starts_with("DBusActivatable=")
                    || t == MANAGED_MARKER
                {
                    continue;
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        if in_main && !wrote_exec {
            out.push_str(&exec_line);
            out.push('\n');
            wrote_exec = true;
        }
    }
    if !wrote_exec {
        out = format!(
            "[Desktop Entry]\n{MANAGED_MARKER}\nType=Application\nName=ChatGPT\n{exec_line}\nTerminal=false\nCategories=Development;\n"
        );
    }
    out
}

/// Find the system entry that launches `executable` among `dirs` (name + content).
pub fn find_system_entry(dirs: &[PathBuf], executable: Option<&Path>) -> Option<(String, String)> {
    let exe_dir = executable.and_then(Path::parent);
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        names.sort();
        for path in names {
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text.contains(MANAGED_MARKER) {
                continue;
            }
            let Some(exec) = crate::install::parse_desktop_exec(&text) else {
                continue;
            };
            let Some(prog) = exec.first().map(PathBuf::from) else {
                continue;
            };
            let prog_name = prog.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let matches_dir = exe_dir.is_some_and(|d| prog.parent() == Some(d));
            let matches_name =
                ["ChatGPT", "chatgpt", "codex-launcher", "Codex"].contains(&prog_name);
            if matches_dir || (executable.is_none() && matches_name) {
                let name = path.file_name()?.to_string_lossy().into_owned();
                return Some((name, text));
            }
        }
    }
    None
}

/// System application directories searched for the app's own entry.
pub fn system_entry_dirs() -> Vec<PathBuf> {
    [
        "/usr/share/applications",
        "/usr/local/share/applications",
        "/var/lib/flatpak/exports/share/applications",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

/// The argv the managed entry runs: `<superba> open-app`, preferring a `superba`
/// binary next to the running executable (the GUI), else the running executable.
pub fn superba_exec() -> Vec<String> {
    let current = std::env::current_exe().ok();
    let gui = current.as_ref().and_then(|c| {
        let name = if cfg!(windows) {
            "superba.exe"
        } else {
            "superba"
        };
        let p = c.with_file_name(name);
        p.exists().then_some(p)
    });
    let program = gui
        .or(current)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "superba".to_owned());
    vec![program, "open-app".to_owned()]
}

/// True if the file at `path` is a Superba-managed entry.
pub fn is_managed(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.lines().any(|l| l.trim() == MANAGED_MARKER))
}
