//! Diagnostics of the local router: the facts behind `spb router doctor`, the
//! Router page's Diagnostics dialog and the detail of a failed start.
//!
//! [`collect`] gathers what a support request needs in one pass: the router
//! program (where it is, whether it runs, macOS quarantine / translocation /
//! code signature), the service definition and what the service manager says
//! about it (`launchctl print` on macOS), the configuration files, the port, the
//! router's own answer and the end of its log; [`findings`] turns them into the
//! problems found, most important first. Every command goes through the injected
//! [`CommandRunner`] and every path is passed in, so tests simulate a Mac on
//! Linux. Nothing here prints a key or token: secrets are reported as present or
//! missing only.

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use superba_core::{Error, Secret};

use crate::client::AdminClient;
use crate::config::{RouterConfig, RouterSecrets};
use crate::manager::is_transient_location;
use crate::paths::RouterPaths;
use crate::runtime::RuntimeInfo;
use crate::service::{
    CommandRecord, CommandRunner, ServiceDiagnosis, ServiceKind, ServiceManager, ServiceSpec,
    ServiceStatus,
};
use crate::watchdog::{self, Liveness};

/// Lines of `router.log` a report keeps.
pub const LOG_LINES: usize = 30;
/// How long `superba-router --version` may take (macOS may hold a first run
/// while it checks the program).
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
/// Timeout of the other diagnostic commands.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// The macOS quarantine attribute.
pub const QUARANTINE_XATTR: &str = "com.apple.quarantine";

/// What [`collect`] looks at.
pub struct DoctorInput<'a> {
    /// Router files.
    pub paths: &'a RouterPaths,
    /// The service definition Superba installs (program, data dir, log).
    pub spec: &'a ServiceSpec,
    /// The service backend.
    pub service: &'a dyn ServiceManager,
    /// Runs `--version`, `xattr`, `codesign`, `sw_vers`.
    pub runner: &'a dyn CommandRunner,
    /// Check the macOS specifics (quarantine, code signature, protected folders).
    pub macos: bool,
    /// The user's home (protected-folder check).
    pub home: Option<&'a Path>,
    /// Where Superba looked for the router program, in order.
    pub candidates: Vec<PathBuf>,
}

/// The router program.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExeFacts {
    /// The program the service runs.
    pub path: PathBuf,
    /// It exists as a file.
    pub exists: bool,
    /// Its execute permission (`None` off Unix or when it does not exist).
    pub executable: Option<bool>,
    /// Size in bytes.
    pub size: Option<u64>,
    /// `<program> --version`.
    pub version: Option<CommandRecord>,
    /// On a disk image or in a Gatekeeper-translocated copy.
    pub transient: bool,
    /// Inside a folder macOS keeps from background processes (Desktop,
    /// Documents, Downloads, iCloud Drive).
    pub protected_folder: bool,
    /// The quarantine attribute of the program (macOS).
    pub quarantine: Option<String>,
    /// The enclosing `.app` and its quarantine attribute (macOS).
    pub bundle: Option<(PathBuf, Option<String>)>,
    /// `codesign -dv --verbose=2 <program>` (macOS).
    pub codesign: Option<CommandRecord>,
    /// Where Superba looked, with whether a program was there.
    pub candidates: Vec<(PathBuf, bool)>,
}

/// One configured member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberFacts {
    /// Member id (the profile id).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Number of lines.
    pub lines: usize,
    /// `secrets.json` holds a non-empty key for it.
    pub has_key: bool,
}

/// `config.json` and `secrets.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFacts {
    /// `config.json` exists.
    pub config_exists: bool,
    /// Why it could not be used, when it exists.
    pub config_error: Option<String>,
    /// Configured port.
    pub port: Option<u16>,
    /// Members in priority order.
    pub members: Vec<MemberFacts>,
    /// `secrets.json` exists.
    pub secrets_exists: bool,
    /// Why it could not be used, when it exists.
    pub secrets_error: Option<String>,
    /// Its permission bits (Unix).
    pub secrets_mode: Option<u32>,
}

/// `runtime.json` of a router that started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeFacts {
    /// Router pid.
    pub pid: u32,
    /// Port it bound.
    pub port: u16,
    /// Its version.
    pub version: String,
    /// Start time, Unix milliseconds.
    pub started_at_ms: u64,
}

