//! Launching the desktop app with loopback CDP (plan 6.1, 12.1).
//!
//! CDP flags: always loopback (`--remote-debugging-address` is never passed and is
//! stripped from user-supplied arguments). With a known port Superba also passes
//! `--remote-allow-origins=http://127.0.0.1:<port>`. In [`superba_core::CdpMode::Auto`]
//! on Linux/macOS the app gets `--remote-debugging-port=0` and the real port is
//! read from `<userData>/DevToolsActivePort` (verified on Linux app 26.1007:
//! written within ~1 s, no port race). Elsewhere a free port is reserved and
//! verified after start; macOS still needs native runtime verification.
//!
//! Environment: the app merges the user's login-shell environment into its own at
//! start-up and the shell wins for variables the rc files set (verified), so a
//! value injected here only sticks if the rc files do not override it. Superba's
//! own rc marker block (written by `EnvPersister` before a launch) carries the same
//! value; the observed value is read back from the app-server child.
//!
//! Platforms:
//! - Linux: spawn the executable in its own process group, output to
//!   `logs/app-launch.log`, reaped by a background thread.
//! - macOS: `/usr/bin/open -n -a <bundle> --env NAME=VALUE ... --args ...` launches
//!   through LaunchServices, so privacy checks belong to the app itself. Only
//!   `env_set` is forwarded: the app does not inherit Superba's environment, which
//!   also satisfies `env_remove`. Callers must quit a running instance first.
//! - Windows: MSIX packages are started with
//!   `IApplicationActivationManager::ActivateApplication(AUMID, args)` (starting
//!   the packaged exe by path loses the package identity); the environment cannot
//!   be injected there (it comes from `HKCU\Environment`). Other installs are
//!   spawned directly.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use superba_core::codex::ENV_CODEX_CLI_PATH;
use superba_core::{AppInstall, CdpMode, Error, LaunchOptions, Result, Timestamp};

/// Flags Superba owns; removed from user-supplied arguments.
pub const OWNED_FLAG_PREFIXES: &[&str] = &[
    "--remote-debugging-port",
    "--remote-debugging-address",
    "--remote-debugging-pipe",
    "--remote-allow-origins",
];

/// How the CDP port is determined for a launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdpPlan {
    /// No CDP (safe mode).
    Disabled,
    /// This port was passed explicitly.
    Fixed(u16),
    /// `--remote-debugging-port=0`; read the port from this `DevToolsActivePort` file.
    ActivePortFile(PathBuf),
}

/// Everything needed to start the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Program to run.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
    /// Variables to set.
    pub env_set: BTreeMap<String, String>,
    /// Variables to remove.
    pub env_remove: Vec<String>,
    /// CDP plan.
    pub cdp: CdpPlan,
}

/// CDP arguments for a port (`0` = let the browser choose; then no allow-origins,
/// which leaves every Origin-carrying client rejected).
pub fn cdp_args(port: u16) -> Vec<String> {
    let mut v = vec![format!("--remote-debugging-port={port}")];
    if port != 0 {
        v.push(format!("--remote-allow-origins=http://127.0.0.1:{port}"));
    }
    v
}

