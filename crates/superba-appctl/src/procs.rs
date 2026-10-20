//! Process snapshot and classification of Codex-related processes (plan 7.7, 12.4).
//!
//! The snapshot comes from `sysinfo` (Linux `/proc`, macOS libproc, Windows
//! ToolHelp/PEB). Classification is pure ([`classify`]) and tested on fixtures:
//!
//! - Electron/Chromium children (`--type=...`) are ignored.
//! - App main: an executable with `resources/app.asar` next to it (Linux, Windows)
//!   or inside `<X>.app/Contents/MacOS/` with `Contents/Resources/app.asar` (macOS).
//! - Codex invocations: `codex[.exe]`, or `node <.../bin/codex[.js]>` (npm wrapper).
//!   `app-server` under the app main = [`CodexProcessKind::AppServer`];
//!   `app-server --listen <non-stdio>` elsewhere = daemon; plain `app-server`
//!   elsewhere = IDE app-server; `exec`/`e`/`review` = exec; no subcommand,
//!   `resume`, `fork` = TUI; `app-server proxy|daemon|generate-*` and tooling
//!   subcommands = other.
//! - `ssh ... <remote command containing app-server>` under the app main =
//!   [`CodexProcessKind::RemoteLink`].
//! - The app's "Run Codex in WSL" mode (user bug 54 (b)): `wsl.exe … --exec
//!   /usr/bin/bash -lc '… "$(wslpath -u '…\bin\wsl\codex')" … app-server …'`
//!   under the app main is its local [`CodexProcessKind::AppServer`]; it sees
//!   `CODEX_AUTHAPI_BASE_URL` only when `WSLENV` names it
//!   ([`ProcEntry::authapi_observation`]). Remote WSL connections (`wsl.exe -d
//!   <distro> -- /usr/bin/bash -lc '… exec codex …'`) are not it.
//!
//! Environment: on Linux the app main process's `/proc/<pid>/environ` is
//! overwritten by Chromium's process-title code, and the app merges the user's
//! login-shell environment into its own at start-up (verified, app 26.1007), so the
//! environment a hot-restarted app-server inherits is observed from the current
//! app-server child instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use superba_core::{
    CodexProcessKind, EnvObservation, IdeKind, ProcessInfo, Timestamp, redact_argv,
};

/// One process from the OS snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcEntry {
    /// pid.
    pub pid: u32,
    /// Parent pid.
    pub ppid: Option<u32>,
    /// Executable path.
    pub exe: Option<PathBuf>,
    /// Full argv.
    pub argv: Vec<String>,
    /// Start time (native milliseconds on Windows, seconds elsewhere).
    pub started_at: Option<Timestamp>,
    /// `KEY=VALUE` environment; `None` when unreadable or clobbered.
    pub env: Option<BTreeMap<String, String>>,
    /// Zombie / defunct.
    pub zombie: bool,
    /// Owned by another OS user (Unix: a different uid than ours). Such a process
    /// is never taken for "the" desktop app: Superba cannot control it.
    pub foreign_user: bool,
    /// Working directory, when readable (read together with the environment).
    pub cwd: Option<PathBuf>,
}

impl ProcEntry {
    /// `CODEX_AUTHAPI_BASE_URL` as the Codex of this process sees it: for the
    /// app's app-server inside WSL ([`is_wsl_app_server`]) only when `WSLENV`
    /// forwards it (else `Absent`), otherwise [`ProcEntry::env_observation`].
    pub fn authapi_observation(&self) -> EnvObservation {
        let key = superba_core::codex::ENV_AUTHAPI_BASE_URL;
        if !is_wsl_app_server(&self.argv) {
            return self.env_observation(key);
        }
        match &self.env {
            None => EnvObservation::Unknown,
            Some(m) => {
                let get = |k: &str| {
                    m.iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(k))
                        .map(|(_, v)| v.clone())
                };
                let forwarded = get(superba_core::wsl::ENV_WSLENV)
                    .is_some_and(|w| superba_core::wsl::wslenv_names(&w, key));
                match get(key) {
                    Some(v) if forwarded => EnvObservation::Present(v),
                    _ => EnvObservation::Absent,
                }
            }
        }
    }

    /// Environment variable lookup (`None` = env unknown).
    pub fn env_observation(&self, key: &str) -> EnvObservation {
        match &self.env {
            None => EnvObservation::Unknown,
            Some(m) => match m.get(key) {
                Some(v) => EnvObservation::Present(v.clone()),
                None => EnvObservation::Absent,
            },
        }
    }

    /// Effective Codex home from the environment: `CODEX_HOME`, else
    /// `$HOME/.codex` (`%USERPROFILE%\.codex` on Windows).
    pub fn codex_home(&self) -> Option<PathBuf> {
        effective_codex_home(self.env.as_ref()?)
    }
}