/// What listens on the configured port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortState {
    /// Nothing: the router could bind it.
    Free,
    /// A Superba router answers there.
    Router {
        /// Its version.
        version: String,
        /// Its instance id.
        instance: String,
    },
    /// Another program holds it.
    Busy {
        /// Why the bind failed / what answered.
        detail: String,
    },
}

/// The configured port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortFacts {
    /// Port.
    pub port: u16,
    /// What is there.
    pub state: PortState,
}

/// The router as its admin API answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveFacts {
    /// The expected instance answers.
    pub up: bool,
    /// Why it does not (no secrets).
    pub detail: Option<String>,
    /// Version of the running router.
    pub version: Option<String>,
    /// Members it serves.
    pub members: Option<usize>,
    /// Uptime in milliseconds.
    pub uptime_ms: Option<u64>,
}

/// `router.log`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogFacts {
    /// Path.
    pub path: PathBuf,
    /// It exists.
    pub exists: bool,
    /// Size in bytes.
    pub size: Option<u64>,
    /// The last lines (at most [`LOG_LINES`]).
    pub tail: Vec<String>,
}

/// The service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceFacts {
    /// Backend.
    pub kind: ServiceKind,
    /// Its status, or why it could not be read.
    pub status: std::result::Result<ServiceStatus, String>,
    /// What the service manager reports.
    pub diagnosis: ServiceDiagnosis,
}

/// Everything [`collect`] found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterFacts {
    /// Superba (and router) version of this build.
    pub superba_version: String,
    /// `macos aarch64`, `linux x86_64`, ...
    pub platform: String,
    /// `sw_vers -productVersion` (macOS).
    pub os_version: Option<String>,
    /// The data dir the service is started with.
    pub data_dir: PathBuf,
    /// The router program.
    pub exe: ExeFacts,
    /// The service.
    pub service: ServiceFacts,
    /// Configuration files.
    pub config: ConfigFacts,
    /// `runtime.json`.
    pub runtime: Option<RuntimeFacts>,
    /// The configured port.
    pub port: Option<PortFacts>,
    /// The router's own answer.
    pub live: LiveFacts,
    /// The log.
    pub log: LogFacts,
    /// The command that runs the router in a terminal, as the service does.
    pub foreground: Vec<String>,
    /// Problems found, most important first.
    pub findings: Vec<Finding>,
}

/// A problem [`findings`] found. The English text is [`Finding::english`]; the
/// switch service localizes each kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    /// The router program is missing.
    ExeMissing {
        /// Where it was expected.
        path: PathBuf,
    },
    /// The router program has no execute permission.
    ExeNotExecutable {
        /// The program.
        path: PathBuf,
    },
    /// `--version` failed.
    ExeDoesNotRun {
        /// Exit status and output.
        detail: String,
    },
    /// The program is on a disk image or in a translocated copy.
    ExeTransient {
        /// The program.
        path: PathBuf,
    },
    /// macOS quarantine attribute on the program or its app.
    Quarantined {
        /// The app (or program) to clear.
        path: PathBuf,
    },
    /// The program or data dir is in a folder macOS keeps from background
    /// processes.
    ProtectedFolder {
        /// The path.
        path: PathBuf,
    },
    /// No router configuration yet.
    NotConfigured,
    /// `config.json` cannot be used.
    ConfigInvalid {
        /// Why.
        detail: String,
    },
    /// `secrets.json` is missing or cannot be used.
    SecretsMissing,
    /// Members without a key in `secrets.json`.
    MembersWithoutKey {
        /// Member ids.
        members: Vec<String>,
    },
    /// Configured, but no service definition.
    ServiceNotInstalled,
    /// The service definition starts another program than this Superba's.
    StaleDefinition {
        /// The program in the definition.
        program: PathBuf,
        /// The program of this Superba.
        expected: PathBuf,
    },
    /// The definition exists but the service manager has not loaded it.
    ServiceNotLoaded,
    /// The service keeps exiting and being restarted.
    CrashLoop {
        /// Last exit code.
        exit: Option<i32>,
        /// Last terminating signal.
        signal: Option<String>,
    },
    /// Exit 78 with no line from the router in its log: the service manager
    /// could not start the program at all.
    NotStartedByServiceManager,
    /// Another program holds the configured port.
    PortBusy {
        /// Port.
        port: u16,
        /// What the check saw.
        detail: String,
    },
    /// The router does not answer.
    RouterDown {
        /// What the check saw.
        detail: String,
    },
    /// The last error the router logged.
    LogError {
        /// The log line.
        line: String,
    },
}

