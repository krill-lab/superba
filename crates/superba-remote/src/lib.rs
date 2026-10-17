//! # superba-remote
//!
//! SSH remote hosts (plan 8). [`SshRemoteHosts`] implements [`RemoteHosts`] with
//! the system OpenSSH client:
//!
//! - **Discovery** ([`discover`]): `<CODEX_HOME>/.codex-global-state.json`
//!   (`codex-managed-remote-connections`, `remote-projects`, auto-connect flags;
//!   `.bak` fallback), `<CODEX_HOME>/codex-app/config.json` declarations, hosts the
//!   renderer reported (`AppController::remote_connections`), live ssh links
//!   (`AppController::processes`) and `~/.ssh/config` as suggestions. Spike S1 status:
//!   the key names and entry shapes come from static analysis of the desktop app
//!   26.1013 bundle (current `{hostId, displayName, source, alias, hostname, sshPort,
//!   identity}` and the legacy `{sshAlias, sshHost, ...}` form); they have not yet been
//!   observed in a live global-state file, so parsing is defensive and the CDP source
//!   (`from_app`) stays authoritative when present.
//! - **Invocation** ([`ssh`]): alias first (reuses `~/.ssh/config`), else
//!   `-i identity -o IdentitiesOnly=yes -p port -l user host`; always
//!   `-o BatchMode=yes -o ConnectTimeout=8`, never touching host key policy. Every
//!   operation is one `ssh ... sh -s` run of a POSIX script fed on stdin ([`script`]);
//!   keys and file contents travel only on stdin.
//! - **Probe**: OS, login shell, the login shell's view (the app starts the remote
//!   app-server with `$SHELL -l -i -c`, so `CODEX_HOME`, `CODEX_AUTHAPI_BASE_URL` and
//!   `codex` are read the same way), `config.toml` / `auth.json`, rc files, machine
//!   id, the `remote-state.json` marker and app-server pids.
//! - **Plan / apply**: remote file bytes come from the injected
//!   [`ConfigEngine::render`] (byte-identical to local results); writes take a
//!   per-home lock (`<CODEX_HOME>/.superba/lock`), are compare-and-swap against the
//!   bytes read just before (`cmp`, checked up front and again right before each
//!   file is published), staged in exclusive private directories next to their
//!   targets and moved into place (`umask 077`, `auth.json` 0600 also through a
//!   symlink, symlinked files written through and verified), all-or-nothing per
//!   host with a checked undo, retried up to 3 times on conflict, with timestamped
//!   backups in `<CODEX_HOME>/.superba/backups/<id>/` and one-time first-write
//!   pre-images (a failure to record them aborts the write). The intended receipt
//!   is journaled locally before the remote write runs. PAT mode also writes `CODEX_AUTHAPI_BASE_URL` marker
//!   blocks into the rc files the app's login shell reads ([`envrc`]) and comments
//!   out legacy tutorial lines. ChatGPT logins found on a host are stashed locally
//!   through [`ConfigEngine::stash_login`] before being replaced; the host baseline
//!   (pure [`ConfigEngine::capture_baseline`], plus the host's own value of every
//!   key a later render takes over, so released keys get the user's value back)
//!   and the ownership record live under `DataDir::remote_dir`.
//! - **Turn activity**: newest rollout events and mtimes, ages computed remotely.
//! - **Verify**: a new app-server pid (not in the pre-restart list), its
//!   `CODEX_AUTHAPI_BASE_URL` (`/proc/<pid>/environ`, macOS `ps -E`), and file
//!   hashes equal to what was written. The restart IPC itself is sent locally by
//!   `AppController::hot_restart` with the host's `hostId`.

pub mod discover;
pub mod envrc;
mod hostops;
pub mod install;
pub mod probe;
pub mod records;
pub mod scope;
pub mod script;
pub mod sessions;
pub mod ssh;
pub mod store;
pub mod texts;
pub mod turns;
pub mod util;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use superba_core::codex::{ENV_AUTHAPI_BASE_URL, LEGACY_KRILL_PROVIDER};
use superba_core::env::legacy_krill_export_value;
use superba_core::{
    AuthMode, Baseline, BaselineExtras, BlockReason, Clock, CodexFileKind, CodexHome, CodexVersion,
    ConfigEngine, DataDir, EnvApplied, EnvObservation, EnvOp, EnvPlan, Error, FileChange,
    FileWrite, KeySnapshot, Notice, PlannedFile, ProcessInfo, RcLine, RemoteApplyReport,
    RemoteApplyScope, RemoteHost, RemoteHosts, RemoteInstallReport, RemoteOs, RemotePlan,
    RemoteState, RemoteVerification, RemoteVerifyRequest, RenderInput, RenderedFiles, ResolvedMode,
    ResolvedProfile, Result, SecretBytes, SessionReport, StashScope, SystemClock, TurnActivity,
};

use crate::envrc::{RcPlan, plan_env, shell_supported};
use crate::probe::{RemoteMarker, Snapshot, parse_pids, snapshot_from_records};
use crate::records::{FileRead, Records};
use crate::script::{Expect, FileOp, Mode, REMOTE_DIR, WriteSpec, backup_name, new_nonce};
use crate::ssh::{RemoteShell, SystemSsh};
use crate::store::{HostState, HostStore, JournalEntry, JournalStatus};
use crate::util::{new_apply_id, rjoin, sha256_hex};

pub use crate::ssh::{LocalShell, ScriptOutput};

/// Write attempts per host before giving up on concurrent modifications (plan 8.4).
pub const MAX_WRITE_ATTEMPTS: usize = 3;

