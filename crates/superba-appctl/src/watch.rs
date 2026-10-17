//! Cheap change check for the environment monitor of the switch service
//! ([`superba_core::AppController::environment_stamp`]).
//!
//! Detection (installs, Codex CLIs, `--version`) reads directories and may start
//! processes; the monitor runs it only when something it depends on changed. What
//! it depends on is a short list of paths, built once per detection
//! ([`watch_paths`]: install locations, the directories installs and CLIs are
//! found in, PATH, the files found, the programs launcher scripts run); the
//! check itself ([`metadata_stamp`]) only reads the metadata of those paths: no
//! directory listing, no file content, no process.

use std::collections::BTreeSet;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use superba_core::codex::ENV_SUPERBA_APP_PATH;
use superba_core::{AppInstall, CodexBinary, EnvSource};

use crate::install::{ENV_SUPERBA_APP_DIR, candidate_roots};

/// The paths whose metadata changes when an install or a Codex CLI is added,
/// updated, moved or removed.
///
/// - explicit install paths (`SUPERBA_APP_PATH`, `SUPERBA_APP_DIR`);
/// - the platform install locations and the directories they are discovered in
///   (Linux: fixed roots, `.desktop` directories, `~/Applications`, `/opt`;
///   macOS: `/Applications`, `~/Applications`; Windows: `Programs\ChatGPT`,
///   `Programs\Codex`): a directory's modification time changes when an entry is
///   added, removed or renamed;
/// - each install's directory, executable, `app.asar` and bundled Codex;
/// - the PATH directories and the other directories CLIs are looked for in;
/// - the official standalone Codex layout in the Codex home
///   (`packages/standalone` and its `current` link, which its installer
///   switches);
/// - each binary found and, for a launcher script, the program it runs (read
///   here, once per detection, so the check never reads file contents).
pub fn watch_paths(
    env: &dyn EnvSource,
    home_dir: &Path,
    codex_home: &Path,
    installs: &[AppInstall],
    binaries: &[CodexBinary],
) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for key in [ENV_SUPERBA_APP_PATH, ENV_SUPERBA_APP_DIR] {
        if let Some(value) = env.var_os(key).filter(|v| !v.is_empty()) {
            paths.insert(PathBuf::from(value));
        }
    }
    let roots = candidate_roots(env, home_dir);
    paths.extend(roots.installs);
    paths.extend(roots.desktop_dirs);
    paths.extend(roots.scan_dirs);
    for install in installs {
        paths.insert(install.path.clone());
        paths.insert(install.executable.clone());
        // Linux/Windows layout and macOS bundle layout; a missing one is stable.
        paths.insert(install.path.join("resources/app.asar"));
        paths.insert(install.path.join("Contents/Resources/app.asar"));
        paths.insert(install.path.join("Contents/Info.plist"));
        if let Some(codex) = &install.bundled_codex {
            paths.insert(codex.clone());
        }
    }
    let cwd = env
        .var_os("PWD")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home_dir.to_owned());
    if let Some(path_value) = env.var_os("PATH") {
        for directory in std::env::split_paths(&path_value) {
            paths.insert(if directory.is_absolute() {
                directory
            } else {
                cwd.join(directory)
            });
        }
    }
    paths.extend(crate::clibin::watched_directories(env, home_dir));
    let standalone = codex_home.join("packages/standalone");
    paths.insert(codex_home.join("packages"));
    paths.insert(standalone.join("current"));
    paths.insert(standalone);
    for binary in binaries {
        paths.insert(binary.path.clone());
        let target = crate::codexid::resolve_binary(&binary.path);
        if target != binary.path {
            paths.insert(target);
        }
    }
    paths.into_iter().collect()
}

/// Hash of the metadata of `paths`: existence, kind, size and modification time
/// (symbolic links followed, so a re-pointed link changes it too, as does the
/// directory holding it). Reads metadata only.
pub fn metadata_stamp(paths: &[PathBuf]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for path in paths {
        path.hash(&mut hasher);
        match fs::metadata(path) {
            Ok(meta) => {
                (true, meta.is_dir(), meta.len()).hash(&mut hasher);
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .hash(&mut hasher);
            }
            Err(_) => false.hash(&mut hasher),
        }
    }
    #[cfg(target_os = "windows")]
    crate::install::registry_appx_stamp().hash(&mut hasher);
    hasher.finish()
}

/// Identity of a detection result (the paths it found), to reuse a built
/// [`watch_paths`] list between checks.
pub fn inputs_key(installs: &[AppInstall], binaries: &[CodexBinary]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for install in installs {
        (&install.path, &install.executable, &install.bundled_codex).hash(&mut hasher);
    }
    for binary in binaries {
        binary.path.hash(&mut hasher);
    }
    hasher.finish()
}
