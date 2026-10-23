//! Finding a tool's program wherever the user put it (user bugs 109, 110).
//!
//! The process `PATH` alone is not enough: a GUI app started from Finder or
//! the Dock gets launchd's short `PATH`, a Windows app keeps the `PATH` it
//! started with although the user's changed since, and a user may have moved
//! npm's global prefix (`npm config set prefix …`) or installed with pnpm or
//! bun. So a program is looked up, in this order, in:
//!
//! 1. the path the user chose for it (「指定路径…」, `<data>/tools/paths.json`);
//! 2. the process `PATH`;
//! 3. Windows: the user and system `PATH` in the registry (as they are now);
//!    Unix: the login shell's `PATH` (`$SHELL -lc`, cached, with a timeout);
//! 4. Homebrew: `$HOMEBREW_PREFIX/bin`, `brew --prefix`, `/opt/homebrew/bin`,
//!    `/usr/local/bin`, Linuxbrew;
//! 5. the global folders of npm, pnpm and bun, from their variables and
//!    config (`NPM_CONFIG_PREFIX`, `~/.npmrc`, `PNPM_HOME`, `BUN_INSTALL`)
//!    and by asking them (`npm prefix -g`, `pnpm bin -g`, `bun pm bin -g`);
//! 6. the usual per-user folders (`~/.local/bin`, `%APPDATA%\npm`, Volta,
//!    pip's user scripts, …) and the tool's own install folders.
//!
//! Windows names are tried with every `PATHEXT` extension, so an npm
//! `dsh.cmd` shim is found. dsh has no install-location variable of its own
//! (`DSH_HOME` is its data folder): its installs are npm / pip / Desktop, so
//! moving it means moving one of those prefixes, which the list above covers.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use superba_core::{DataDir, EnvSource, Error, Result};

use crate::platform::Os;

/// Where a program was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The path the user chose.
    Override,
    /// The process `PATH`.
    Path,
    /// The registry `PATH` (Windows) or the login shell's (Unix).
    SystemPath,
    /// Homebrew's `bin`.
    Homebrew,
    /// A package manager's global folder (npm, pnpm, bun).
    PackageManager,
    /// A usual install folder.
    Default,
}

impl Source {
    /// Short English label (logs, the "where Superba looked" list).
    pub fn label(self) -> &'static str {
        match self {
            Self::Override => "chosen path",
            Self::Path => "PATH",
            Self::SystemPath => "login PATH",
            Self::Homebrew => "Homebrew",
            Self::PackageManager => "npm / pnpm / bun",
            Self::Default => "default folder",
        }
    }
}

/// A found program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    /// The program as found (a launcher, shim or link; not resolved).
    pub path: PathBuf,
    /// How.
    pub source: Source,
}

/// What a search looked at.
#[derive(Clone, Debug, Default)]
pub struct SearchDirs {
    /// Folders, in order, with their source; no duplicates.
    pub dirs: Vec<(Source, PathBuf)>,
    windows: bool,
    exts: Vec<String>,
}

impl SearchDirs {
    /// The first `name` in the folders.
    pub fn find(&self, name: &str) -> Option<Located> {
        self.find_all(name).into_iter().next()
    }

    /// Every `name` in the folders, in order.
    pub fn find_all(&self, name: &str) -> Vec<Located> {
        let mut out = Vec::new();
        let exts: Vec<&str> = if self.windows && Path::new(name).extension().is_none() {
            self.exts.iter().map(String::as_str).collect()
        } else {
            vec![""]
        };
        for (source, dir) in &self.dirs {
            for ext in &exts {
                let p = dir.join(format!("{name}{ext}"));
                if crate::run::is_executable(&p) {
                    out.push(Located {
                        path: p,
                        source: *source,
                    });
                    break;
                }
            }
        }
        out
    }

    /// The folders as one `PATH` value (for the commands Superba runs, so
    /// npm finds node where Superba found npm).
    pub fn joined(&self) -> Option<OsString> {
        std::env::join_paths(self.dirs.iter().map(|(_, d)| d)).ok()
    }

    /// The folders searched, as display text grouped by source
    /// (`PATH: 12 folders`, `Homebrew: /opt/homebrew/bin`).
    pub fn summary(&self) -> Vec<String> {
        let mut out: Vec<(Source, Vec<String>)> = Vec::new();
        for (s, d) in &self.dirs {
            let shown = superba_core::paths::display_path(d);
            match out.iter_mut().find(|(k, _)| k == s) {
                Some((_, v)) => v.push(shown),
                None => out.push((*s, vec![shown])),
            }
        }
        out.into_iter()
            .map(|(s, v)| {
                if matches!(s, Source::Path | Source::SystemPath) && v.len() > 3 {
                    format!("{}: {} …(+{})", s.label(), v[..2].join(", "), v.len() - 2)
                } else {
                    format!("{}: {}", s.label(), v.join(", "))
                }
            })
            .collect()
    }