/// Pause before retrying when another writer holds the remote lock.
const LOCK_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Time limits of remote operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Probe / snapshot (runs the login shell and `codex --version`).
    pub probe: Duration,
    /// One write or read script.
    pub write: Duration,
    /// Turn activity (contract: 3 s; on timeout the result is `complete = false`).
    pub turn: Duration,
    /// One verification poll.
    pub verify_poll: Duration,
    /// Pause between verification polls.
    pub poll_interval: Duration,
    /// A Codex install (upload of a ~160 MB package, verification, extraction).
    pub install: Duration,
    /// A read-only session scan.
    pub scan: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            probe: Duration::from_secs(45),
            write: Duration::from_secs(45),
            turn: Duration::from_secs(3),
            verify_poll: Duration::from_secs(15),
            poll_interval: Duration::from_millis(500),
            install: Duration::from_secs(20 * 60),
            scan: Duration::from_secs(120),
        }
    }
}

/// [`RemoteHosts`] over the system OpenSSH client.
#[derive(Clone)]
pub struct SshRemoteHosts {
    engine: Arc<dyn ConfigEngine>,
    data_dir: DataDir,
    shell: Arc<dyn RemoteShell>,
    clock: Arc<dyn Clock>,
    home_dir: Option<PathBuf>,
    timeouts: Timeouts,
}

impl std::fmt::Debug for SshRemoteHosts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshRemoteHosts")
            .field("data_dir", &self.data_dir)
            .field("shell", &self.shell)
            .field("home_dir", &self.home_dir)
            .finish_non_exhaustive()
    }
}

impl SshRemoteHosts {
    /// Create with the config engine used to render remote files and the data dir
    /// for per-host data (`DataDir::remote_dir`). Uses the system `ssh`.
    pub fn new(engine: Arc<dyn ConfigEngine>, data_dir: DataDir) -> Self {
        Self {
            engine,
            data_dir,
            shell: Arc::new(SystemSsh::default()),
            clock: Arc::new(SystemClock::new()),
            home_dir: None,
            timeouts: Timeouts::default(),
        }
    }

    /// Run scripts through another transport (tests: [`LocalShell`]).
    pub fn with_shell(mut self, shell: Arc<dyn RemoteShell>) -> Self {
        self.shell = shell;
        self
    }

    /// Use another clock.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The local home directory (for `~/.ssh/config` suggestions). Without it,
    /// discovery uses the parent of a Codex home named `.codex`, if any.
    pub fn with_home_dir(mut self, home_dir: impl Into<PathBuf>) -> Self {
        self.home_dir = Some(home_dir.into());
        self
    }

    /// Override time limits.
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// The config engine used for rendering.
    pub fn engine(&self) -> &Arc<dyn ConfigEngine> {
        &self.engine
    }

    /// The data dir.
    pub fn data_dir(&self) -> &DataDir {
        &self.data_dir
    }

    fn store(&self, host: &RemoteHost) -> HostStore {
        HostStore::new(&self.data_dir, &host.target_id)
    }

    /// Run a script and parse its records.
    async fn run(
        &self,
        host: &RemoteHost,
        script: &str,
        nonce: &str,
        timeout: Duration,
    ) -> Result<Records> {
        let out = self.shell.run(host, script, timeout).await?;
        let recs = Records::parse(&out.stdout, nonce)?;
        tracing::debug!(host = %host.label, status = ?out.status, "remote script finished");
        if out.status != Some(0) && !recs.has("done") && !recs.has("result") {
            let detail: String = out
                .stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output")
                .chars()
                .take(200)
                .collect();
            return Err(Error::Ssh {
                host: host.label.clone(),
                message: format!("remote script failed ({:?}): {detail}", out.status),
            });
        }
        Ok(recs)
    }

    /// Probe the host and keep its paths for later turn checks.
    async fn snapshot(&self, host: &RemoteHost, with_version: bool) -> Result<Snapshot> {
        let nonce = new_nonce();
        let script = script::probe_script(&nonce, with_version);
        let recs = self.run(host, &script, &nonce, self.timeouts.probe).await?;
        let snap = snapshot_from_records(
            &host.target_id,
            &recs,
            self.clock.now(),
            self.engine.as_ref(),
        )?;
        let mut snap = snap;
        let store = self.store(host);
        let mut st = store.load_state().await?;
        snap.state.owned_paths = st.last_written.iter().map(|k| k.path.clone()).collect();
        if st.codex_home.as_deref() != Some(&snap.state.codex_home)
            || st.home.as_deref() != Some(&snap.home)
        {
            st.schema = 1;
            st.target_id = Some(host.target_id.clone());
            st.codex_home = Some(snap.state.codex_home.clone());
            st.home = Some(snap.home.clone());
            st.updated_at = self.clock.now();
            store.save_state(&st).await?;
        }
        Ok(snap)
    }

