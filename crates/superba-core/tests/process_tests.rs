//! Every child process of Superba is created through `superba_core::process`, so
//! none can forget `CREATE_NO_WINDOW` (the GUI is a windows-subsystem program and a
//! child without it opens a console window of its own).

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The lines of a source file that are not inside a `#[cfg(test)]` item.
fn non_test_lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut skip_depth: Option<i32> = None;
    let mut pending_skip = false;
    for (index, line) in text.lines().enumerate() {
        if let Some(depth) = skip_depth.as_mut() {
            *depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if *depth <= 0 {
                skip_depth = None;
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("#[cfg(test)]") {
            pending_skip = true;
            continue;
        }
        if pending_skip {
            if trimmed.starts_with("#[") {
                continue;
            }
            pending_skip = false;
            let depth = line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if depth > 0 {
                skip_depth = Some(depth);
            }
            continue;
        }
        out.push((index + 1, line));
    }
    out
}

#[test]
fn no_command_new_outside_the_helper() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for entry in fs::read_dir(crates).unwrap().flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(files.len() > 50, "found only {} source files", files.len());
    let mut offenders = Vec::new();
    for file in files {
        if file.ends_with("superba-core/src/process.rs") {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (line_no, line) in non_test_lines(&text) {
            let code = line.split("//").next().unwrap_or_default();
            // clap's `Command::new` builds a CLI definition, not a process.
            let spawns = code.contains("Command::new(") && !code.contains("clap::Command");
            let shell = code.contains("\"powershell") || code.contains("\"pwsh");
            if (spawns && !is_clap_file(&text)) || shell && !is_allowed_shell(&file) {
                offenders.push(format!("{}:{line_no}: {}", file.display(), line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "build child processes with superba_core::process::command (hidden console window on Windows):\n{}",
        offenders.join("\n")
    );
}

fn is_clap_file(text: &str) -> bool {
    text.contains("use clap::Command") || text.contains("use clap::{") && text.contains("Command")
}

/// PowerShell may only be named by the hidden, cached `Get-AppxPackage` fallback, and by the
/// notification hook's command line: a string written into the tools' config files for them to
/// run, never a process Superba spawns itself.
fn is_allowed_shell(file: &Path) -> bool {
    file.ends_with("superba-appctl/src/install.rs")
        || file.ends_with("superba-notify/src/hooks/windows.rs")
}

#[test]
fn helper_commands_run_and_detached_commands_run() {
    let program = if cfg!(windows) { "cmd" } else { "true" };
    let mut command = superba_core::process::command(program);
    if cfg!(windows) {
        command.args(["/C", "exit 0"]);
    }
    assert!(command.status().unwrap().success());
    let mut detached = superba_core::process::detached_command(program);
    if cfg!(windows) {
        detached.args(["/C", "exit 0"]);
    }
    assert!(detached.status().unwrap().success());
}
