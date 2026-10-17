//! Per-user macOS LaunchAgents, independent of the executing platform (every
//! `launchctl` call goes through the injected [`super::CommandRunner`], so the
//! whole lifecycle is unit tested on Linux).
//!
//! launchd behaviour this module relies on (macOS 11 to 15, user domain
//! `gui/<uid>`; it cannot be exercised in CI, see the tests for the simulated
//! sequences):
//!
//! - `launchctl bootout gui/<uid>/<label>` may return before the old process has
//!   exited (it is sent SIGTERM, then SIGKILL after its exit timeout; the call
//!   can even fail with 36 "Operation now in progress"). Bootstrapping the same
//!   label again too early fails with 37 "Operation already in progress" or 5
//!   "Input/output error". So `install` waits until `launchctl print` no longer
//!   finds the label, then bootstraps and retries 5 / 36 / 37 for a few seconds.
//! - `bootstrap` also fails with 5 for permanent reasons: the label is still
//!   loaded, the plist has a bad owner or mode, or (macOS 13+) the user turned
//!   the item off under System Settings > General > Login Items ("Allow in the
//!   Background"). A failed bootstrap of a label that `print` then shows loaded
//!   is a bootstrap that went through (another Superba process, or an earlier
//!   attempt reported late).
//! - `launchctl print` exits with 113 "Could not find service" for an unloaded
//!   label. Its output nests blocks (`arguments = { ... }`, `resource coalition
//!   = { ... state = active ... }`): only the job's own top-level keys are read.
//! - A job that exits with 78 (EX_CONFIG) without writing anything was usually
//!   never started by xpcproxy: the program cannot be executed or the log file
//!   cannot be opened. superba-router itself also exits with 78 when its
//!   configuration is missing or invalid (it logs why first).
//! - `kickstart -k` restarts a loaded job only; an unloaded one is bootstrapped.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use superba_core::{Error, Result};

use super::system::{atomic_write, path_text, remove_file, validate};
use super::{
    CommandOutput, CommandRecord, LAUNCHD_LABEL, NOFILE_HARD, NOFILE_SOFT, ServiceDiagnosis,
    ServiceHost, ServiceKind, ServiceManager, ServiceSpec, ServiceStatus, command_line,
};

/// `launchctl` by absolute path: an app started from the Finder or a login item
/// may have an unusual `PATH`.
pub const LAUNCHCTL: &str = "/bin/launchctl";

/// Interval between `launchctl print` polls.
const POLL: Duration = Duration::from_millis(250);
/// Polls while waiting for a booted-out router to go away (the router's own
/// graceful shutdown takes up to 6 s; launchd kills it after its exit timeout).
const UNLOAD_POLLS: u32 = 48;
/// `bootstrap` attempts for transient failures (5 / 36 / 37), [`BOOTSTRAP_RETRY`] apart.
const BOOTSTRAP_ATTEMPTS: u32 = 12;
/// Delay between `bootstrap` attempts.
const BOOTSTRAP_RETRY: Duration = Duration::from_millis(500);
/// Longest launchctl message kept in an error.
const MESSAGE_MAX: usize = 400;

#[derive(Debug)]
pub(super) struct LaunchAgent {
    host: ServiceHost,
}

impl LaunchAgent {
    pub(super) fn new(host: ServiceHost) -> Self {
        Self { host }
    }

    fn path(&self) -> PathBuf {
        self.host
            .home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"))
    }

    fn domain(&self) -> Result<String> {
        self.host
            .uid
            .map(|uid| format!("gui/{uid}"))
            .ok_or_else(|| Error::Unsupported {
                what: "LaunchAgent requires the current user's uid".into(),
            })
    }

    fn target(&self) -> Result<String> {
        Ok(format!("{}/{LAUNCHD_LABEL}", self.domain()?))
    }