    /// Everything needed to write `desired` onto the snapshot's files.
    async fn compute(
        &self,
        host: &RemoteHost,
        snap: &Snapshot,
        desired: &ResolvedProfile,
        scope: RemoteApplyScope,
        st: &HostState,
    ) -> Result<Computed> {
        let mut c = Computed::default();
        let state = &snap.state;
        if let Some(reason) = scope::refusal(desired, scope) {
            c.blocked.push(BlockReason::UnsupportedTarget {
                detail: format!("{}: {reason}", host.label),
            });
            return Ok(c);
        }
        if let RemoteOs::Other(name) = &state.os {
            c.blocked.push(BlockReason::UnsupportedTarget {
                detail: format!(
                    "{}: the remote system is {name:?}; only Linux and macOS hosts are supported",
                    host.label
                ),
            });
            return Ok(c);
        }
        for (read, path) in [
            (&snap.config_read, snap.config_path()),
            (&snap.auth_read, snap.auth_path()),
        ] {
            match read {
                FileRead::Unreadable => c.blocked.push(BlockReason::FileNotWritable {
                    path: PathBuf::from(path),
                }),
                FileRead::TooLarge => c.blocked.push(BlockReason::Unparseable {
                    path: PathBuf::from(path),
                    message: "file too large".into(),
                }),
                _ => {}
            }
        }
        if let Ok(cs) = &snap.inspected
            && let Some(msg) = &cs.parse_error
        {
            c.blocked.push(BlockReason::Unparseable {
                path: PathBuf::from(snap.config_path()),
                message: msg.clone(),
            });
        }
        if !c.blocked.is_empty() {
            return Ok(c);
        }

        let store = self.store(host);
        let stored_baseline = store.load_baseline().await?;
        let mut login_stash: Option<SecretBytes> = None;
        let mut env_op: Option<EnvOp> = None;
        match &desired.mode {
            ResolvedMode::Generation { .. } => {
                c.blocked.push(BlockReason::UnsupportedTarget {
                    detail: format!(
                        "{}: point-in-time restore is not available for remote hosts",
                        host.label
                    ),
                });
                return Ok(c);
            }
            ResolvedMode::Original { .. } => {
                let Some(b) = &stored_baseline else {
                    c.blocked.push(BlockReason::NoBaseline);
                    return Ok(c);
                };
                env_op = Some(EnvOp::RestoreBaseline {
                    snapshots: b.env.clone(),
                    legacy_lines: b.legacy_env_lines.clone(),
                });
            }
            ResolvedMode::Official => {
                login_stash = self
                    .engine
                    .load_login_stash(&StashScope::Remote(host.target_id.clone()))
                    .await?;
            }
            ResolvedMode::Token {
                auth, endpoints, ..
            } => {
                // A settings sync never touches the connection (PAT env included).
                if auth.needs_authapi_env() && scope != RemoteApplyScope::Settings {
                    let origin = endpoints
                        .authapi_origin
                        .as_ref()
                        .unwrap_or(&endpoints.origin);
                    if !shell_supported(&state.shell) {
                        c.blocked.push(BlockReason::UnsupportedTarget {
                            detail: format!(
                                "{}: Superba cannot set {ENV_AUTHAPI_BASE_URL} for the login shell {:?}; use API key mode",
                                host.label, state.shell
                            ),
                        });
                        return Ok(c);
                    }
                    env_op = Some(EnvOp::Set {
                        value: origin.as_str().to_owned(),
                        take_over_foreign: false,
                    });
                }
            }
            _ => {
                c.blocked.push(BlockReason::UnsupportedTarget {
                    detail: format!("{}: unsupported mode for remote hosts", host.label),
                });
                return Ok(c);
            }
        }

        let captured = match stored_baseline {
            Some(b) => b,
            None => {
                let b = self.engine.capture_baseline(
                    Path::new(&state.codex_home),
                    &snap.files,
                    BaselineExtras {
                        legacy_env_lines: legacy_lines(snap),
                        ..BaselineExtras::default()
                    },
                    self.clock.now(),
                )?;
                c.new_baseline = Some(b.clone());
                b
            }
        };
        c.captured = captured.keys.iter().map(|k| k.path.clone()).collect();
        let baseline = render_baseline(captured, st, &desired.mode);
        let original_files = match desired.mode {
            ResolvedMode::Original { .. } => snap.first_write.as_ref(),
            _ => None,
        };
        let rendered = scope::render(
            self.engine.as_ref(),
            &RenderInput {
                files: &snap.files,
                desired,
                baseline: Some(&baseline),
                login_stash: login_stash.as_ref(),
                original_files,
                last_written: (!st.last_written.is_empty()).then_some(st.last_written.as_slice()),
            },
            scope,
        )?;
        c.files = vec![
            PlannedFile {
                kind: CodexFileKind::AuthJson,
                path: PathBuf::from(snap.auth_path()),
                before_sha256: state.auth_sha256.clone(),
                change: rendered.auth_json.clone(),
            },
            PlannedFile {
                kind: CodexFileKind::ConfigToml,
                path: PathBuf::from(snap.config_path()),
                before_sha256: state.config_sha256.clone(),
                change: rendered.config_toml.clone(),
            },
        ];
        if let Some(op) = &env_op {
            match plan_env(&state.shell, &snap.rc, op) {
                Ok(rc) => {
                    if !rc.edits.is_empty() {
                        c.env = Some(EnvPlan {
                            var: ENV_AUTHAPI_BASE_URL.to_owned(),
                            op: op.clone(),
                            edits: rc.edits.clone(),
                            scope_notes: vec![texts::env_scope()],
                        });
                    }
                    c.rc = rc;
                }
                Err(e) => c.blocked.push(BlockReason::UnsupportedTarget {
                    detail: format!("{}: {e}", host.label),
                }),
            }
        }
        c.rendered = rendered;
        Ok(c)
    }