/// `CODEX_HOME` if set and non-empty, else `<HOME or USERPROFILE>/.codex`.
pub fn effective_codex_home(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    if let Some(h) = env.get("CODEX_HOME").filter(|v| !v.trim().is_empty()) {
        return Some(PathBuf::from(h));
    }
    env.get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .filter(|v| !v.trim().is_empty())
        .map(|h| Path::new(h).join(".codex"))
}

/// Parse a raw environment block into a map; `None` if it holds no `KEY=VALUE`
/// entry (e.g. clobbered by Chromium's process-title code).
pub fn parse_environ<I, S>(entries: I) -> Option<BTreeMap<String, String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut map = BTreeMap::new();
    for e in entries {
        let e = e.as_ref();
        if let Some((k, v)) = e.split_once('=')
            && !k.is_empty()
            && !k.contains(char::is_whitespace)
        {
            map.insert(k.to_owned(), v.to_owned());
        }
    }
    (!map.is_empty()).then_some(map)
}

fn base_name(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

fn strip_exe(s: &str) -> &str {
    let lower = s.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".js"] {
        if lower.ends_with(ext) {
            return &s[..s.len() - ext.len()];
        }
    }
    s
}

/// True for Chromium/Electron helper processes.
pub fn is_electron_child(argv: &[String]) -> bool {
    // Chromium rewrites the command line of its children with setproctitle on
    // Linux: `/proc/<pid>/cmdline` then holds one space-separated string instead
    // of NUL-separated words, so `--type=` can sit inside the first "word".
    argv.iter()
        .any(|a| a.starts_with("--type=") || a.contains(" --type="))
}

/// `argv` is the desktop app's local app-server in "Run Codex in WSL" mode:
/// `wsl.exe` running `/usr/bin/bash -lc` (after `--exec`) with a script that
/// starts the app's own Linux Codex (`…\bin\wsl\<16 hex>\codex` under the
/// Windows Codex home, with `CODEX_SQLITE_HOME`) as `app-server`, inside a
/// process-identity wrapper (seen with app 26.1013 in the test VM). The app's
/// remote WSL connections run `wsl.exe -d <distro> -- … exec codex …` and are
/// not it.
pub fn is_wsl_app_server(argv: &[String]) -> bool {
    let Some(first) = argv.first() else {
        return false;
    };
    if !strip_exe(base_name(first)).eq_ignore_ascii_case("wsl") {
        return false;
    }
    let Some(exec) = argv.iter().position(|a| a == "--exec") else {
        return false;
    };
    let script = argv[exec + 1..].join(" ");
    script.contains("app-server")
        && (script.contains("CODEX_SQLITE_HOME=")
            || script.to_ascii_lowercase().contains("\\bin\\wsl\\"))
}

/// The arguments after the `codex` program token, if `argv` invokes Codex
/// (`codex ...`, `codex.exe ...`, `node .../codex[.js] ...`).
pub fn codex_args(argv: &[String], exe: Option<&Path>) -> Option<Vec<String>> {
    let first = argv.first()?;
    let is_codex = |s: &str| strip_exe(base_name(s)) == "codex";
    if is_codex(first) {
        return Some(argv[1..].to_vec());
    }
    let first_base = strip_exe(base_name(first)).to_ascii_lowercase();
    if first_base == "node" || first_base == "nodejs" {
        let second = argv.get(1)?;
        if is_codex(second) {
            return Some(argv[2..].to_vec());
        }
        return None;
    }
    // Some platforms report a bare process name in argv[0]; trust the exe then.
    if exe.is_some_and(|e| e.file_name().and_then(|n| n.to_str()).is_some_and(is_codex)) {
        return Some(argv[1..].to_vec());
    }
    None
}

/// Global options of `codex` that take a value.
const VALUE_OPTS: &[&str] = &[
    "-c",
    "--config",
    "--enable",
    "--disable",
    "--remote",
    "--remote-auth-token-env",
    "-i",
    "--image",
    "-m",
    "--model",
    "--local-provider",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-C",
    "--cd",
    "--add-dir",
    "-a",
    "--ask-for-approval",
    "--code-mode-host",
    "--listen",
];