impl Finding {
    /// True for problems that keep the router from running (the rest are
    /// warnings or context).
    pub fn is_error(&self) -> bool {
        !matches!(
            self,
            Finding::ProtectedFolder { .. }
                | Finding::NotConfigured
                | Finding::StaleDefinition { .. }
                | Finding::LogError { .. }
                | Finding::RouterDown { .. }
        )
    }

    /// English description with what to do.
    pub fn english(&self) -> String {
        match self {
            Finding::ExeMissing { path } => format!(
                "The router program {} is missing; reinstall Superba.",
                path.display()
            ),
            Finding::ExeNotExecutable { path } => format!(
                "The router program {} is not executable; reinstall Superba (or run chmod +x on it).",
                path.display()
            ),
            Finding::ExeDoesNotRun { detail } => format!(
                "The router program does not run ({detail}); macOS may be blocking it."
            ),
            Finding::ExeTransient { path } => format!(
                "Superba runs from a temporary location ({}); move Superba.app to the Applications folder and open it from there.",
                path.display()
            ),
            Finding::Quarantined { path } => format!(
                "macOS marks {} as downloaded (quarantine), so launchd may refuse to start the router: run  xattr -dr com.apple.quarantine \"{}\"  in Terminal, then turn the router on again.",
                path.display(),
                path.display()
            ),
            Finding::ProtectedFolder { path } => format!(
                "{} is in a folder macOS protects (Desktop, Documents, Downloads or iCloud Drive); background services may not be allowed to use it. Move it elsewhere.",
                path.display()
            ),
            Finding::NotConfigured => {
                "The router is not set up yet: add at least one member that uses an API key.".into()
            }
            Finding::ConfigInvalid { detail } => {
                format!("The router configuration cannot be used: {detail}")
            }
            Finding::SecretsMissing => {
                "The router's key file is missing or unreadable; turn the router on again in Superba to rewrite it.".into()
            }
            Finding::MembersWithoutKey { members } => format!(
                "These members have no key in the router's key file: {}.",
                members.join(", ")
            ),
            Finding::ServiceNotInstalled => {
                "The background service is not installed; turn the router on (or click Install) to install it.".into()
            }
            Finding::StaleDefinition { program, expected } => format!(
                "The background service starts {} but this Superba ships {}; reinstall the service.",
                program.display(),
                expected.display()
            ),
            Finding::ServiceNotLoaded => {
                "The service definition exists but the service manager has not loaded it (on macOS 13+, check System Settings > General > Login Items: superba-router must be allowed in the background).".into()
            }
            Finding::CrashLoop { exit, signal } => {
                let how = match (exit, signal) {
                    (_, Some(signal)) => format!("signal {signal}"),
                    (Some(code), None) => format!("exit code {code}"),
                    (None, None) => "an error".into(),
                };
                format!(
                    "The router keeps stopping right after it starts ({how}); see the log lines below."
                )
            }
            Finding::NotStartedByServiceManager => {
                "The service manager could not start the program (exit 78 without any router output): the program or its log file cannot be used by background services.".into()
            }
            Finding::PortBusy { port, detail } => format!(
                "Port {port} on 127.0.0.1 is used by another program ({detail}); find it with  lsof -nP -iTCP:{port} -sTCP:LISTEN"
            ),
            Finding::RouterDown { detail } => format!("The router does not answer: {detail}"),
            Finding::LogError { line } => format!("Last error in the router log: {line}"),
        }
    }
}

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// The `.app` directory that contains `path`, if any.
pub fn enclosing_bundle(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| a.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// `path` is inside `~/Desktop`, `~/Documents`, `~/Downloads` or iCloud Drive
/// (`~/Library/Mobile Documents`), which macOS (TCC) keeps from background
/// processes that were never granted access.
pub fn in_protected_folder(path: &Path, home: &Path) -> bool {
    [
        "Desktop",
        "Documents",
        "Downloads",
        "Library/Mobile Documents",
    ]
    .iter()
    .any(|d| path.starts_with(home.join(d)))
}

/// The quarantine attribute of `path` (`xattr -p`), `None` when it has none or
/// `xattr` cannot tell.
pub fn quarantine(runner: &dyn CommandRunner, path: &Path) -> Option<String> {
    let mut args = os_args(&["-p", QUARANTINE_XATTR]);
    args.push(path.as_os_str().to_owned());
    let output = runner
        .run_timeout("/usr/bin/xattr", &args, COMMAND_TIMEOUT)
        .ok()?;
    (output.success() && !output.stdout.trim().is_empty()).then(|| output.stdout.trim().to_owned())
}

fn exe_facts(input: &DoctorInput<'_>) -> ExeFacts {
    let path = input.spec.exe.clone();
    let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file());
    #[cfg(unix)]
    let executable = meta.as_ref().map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    });
    #[cfg(not(unix))]
    let executable = None;
    let mut facts = ExeFacts {
        exists: meta.is_some(),
        executable,
        size: meta.as_ref().map(|m| m.len()),
        transient: is_transient_location(&path),
        candidates: input
            .candidates
            .iter()
            .map(|c| (c.clone(), c.is_file()))
            .collect(),
        ..ExeFacts::default()
    };
    if facts.exists {
        facts.version = Some(CommandRecord::run(
            input.runner,
            &path.to_string_lossy(),
            &os_args(&["--version"]),
            VERSION_TIMEOUT,
        ));
    }
    if input.macos {
        if let Some(home) = input.home {
            facts.protected_folder = in_protected_folder(&path, home);
        }
        if facts.exists {
            facts.quarantine = quarantine(input.runner, &path);
            let mut args = os_args(&["-dv", "--verbose=2"]);
            args.push(path.as_os_str().to_owned());
            facts.codesign = Some(CommandRecord::run(
                input.runner,
                "/usr/bin/codesign",
                &args,
                COMMAND_TIMEOUT,
            ));
        }
        facts.bundle = enclosing_bundle(&path).map(|b| {
            let q = quarantine(input.runner, &b);
            (b, q)
        });
    }
    facts.path = path;
    facts
}