    /// Warnings shown with a remote plan (plan 8.2 risk note, 8.6 shared home).
    fn warnings(
        &self,
        host: &RemoteHost,
        snap: &Snapshot,
        desired: &ResolvedProfile,
        scope: RemoteApplyScope,
        c: &Computed,
    ) -> Vec<Notice> {
        let label = host.label.as_str();
        let pat = scope != RemoteApplyScope::Settings
            && desired.auth_mode().is_some_and(AuthMode::needs_authapi_env);
        let mut w = vec![Notice::warning(match &host.host_id {
            None => texts::no_host_id(label),
            Some(_) => texts::pkill(label),
        })];
        if !snap.login_ok {
            w.push(Notice::warning(texts::login_shell(label)));
        }
        if pat && snap.state.codex_path.is_none() {
            w.push(Notice::warning(texts::codex_missing(label)));
        } else if pat && snap.state.codex_version.is_none() {
            w.push(Notice::warning(texts::version_unknown(label)));
        }
        if snap.state.shared_home_suspected() {
            let other = snap.state.marker_machine_id.as_deref().unwrap_or("?");
            w.push(Notice::warning(texts::shared_home(label, other)));
        }
        if snap.state.model_provider.as_deref() == Some(LEGACY_KRILL_PROVIDER) {
            w.push(Notice::warning(texts::legacy_provider(label)));
        }
        w.extend(c.rc.warnings.iter().cloned());
        w.extend(c.rendered.notes.iter().cloned());
        match scope {
            RemoteApplyScope::Connection => w.push(Notice::info(texts::scope_connection(label))),
            RemoteApplyScope::Settings => w.push(Notice::info(texts::scope_settings(label))),
            _ => {}
        }
        w
    }

    /// One write attempt. `Ok(None)` = precondition conflict (retry).
    async fn write_once(
        &self,
        host: &RemoteHost,
        snap: &Snapshot,
        c: &Computed,
        st: &HostState,
    ) -> Result<WriteOutcome> {
        let store = self.store(host);
        let now = self.clock.now();
        let id = new_apply_id(now.as_millis());
        let backup_dir = rjoin(
            &snap.state.codex_home,
            &format!("{REMOTE_DIR}/backups/{id}"),
        );

        let mut ops: Vec<FileOp> = Vec::new();
        let mut writes: Vec<FileWrite> = Vec::new();
        // auth.json first, then config.toml (the engine's order), then rc files.
        for pf in &c.files {
            let current = match pf.kind {
                CodexFileKind::AuthJson => snap.files.auth_json.as_ref(),
                CodexFileKind::ConfigToml => snap.files.config_toml.as_ref(),
            };
            let new = match &pf.change {
                FileChange::Unchanged => continue,
                FileChange::Write { contents, .. } => Some(contents.expose().to_vec()),
                FileChange::Remove => {
                    if current.is_none() {
                        continue;
                    }
                    None
                }
            };
            let path = pf.path.to_string_lossy().into_owned();
            writes.push(FileWrite {
                kind: pf.kind,
                path: pf.path.clone(),
                before_sha256: current.map(|b| sha256_hex(b.expose())),
                after_sha256: new.as_deref().map(sha256_hex),
                backup: current.map(|_| PathBuf::from(rjoin(&backup_dir, &backup_name(&path)))),
            });
            ops.push(FileOp {
                path,
                expect: current.map_or(Expect::Absent, |b| Expect::Bytes(b.expose().to_vec())),
                new,
                mode: if pf.kind == CodexFileKind::AuthJson {
                    Mode::Private
                } else {
                    Mode::Preserve
                },
                backup: current.is_some(),
            });
        }
        let mut env_backups = Vec::new();
        for w in &c.rc.writes {
            if w.before.is_none() && w.after.is_none() {
                continue;
            }
            if w.before.is_some() {
                env_backups.push(PathBuf::from(rjoin(&backup_dir, &backup_name(&w.path))));
            }
            ops.push(FileOp {
                path: w.path.clone(),
                expect: w
                    .before
                    .as_ref()
                    .map_or(Expect::Absent, |b| Expect::Bytes(b.clone())),
                new: w.after.clone(),
                mode: Mode::Preserve,
                backup: w.before.is_some(),
            });
        }
        let env = (!c.rc.edits.is_empty()).then(|| EnvApplied {
            id: id.clone(),
            var: ENV_AUTHAPI_BASE_URL.to_owned(),
            edits: c.rc.edits.clone(),
            backups: env_backups,
            applied_at: now,
        });
        let report = RemoteApplyReport {
            target_id: host.target_id.clone(),
            applied_at: now,
            files: writes,
            env,
        };
        if ops.is_empty() {
            return Ok(WriteOutcome::Written(report));
        }

        let marker = RemoteMarker {
            schema: 1,
            tool: "superba".into(),
            machine_id: snap.state.machine_id.clone(),
            target_id: Some(host.target_id.to_string()),
            apply_id: Some(id.clone()),
            written_at: Some(now.as_millis()),
        };
        let spec = WriteSpec {
            codex_home: snap.state.codex_home.clone(),
            backup_dir: backup_dir.clone(),
            first_write: true,
            marker: Some(
                serde_json::to_vec_pretty(&marker).map_err(|e| Error::other(e.to_string()))?,
            ),
            ops,
        };
        let mut entry = JournalEntry {
            id: id.clone(),
            status: JournalStatus::Pending,
            created_at: now,
            paths: spec.ops.iter().map(|o| o.path.clone()).collect(),
            backup_dir: Some(backup_dir),
            // The intended receipt is journaled before anything runs, so an
            // interrupted or unconfirmed write can still be rolled back.
            report: Some(report.clone()),
            prev_last_written: st.last_written.clone(),
            prev_generation: st.generation,
            detail: None,
        };
        store.write_journal(&entry).await?;
        let nonce = new_nonce();
        let script = script::write_script(&nonce, &spec);
        let recs = match self.run(host, &script, &nonce, self.timeouts.write).await {
            Ok(r) => r,
            Err(e) => {
                // Unknown outcome (e.g. timeout): the entry stays pending with its
                // receipt so the caller can roll back.
                entry.detail = Some(format!("outcome unknown: {e}"));
                store.write_journal(&entry).await?;
                return Err(e);
            }
        };
        match recs.get("result") {
            Some("ok") => {
                entry.status = JournalStatus::Applied;
                if let Err(e) = store.write_journal(&entry).await {
                    return Err(self.undo_after_local_failure(host, &report, e).await);
                }
                Ok(WriteOutcome::Written(report))
            }
            Some("conflict") => {
                entry.status = JournalStatus::Conflict;
                entry.detail = Some(recs.all("conflict").join(", "));
                store.write_journal(&entry).await?;
                Ok(WriteOutcome::Retry { locked: false })
            }
            Some("locked") => {
                entry.status = JournalStatus::Conflict;
                entry.detail = Some("locked by another writer".into());
                store.write_journal(&entry).await?;
                Ok(WriteOutcome::Retry { locked: true })
            }
            Some("partial") => {
                let left = recs.all("undo_failed").join(", ");
                let detail = format!(
                    "{}; could not restore: {left}",
                    recs.all("error").join("; ")
                );
                entry.detail = Some(detail.clone());
                store.write_journal(&entry).await?;
                Err(Error::Ssh {
                    host: host.label.clone(),
                    message: format!(
                        "remote write failed and was only partly undone ({detail}); journal {id} keeps the receipt"
                    ),
                })
            }
            _ => {
                let detail = recs.all("error").join("; ");
                entry.status = JournalStatus::Failed;
                entry.detail = Some(detail.clone());
                store.write_journal(&entry).await?;
                Err(Error::Ssh {
                    host: host.label.clone(),
                    message: format!("remote write failed, nothing changed: {detail}"),
                })
            }
        }
    }