/// Positional words of a codex command line (option values skipped), plus the
/// value of `--listen` if any.
pub fn codex_words(args: &[String]) -> (Vec<String>, Option<String>) {
    let mut words = Vec::new();
    let mut listen = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            break;
        }
        if a.starts_with('-') {
            if let Some((flag, value)) = a.split_once('=') {
                if flag == "--listen" {
                    listen = Some(value.to_owned());
                }
            } else if VALUE_OPTS.contains(&a.as_str()) {
                if a == "--listen" {
                    listen = args.get(i + 1).cloned();
                }
                i += 1;
            }
        } else {
            words.push(a.clone());
        }
        i += 1;
    }
    (words, listen)
}

/// What a codex command line does, before looking at the parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexCommand {
    /// `codex app-server` serving a client (stdio unless `listen` says otherwise).
    AppServer {
        /// `--listen` value.
        listen: Option<String>,
    },
    /// Interactive TUI (no subcommand, `resume`, `fork`).
    Tui,
    /// `exec` / `e` / `review`.
    Exec,
    /// Prints something and exits without a session: `--version` / `--help`
    /// without a subcommand, `help`, `features list`, `debug models` (Superba's
    /// own version, feature and model-list probes). Not a Codex consumer.
    Info,
    /// Anything else (tooling, proxies, daemon management).
    Other,
}

/// Flags that make `codex` without a subcommand print and exit.
const INFO_FLAGS: &[&str] = &["--version", "-V", "--help", "-h"];

/// Classify the codex arguments (the part after the program token).
pub fn codex_command(args: &[String]) -> CodexCommand {
    let (words, listen) = codex_words(args);
    let second = words.get(1).map(String::as_str);
    match words.first().map(String::as_str) {
        None if args
            .iter()
            .take_while(|a| *a != "--")
            .any(|a| INFO_FLAGS.contains(&a.as_str())) =>
        {
            CodexCommand::Info
        }
        Some("help") => CodexCommand::Info,
        Some("features") if second == Some("list") => CodexCommand::Info,
        Some("debug") if second == Some("models") => CodexCommand::Info,
        None | Some("resume") | Some("fork") => CodexCommand::Tui,
        Some("exec") | Some("e") | Some("review") => CodexCommand::Exec,
        Some("app-server") => match words.get(1).map(String::as_str) {
            None => CodexCommand::AppServer { listen },
            Some(_) => CodexCommand::Other,
        },
        Some(_) => CodexCommand::Other,
    }
}

/// ssh options that take a value (OpenSSH).
const SSH_VALUE_FLAGS: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// The destination of an `ssh` command line and the remote command, if `argv` is
/// ssh.
pub fn ssh_destination(argv: &[String]) -> Option<(String, String)> {
    let prog = strip_exe(base_name(argv.first()?));
    if prog != "ssh" {
        return None;
    }
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if let Some(flags) = a.strip_prefix('-')
            && !flags.is_empty()
        {
            // `-p22` carries its value; `-p 22` takes the next word; `-Tq` are flags.
            let mut chars = flags.chars();
            let mut takes_next = false;
            while let Some(c) = chars.next() {
                if SSH_VALUE_FLAGS.contains(c) {
                    takes_next = chars.as_str().is_empty();
                    break;
                }
            }
            i += if takes_next { 2 } else { 1 };
            continue;
        }
        break;
    }
    let dest = argv.get(i)?.clone();
    let command = argv.get(i + 1..).map(|r| r.join(" ")).unwrap_or_default();
    Some((dest, command))
}

/// Facts needed to classify one process.
#[derive(Clone, Copy, Debug)]
pub struct ClassifyInput<'a> {
    /// The process.
    pub proc: &'a ProcEntry,
    /// Its parent, if in the snapshot.
    pub parent: Option<&'a ProcEntry>,
    /// Whether the parent is an app main process.
    pub parent_is_app_main: bool,
    /// Whether this process's executable is a desktop app main executable.
    pub is_app_exe: bool,
}