fn config_facts(paths: &RouterPaths) -> ConfigFacts {
    let mut facts = ConfigFacts {
        config_exists: paths.config_json().is_file(),
        secrets_exists: paths.secrets_json().is_file(),
        ..ConfigFacts::default()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        facts.secrets_mode = std::fs::metadata(paths.secrets_json())
            .ok()
            .map(|m| m.permissions().mode() & 0o777);
    }
    let secrets = match RouterSecrets::load(paths) {
        Ok(s) => Some(s),
        Err(e) => {
            if facts.secrets_exists {
                facts.secrets_error = Some(e.to_string());
            }
            None
        }
    };
    let has_key = |id: &str| {
        secrets.as_ref().is_some_and(|s| {
            s.member_keys
                .get(id)
                .is_some_and(|k: &Secret| !k.is_empty())
        })
    };
    match RouterConfig::load(paths) {
        Ok(config) => {
            facts.port = Some(config.listen_port).filter(|p| *p != 0);
            facts.members = config
                .members
                .iter()
                .map(|m| MemberFacts {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    lines: m.lines.len(),
                    has_key: has_key(&m.id),
                })
                .collect();
        }
        Err(e) => {
            if facts.config_exists {
                facts.config_error = Some(e.to_string());
            }
        }
    }
    facts
}