    fn launchctl(&self, args: &[&str]) -> std::io::Result<CommandOutput> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        self.host.runner.run(LAUNCHCTL, &args)
    }

    /// `launchctl print <target>`.
    fn print(&self, target: &str) -> std::io::Result<CommandOutput> {
        self.launchctl(&["print", target])
    }

    /// launchd has the label loaded (`print` succeeds); false when it cannot tell.
    fn loaded(&self, target: &str) -> bool {
        self.print(target).is_ok_and(|o| o.success())
    }

    /// Boot the label out and wait until launchd no longer has it. Returns the
    /// `bootout` answer when the label is still loaded afterwards.
    fn unload(&self, target: &str) -> std::result::Result<(), Option<CommandOutput>> {
        let bootout = self.launchctl(&["bootout", target]).ok();
        for poll in 0..=UNLOAD_POLLS {
            if !self.loaded(target) {
                return Ok(());
            }
            if poll < UNLOAD_POLLS {
                self.host.runner.sleep(POLL);
            }
        }
        Err(bootout)
    }

    /// `bootstrap <domain> <plist>`, retrying the transient failures that follow
    /// a bootout.
    fn bootstrap(&self, domain: &str, target: &str) -> Result<()> {
        let plist = self.path();
        let args: Vec<OsString> = vec!["bootstrap".into(), domain.into(), plist.clone().into()];
        let mut last = None;
        for attempt in 0..BOOTSTRAP_ATTEMPTS {
            let output = self
                .host
                .runner
                .run(LAUNCHCTL, &args)
                .map_err(|e| Error::Process {
                    message: format!("{}: {e}", command_line(LAUNCHCTL, &args)),
                })?;
            if output.success() {
                return Ok(());
            }
            if self.loaded(target) {
                return Ok(());
            }
            let retry = transient_bootstrap_failure(&output);
            last = Some(output);
            if !retry {
                break;
            }
            if attempt + 1 < BOOTSTRAP_ATTEMPTS {
                self.host.runner.sleep(BOOTSTRAP_RETRY);
            }
        }
        let output = last.unwrap_or_default();
        Err(Error::Process {
            message: format!(
                "{} failed ({}): {}{}",
                command_line(LAUNCHCTL, &args),
                output.exit_text(),
                output.message(MESSAGE_MAX),
                bootstrap_hint(&output)
            ),
        })
    }

    fn plist(&self, spec: &ServiceSpec) -> Result<String> {
        let mut plist = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n",
        );
        plist.push_str(&format!(
            "  <key>Label</key>\n  <string>{LAUNCHD_LABEL}</string>\n"
        ));
        plist.push_str("  <key>ProgramArguments</key>\n  <array>\n");
        for arg in [
            path_text(&spec.exe)?,
            "run",
            "--data-dir",
            path_text(&spec.data_dir)?,
        ] {
            plist.push_str(&format!("    <string>{}</string>\n", xml(arg)));
        }
        let log = xml(path_text(&spec.log_file)?);
        for line in [
            "  </array>".to_owned(),
            "  <key>RunAtLoad</key>".to_owned(),
            "  <true/>".to_owned(),
            "  <key>KeepAlive</key>".to_owned(),
            "  <true/>".to_owned(),
            // Interactive, not Background: the router is on the latency path of
            // every Codex request the user makes (launchd.plist(5) throttles
            // Background jobs' CPU, I/O and network).
            "  <key>ProcessType</key>".to_owned(),
            "  <string>Interactive</string>".to_owned(),
            "  <key>ThrottleInterval</key>".to_owned(),
            "  <integer>5</integer>".to_owned(),
            // launchd starts agents with a soft limit of 256 open files, which
            // ~110 concurrent requests exhaust (user bug 35).
            "  <key>SoftResourceLimits</key>".to_owned(),
            "  <dict>".to_owned(),
            "    <key>NumberOfFiles</key>".to_owned(),
            format!("    <integer>{NOFILE_SOFT}</integer>"),
            "  </dict>".to_owned(),
            "  <key>HardResourceLimits</key>".to_owned(),
            "  <dict>".to_owned(),
            "    <key>NumberOfFiles</key>".to_owned(),
            format!("    <integer>{NOFILE_HARD}</integer>"),
            "  </dict>".to_owned(),
            "  <key>StandardOutPath</key>".to_owned(),
            format!("  <string>{log}</string>"),
            "  <key>StandardErrorPath</key>".to_owned(),
            format!("  <string>{log}</string>"),
        ] {
            plist.push_str(&line);
            plist.push('\n');
        }
        if !spec.env.is_empty() {
            plist.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
            for (key, value) in &spec.env {
                plist.push_str(&format!(
                    "    <key>{}</key>\n    <string>{}</string>\n",
                    xml(key),
                    xml(value)
                ));
            }
            plist.push_str("  </dict>\n");
        }
        plist.push_str("</dict>\n</plist>\n");
        Ok(plist)
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unxml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The first `<string>` of `ProgramArguments` in a plist Superba wrote.
fn plist_program(plist: &str) -> Option<PathBuf> {
    let rest = &plist[plist.find("<key>ProgramArguments</key>")?..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    Some(PathBuf::from(unxml(&rest[start..start + end])))
}

/// launchctl's error number in `Bootstrap failed: 5: Input/output error`, else
/// the exit code (launchctl exits with the error number).
fn launchctl_error(output: &CommandOutput) -> Option<i32> {
    let text = format!("{}\n{}", output.stderr, output.stdout);
    text.split("failed: ")
        .nth(1)
        .and_then(|rest| rest.split(':').next())
        .and_then(|n| n.trim().parse().ok())
        .or(output.code)
}

/// 5 (EIO), 36 (EINPROGRESS) and 37 (EALREADY) right after a bootout: launchd is
/// still tearing the previous instance down.
fn transient_bootstrap_failure(output: &CommandOutput) -> bool {
    matches!(launchctl_error(output), Some(5 | 36 | 37))
        || output.stderr.contains("Input/output error")
        || output.stderr.contains("in progress")
}

/// What a bootstrap error usually means on a user's Mac.
fn bootstrap_hint(output: &CommandOutput) -> &'static str {
    match launchctl_error(output) {
        Some(5) => {
            ". macOS refused to load the router's LaunchAgent. On macOS 13 and later this \
             happens when \"superba-router\" is turned off under System Settings > General > \
             Login Items (Allow in the Background): turn it on and try again. Run \
             `spb router doctor` for details"
        }
        Some(36 | 37) => {
            ". launchd is still stopping the previous router; try again in a few seconds"
        }
        Some(122) => {
            ". The plist or ~/Library/LaunchAgents has the wrong owner or permissions \
             (it must belong to you and not be writable by others)"
        }
        Some(112 | 125) => {
            ". There is no graphical login session for this user (for example over SSH); \
             log in on the Mac itself and try again"
        }
        Some(119) => {
            ". The service is disabled: run `launchctl enable gui/$(id -u)/com.krill.superba.router` \
             and try again"
        }
        _ => ". Run `spb router doctor` for details",
    }
}