    fn push(&mut self, source: Source, dir: PathBuf) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            return;
        }
        let key = norm(&dir, self.windows);
        if self.dirs.iter().any(|(_, d)| norm(d, self.windows) == key) {
            return;
        }
        self.dirs.push((source, dir));
    }
}

fn norm(p: &Path, windows: bool) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_owned();
    if windows { s.to_lowercase() } else { s }
}

/// What a search needs to know.
pub struct Env<'a> {
    /// Variables.
    pub env: &'a dyn EnvSource,
    /// Home.
    pub home: &'a Path,
    /// OS.
    pub os: Os,
}

impl Env<'_> {
    fn var(&self, k: &str) -> Option<PathBuf> {
        self.env.var_non_empty(k).map(PathBuf::from)
    }

    /// A system folder (`/opt/homebrew/bin`), under `SUPERBA_SEARCH_ROOT`
    /// when that is set: tests give the search a fake filesystem that way
    /// (and the login shell is not asked then).
    fn sys(&self, p: &str) -> PathBuf {
        match self.var("SUPERBA_SEARCH_ROOT") {
            Some(root) => root.join(p.trim_start_matches('/')),
            None => PathBuf::from(p),
        }
    }

    /// Every folder a program may be in, in search order (without overrides).
    pub fn dirs(&self) -> SearchDirs {
        let windows = self.os == Os::Windows;
        let mut s = SearchDirs {
            windows,
            exts: pathext(self.env),
            ..Default::default()
        };
        let process: Vec<PathBuf> = self
            .env
            .var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for d in &process {
            s.push(Source::Path, d.clone());
        }
        for d in system_path(self) {
            s.push(Source::SystemPath, d);
        }
        if !windows {
            if let Some(p) = self.var("HOMEBREW_PREFIX") {
                s.push(Source::Homebrew, p.join("bin"));
            }
            let mut brews = Vec::new();
            if self.os == Os::Macos {
                brews.push(self.sys("/opt/homebrew/bin"));
                brews.push(self.sys("/usr/local/bin"));
            } else {
                brews.push(self.sys("/home/linuxbrew/.linuxbrew/bin"));
                brews.push(self.home.join(".linuxbrew").join("bin"));
            }
            for b in brews {
                s.push(Source::Homebrew, b);
            }
            if let Some(brew) = s.find("brew")
                && let Some(p) = query(&brew.path, &["--prefix"], &s)
            {
                s.push(Source::Homebrew, PathBuf::from(p).join("bin"));
            }
        }
        for d in self.package_manager_dirs(&s) {
            s.push(Source::PackageManager, d);
        }
        for d in self.default_dirs() {
            s.push(Source::Default, d);
        }
        s
    }

    /// The global `bin` folders of npm, pnpm and bun.
    fn package_manager_dirs(&self, so_far: &SearchDirs) -> Vec<PathBuf> {
        let windows = self.os == Os::Windows;
        let npm_bin = |prefix: PathBuf| {
            if windows { prefix } else { prefix.join("bin") }
        };
        let mut out = Vec::new();
        for var in ["NPM_CONFIG_PREFIX", "npm_config_prefix"] {
            if let Some(p) = self.var(var) {
                out.push(npm_bin(p));
            }
        }
        if let Some(p) = npmrc_prefix(self.home) {
            out.push(npm_bin(p));
        }
        if let Some(p) = self.var("PNPM_HOME") {
            out.push(p);
        }
        if let Some(p) = self.var("BUN_INSTALL") {
            out.push(p.join("bin"));
        }
        if let Some(npm) = so_far.find("npm")
            && let Some(p) = query(&npm.path, &["prefix", "-g"], so_far)
        {
            out.push(npm_bin(PathBuf::from(p)));
        }
        if let Some(pnpm) = so_far.find("pnpm")
            && let Some(p) = query(&pnpm.path, &["bin", "-g"], so_far)
        {
            out.push(PathBuf::from(p));
        }
        if let Some(bun) = so_far.find("bun")
            && let Some(p) = query(&bun.path, &["pm", "bin", "-g"], so_far)
        {
            out.push(PathBuf::from(p));
        }
        out
    }

    /// The usual per-user folders.
    fn default_dirs(&self) -> Vec<PathBuf> {
        let h = self.home;
        let mut out = vec![h.join(".local").join("bin")];
        if let Some(p) = self.var("NVM_BIN") {
            out.push(p);
        }
        if let Some(p) = self.var("PIPX_BIN_DIR") {
            out.push(p);
        }
        out.push(h.join(".volta").join("bin"));
        out.push(h.join(".bun").join("bin"));
        match self.os {
            Os::Windows => {
                if let Some(a) = self.var("APPDATA") {
                    out.push(a.join("npm"));
                    out.extend(glob_dirs(&a.join("Python"), "Python", &["Scripts"]));
                }
                if let Some(l) = self.var("LOCALAPPDATA") {
                    out.push(l.join("pnpm"));
                    out.push(l.join("Microsoft").join("WinGet").join("Links"));
                    out.push(l.join("Volta").join("bin"));
                }
            }
            Os::Macos => {
                out.push(h.join("Library").join("pnpm"));
                out.push(h.join(".npm-global").join("bin"));
                out.extend(glob_dirs(&h.join("Library").join("Python"), "3", &["bin"]));
                out.push(self.sys("/usr/local/bin"));
            }
            Os::Linux => {
                out.push(h.join(".local").join("share").join("pnpm"));
                out.push(h.join(".npm-global").join("bin"));
                out.push(self.sys("/usr/local/bin"));
                out.push(self.sys("/usr/bin"));
            }
        }
        // The tools' own installers.
        out.push(h.join(".opencode").join("bin"));
        out.push(h.join(".pi").join("agent").join("bin"));
        out.push(h.join(".claude").join("local"));
        out
    }
}