/// Classify a process; `None` for unrelated processes and Electron children.
pub fn classify(input: ClassifyInput<'_>) -> Option<CodexProcessKind> {
    let p = input.proc;
    if is_electron_child(&p.argv) {
        return None;
    }
    if input.is_app_exe {
        return Some(CodexProcessKind::AppMain);
    }
    if let Some(args) = codex_args(&p.argv, p.exe.as_deref()) {
        return Some(match codex_command(&args) {
            // Short-lived probes (Superba runs `codex --version`, `features
            // list` and `debug models` itself) never talk to a line.
            CodexCommand::Info => return None,
            CodexCommand::AppServer { listen } => {
                if input.parent_is_app_main {
                    CodexProcessKind::AppServer
                } else if listen.as_deref().is_some_and(|l| {
                    let l = l.trim();
                    !l.is_empty() && l != "stdio://" && l != "off"
                }) {
                    CodexProcessKind::CliDaemon
                } else {
                    CodexProcessKind::IdeAppServer
                }
            }
            CodexCommand::Tui => CodexProcessKind::CliTui,
            CodexCommand::Exec => CodexProcessKind::CliExec,
            CodexCommand::Other => CodexProcessKind::Other,
        });
    }
    if input.parent_is_app_main
        && let Some((dest, command)) = ssh_destination(&p.argv)
        && command.contains("app-server")
    {
        return Some(CodexProcessKind::RemoteLink { host: dest });
    }
    if input.parent_is_app_main && is_wsl_app_server(&p.argv) {
        // "Run Codex in WSL": the app's local app-server, inside WSL.
        return Some(CodexProcessKind::AppServer);
    }
    None
}

/// True if `exe` is a desktop app main executable (file checks, see module docs).
pub fn is_app_main_exe(exe: &Path) -> bool {
    // The bundled CLI lives in `<install>/resources/` (or `CodexCLI.app`), so the
    // sibling checks below never match it, whatever the case of its name.
    let Some(dir) = exe.parent() else {
        return false;
    };
    if dir.join("resources").join("app.asar").exists() {
        return true;
    }
    // macOS: <X>.app/Contents/MacOS/<exe> with Contents/Resources/app.asar.
    if dir.file_name().and_then(|n| n.to_str()) == Some("MacOS")
        && let Some(contents) = dir.parent()
    {
        return contents.join("Resources").join("app.asar").exists();
    }
    false
}

/// A classified snapshot.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// All processes by pid.
    pub all: BTreeMap<u32, ProcEntry>,
    /// Classified processes (pid, kind), in pid order.
    pub classified: Vec<(u32, CodexProcessKind)>,
}

impl Snapshot {
    /// Classify an OS snapshot. `is_app_exe` decides app main executables
    /// (injectable for tests).
    pub fn classify_entries(
        entries: Vec<ProcEntry>,
        is_app_exe: &dyn Fn(&ProcEntry) -> bool,
    ) -> Snapshot {
        let all: BTreeMap<u32, ProcEntry> = entries.into_iter().map(|e| (e.pid, e)).collect();
        let app_mains: Vec<u32> = all
            .values()
            .filter(|e| !is_electron_child(&e.argv) && is_app_exe(e))
            .map(|e| e.pid)
            .collect();
        let mut classified = Vec::new();
        for e in all.values() {
            let parent = e.ppid.and_then(|pp| all.get(&pp));
            let parent_is_app_main = e.ppid.is_some_and(|pp| app_mains.contains(&pp));
            let input = ClassifyInput {
                proc: e,
                parent,
                parent_is_app_main,
                is_app_exe: app_mains.contains(&e.pid),
            };
            if let Some(kind) = classify(input) {
                classified.push((e.pid, kind));
            }
        }
        // Drop the native child of an npm `node .../codex` wrapper (same invocation);
        // the wrapper is what the user started.
        let kinds: BTreeMap<u32, CodexProcessKind> = classified.iter().cloned().collect();
        classified.retain(|(pid, kind)| {
            let Some(pp) = all.get(pid).and_then(|e| e.ppid) else {
                return true;
            };
            let parent_wrapper = all.get(&pp).is_some_and(|parent| {
                parent
                    .argv
                    .first()
                    .is_some_and(|a| strip_exe(base_name(a)).eq_ignore_ascii_case("node"))
            });
            !(parent_wrapper && kinds.get(&pp) == Some(kind))
        });
        Snapshot { all, classified }
    }

