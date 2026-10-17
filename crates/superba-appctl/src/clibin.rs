//! Codex CLI discovery using an injected PATH and home directory.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use superba_core::{AppInstall, CodexBinary, CodexVersion, EnvSource, InstallSource};
use tokio::task::JoinSet;

use crate::install::codex_version;

fn path_parts(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy();
    let windows =
        cfg!(target_os = "windows") || text.contains('\\') || text.as_bytes().get(1) == Some(&b':');
    let text = text.replace('\\', "/");
    let mut parts = if text.starts_with('/') {
        vec!["/".to_owned()]
    } else {
        Vec::new()
    };
    for part in text
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." && parts.last().is_some_and(|part| part != ".." && part != "/") {
            parts.pop();
        } else {
            parts.push(if windows {
                part.to_ascii_lowercase()
            } else {
                part.to_owned()
            });
        }
    }
    parts
}

fn contains_parts(parts: &[String], needle: &[&str]) -> bool {
    parts.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(part, expected)| part.eq_ignore_ascii_case(expected))
    })
}

fn source_for(path: &Path, installs: &[AppInstall]) -> InstallSource {
    let parts = path_parts(path);
    if contains_parts(&parts, &["node_modules", "@openai", "codex"])
        || contains_parts(&parts, &["npm"])
        || contains_parts(&parts, &[".npm-global"])
        || contains_parts(&parts, &[".nvm", "versions", "node"])
    {
        InstallSource::Npm
    } else if contains_parts(&parts, &["Cellar"])
        || contains_parts(&parts, &["opt", "homebrew"])
        || contains_parts(&parts, &["home", "linuxbrew"])
    {
        InstallSource::Brew
    } else if contains_parts(&parts, &[".cargo", "bin"]) {
        InstallSource::Cargo
    } else if contains_parts(&parts, &["OpenAI", "Codex", "bin"])
        || installs.iter().any(|install| {
            let root = path_parts(&install.path);
            !root.is_empty() && parts.len() > root.len() && parts.starts_with(&root)
        })
    {
        InstallSource::AppBundled
    } else {
        InstallSource::Unknown
    }
}

/// Classify a CLI by canonical path components, accepting Unix and Windows separators.
/// If the resolved location is unknown, the original path can identify a global bin directory.
pub fn classify_source(path: &Path, resolved: &Path, installs: &[AppInstall]) -> InstallSource {
    let source = source_for(resolved, installs);
    if source == InstallSource::Unknown {
        source_for(path, installs)
    } else {
        source
    }
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn binary_names(env: &dyn EnvSource) -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        // PATHEXT is injected too; Windows filesystem lookup is case insensitive.
        let extensions = env
            .var_non_empty("PATHEXT")
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
        let mut names = vec!["codex".to_owned()];
        for extension in extensions.split(';').map(str::trim) {
            if extension.starts_with('.') && !extension.contains(['/', '\\']) {
                let name = format!("codex{}", extension.to_ascii_lowercase());
                if !names.contains(&name) {
                    names.push(name)
                }
            }
        }
        names
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = env;
        vec!["codex".to_owned()]
    }
}

fn subdirectories(root: &Path) -> Vec<PathBuf> {
    let mut directories: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    directories
}

fn extra_directories(env: &dyn EnvSource, home: &Path) -> Vec<PathBuf> {
    let mut directories = vec![
        home.join(".npm-global/bin"),
        home.join(".local/bin"),
        home.join(".cargo/bin"),
    ];
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    directories.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ]
        .map(PathBuf::from),
    );
    directories.extend(
        subdirectories(&home.join(".nvm/versions/node"))
            .into_iter()
            .map(|dir| dir.join("bin")),
    );
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = env.var_os("APPDATA").filter(|value| !value.is_empty()) {
            directories.push(PathBuf::from(appdata).join("npm"));
        }
        if let Some(local) = env.var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
            directories.extend(subdirectories(
                &PathBuf::from(local).join("OpenAI/Codex/bin"),
            ));
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = env;
    directories
}

struct Discovery<'a> {
    installs: &'a [AppInstall],
    seen: HashMap<PathBuf, usize>,
    binaries: Vec<CodexBinary>,
}

impl Discovery<'_> {
    fn add(
        &mut self,
        path: PathBuf,
        rank: Option<usize>,
        bundled: bool,
        version: Option<&CodexVersion>,
    ) {
        if !executable_file(&path) {
            return;
        }
        let Ok(resolved) = fs::canonicalize(&path) else {
            return;
        };
        if let Some(&index) = self.seen.get(&resolved) {
            if bundled {
                let binary = &mut self.binaries[index];
                binary.source = InstallSource::AppBundled;
                if binary.version.is_none() {
                    binary.version = version.cloned();
                }
            }
            return;
        }
        let source = if bundled {
            InstallSource::AppBundled
        } else {
            classify_source(&path, &resolved, self.installs)
        };
        self.seen.insert(resolved, self.binaries.len());
        self.binaries.push(CodexBinary {
            path,
            path_rank: rank,
            source,
            version: version.cloned(),
        });
    }
}

/// Find executable Codex CLIs in the supplied PATH, then known extra directories and app bundles.
/// Canonical paths deduplicate symlinks while preserving the earliest PATH directory's rank.
/// Relative PATH entries use the supplied `PWD`, falling back to `home_dir`; no process env is read.
/// Bundled metadata supplies versions without executing binaries.
pub fn find_codex_binaries(
    env: &dyn EnvSource,
    home_dir: &Path,
    installs: &[AppInstall],
) -> Vec<CodexBinary> {
    let names = binary_names(env);
    let mut discovery = Discovery {
        installs,
        seen: HashMap::new(),
        binaries: Vec::new(),
    };
    let cwd = env
        .var_os("PWD")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home_dir.to_owned());
    if let Some(path_value) = env.var_os("PATH") {
        for (rank, directory) in std::env::split_paths(&path_value).enumerate() {
            let directory = if directory.is_absolute() {
                directory
            } else {
                cwd.join(directory)
            };
            for name in &names {
                discovery.add(directory.join(name), Some(rank), false, None);
            }
        }
    }
    for directory in extra_directories(env, home_dir) {
        for name in &names {
            discovery.add(directory.join(name), None, false, None);
        }
    }
    for install in installs {
        if let Some(path) = &install.bundled_codex {
            discovery.add(
                path.clone(),
                None,
                true,
                install.bundled_codex_version.as_ref(),
            );
        }
    }
    discovery.binaries
}

/// Discover CLIs and concurrently probe missing versions, keeping discovery order and ranks.
/// Each version command has the five-second deadline enforced by [`codex_version`].
pub async fn codex_binaries(
    env: &dyn EnvSource,
    home_dir: &Path,
    installs: &[AppInstall],
) -> Vec<CodexBinary> {
    let mut binaries = find_codex_binaries(env, home_dir, installs);
    let mut probes = JoinSet::new();
    for (index, binary) in binaries.iter().enumerate() {
        if binary.version.is_none() {
            let path = binary.path.clone();
            probes.spawn(async move { (index, codex_version(&path).await) });
        }
    }
    while let Some(result) = probes.join_next().await {
        if let Ok((index, version)) = result {
            binaries[index].version = version;
        }
    }
    binaries
}