    /// [`RemoteHosts::plan_apply_scoped`]: the plan keeps `desired` and `scope`,
    /// so [`RemoteHosts::apply`] renders with the same scope.
    pub(crate) async fn plan_in_scope(
        &self,
        host: &RemoteHost,
        desired: &ResolvedProfile,
        scope: RemoteApplyScope,
    ) -> Result<RemotePlan> {
        let snap = self.snapshot(host, true).await?;
        let st = self.store(host).load_state().await?;
        let c = self.compute(host, &snap, desired, scope, &st).await?;
        let warnings = if c.blocked.is_empty() {
            self.warnings(host, &snap, desired, scope, &c)
        } else {
            Vec::new()
        };
        Ok(RemotePlan {
            target_id: host.target_id.clone(),
            state: snap.state.clone(),
            files: c.files,
            key_changes: c.rendered.key_changes,
            auth_change: c.rendered.auth_change,
            env: c.env,
            blocked: c.blocked,
            warnings,
            scope,
            desired: Some(desired.clone()),
        })
    }

    /// The remote write succeeded but local bookkeeping failed: undo the remote
    /// change so no unrecorded change is left behind, and explain both.
    async fn undo_after_local_failure(
        &self,
        host: &RemoteHost,
        report: &RemoteApplyReport,
        local: Error,
    ) -> Error {
        match self.rollback(host, report).await {
            Ok(()) => Error::other(format!(
                "{}: recording the change locally failed ({local}); the remote change was rolled back",
                host.label
            )),
            Err(e) => Error::other(format!(
                "{}: recording the change locally failed ({local}) and rolling it back failed too ({e})",
                host.label
            )),
        }
    }
}

/// Outcome of one write attempt.
#[derive(Debug)]
enum WriteOutcome {
    /// Written.
    Written(RemoteApplyReport),
    /// Nothing written; try again (files changed, or another writer held the lock).
    Retry {
        /// The per-home lock was held.
        locked: bool,
    },
}

/// Planning result for one host.
#[derive(Debug, Default)]
struct Computed {
    rendered: RenderedFiles,
    files: Vec<PlannedFile>,
    env: Option<EnvPlan>,
    rc: RcPlan,
    blocked: Vec<BlockReason>,
    new_baseline: Option<Baseline>,
    /// Paths of the captured baseline (their recorded value never changes).
    captured: BTreeSet<String>,
}

/// Legacy tutorial lines in the host's rc files (recorded in the baseline).
fn legacy_lines(snap: &Snapshot) -> Vec<RcLine> {
    let mut out = Vec::new();
    for f in &snap.rc {
        let Some(text) = f
            .content
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok())
        else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if let Some(value) = legacy_krill_export_value(line) {
                out.push(RcLine {
                    path: PathBuf::from(&f.path),
                    line_no: i + 1,
                    text: line.to_owned(),
                    value,
                });
            }
        }
    }
    out
}

/// The baseline a render sees: the captured one plus the host's own values of
/// the keys Superba took over later, so a released model key or setting returns
/// to the user's value instead of being removed. For a restore to the original
/// it counts as released (as a local home after an official switch) while
/// Superba owns none of the host's connection keys: the recorded keys Superba no
/// longer owns are then put back too.
fn render_baseline(mut b: Baseline, st: &HostState, mode: &ResolvedMode) -> Baseline {
    for v in &st.user_values {
        if !b.keys.iter().any(|k| k.path == v.path) {
            b.keys.push(v.clone());
        }
    }
    if matches!(mode, ResolvedMode::Original { .. }) {
        b.released = !st
            .last_written
            .iter()
            .any(|k| scope::family_of(&k.path) == RemoteApplyScope::Connection);
    }
    b
}

fn to_map(keys: &[KeySnapshot]) -> BTreeMap<String, Option<String>> {
    keys.iter()
        .map(|k| (k.path.clone(), k.value.clone()))
        .collect()
}

fn from_map(map: BTreeMap<String, Option<String>>) -> Vec<KeySnapshot> {
    map.into_iter()
        .map(|(path, value)| KeySnapshot { path, value })
        .collect()
}