    /// Kind of `pid`, if classified.
    pub fn kind_of(&self, pid: u32) -> Option<&CodexProcessKind> {
        self.classified
            .iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, k)| k)
    }

    /// App main pids.
    pub fn app_mains(&self) -> Vec<u32> {
        self.classified
            .iter()
            .filter(|(_, k)| *k == CodexProcessKind::AppMain)
            .map(|(p, _)| *p)
            .collect()
    }

    /// App-server children of an app main.
    pub fn app_servers_of(&self, main_pid: u32) -> Vec<&ProcEntry> {
        self.classified
            .iter()
            .filter(|(_, k)| *k == CodexProcessKind::AppServer)
            .filter_map(|(p, _)| self.all.get(p))
            .filter(|e| e.ppid == Some(main_pid))
            .collect()
    }

    /// Effective Codex home of a classified process; app mains use their
    /// app-server child (their own environment is not readable on Linux).
    pub fn codex_home_of(&self, pid: u32) -> Option<PathBuf> {
        let e = self.all.get(&pid)?;
        if self.kind_of(pid) == Some(&CodexProcessKind::AppMain) {
            return self
                .app_servers_of(pid)
                .iter()
                .find_map(|c| c.codex_home())
                .or_else(|| e.codex_home());
        }
        e.codex_home()
    }

    /// `CODEX_AUTHAPI_BASE_URL` as an app-server of `main_pid` would inherit it.
    pub fn app_authapi_env(&self, main_pid: u32) -> EnvObservation {
        let key = superba_core::codex::ENV_AUTHAPI_BASE_URL;
        for c in self.app_servers_of(main_pid) {
            let obs = c.authapi_observation();
            if obs != EnvObservation::Unknown {
                return obs;
            }
        }
        self.all
            .get(&main_pid)
            .map(|e| e.env_observation(key))
            .unwrap_or_default()
    }

    /// The editor whose Codex extension runs `pid`: the extension directory of
    /// its executable (`~/.vscode/extensions/openai.chatgpt-…/bin/…`), else the
    /// first ancestor (up to eight levels) whose program is an editor
    /// (`Code.exe`, `Code Helper (Plugin)`, `cursor`, ...). Only app-servers
    /// outside the desktop app are looked at.
    pub fn ide_of(&self, pid: u32) -> Option<IdeKind> {
        let e = self.all.get(&pid)?;
        if !matches!(
            self.kind_of(pid),
            Some(CodexProcessKind::IdeAppServer | CodexProcessKind::Other)
        ) {
            return None;
        }
        let exe = e.exe.clone().or_else(|| e.argv.first().map(PathBuf::from));
        if let Some(ide) = exe.as_deref().and_then(IdeKind::from_extension_path) {
            return Some(ide);
        }
        // The npm wrapper (`node …/codex.js`) carries the extension path in argv.
        if let Some(ide) = e
            .argv
            .iter()
            .take(2)
            .find_map(|a| IdeKind::from_extension_path(Path::new(a)))
        {
            return Some(ide);
        }
        let mut seen = vec![pid];
        let mut cur = e.ppid;
        for _ in 0..8 {
            let Some(pp) = cur.filter(|pp| !seen.contains(pp)) else {
                break;
            };
            seen.push(pp);
            let Some(parent) = self.all.get(&pp) else {
                break;
            };
            let name = parent
                .exe
                .as_deref()
                .and_then(|x| x.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .or_else(|| parent.argv.first().map(|a| base_name(a).to_owned()));
            if let Some(ide) = name.as_deref().and_then(IdeKind::from_host_name) {
                return Some(ide);
            }
            cur = parent.ppid;
        }
        None
    }

    /// Build a [`ProcessInfo`] for a classified pid.
    pub fn process_info(&self, pid: u32) -> Option<ProcessInfo> {
        let e = self.all.get(&pid)?;
        let kind = self.kind_of(pid)?.clone();
        let kind_for_cwd = kind.clone();
        let authapi_env = if kind == CodexProcessKind::AppMain {
            self.app_authapi_env(pid)
        } else {
            e.authapi_observation()
        };
        Some(ProcessInfo {
            pid,
            ppid: e.ppid,
            kind,
            exe: e.exe.clone(),
            cmdline: redact_cmdline(&e.argv),
            started_at: e.started_at,
            authapi_env,
            codex_home: self.codex_home_of(pid),
            stale: false,
            ide: self.ide_of(pid),
            // The folder a terminal codex runs in tells two terminals apart; an
            // editor's app-server runs in the editor's program folder (seen on
            // Windows), which says nothing.
            cwd: matches!(
                kind_for_cwd,
                CodexProcessKind::CliTui | CodexProcessKind::CliExec | CodexProcessKind::CliDaemon
            )
            .then(|| e.cwd.clone())
            .flatten(),
        })
    }
}