/// Top-level `key = value` pairs of `launchctl print <service>`, in order.
/// Nested blocks (`arguments = { ... }`, `environment = { ... }`, `resource
/// coalition = { ... }`, ...) are skipped: their `state = active` lines describe
/// coalitions, not the job.
pub fn launchctl_print_fields(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line == "}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if let Some(head) = line.strip_suffix('{') {
            // A block of the job (`arguments = {`): note that it exists. The
            // header line (`gui/501/<label> = {`) opens the job itself.
            if depth == 1
                && let Some((key, _)) = head.split_once('=')
            {
                out.push((key.trim().to_owned(), "{…}".to_owned()));
            }
            depth += 1;
            continue;
        }
        if depth <= 1
            && let Some((key, value)) = line.split_once(" = ")
        {
            out.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    out
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

impl ServiceManager for LaunchAgent {
    fn kind(&self) -> ServiceKind {
        ServiceKind::LaunchAgent
    }

    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let domain = self.domain()?;
        let target = self.target()?;
        validate(&self.host, spec)?;
        let plist = self.plist(spec)?;
        // launchd opens StandardOutPath itself and does not create directories:
        // a missing log directory makes the job exit 78 before the router runs.
        if let Some(parent) = spec.log_file.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, &e))?;
        }
        atomic_write(&self.path(), &plist, 0o644)?;
        if let Err(bootout) = self.unload(&target) {
            let answer = bootout
                .map(|o| format!(" ({}: {})", o.exit_text(), o.message(MESSAGE_MAX)))
                .unwrap_or_default();
            return Err(Error::Process {
                message: format!(
                    "the router service that is already loaded did not stop: `{LAUNCHCTL} \
                     bootout {target}` did not unload it within {} s{answer}. Run `spb router \
                     doctor` for details",
                    (POLL * UNLOAD_POLLS).as_secs()
                ),
            });
        }
        // Clears a "disabled" override left by `launchctl disable` (ignored when
        // there is none).
        let _ = self.launchctl(&["enable", &target]);
        self.bootstrap(&domain, &target)?;
        self.status(spec)
    }

    fn uninstall(&self, spec: &ServiceSpec) -> Result<()> {
        let target = self.target()?;
        validate(&self.host, spec)?;
        let _ = self.launchctl(&["bootout", &target]);
        remove_file(&self.path())
    }

    fn status(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let target = self.target()?;
        validate(&self.host, spec)?;
        let mut status = ServiceStatus {
            kind: self.kind(),
            installed: self.path().exists(),
            running: None,
            pid: None,
            detail: None,
        };
        if let Ok(output) = self.print(&target) {
            status.running = Some(false);
            if output.success() {
                let fields = launchctl_print_fields(&output.stdout);
                if let Some(state) = field(&fields, "state") {
                    status.running = Some(state == "running");
                    status.detail = Some(state.to_owned());
                }
                status.pid = field(&fields, "pid")
                    .and_then(|v| v.parse().ok())
                    .filter(|pid| *pid != 0);
            }
        }
        Ok(status)
    }

    fn restart(&self, spec: &ServiceSpec) -> Result<()> {
        let target = self.target()?;
        validate(&self.host, spec)?;
        if self.loaded(&target)
            && self
                .launchctl(&["kickstart", "-k", &target])
                .is_ok_and(|o| o.success())
        {
            return Ok(());
        }
        // Not loaded (turned off under Login Items, booted out by hand, removed
        // after a failed start) or kickstart refused: load the definition again.
        self.install(spec).map(|_| ())
    }

    fn diagnose(&self, spec: &ServiceSpec) -> ServiceDiagnosis {
        let path = self.path();
        let definition = std::fs::read_to_string(&path).ok();
        let mut d = ServiceDiagnosis {
            program: definition.as_deref().and_then(plist_program),
            definition_path: Some(path),
            definition,
            ..ServiceDiagnosis::default()
        };
        let Ok(target) = self.target() else {
            d.fields
                .push(("error".into(), "the current user's uid is unknown".into()));
            return d;
        };
        let _ = spec;
        let args: Vec<OsString> = vec!["print".into(), target.into()];
        let record = CommandRecord::run(
            self.host.runner.as_ref(),
            LAUNCHCTL,
            &args,
            Duration::from_secs(10),
        );
        if let Ok(output) = &record.result {
            d.loaded = Some(output.code == Some(0));
            if output.code == Some(0) {
                d.fields = launchctl_print_fields(&output.stdout)
                    .into_iter()
                    .filter(|(_, v)| v != "{…}")
                    .collect();
            }
        }
        let state = d.field("state").map(str::to_owned);
        let runs: u32 = d.field("runs").and_then(|v| v.parse().ok()).unwrap_or(0);
        d.last_exit_code = d
            .field("last exit code")
            .and_then(|v| v.split(':').next())
            .and_then(|v| v.trim().parse().ok());
        d.last_signal = d.field("last terminating signal").map(str::to_owned);
        d.crash_looping = d.loaded == Some(true)
            && state.as_deref() != Some("running")
            && runs >= 2
            && (d.last_exit_code.is_some_and(|c| c != 0) || d.last_signal.is_some());
        d.query = Some(record);
        d
    }
}