/// `last_written` after a render of `desired` in `scope`: the keys in its view
/// are exactly what it owns now (released keys drop out); the others keep their
/// record.
fn next_last_written(
    prev: &[KeySnapshot],
    rendered: &RenderedFiles,
    scope: RemoteApplyScope,
    desired: &ResolvedProfile,
) -> Vec<KeySnapshot> {
    let mut map = to_map(prev);
    map.retain(|path, _| !scope::in_view(scope, desired, path));
    for k in &rendered.owned {
        if scope::in_view(scope, desired, &k.path) {
            map.insert(k.path.clone(), k.value.clone());
        }
    }
    from_map(map)
}

/// [`HostState::user_values`] after a render: a key outside the captured baseline
/// that it takes over records its value from just before (replacing an older
/// one, so the user's latest value is what comes back); captured keys keep their
/// takeover-time value in the baseline, as in the local engine.
fn next_user_values(
    prev: &[KeySnapshot],
    captured: &BTreeSet<String>,
    rendered: &RenderedFiles,
    scope: RemoteApplyScope,
    desired: &ResolvedProfile,
) -> Vec<KeySnapshot> {
    let mut map = to_map(prev);
    for k in &rendered.newly_owned {
        if scope::in_view(scope, desired, &k.path) && !captured.contains(&k.path) {
            map.insert(k.path.clone(), k.value.clone());
        }
    }
    from_map(map)
}

/// Infer the local home from a Codex home named `.codex`.
fn infer_home(home: &CodexHome) -> Option<PathBuf> {
    let root = home.root();
    (root.file_name()? == ".codex").then(|| root.parent().map(Path::to_path_buf))?
}

#[async_trait]
impl RemoteHosts for SshRemoteHosts {
    async fn discover(
        &self,
        home: &CodexHome,
        from_app: &[RemoteHost],
        live: &[ProcessInfo],
    ) -> Result<Vec<RemoteHost>> {
        let home_dir = self.home_dir.clone().or_else(|| infer_home(home));
        let home = home.clone();
        let from_app = from_app.to_vec();
        let live = live.to_vec();
        tokio::task::spawn_blocking(move || {
            discover::discover_from_files(&home, home_dir.as_deref(), &from_app, &live)
        })
        .await
        .map_err(|e| Error::other(format!("discovery task failed: {e}")))
    }

    async fn probe(&self, host: &RemoteHost) -> Result<RemoteState> {
        Ok(self.snapshot(host, true).await?.state)
    }

    async fn plan_apply(&self, host: &RemoteHost, desired: &ResolvedProfile) -> Result<RemotePlan> {
        self.plan_in_scope(host, desired, RemoteApplyScope::Both)
            .await
    }

    async fn plan_apply_scoped(
        &self,
        host: &RemoteHost,
        desired: &ResolvedProfile,
        scope: RemoteApplyScope,
    ) -> Result<RemotePlan> {
        self.plan_scoped(host, desired, scope).await
    }

    async fn install_codex(
        &self,
        host: &RemoteHost,
        package: &Path,
        version: &CodexVersion,
    ) -> Result<RemoteInstallReport> {
        self.install(host, package, version).await
    }

    async fn rollback_codex(&self, host: &RemoteHost) -> Result<RemoteInstallReport> {
        self.rollback_install(host).await
    }

    async fn scan_sessions(&self, host: &RemoteHost) -> Result<SessionReport> {
        self.scan(host).await
    }

    async fn apply(&self, host: &RemoteHost, plan: &RemotePlan) -> Result<RemoteApplyReport> {
        if plan.target_id != host.target_id {
            return Err(Error::invalid(format!(
                "plan is for {}, not {}",
                plan.target_id, host.target_id
            )));
        }
        if !plan.blocked.is_empty() {
            return Err(Error::Blocked {
                reasons: plan.blocked.clone(),
            });
        }
        let desired = plan.desired.as_ref().ok_or_else(|| {
            Error::invalid("remote plan carries no desired state (plans cannot be deserialized)")
        })?;
        let store = self.store(host);
        let mut last_locked = false;
        for attempt in 1..=MAX_WRITE_ATTEMPTS {
            // Re-read just before writing; re-render when the files changed.
            let snap = self.snapshot(host, false).await?;
            let st = store.load_state().await?;
            let c = self.compute(host, &snap, desired, plan.scope, &st).await?;
            if !c.blocked.is_empty() {
                return Err(Error::Blocked { reasons: c.blocked });
            }
            if let Some(b) = &c.new_baseline {
                store.save_baseline_once(b).await?;
            }
            if c.rendered
                .auth_change
                .as_ref()
                .is_some_and(|a| a.stash_login)
                && let Some(auth) = &snap.files.auth_json
            {
                self.engine
                    .stash_login(&StashScope::Remote(host.target_id.clone()), auth)
                    .await?;
            }
            match self.write_once(host, &snap, &c, &st).await? {
                WriteOutcome::Written(report) => {
                    // Ownership is recorded even when no byte changed (a key
                    // taken over or released at the value it already had).
                    let written = !report.files.is_empty() || report.env.is_some();
                    let saved = async {
                        let mut st = store.load_state().await?;
                        if written {
                            st.generation += 1;
                        }
                        st.user_values = next_user_values(
                            &st.user_values,
                            &c.captured,
                            &c.rendered,
                            plan.scope,
                            desired,
                        );
                        st.last_written =
                            next_last_written(&st.last_written, &c.rendered, plan.scope, desired);
                        st.updated_at = self.clock.now();
                        store.save_state(&st).await
                    };
                    if let Err(e) = saved.await {
                        if !written {
                            return Err(e);
                        }
                        return Err(self.undo_after_local_failure(host, &report, e).await);
                    }
                    return Ok(report);
                }
                WriteOutcome::Retry { locked } => {
                    last_locked = locked;
                    tracing::info!(host = %host.label, attempt, locked, "remote write not done; retrying");
                    if locked {
                        tokio::time::sleep(LOCK_RETRY_DELAY).await;
                    }
                }
            }
        }
        if last_locked {
            return Err(Error::Locked {
                what: format!(
                    "{}: another Superba is writing this Codex home; nothing was written",
                    host.label
                ),
            });
        }
        Err(Error::Conflict {
            message: format!(
                "{}: the remote files kept changing; nothing was written after {MAX_WRITE_ATTEMPTS} attempts",
                host.label
            ),
        })
    }