/// [`redact_argv`] plus `key=value` words whose key names a secret (Codex `-c
/// api_key=...` overrides) and embedded `sk-...` / `nb_...` tokens.
pub fn redact_cmdline(argv: &[String]) -> Vec<String> {
    const SENSITIVE: &[&str] = &[
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "auth",
        "credential",
    ];
    redact_argv(argv)
        .into_iter()
        .map(|a| {
            if let Some((k, _)) = a.split_once('=') {
                let lk = k.trim_start_matches('-').to_ascii_lowercase();
                if SENSITIVE.iter().any(|w| lk.contains(w)) {
                    return format!("{k}=***");
                }
            }
            mask_api_key_tokens(&a)
        })
        .collect()
}

/// Replace every `sk-...` / `nb_...` token (letters, digits, `-`, `_`) with `***`.
fn mask_api_key_tokens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("sk-").into_iter().chain(rest.find("nb_")).min() {
        let boundary = rest[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let len = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(tail.len());
        if boundary {
            out.push_str("***");
        } else {
            out.push_str(&tail[..len]);
        }
        rest = &tail[len..];
    }
    out.push_str(rest);
    out
}

/// Normalize a path for comparison (canonical when it exists, in the plain
/// `C:\...` form on Windows, trailing separators removed otherwise).
pub fn norm_path(p: &Path) -> PathBuf {
    crate::appx::canonical(p)
        .unwrap_or_else(|_| crate::appx::plain_path(&p.components().collect::<PathBuf>()))
}

/// What to read for each process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadOpts {
    /// Read environments (slower; needed for homes and env observations).
    pub environ: bool,
}

/// Take an OS snapshot of all processes visible to this user.
pub fn snapshot_os(opts: ReadOpts) -> Vec<ProcEntry> {
    use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    let mut kind = ProcessRefreshKind::nothing()
        .with_cmd(UpdateKind::Always)
        .with_exe(UpdateKind::Always)
        .with_user(UpdateKind::Always);
    if opts.environ {
        kind = kind
            .with_environ(UpdateKind::Always)
            .with_cwd(UpdateKind::Always);
    }
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    #[cfg(windows)]
    let our_uid = sys
        .process(sysinfo::Pid::from_u32(std::process::id()))
        .and_then(|p| p.user_id());
    sys.processes()
        .values()
        .filter(|p| p.thread_kind().is_none())
        .map(|p| {
            let argv: Vec<String> = p
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let env = if opts.environ {
                parse_environ(p.environ().iter().map(|e| e.to_string_lossy().into_owned()))
            } else {
                None
            };
            #[cfg(not(windows))]
            let started_at = (p.start_time() > 0).then(|| {
                Timestamp::from_millis(i64::try_from(p.start_time()).unwrap_or(0) * 1000)
            });
            #[cfg(windows)]
            let started_at = crate::osproc::creation_time(p.pid().as_u32()).filter(|t| {
                // Do not associate an already replaced process with metadata
                // collected by sysinfo before this native query.
                t.as_millis() / 1000 == i64::try_from(p.start_time()).unwrap_or(0)
            });
            ProcEntry {
                pid: p.pid().as_u32(),
                ppid: p.parent().map(|pp| pp.as_u32()),
                exe: p.exe().map(Path::to_path_buf),
                argv,
                started_at,
                env,
                zombie: matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead),
                #[cfg(not(windows))]
                foreign_user: foreign_user(p),
                #[cfg(windows)]
                foreign_user: matches!((our_uid, p.user_id()), (Some(ours), Some(owner)) if ours != owner),
                cwd: opts
                    .environ
                    .then(|| p.cwd().map(Path::to_path_buf))
                    .flatten()
                    .filter(|c| !c.as_os_str().is_empty()),
            }
        })
        .collect()
}

/// True when `p` belongs to another OS user (Unix uid comparison).
#[cfg(unix)]
fn foreign_user(p: &sysinfo::Process) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    p.user_id().is_some_and(|uid| **uid != me)
}

#[cfg(not(any(unix, windows)))]
fn foreign_user(_p: &sysinfo::Process) -> bool {
    false
}

/// Executable name of the desktop app's Windows sandbox setup helper.
pub const SANDBOX_SETUP_EXE: &str = "codex-windows-sandbox-setup";