/// Remove flags Superba owns from user-supplied arguments.
pub fn sanitize_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if let Some(prefix) = OWNED_FLAG_PREFIXES.iter().find(|p| a.starts_with(**p)) {
            // `--flag value` form: drop the value too.
            if a.len() == prefix.len() {
                skip_next = true;
            }
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Split `SUPERBA_APP_ARGS` (whitespace separated).
pub fn split_app_args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// Environment changes for an isolated `HOME` (e2e): `HOME` (and `USERPROFILE` on
/// Windows) point at it, `ZDOTDIR` too (so the app's login-shell probe reads the
/// isolated zsh rc), and inherited XDG base directories and `CODEX_HOME` are
/// removed so nothing resolves into the real home.
pub fn home_override_env(home: &Path) -> (BTreeMap<String, String>, Vec<String>) {
    let h = home.to_string_lossy().into_owned();
    let mut set = BTreeMap::new();
    set.insert("HOME".to_owned(), h.clone());
    set.insert("ZDOTDIR".to_owned(), h.clone());
    if cfg!(windows) {
        set.insert("USERPROFILE".to_owned(), h);
    }
    let remove = [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "CODEX_HOME",
        "CODEX_SQLITE_HOME",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    (set, remove)
}

/// Build the launch spec. `app_args` are the `SUPERBA_APP_ARGS`; `cdp` is the
/// resolved plan; `program` is the executable to run.
pub fn build_spec(
    program: &Path,
    options: &LaunchOptions,
    app_args: &[String],
    cdp: CdpPlan,
) -> LaunchSpec {
    let mut args = Vec::new();
    match &cdp {
        CdpPlan::Disabled => {}
        CdpPlan::Fixed(p) => args.extend(cdp_args(*p)),
        CdpPlan::ActivePortFile(_) => args.extend(cdp_args(0)),
    }
    args.extend(sanitize_args(app_args));
    args.extend(sanitize_args(&options.extra_args));
    let mut env_set = BTreeMap::new();
    let mut env_remove: Vec<String> = Vec::new();
    if let Some(home) = &options.home_override {
        let (set, remove) = home_override_env(home);
        env_set.extend(set);
        env_remove.extend(remove);
    }
    for k in &options.env_remove {
        env_set.remove(k);
        if !env_remove.contains(k) {
            env_remove.push(k.clone());
        }
    }
    for (k, v) in &options.env {
        env_remove.retain(|r| r != k);
        env_set.insert(k.clone(), v.clone());
    }
    // v0.3: the app's Codex is pinned only through `LaunchOptions::env`; a value
    // inherited from Superba's own environment must never pin it silently.
    if !env_set.contains_key(ENV_CODEX_CLI_PATH)
        && !env_remove.iter().any(|k| k == ENV_CODEX_CLI_PATH)
    {
        env_remove.push(ENV_CODEX_CLI_PATH.to_owned());
    }
    LaunchSpec {
        program: program.to_path_buf(),
        args,
        env_set,
        env_remove,
        cdp,
    }
}

/// Decide the CDP plan. `user_data_dir` is the app's Electron user-data directory
/// when known (needed for `DevToolsActivePort`).
pub fn cdp_plan(mode: CdpMode, user_data_dir: Option<&Path>) -> Result<CdpPlan> {
    Ok(match mode {
        CdpMode::Disabled => CdpPlan::Disabled,
        CdpMode::Port(0) => return Err(Error::invalid("CDP port 0 is reserved for Auto")),
        CdpMode::Port(p) => CdpPlan::Fixed(p),
        CdpMode::Auto => match user_data_dir {
            Some(dir) if cfg!(any(target_os = "linux", target_os = "macos")) => {
                CdpPlan::ActivePortFile(dir.join("DevToolsActivePort"))
            }
            _ => CdpPlan::Fixed(crate::cdp::free_loopback_port()?),
        },
    })
}

/// The program to start for an install: the executable (on macOS the binary inside
/// the bundle).
pub fn program_for(install: &AppInstall) -> PathBuf {
    install.executable.clone()
}

/// LaunchServices arguments, with deterministic environment ordering and no shell
/// quoting. Everything after `--args` is passed to the app unchanged.
///
/// LaunchServices does not inherit Superba's environment, so `env_remove` needs
/// nothing, except for `CODEX_CLI_PATH`: a value in launchd's environment
/// (`launchctl setenv`) would still pin the app's Codex, so a removed
/// `CODEX_CLI_PATH` is passed empty (the app ignores an empty value and uses its
/// bundled Codex; macOS behaviour not verified on a Mac yet).
pub fn macos_open_argv(
    bundle: &Path,
    env: &BTreeMap<String, String>,
    env_remove: &[String],
    args: &[String],
) -> Vec<OsString> {
    let mut argv = vec!["-n".into(), "-a".into(), bundle.as_os_str().to_owned()];
    let mut env = env.clone();
    if env_remove.iter().any(|k| k == ENV_CODEX_CLI_PATH) {
        env.entry(ENV_CODEX_CLI_PATH.to_owned()).or_default();
    }
    for (name, value) in &env {
        argv.push("--env".into());
        argv.push(format!("{name}={value}").into());
    }
    argv.push("--args".into());
    argv.extend(args.iter().map(OsString::from));
    argv
}

/// Find the new bundle main process after LaunchServices accepted the launch.
/// OS start times have second resolution, so compare at that resolution and
/// exclude every pre-launch pid, including processes started in the same second.
pub fn macos_launched_pid(
    entries: &[crate::procs::ProcEntry],
    executable: &Path,
    before: &[u32],
    launched_at: Timestamp,
) -> Option<u32> {
    entries
        .iter()
        .filter(|e| {
            !e.zombie
                && !e.foreign_user
                && !before.contains(&e.pid)
                && e.exe.as_deref() == Some(executable)
                && !crate::procs::is_electron_child(&e.argv)
                && e.started_at
                    .is_some_and(|at| at.as_millis() / 1_000 >= launched_at.as_millis() / 1_000)
        })
        .min_by_key(|e| (e.started_at, e.pid))
        .map(|e| e.pid)
}

/// A started process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawned {
    /// pid of the main process (discovered after LaunchServices activation on
    /// macOS, returned by MSIX activation or direct spawning elsewhere).
    pub pid: Option<u32>,
}

/// Start the process described by `spec`. `log` receives stdout/stderr where the
/// platform allows it. `install` identifies the macOS bundle or Windows package.
pub fn spawn(spec: &LaunchSpec, install: &AppInstall, log: Option<&Path>) -> Result<Spawned> {
    imp::spawn(spec, install, log)
}

#[cfg(not(target_os = "macos"))]
fn configure_command(spec: &LaunchSpec, log: Option<&Path>) -> std::process::Command {
    let mut cmd = std::process::Command::new(&spec.program);
    cmd.args(&spec.args);
    for k in &spec.env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in &spec.env_set {
        cmd.env(k, v);
    }
    if let Some(dir) = spec.program.parent() {
        cmd.current_dir(dir);
    }
    configure_stdio(&mut cmd, log);
    cmd
}

fn configure_stdio(cmd: &mut std::process::Command, log: Option<&Path>) {
    use std::process::Stdio;

    cmd.stdin(Stdio::null());
    let log_file = log.and_then(|p| {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::File::create(p).ok()
    });
    match log_file.and_then(|f| f.try_clone().ok().map(|g| (f, g))) {
        Some((out, err)) => {
            cmd.stdout(Stdio::from(out));
            cmd.stderr(Stdio::from(err));
        }
        None => {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn spawn_direct(mut cmd: std::process::Command) -> Result<Spawned> {
    let mut child = cmd.spawn().map_err(|e| Error::Process {
        message: format!("cannot start the app: {e}"),
    })?;
    let pid = child.id();
    // Reap the child when it exits so it never lingers as a zombie (a zombie still
    // answers liveness checks).
    let _ = std::thread::Builder::new()
        .name(format!("superba-reap-{pid}"))
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(Spawned { pid: Some(pid) })
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::os::unix::process::CommandExt;
    use std::path::Path;

    use superba_core::{AppInstall, Result};

    use super::{LaunchSpec, Spawned, configure_command, spawn_direct};

    pub(super) fn spawn(
        spec: &LaunchSpec,
        _install: &AppInstall,
        log: Option<&Path>,
    ) -> Result<Spawned> {
        let mut cmd = configure_command(spec, log);
        // Own process group: a Ctrl-C in the terminal running `spb` must not reach
        // the app.
        cmd.process_group(0);
        spawn_direct(cmd)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! LaunchServices gives the app its own TCC responsibility. Native launch
    //! and privacy behavior must be checked on macOS; argv/PID selection are pure.
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use superba_core::{AppInstall, Error, Result, Timestamp};

    use super::{LaunchSpec, Spawned, configure_stdio, macos_launched_pid, macos_open_argv};
    use crate::procs::{ReadOpts, norm_path, snapshot_os};

    pub(super) fn spawn(
        spec: &LaunchSpec,
        install: &AppInstall,
        log: Option<&Path>,
    ) -> Result<Spawned> {
        let before: Vec<u32> = snapshot_os(ReadOpts::default())
            .iter()
            .map(|e| e.pid)
            .collect();
        let executable = norm_path(&spec.program);
        let mut cmd = Command::new("/usr/bin/open");
        cmd.args(macos_open_argv(
            &install.path,
            &spec.env_set,
            &spec.env_remove,
            &spec.args,
        ));
        // LaunchServices does not inherit this process's environment; omitted
        // env_remove entries therefore never reach the app through Superba.
        configure_stdio(&mut cmd, log);
        let launched_at = Timestamp::now();
        let mut child = cmd.spawn().map_err(|e| Error::Process {
            message: format!("cannot launch the app through LaunchServices: {e}"),
        })?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().map_err(|e| Error::Process {
                message: format!("cannot wait for LaunchServices: {e}"),
            })? {
                if !status.success() {
                    return Err(Error::Process {
                        message: format!("LaunchServices rejected the app launch: {status}"),
                    });
                }
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout {
                    what: "LaunchServices accepting the app launch".into(),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut entries = snapshot_os(ReadOpts::default());
            for e in &mut entries {
                e.exe = e.exe.as_deref().map(norm_path);
            }
            if let Some(pid) = macos_launched_pid(&entries, &executable, &before, launched_at) {
                return Ok(Spawned { pid: Some(pid) });
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    what: "the app main process after LaunchServices activation".into(),
                });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
mod imp {
    //! Windows: MSIX activation through COM, direct spawn otherwise. Not exercised
    //! in CI (Linux); kept small. Environment variables cannot be passed to an
    //! activated package.
    use std::os::windows::process::CommandExt;
    use std::path::Path;

    use superba_core::{AppInstall, Error, Result};

    use super::{LaunchSpec, Spawned, configure_command, spawn_direct};

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    fn aumid(install: &AppInstall) -> Option<String> {
        // MSIX installs carry their package family name as id
        // (`OpenAI.Codex_2p2nqsd0c76g0`); the application id comes from the manifest.
        if !install.id.contains('_') || install.id.contains(':') {
            return None;
        }
        let manifest = std::fs::read_to_string(install.path.join("AppxManifest.xml")).ok();
        let app_id = manifest
            .as_deref()
            .and_then(crate::install::parse_appx_application_id)
            .unwrap_or_else(|| "App".to_owned());
        Some(format!("{}!{}", install.id, app_id))
    }

    fn quote_arg(a: &str) -> String {
        if !a.is_empty() && !a.contains([' ', '\t', '"']) {
            return a.to_owned();
        }
        format!("\"{}\"", a.replace('"', "\\\""))
    }

    fn activate(aumid: &str, args: &[String]) -> Result<u32> {
        use windows::Win32::System::Com::{
            CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        };
        use windows::Win32::UI::Shell::{
            AO_NONE, ApplicationActivationManager, IApplicationActivationManager,
        };
        use windows::core::HSTRING;
        let joined = args
            .iter()
            .map(|a| quote_arg(a))
            .collect::<Vec<_>>()
            .join(" ");
        // SAFETY: plain COM calls on this thread; the interface pointer is released
        // when dropped and COM is uninitialized afterwards.
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = (|| -> windows::core::Result<u32> {
                let mgr: IApplicationActivationManager =
                    CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
                mgr.ActivateApplication(&HSTRING::from(aumid), &HSTRING::from(joined), AO_NONE)
            })();
            if init.is_ok() {
                CoUninitialize();
            }
            result.map_err(|e| Error::Process {
                message: format!("ActivateApplication({aumid}) failed: {e}"),
            })
        }
    }

    pub(super) fn spawn(
        spec: &LaunchSpec,
        install: &AppInstall,
        log: Option<&Path>,
    ) -> Result<Spawned> {
        if let Some(id) = aumid(install) {
            let pid = activate(&id, &spec.args)?;
            return Ok(Spawned { pid: Some(pid) });
        }
        let mut cmd = configure_command(spec, log);
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        spawn_direct(cmd)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    use superba_core::{AppInstall, Error, Result};

    use super::{LaunchSpec, Spawned};

    pub(super) fn spawn(_: &LaunchSpec, _: &AppInstall, _: Option<&Path>) -> Result<Spawned> {
        Err(Error::Unsupported {
            what: "launching the desktop app on this platform".into(),
        })
    }
}