    async fn rollback(&self, host: &RemoteHost, report: &RemoteApplyReport) -> Result<()> {
        // (path, before hash, after hash, backup, mode)
        struct Item {
            path: String,
            before: Option<String>,
            after: Option<String>,
            backup: Option<String>,
            mode: Mode,
        }
        let mut items: Vec<Item> = report
            .files
            .iter()
            .map(|f| Item {
                path: f.path.to_string_lossy().into_owned(),
                before: f.before_sha256.clone(),
                after: f.after_sha256.clone(),
                backup: f.backup.as_ref().map(|b| b.to_string_lossy().into_owned()),
                mode: if f.kind == CodexFileKind::AuthJson {
                    Mode::Private
                } else {
                    Mode::Preserve
                },
            })
            .collect();
        if let Some(env) = &report.env {
            let mut seen = BTreeSet::new();
            for e in &env.edits {
                let superba_core::EnvLocation::ShellRc { path, .. } = &e.location else {
                    continue;
                };
                let p = path.to_string_lossy().into_owned();
                if !seen.insert(p.clone()) {
                    continue;
                }
                let of_file: Vec<_> = env
                    .edits
                    .iter()
                    .filter(|x| matches!(&x.location, superba_core::EnvLocation::ShellRc { path: q, .. } if q == path))
                    .collect();
                let before = of_file.first().and_then(|x| x.before.clone());
                let after = of_file.last().and_then(|x| x.after.clone());
                let bn = backup_name(&p);
                let backup = env
                    .backups
                    .iter()
                    .map(|b| b.to_string_lossy().into_owned())
                    .find(|b| b.rsplit('/').next() == Some(bn.as_str()));
                items.push(Item {
                    path: p,
                    before,
                    after,
                    backup,
                    mode: Mode::Preserve,
                });
            }
        }
        if items.is_empty() {
            return Ok(());
        }
        // The lock lives in the Codex home: the one seen last, else the directory
        // of a Codex file in the report.
        let lock_home = self
            .store(host)
            .load_state()
            .await?
            .codex_home
            .or_else(|| {
                report.files.first().and_then(|f| {
                    let p = f.path.to_string_lossy();
                    p.rsplit_once('/').map(|(dir, _)| dir.to_owned())
                })
            })
            .unwrap_or_default();
        let mut conflicts = Vec::new();
        let mut completed = false;
        let mut last_locked = false;
        for _attempt in 1..=MAX_WRITE_ATTEMPTS {
            conflicts.clear();
            let mut paths: Vec<String> = items.iter().map(|i| i.path.clone()).collect();
            let backup_index: Vec<Option<usize>> = items
                .iter()
                .map(|i| {
                    i.backup.as_ref().map(|b| {
                        paths.push(b.clone());
                        paths.len() - 1
                    })
                })
                .collect();
            let nonce = new_nonce();
            let recs = self
                .run(
                    host,
                    &script::read_script(&nonce, &paths),
                    &nonce,
                    self.timeouts.write,
                )
                .await?;
            recs.require_done("read")?;
            let mut ops = Vec::new();
            for (n, item) in items.iter().enumerate() {
                let cur = recs
                    .file(&format!("f.{n}"))
                    .cloned()
                    .unwrap_or(FileRead::Absent);
                if cur.is_unavailable() {
                    conflicts.push(item.path.clone());
                    continue;
                }
                let cur_hash = cur.bytes().map(|b| sha256_hex(b.expose()));
                if cur_hash == item.before {
                    continue; // already as before
                }
                if cur_hash != item.after {
                    conflicts.push(item.path.clone());
                    continue;
                }
                let new = match &item.before {
                    None => None,
                    Some(want) => {
                        let bytes = backup_index[n]
                            .and_then(|bi| recs.file(&format!("f.{bi}")))
                            .and_then(FileRead::bytes)
                            .map(|b| b.expose().to_vec());
                        match bytes {
                            Some(b) if &sha256_hex(&b) == want => Some(b),
                            _ => {
                                conflicts.push(item.path.clone());
                                continue;
                            }
                        }
                    }
                };
                ops.push(FileOp {
                    path: item.path.clone(),
                    expect: cur
                        .bytes()
                        .map_or(Expect::Absent, |b| Expect::Bytes(b.expose().to_vec())),
                    new,
                    mode: item.mode,
                    backup: false,
                });
            }
            if ops.is_empty() {
                completed = true;
                break;
            }
            let spec = WriteSpec {
                codex_home: lock_home.clone(),
                ops,
                ..WriteSpec::default()
            };
            let nonce = new_nonce();
            let recs = self
                .run(
                    host,
                    &script::write_script(&nonce, &spec),
                    &nonce,
                    self.timeouts.write,
                )
                .await?;
            match recs.get("result") {
                Some("ok") => {
                    completed = true;
                    break;
                }
                Some("conflict") => last_locked = false,
                Some("locked") => {
                    last_locked = true;
                    tokio::time::sleep(LOCK_RETRY_DELAY).await;
                }
                Some("partial") => {
                    return Err(Error::Ssh {
                        host: host.label.clone(),
                        message: format!(
                            "remote rollback failed and was only partly undone: {}; could not restore: {}",
                            recs.all("error").join("; "),
                            recs.all("undo_failed").join(", ")
                        ),
                    });
                }
                _ => {
                    return Err(Error::Ssh {
                        host: host.label.clone(),
                        message: format!(
                            "remote rollback failed, nothing changed: {}",
                            recs.all("error").join("; ")
                        ),
                    });
                }
            }
        }
        if !completed {
            // Retries exhausted: nothing (more) was restored and local state stays.
            if last_locked {
                return Err(Error::Locked {
                    what: format!("{}: another Superba is writing this Codex home", host.label),
                });
            }
            return Err(Error::Conflict {
                message: format!(
                    "{}: the remote files kept changing; rollback gave up after {MAX_WRITE_ATTEMPTS} attempts",
                    host.label
                ),
            });
        }
        let store = self.store(host);
        if let Some(mut entry) = store.find_by_report(report).await? {
            let mut st = store.load_state().await?;
            st.last_written = entry.prev_last_written.clone();
            st.generation = entry.prev_generation;
            st.updated_at = self.clock.now();
            store.save_state(&st).await?;
            entry.status = JournalStatus::RolledBack;
            store.write_journal(&entry).await?;
        }
        if conflicts.is_empty() {
            Ok(())
        } else {
            Err(Error::Conflict {
                message: format!(
                    "{}: changed since Superba wrote them, left alone: {}",
                    host.label,
                    conflicts.join(", ")
                ),
            })
        }
    }