/// The last `max` lines of `path` (reads at most its last 64 KiB).
pub fn log_tail(path: &Path, max: usize) -> LogFacts {
    let mut facts = LogFacts {
        path: path.to_path_buf(),
        ..LogFacts::default()
    };
    let Ok(mut file) = std::fs::File::open(path) else {
        return facts;
    };
    facts.exists = true;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    facts.size = Some(len);
    let start = len.saturating_sub(64 * 1024);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_ok() {
        let _ = file.take(64 * 1024).read_to_end(&mut bytes);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // The first line may be cut.
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(max);
    facts.tail = lines[skip..]
        .iter()
        .map(|l| crate::service::cut(l.trim_end(), 400))
        .collect();
    facts
}

/// What is on `port` of 127.0.0.1: free (the router could bind it), a Superba
/// router, or another program.
pub async fn port_state(port: u16) -> PortState {
    let bound = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    let bind_error = match bound {
        Ok(listener) => {
            drop(listener);
            return PortState::Free;
        }
        Err(e) => e.to_string(),
    };
    match AdminClient::new(port, Secret::new("")) {
        Ok(client) => match client.health().await {
            Ok(reply) => PortState::Router {
                version: reply.version,
                instance: reply.instance_id,
            },
            Err(e) => PortState::Busy {
                detail: format!("{bind_error}; {}", short_error(&e)),
            },
        },
        Err(_) => PortState::Busy { detail: bind_error },
    }
}

fn short_error(e: &Error) -> String {
    crate::service::cut(&e.detail(), 160)
}

fn command_text(record: &CommandRecord) -> String {
    match &record.result {
        Ok(o) => {
            let out = if o.stderr.trim().is_empty() {
                o.stdout.trim()
            } else {
                o.stderr.trim()
            };
            let exit = match o.code {
                Some(c) => format!("exit {c}"),
                None => "stopped".into(),
            };
            if out.is_empty() {
                exit
            } else {
                format!("{exit}: {}", crate::service::cut(out, 200))
            }
        }
        Err(e) => e.clone(),
    }
}

/// Gather the facts (several local commands and loopback calls; a few seconds
/// at most, longer only when macOS holds the program's first run).
pub async fn collect(input: DoctorInput<'_>) -> RouterFacts {
    let exe = exe_facts(&input);
    let config = config_facts(input.paths);
    let service = ServiceFacts {
        kind: input.service.kind(),
        status: input.service.status(input.spec).map_err(|e| e.to_string()),
        diagnosis: input.service.diagnose(input.spec),
    };
    let runtime = RuntimeInfo::load(input.paths).ok().map(|r| RuntimeFacts {
        pid: r.pid,
        port: r.port,
        version: r.version,
        started_at_ms: r.started_at_ms,
    });
    let port = match config.port {
        Some(port) => Some(PortFacts {
            port,
            state: port_state(port).await,
        }),
        None => None,
    };
    let live = match watchdog::check(input.paths).await {
        Liveness::Up(s) => LiveFacts {
            up: true,
            detail: None,
            version: Some(s.version.clone()),
            members: Some(s.members.len()),
            uptime_ms: Some(s.uptime_ms),
        },
        Liveness::Down { detail } => LiveFacts {
            up: false,
            detail: Some(detail),
            version: None,
            members: None,
            uptime_ms: None,
        },
    };
    let os_version = if input.macos {
        input
            .runner
            .run_timeout(
                "/usr/bin/sw_vers",
                &os_args(&["-productVersion"]),
                COMMAND_TIMEOUT,
            )
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout.trim().to_owned())
    } else {
        None
    };
    let mut foreground = vec![input.spec.exe.to_string_lossy().into_owned()];
    foreground.extend(
        input
            .spec
            .run_args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned()),
    );
    let mut facts = RouterFacts {
        superba_version: crate::VERSION.into(),
        platform: format!(
            "{} {}",
            if input.macos {
                "macos"
            } else {
                std::env::consts::OS
            },
            std::env::consts::ARCH
        ),
        os_version,
        data_dir: input.spec.data_dir.clone(),
        exe,
        service,
        config,
        runtime,
        port,
        live,
        log: log_tail(&input.spec.log_file, LOG_LINES),
        foreground,
        findings: Vec::new(),
    };
    facts.findings = findings(&facts, input.macos, input.home);
    facts
}