/// The desktop app's first-start sandbox setup processes of this user
/// (`codex-windows-sandbox-setup.exe`, started by its app-server or by the
/// app's sandbox service): while one runs, the app shows "Finishing Windows
/// setup" and restarts its app-server when it is done (seen in the Windows
/// VM: setup 45 s, app-server restart at its end). Pids in snapshot order.
pub fn app_setup_pids(snap: &Snapshot) -> Vec<u32> {
    snap.all
        .values()
        .filter(|e| !e.foreign_user && !e.zombie)
        .filter(|e| {
            let program = e
                .exe
                .as_deref()
                .map(|x| x.to_string_lossy().into_owned())
                .or_else(|| e.argv.first().cloned());
            program
                .is_some_and(|p| strip_exe(base_name(&p)).eq_ignore_ascii_case(SANDBOX_SETUP_EXE))
        })
        .map(|e| e.pid)
        .collect()
}

/// Real app-main detection for OS snapshots: an Electron-style layout
/// ([`is_app_main_exe`]) that is also a recognised ChatGPT / Codex install (other
/// Electron apps ship `resources/app.asar` too), owned by the current user.
pub fn is_codex_app_entry(e: &ProcEntry) -> bool {
    if e.foreign_user {
        return false;
    }
    let exe = e.exe.clone().or_else(|| e.argv.first().map(PathBuf::from));
    exe.is_some_and(|x| is_app_main_exe(&x) && crate::install::inspect_install(&x).is_some())
}

/// Classify an OS snapshot with the real file-based app detection.
pub fn snapshot(opts: ReadOpts) -> Snapshot {
    let entries = snapshot_os(opts);
    Snapshot::classify_entries(entries, &is_codex_app_entry)
}

/// Liveness of one pid with its recorded start time (pid reuse safe).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Running and (when given) the start time matches.
    Alive,
    /// Gone, a zombie, or the pid was reused by another process.
    Gone,
}

/// Start times are compared with this tolerance (OS resolution is often 1 s, and
/// times recorded right after spawn may be rounded differently).
pub const START_TIME_SLACK_MS: i64 = 2_000;

/// Check whether `pid` (started at `started_at`, if known) still runs.
#[cfg(windows)]
pub fn liveness(pid: u32, started_at: Option<Timestamp>) -> Liveness {
    let facts = crate::osproc::facts(pid);
    if facts.gone
        || facts.running == Some(false)
        || !crate::osproc::same_process(started_at, facts.created)
    {
        Liveness::Gone
    } else {
        // An unreadable process is not evidence that it exited.
        Liveness::Alive
    }
}

/// Check the process table on platforms without Windows process handles.
#[cfg(not(windows))]
pub fn liveness(pid: u32, started_at: Option<Timestamp>) -> Liveness {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
    let mut sys = System::new();
    let spid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let Some(p) = sys.process(spid) else {
        return Liveness::Gone;
    };
    if matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead) {
        return Liveness::Gone;
    }
    if let Some(at) = started_at {
        let now = i64::try_from(p.start_time()).unwrap_or(0) * 1000;
        if (now - at.as_millis()).abs() > START_TIME_SLACK_MS {
            return Liveness::Gone;
        }
    }
    Liveness::Alive
}

/// Send a polite termination request (SIGTERM on Unix; `false` elsewhere or when
/// the process is gone).
pub fn terminate(pid: u32) -> bool {
    signal(pid, sysinfo::Signal::Term)
}

/// Send SIGINT (Unix; `false` elsewhere or when the process is gone).
pub fn interrupt(pid: u32) -> bool {
    signal(pid, sysinfo::Signal::Interrupt)
}

/// Send SIGHUP (Unix; `false` elsewhere or when the process is gone).
pub fn hangup(pid: u32) -> bool {
    signal(pid, sysinfo::Signal::Hangup)
}

/// Force-kill a process (SIGKILL / TerminateProcess).
pub fn kill(pid: u32) -> bool {
    signal(pid, sysinfo::Signal::Kill)
}

fn signal(pid: u32, sig: sysinfo::Signal) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let spid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[spid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    match sys.process(spid) {
        Some(p) => match p.kill_with(sig) {
            Some(sent) => sent,
            // Signal unsupported on this platform: only Kill maps everywhere.
            None => sig == sysinfo::Signal::Kill && p.kill(),
        },
        None => false,
    }
}