/// `dir/<prefix>*/<sub…>` folders (pip's `Python312\Scripts`).
fn glob_dirs(dir: &Path, prefix: &str, sub: &[&str]) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| sub.iter().fold(e.path(), |p, s| p.join(s)))
        .collect();
    v.sort();
    v.reverse();
    v
}

/// `prefix=` in `~/.npmrc`.
pub fn npmrc_prefix(home: &Path) -> Option<PathBuf> {
    let rc = std::fs::read_to_string(home.join(".npmrc")).ok()?;
    rc.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        if k.trim() != "prefix" {
            return None;
        }
        let v = v.trim().trim_matches('"');
        Some(
            v.strip_prefix("~/")
                .map_or_else(|| PathBuf::from(v), |r| home.join(r)),
        )
    })
}

fn pathext(env: &dyn EnvSource) -> Vec<String> {
    let mut v: Vec<String> = env
        .var_non_empty("PATHEXT")
        .map(|p| {
            p.split(';')
                .filter(|e| !e.trim().is_empty())
                .map(|e| e.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    for d in [".exe", ".cmd", ".bat", ".com"] {
        if !v.iter().any(|e| e == d) {
            v.push(d.into());
        }
    }
    v
}

// ---------------------------------------------------------------------------
// the PATH the user's shell / the registry has now

/// Answers by command line: when, and what.
type Answers = HashMap<String, (Instant, Option<String>)>;
static QUERIES: OnceLock<Mutex<Answers>> = OnceLock::new();
const QUERY_TTL: Duration = Duration::from_secs(300);
const QUERY_TIMEOUT: Duration = Duration::from_secs(4);

/// Forget answers of `npm prefix -g`, `brew --prefix` and the login shell.
pub fn forget() {
    if let Some(q) = QUERIES.get() {
        q.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}

/// The first line `program args` prints (cached; `None` on failure/timeout).
fn query(program: &Path, args: &[&str], dirs: &SearchDirs) -> Option<String> {
    let path = dirs.joined();
    let key = format!(
        "{}|{}|{}",
        program.display(),
        args.join(" "),
        path.as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    cached(&key, || {
        let mut c = superba_core::process::command(program);
        c.args(args);
        if let Some(p) = &path {
            c.env("PATH", p);
        }
        c.env("HOMEBREW_NO_AUTO_UPDATE", "1");
        c.env("NO_COLOR", "1");
        capture(c)
    })
}

fn cached(key: &str, f: impl FnOnce() -> Option<String>) -> Option<String> {
    let map = QUERIES.get_or_init(Default::default);
    if let Some((at, v)) = map.lock().unwrap_or_else(|p| p.into_inner()).get(key)
        && at.elapsed() < QUERY_TTL
    {
        return v.clone();
    }
    let v = f();
    map.lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.to_owned(), (Instant::now(), v.clone()));
    v
}

/// Run `c` (stdin closed), its first non-empty stdout line within the timeout.
fn capture(mut c: std::process::Command) -> Option<String> {
    use std::io::Read;
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = c.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let s = reader.join().ok()?;
                if !st.success() {
                    return None;
                }
                return s
                    .lines()
                    .map(str::trim)
                    .rfind(|l| !l.is_empty())
                    .map(str::to_owned);
            }
            Ok(None) if start.elapsed() < QUERY_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Windows: the registry `PATH` (user, then system), expanded. Unix: the
/// login shell's `PATH` (`$SHELL -lc`; macOS falls back to `/bin/zsh`).
fn system_path(e: &Env<'_>) -> Vec<PathBuf> {
    match e.os {
        Os::Windows => registry_path(e.env),
        _ => {
            if e.env.var_non_empty("SUPERBA_NO_LOGIN_SHELL").is_some()
                || e.env.var_non_empty("SUPERBA_SEARCH_ROOT").is_some()
            {
                return Vec::new();
            }
            let shell = e
                .env
                .var_non_empty("SHELL")
                .or_else(|| (e.os == Os::Macos).then(|| "/bin/zsh".to_owned()));
            let Some(shell) = shell else {
                return Vec::new();
            };
            let home = e.home.to_path_buf();
            let key = format!("login|{shell}|{}", home.display());
            let got = cached(&key, || {
                let mut c = superba_core::process::command(&shell);
                // `-l` reads the profile (Homebrew's `shellenv`, nvm, …);
                // the marker skips anything the profile prints.
                c.args(["-lc", "printf '\\n__SUPERBA_PATH__%s\\n' \"$PATH\""])
                    .env("HOME", &home)
                    .current_dir(&home);
                capture(c).and_then(|l| l.strip_prefix("__SUPERBA_PATH__").map(str::to_owned))
            });
            got.map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default()
        }
    }
}

#[cfg(windows)]
fn registry_path(env: &dyn EnvSource) -> Vec<PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
    let expand = |s: &str| -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            match after.find('%') {
                Some(j) => {
                    let name = &after[..j];
                    match env.var(name) {
                        Some(v) => out.push_str(&v),
                        None => {
                            out.push('%');
                            out.push_str(name);
                            out.push('%');
                        }
                    }
                    rest = &after[j + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    };
    let mut out = Vec::new();
    for (root, sub) in [
        (HKEY_CURRENT_USER, "Environment"),
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
    ] {
        if let Ok(k) = RegKey::predef(root).open_subkey_with_flags(sub, KEY_READ)
            && let Ok(v) = k.get_value::<String, _>("Path")
        {
            out.extend(
                v.split(';')
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| PathBuf::from(expand(p.trim()))),
            );
        }
    }
    out
}

#[cfg(not(windows))]
fn registry_path(_env: &dyn EnvSource) -> Vec<PathBuf> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// the user's choice

/// The paths the user chose per command (`<data>/tools/paths.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overrides {
    /// Command (`dsh`, `claude`, `gemini`) → program.
    #[serde(default)]
    pub paths: std::collections::BTreeMap<String, PathBuf>,
}

impl Overrides {
    fn file(data: &DataDir) -> PathBuf {
        crate::state::tools_dir(data).join("paths.json")
    }

    /// Read (missing or unreadable: none).
    pub fn load(data: &DataDir) -> Self {
        std::fs::read(Self::file(data))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The chosen program of `command`, when it still exists.
    pub fn get(data: &DataDir, command: &str) -> Option<PathBuf> {
        Self::load(data)
            .paths
            .get(command)
            .filter(|p| p.is_file())
            .cloned()
    }

    /// Set (`Some`) or clear (`None`) the program of `command`.
    pub fn set(data: &DataDir, command: &str, path: Option<PathBuf>) -> Result<()> {
        let mut o = Self::load(data);
        match path {
            Some(p) => o.paths.insert(command.to_owned(), p),
            None => o.paths.remove(command),
        };
        let file = Self::file(data);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
        }
        let text = serde_json::to_vec_pretty(&o)
            .map_err(|e| Error::parse(file.display().to_string(), e))?;
        crate::fsx::write_atomic(&file, &text)
    }
}

/// `path` is a program that answers `--version` (the check before a chosen
/// path is saved): its version line.
pub fn validate(path: &Path) -> Result<String> {
    if !crate::run::is_executable(path) {
        return Err(Error::invalid(format!(
            "{} is not a program that can be run",
            path.display()
        )));
    }
    let mut c = superba_core::process::command(path);
    c.arg("--version");
    capture(c).ok_or_else(|| {
        Error::invalid(format!(
            "{} did not answer `--version`; choose the tool's program",
            path.display()
        ))
    })
}

/// `command` from the user's choice, else the search.
pub fn locate(e: &Env<'_>, data: Option<&DataDir>, command: &str) -> (Option<Located>, SearchDirs) {
    let dirs = e.dirs();
    if let Some(p) = data.and_then(|d| Overrides::get(d, command)) {
        return (
            Some(Located {
                path: p,
                source: Source::Override,
            }),
            dirs,
        );
    }
    (dirs.find(command), dirs)
}