/// The problems in `facts`, most important first.
pub fn findings(facts: &RouterFacts, macos: bool, home: Option<&Path>) -> Vec<Finding> {
    let mut out = Vec::new();
    let exe = &facts.exe;
    if !exe.exists {
        out.push(Finding::ExeMissing {
            path: exe.path.clone(),
        });
    } else if exe.executable == Some(false) {
        out.push(Finding::ExeNotExecutable {
            path: exe.path.clone(),
        });
    }
    if exe.transient {
        out.push(Finding::ExeTransient {
            path: exe.path.clone(),
        });
    }
    if macos {
        let bundle_quarantined = exe.bundle.as_ref().filter(|(_, q)| q.is_some());
        if let Some((bundle, _)) = bundle_quarantined {
            out.push(Finding::Quarantined {
                path: bundle.clone(),
            });
        } else if exe.quarantine.is_some() {
            out.push(Finding::Quarantined {
                path: exe
                    .bundle
                    .as_ref()
                    .map(|(b, _)| b.clone())
                    .unwrap_or_else(|| exe.path.clone()),
            });
        }
        if let Some(home) = home {
            for path in [&exe.path, &facts.data_dir] {
                if in_protected_folder(path, home) {
                    out.push(Finding::ProtectedFolder { path: path.clone() });
                }
            }
        }
    }
    if let Some(version) = &exe.version
        && !version.success()
    {
        out.push(Finding::ExeDoesNotRun {
            detail: command_text(version),
        });
    }
    let config = &facts.config;
    let configured = config.config_exists && config.config_error.is_none();
    if !config.config_exists {
        out.push(Finding::NotConfigured);
    } else if let Some(detail) = &config.config_error {
        out.push(Finding::ConfigInvalid {
            detail: detail.clone(),
        });
    }
    if configured && (!config.secrets_exists || config.secrets_error.is_some()) {
        out.push(Finding::SecretsMissing);
    } else if configured {
        let missing: Vec<String> = config
            .members
            .iter()
            .filter(|m| !m.has_key)
            .map(|m| m.id.clone())
            .collect();
        if !missing.is_empty() {
            out.push(Finding::MembersWithoutKey { members: missing });
        }
    }
    let d = &facts.service.diagnosis;
    let installed = facts.service.status.as_ref().is_ok_and(|s| s.installed);
    if config.config_exists && !installed {
        out.push(Finding::ServiceNotInstalled);
    }
    if installed
        && let Some(program) = &d.program
        && program != &exe.path
    {
        out.push(Finding::StaleDefinition {
            program: program.clone(),
            expected: exe.path.clone(),
        });
    }
    if installed && d.loaded == Some(false) {
        out.push(Finding::ServiceNotLoaded);
    }
    let router_wrote = facts
        .log
        .tail
        .iter()
        .any(|l| l.contains("superba_router") || l.contains("superba-router"));
    if d.crash_looping {
        out.push(Finding::CrashLoop {
            exit: d.last_exit_code,
            signal: d.last_signal.clone(),
        });
    }
    if facts.service.kind == ServiceKind::LaunchAgent
        && d.last_exit_code == Some(78)
        && !router_wrote
    {
        out.push(Finding::NotStartedByServiceManager);
    }
    if let Some(port) = &facts.port
        && let PortState::Busy { detail } = &port.state
        && !facts.live.up
    {
        out.push(Finding::PortBusy {
            port: port.port,
            detail: detail.clone(),
        });
    }
    if configured
        && !facts.live.up
        && let Some(detail) = &facts.live.detail
    {
        out.push(Finding::RouterDown {
            detail: detail.clone(),
        });
    }
    if !facts.live.up
        && let Some(line) = facts.log.tail.iter().rev().find(|l| l.contains(" ERROR "))
    {
        out.push(Finding::LogError { line: line.clone() });
    }
    out
}

/// A short English account of why a started router is not answering, for the
/// error of a failed start: the service manager's view, the port and the last
/// log lines. Synchronous parts only (no network).
pub fn start_failure_detail(
    paths: &RouterPaths,
    spec: &ServiceSpec,
    service: &dyn ServiceManager,
    log_lines: usize,
) -> String {
    let mut parts = Vec::new();
    let d = service.diagnose(spec);
    let keys = [
        "state",
        "runs",
        "last exit code",
        "last terminating signal",
        "job state",
        "pid",
        "error",
    ];
    let facts: Vec<String> = keys
        .iter()
        .filter_map(|k| d.field(k).map(|v| format!("{k} = {v}")))
        .collect();
    let kind = service.kind();
    if d.loaded == Some(false) {
        parts.push(format!("service ({kind:?}): not loaded"));
    } else if !facts.is_empty() {
        parts.push(format!("service ({kind:?}): {}", facts.join(", ")));
    }
    if let Ok(config) = RouterConfig::load(paths)
        && config.listen_port != 0
        && let Err(e) =
            TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, config.listen_port)))
        && RuntimeInfo::load(paths).is_err()
    {
        parts.push(format!("port {} is in use: {e}", config.listen_port));
    }
    let log = log_tail(&spec.log_file, log_lines);
    if !log.tail.is_empty() {
        parts.push(format!(
            "{} (last lines):\n  {}",
            log.path.display(),
            log.tail.join("\n  ")
        ));
    } else {
        parts.push(format!("{}: no output", log.path.display()));
    }
    if kind == ServiceKind::LaunchAgent
        && d.last_exit_code == Some(78)
        && !log
            .tail
            .iter()
            .any(|l| l.contains("superba_router") || l.contains("superba-router"))
    {
        parts.push(Finding::NotStartedByServiceManager.english());
    }
    parts.push("Run `spb router doctor` for a full report.".into());
    parts.join("\n")
}