    async fn turn_activity(&self, host: &RemoteHost) -> Result<TurnActivity> {
        let st = self.store(host).load_state().await?;
        let nonce = new_nonce();
        let script = script::turn_script(&nonce, st.codex_home.as_deref());
        let now = self.clock.now();
        let recs = match self.run(host, &script, &nonce, self.timeouts.turn).await {
            Ok(r) => r,
            Err(Error::Timeout { .. }) => {
                return Ok(TurnActivity {
                    target_id: host.target_id.clone(),
                    checked_at: now,
                    busy: Vec::new(),
                    complete: false,
                });
            }
            Err(e) => return Err(e),
        };
        Ok(TurnActivity {
            target_id: host.target_id.clone(),
            checked_at: now,
            busy: turns::busy_threads(&host.target_id, &recs, now),
            complete: turns::scan_complete(&recs),
        })
    }

    async fn verify(
        &self,
        host: &RemoteHost,
        request: &RemoteVerifyRequest,
    ) -> Result<RemoteVerification> {
        let started = std::time::Instant::now();
        let total = Duration::from_millis(request.timeout_ms);
        let paths: Vec<String> = request
            .report
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().into_owned())
            .collect();
        let previous: BTreeSet<u32> = request.previous_app_server_pids.iter().copied().collect();
        loop {
            let nonce = new_nonce();
            let recs = self
                .run(
                    host,
                    &script::verify_script(&nonce, &request.previous_app_server_pids, &paths),
                    &nonce,
                    self.timeouts.verify_poll,
                )
                .await?;
            recs.require_done("verify")?;
            let pids = parse_pids(recs.get("pids").unwrap_or_default());
            let new: Vec<u32> = pids.into_iter().filter(|p| !previous.contains(p)).collect();
            let mut authapi_env = EnvObservation::Unknown;
            for pid in &new {
                if let Some(v) = recs.get(&format!("env.{pid}")) {
                    authapi_env = EnvObservation::Present(v.to_owned());
                    break;
                }
                if recs.has(&format!("envabsent.{pid}")) {
                    authapi_env = EnvObservation::Absent;
                    break;
                }
            }
            let mut mismatched = Vec::new();
            for (i, f) in request.report.files.iter().enumerate() {
                // A removal must read back as explicitly absent; unreadable, too
                // large or missing records never match.
                let ok = match (recs.file(&format!("f.{i}")), &f.after_sha256) {
                    (Some(FileRead::Bytes(b)), Some(want)) => &sha256_hex(b.expose()) == want,
                    (Some(FileRead::Absent), None) => true,
                    _ => false,
                };
                if !ok {
                    mismatched.push(f.path.to_string_lossy().into_owned());
                }
            }
            let files_match = mismatched.is_empty();
            let new_app_server = !new.is_empty();
            let env_ok = match (&request.expected_authapi, &authapi_env) {
                (None, _) | (_, EnvObservation::Unknown) => true,
                (Some(want), EnvObservation::Present(got)) => {
                    got.trim_end_matches('/') == want.trim_end_matches('/')
                }
                (Some(_), EnvObservation::Absent) => false,
            };
            let done = new_app_server && files_match && env_ok;
            if done || started.elapsed() >= total {
                let detail = if !new_app_server {
                    Some(format!(
                        "no new app-server appeared within {} ms",
                        request.timeout_ms
                    ))
                } else if !files_match {
                    Some(format!("changed since apply: {}", mismatched.join(", ")))
                } else if !env_ok {
                    Some(format!(
                        "{ENV_AUTHAPI_BASE_URL} of the new app-server is {:?}, expected {:?}",
                        authapi_env.value(),
                        request.expected_authapi
                    ))
                } else {
                    None
                };
                return Ok(RemoteVerification {
                    new_app_server,
                    authapi_env,
                    files_match,
                    detail,
                });
            }
            tokio::time::sleep(self.timeouts.poll_interval).await;
        }
    }
}
