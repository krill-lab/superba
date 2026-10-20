//! [`ShellEnvPersister`]: persists `CODEX_AUTHAPI_BASE_URL` (or any variable) for
//! future processes (plan 6.1, 9.9).
//!
//! | Platform | Locations |
//! |---|---|
//! | Linux | marker blocks in `~/.profile`, `~/.bashrc` (top), `~/.bash_profile` (if present), `~/.zshenv`, fish `conf.d/superba.fish`; `~/.config/environment.d/60-superba.conf` (systemd user session, after re-login) |
//! | macOS | `~/.zshenv` (+ bash files when used), a LaunchAgent re-running `launchctl setenv` at login, and `launchctl setenv` now |
//! | Windows | `HKCU\Environment` + `WM_SETTINGCHANGE` broadcast; with the desktop app's "Run Codex in WSL" also `WSLENV` naming `CODEX_AUTHAPI_BASE_URL` |
//!
//! Legacy Krill tutorial lines (`export CODEX_AUTHAPI_BASE_URL=<krill origin>`,
//! appended to `~/.bashrc` / `~/.zshrc` after Superba's block and therefore
//! overriding it) are commented out with `# [superba] ` and restored by
//! `RestoreBaseline`. Other lines for the variable ("foreign") are reported and
//! only taken over on request. Every file edit keeps a pre-image under
//! `backups/rc/<id>/` and records before/after hashes, so `revert` restores bytes
//! exactly when nothing else changed and edits surgically otherwise.

mod native;
pub(crate) mod rc;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
pub use native::{MemoryNativeEnv, NativeEnv, SystemNativeEnv};
use serde::{Deserialize, Serialize};
use superba_core::{
    Clock, EnvApplied, EnvEdit, EnvEditAction, EnvLocation, EnvObservation, EnvOp, EnvPersister,
    EnvPlan, EnvReport, EnvSnapshot, Error, LocalContext, LocalizedText, PersistedValue, RcLine,
    Result, ShellKind, SystemClock,
};

use self::native::location_key;
use self::rc::{Dialect, Placement};
use crate::fsx::{self, Mode};

/// Which platform's locations to manage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvPlatform {
    /// Linux and other freedesktop Unixes.
    Linux,
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
}

impl EnvPlatform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            EnvPlatform::MacOs
        } else if cfg!(windows) {
            EnvPlatform::Windows
        } else {
            EnvPlatform::Linux
        }
    }
}

/// A file location.
#[derive(Clone, Debug)]
struct FileLoc {
    location: EnvLocation,
    path: PathBuf,
    kind: FileKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FileKind {
    /// A user file holding our marker block among other content.
    Block {
        dialect: Dialect,
        placement: Placement,
    },
    /// macOS LaunchAgent plist (one per variable, wholly ours).
    Plist,
}

/// Bookkeeping needed by `Clear` (`<data>/env-state.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct EnvState {
    #[serde(default)]
    vars: BTreeMap<String, VarState>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct VarState {
    /// Native locations Superba set, with the value that was there before
    /// (`None` = unset), keyed by [`location_key`].
    #[serde(default)]
    native_originals: BTreeMap<String, Option<String>>,
    /// User (non-Krill) lines commented out on request (restored by `Clear`).
    #[serde(default)]
    commented_foreign: Vec<RcLine>,
    /// Legacy lines can appear after the first baseline was captured.
    #[serde(default)]
    commented_legacy: Vec<RcLine>,
    /// The value Superba last set (GH#5): a location holding another value was
    /// changed outside Superba, so clearing or restoring leaves it alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    written: Option<String>,
}

impl VarState {
    fn commented(&self) -> impl Iterator<Item = &RcLine> {
        self.commented_foreign.iter().chain(&self.commented_legacy)
    }

    fn record_comment(&mut self, path: &Path, line_no: usize, original: &str, var: &str) {
        let lines = if superba_core::env::legacy_krill_export_value(original).is_some() {
            &mut self.commented_legacy
        } else {
            &mut self.commented_foreign
        };
        // Keep one record per occurrence, including duplicate assignments.
        lines.push(RcLine {
            path: path.to_path_buf(),
            line_no,
            text: original.to_owned(),
            value: superba_core::env::export_value(original, var).unwrap_or_default(),
        });
    }

    fn forget_comment(&mut self, path: &Path, original: &str) {
        for lines in [&mut self.commented_foreign, &mut self.commented_legacy] {
            if let Some(index) = lines
                .iter()
                .position(|l| l.path == path && l.text == original)
            {
                lines.remove(index);
                break;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.native_originals.is_empty()
            && self.commented_foreign.is_empty()
            && self.commented_legacy.is_empty()
            && self.written.is_none()
    }

    /// GH#5: `current` at a location Superba manages was changed since Superba
    /// set it (removing it counts as Superba's own clear having happened).
    fn changed_since_written(&self, current: Option<&str>) -> bool {
        match (self.written.as_deref(), current) {
            (Some(written), Some(current)) => written != current,
            _ => false,
        }
    }
}

/// Plan note: a location kept because its value changed after Superba set it.
fn kept_note(location: &EnvLocation, value: &str) -> LocalizedText {
    LocalizedText::new(
        "env.scope.kept_changed",
        "Left {location} as it is: its value was changed to {value} after Superba set it.",
    )
    .arg("location", location_label(location))
    .arg("value", value)
}

fn location_label(location: &EnvLocation) -> String {
    match location {
        EnvLocation::ShellRc { path, .. } => path.display().to_string(),
        EnvLocation::EnvironmentD { path } | EnvLocation::LaunchAgent { path } => {
            path.display().to_string()
        }
        EnvLocation::Launchctl => "launchctl".to_owned(),
        EnvLocation::WindowsUserEnv => r"HKCU\Environment".to_owned(),
        other => format!("{other:?}"),
    }
}

/// A non-UTF-8 rc file is refused individually, never treated as an absent file
/// that may be overwritten. Other locations remain usable in the same plan.
#[derive(Default)]
struct PlannedTexts {
    files: BTreeMap<PathBuf, Option<String>>,
    non_utf8: BTreeSet<PathBuf>,
}

impl PlannedTexts {
    fn read(&mut self, path: &Path) -> Result<Option<String>> {
        if let Some(text) = self.files.get(path) {
            return Ok(text.clone());
        }
        let text = match read_text(path) {
            Ok(text) => text,
            Err(Error::Parse { .. }) => {
                tracing::warn!(path = %path.display(), "skipping rc file containing non-UTF-8 bytes; convert it to UTF-8");
                self.non_utf8.insert(path.to_path_buf());
                None
            }
            Err(error) => return Err(error),
        };
        self.files.insert(path.to_path_buf(), text.clone());
        Ok(text)
    }

    fn notices(&self) -> impl Iterator<Item = LocalizedText> + '_ {
        self.non_utf8.iter().map(|path| {
            LocalizedText::new(
                "env.scope.non_utf8_skipped",
                "Skipped {path}: it contains non-UTF-8 bytes; convert it to UTF-8 so Superba can scan and edit it. Other locations are still planned.",
            ).arg("path", superba_core::paths::display_path(path))
        })
    }
}

/// [`EnvPersister`] for the local machine (shell rc blocks + platform stores).
#[derive(Debug, Clone)]
pub struct ShellEnvPersister {
    ctx: LocalContext,
    platform: EnvPlatform,
    native: Arc<dyn NativeEnv>,
    clock: Arc<dyn Clock>,
}

impl ShellEnvPersister {
    /// Create a persister writing under `ctx.home_dir` (rc backups in the data dir)
    /// for the current platform.
    pub fn new(ctx: LocalContext) -> Self {
        Self::with_platform(ctx, EnvPlatform::current(), Arc::new(SystemNativeEnv))
    }

    /// Persister for an explicit platform and native store (tests exercise the
    /// macOS / Windows planning logic on Linux with [`MemoryNativeEnv`]).
    pub fn with_platform(
        ctx: LocalContext,
        platform: EnvPlatform,
        native: Arc<dyn NativeEnv>,
    ) -> Self {
        Self {
            ctx,
            platform,
            native,
            clock: Arc::new(SystemClock::new()),
        }
    }

    /// Replace the clock (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The context this persister was created with.
    pub fn context(&self) -> &LocalContext {
        &self.ctx
    }

    /// The platform whose locations are managed.
    pub fn platform(&self) -> EnvPlatform {
        self.platform
    }

    fn home(&self) -> &Path {
        &self.ctx.home_dir
    }

    fn shell(&self) -> Option<ShellKind> {
        self.ctx
            .env
            .var_non_empty("SHELL")
            .map(|s| ShellKind::from_shell_path(&s))
    }

    fn config_home(&self) -> PathBuf {
        self.ctx
            .env
            .var_non_empty("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| self.home().join(".config"))
    }

    fn zdotdir(&self) -> PathBuf {
        self.ctx
            .env
            .var_non_empty("ZDOTDIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| self.home().to_path_buf())
    }

    fn state_path(&self) -> PathBuf {
        self.ctx.data_dir.root().join("env-state.json")
    }

    fn load_state(&self) -> Result<EnvState> {
        Ok(fsx::read_json(&self.state_path(), "env-state.json")?.unwrap_or_default())
    }

    fn save_state(&self, s: &EnvState) -> Result<()> {
        fsx::write_json(&self.state_path(), s)
    }

    fn rc(&self, path: PathBuf, shell: ShellKind, placement: Placement) -> FileLoc {
        FileLoc {
            location: EnvLocation::ShellRc {
                path: path.clone(),
                shell: shell.clone(),
            },
            path,
            kind: FileKind::Block {
                dialect: Dialect::Shell(shell),
                placement,
            },
        }
    }

    /// Files Superba writes the variable into on this platform.
    fn file_locations(&self, var: &str) -> Vec<FileLoc> {
        let h = self.home();
        let shell = self.shell();
        let is = |k: ShellKind| shell.as_ref() == Some(&k);
        let exists = |p: &Path| p.exists();
        let mut v = Vec::new();
        match self.platform {
            EnvPlatform::Windows => {}
            EnvPlatform::Linux | EnvPlatform::MacOs => {
                let mac = self.platform == EnvPlatform::MacOs;
                let profile = h.join(".profile");
                if !mac || exists(&profile) {
                    v.push(self.rc(profile, ShellKind::Sh, Placement::Bottom));
                }
                let bash_profile = h.join(".bash_profile");
                if exists(&bash_profile) || (mac && is(ShellKind::Bash)) {
                    v.push(self.rc(bash_profile, ShellKind::Bash, Placement::Bottom));
                }
                let bashrc = h.join(".bashrc");
                if exists(&bashrc) || (!mac && is(ShellKind::Bash)) {
                    v.push(self.rc(bashrc, ShellKind::Bash, Placement::Top));
                }
                let zdot = self.zdotdir();
                let zshenv = zdot.join(".zshenv");
                if mac || exists(&zshenv) || exists(&zdot.join(".zshrc")) || is(ShellKind::Zsh) {
                    v.push(self.rc(zshenv, ShellKind::Zsh, Placement::Bottom));
                }
                let fish_dir = self.config_home().join("fish");
                if exists(&fish_dir) || is(ShellKind::Fish) {
                    let path = fish_dir.join("conf.d").join("superba.fish");
                    v.push(FileLoc {
                        location: EnvLocation::ShellRc {
                            path: path.clone(),
                            shell: ShellKind::Fish,
                        },
                        path,
                        kind: FileKind::Block {
                            dialect: Dialect::Shell(ShellKind::Fish),
                            placement: Placement::Bottom,
                        },
                    });
                }
                if !mac {
                    let path = self
                        .config_home()
                        .join("environment.d")
                        .join("60-superba.conf");
                    v.push(FileLoc {
                        location: EnvLocation::EnvironmentD { path: path.clone() },
                        path,
                        kind: FileKind::Block {
                            dialect: Dialect::EnvironmentD,
                            placement: Placement::Bottom,
                        },
                    });
                } else {
                    let path = h
                        .join("Library")
                        .join("LaunchAgents")
                        .join(format!("{}.plist", rc::launch_agent_label(var)));
                    v.push(FileLoc {
                        location: EnvLocation::LaunchAgent { path: path.clone() },
                        path,
                        kind: FileKind::Plist,
                    });
                }
            }
        }
        v
    }

    /// The value at a native location: `WSLENV` for
    /// [`EnvLocation::WslEnvForward`], else the variable itself.
    fn native_get(&self, loc: &EnvLocation, var: &str) -> Result<Option<String>> {
        match loc {
            EnvLocation::WslEnvForward => self
                .native
                .get(&EnvLocation::WindowsUserEnv, superba_core::wsl::ENV_WSLENV),
            _ => self.native.get(loc, var),
        }
    }

    fn native_set(&self, loc: &EnvLocation, var: &str, value: &str) -> Result<()> {
        match loc {
            EnvLocation::WslEnvForward => self.native.set(
                &EnvLocation::WindowsUserEnv,
                superba_core::wsl::ENV_WSLENV,
                value,
            ),
            _ => self.native.set(loc, var, value),
        }
    }

    fn native_delete(&self, loc: &EnvLocation, var: &str) -> Result<()> {
        match loc {
            EnvLocation::WslEnvForward => self
                .native
                .delete(&EnvLocation::WindowsUserEnv, superba_core::wsl::ENV_WSLENV),
            _ => self.native.delete(loc, var),
        }
    }

    /// The desktop app runs this computer's Codex inside WSL ("Run Codex in
    /// WSL", user bug 54 (b)): its `[desktop]` setting in this Codex home (or
    /// the global state it migrates from). That Codex sees a Windows variable
    /// only when `WSLENV` names it.
    fn app_runs_codex_in_wsl(&self) -> bool {
        if self.platform != EnvPlatform::Windows {
            return false;
        }
        let home = self.ctx.codex_home();
        let config = std::fs::read_to_string(home.config_toml()).ok();
        let global = std::fs::read(home.global_state_json())
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        superba_core::wsl::runs_codex_in_wsl(config.as_deref(), global.as_ref())
    }

    fn native_locations(&self) -> Vec<EnvLocation> {
        match self.platform {
            EnvPlatform::Linux => Vec::new(),
            EnvPlatform::MacOs => vec![EnvLocation::Launchctl],
            EnvPlatform::Windows => vec![EnvLocation::WindowsUserEnv],
        }
    }

    /// Existing files scanned for legacy / foreign lines.
    fn scan_files(&self, var: &str) -> Vec<(PathBuf, ShellKind)> {
        if self.platform == EnvPlatform::Windows {
            return Vec::new();
        }
        let h = self.home();
        let zdot = self.zdotdir();
        let mut v: Vec<(PathBuf, ShellKind)> = vec![
            (h.join(".bashrc"), ShellKind::Bash),
            (h.join(".bash_profile"), ShellKind::Bash),
            (h.join(".bash_login"), ShellKind::Bash),
            (h.join(".profile"), ShellKind::Sh),
            (zdot.join(".zshenv"), ShellKind::Zsh),
            (zdot.join(".zprofile"), ShellKind::Zsh),
            (zdot.join(".zshrc"), ShellKind::Zsh),
            (zdot.join(".zlogin"), ShellKind::Zsh),
            (
                self.config_home().join("fish").join("config.fish"),
                ShellKind::Fish,
            ),
        ];
        if let Ok(rd) = std::fs::read_dir(self.config_home().join("fish").join("conf.d")) {
            let mut extra: Vec<_> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().is_some_and(|e| e == "fish")
                        && p.file_name().is_some_and(|n| n != "superba.fish")
                })
                .collect();
            extra.sort();
            v.extend(extra.into_iter().map(|p| (p, ShellKind::Fish)));
        }
        let _ = var;
        let mut seen = BTreeSet::new();
        v.into_iter()
            .filter(|(p, _)| p.is_file() && seen.insert(p.clone()))
            .collect()
    }

    // ------------------------------------------------------------- inspect

    fn inspect_sync(&self, var: &str) -> Result<EnvReport> {
        let st = self.load_state()?;
        let vs = st.vars.get(var).cloned().unwrap_or_default();
        let mut persisted = Vec::new();
        let mut texts = PlannedTexts::default();
        for loc in self.file_locations(var) {
            let Some(text) = texts.read(&loc.path)? else {
                continue;
            };
            let value = file_value(&loc, &text, var);
            persisted.push(PersistedValue {
                location: loc.location.clone(),
                managed: value.is_some(),
                value,
            });
        }
        for loc in self.native_locations() {
            let value = self.native.get(&loc, var)?;
            persisted.push(PersistedValue {
                managed: vs.native_originals.contains_key(&location_key(&loc)),
                location: loc,
                value,
            });
        }
        let mut legacy_lines = Vec::new();
        let mut foreign_lines = Vec::new();
        for (path, _) in self.scan_files(var) {
            let Some(text) = texts.read(&path)? else {
                continue;
            };
            for f in rc::scan(&text, var) {
                let line = RcLine {
                    path: path.clone(),
                    line_no: f.line_no,
                    text: f.text,
                    value: f.value,
                };
                if f.legacy {
                    legacy_lines.push(line);
                } else {
                    foreign_lines.push(line);
                }
            }
        }
        Ok(EnvReport {
            var: var.to_owned(),
            persisted,
            legacy_lines,
            foreign_lines,
            process_value: match self.ctx.env.var(var) {
                Some(v) => EnvObservation::Present(v),
                None => EnvObservation::Absent,
            },
            superba_value: vs.written.clone(),
        })
    }

    // ---------------------------------------------------------------- plan

    /// [`EnvPersister::plan_launch`]: on Windows the edits of persisting the
    /// value (`HKCU\Environment`, and the `WSLENV` entry while the app runs
    /// Codex in WSL); elsewhere none (the launch passes the value, and the rc
    /// files are a switch's business).
    pub fn plan_launch_sync(&self, var: &str, value: &str) -> Result<EnvPlan> {
        let op = EnvOp::Set {
            value: value.to_owned(),
            take_over_foreign: false,
        };
        if self.platform != EnvPlatform::Windows {
            return Ok(EnvPlan {
                var: var.to_owned(),
                op,
                edits: Vec::new(),
                scope_notes: Vec::new(),
            });
        }
        self.plan_sync(var, &op)
    }

    fn plan_sync(&self, var: &str, op: &EnvOp) -> Result<EnvPlan> {
        validate_name(var)?;
        if let EnvOp::Set { value, .. } = op {
            validate_value(value)?;
        }
        let st = self.load_state()?;
        let vs = st.vars.get(var).cloned().unwrap_or_default();
        let mut edits = Vec::new();

        // Per file: current text, edits in order (comments first, then the block).
        let mut texts = PlannedTexts::default();
        // Locations a clear or restore leaves alone (GH#5).
        let mut kept: Vec<LocalizedText> = Vec::new();

        // 1. Legacy / foreign lines.
        let mut line_edits: Vec<(PathBuf, ShellKind, EnvEditAction)> = Vec::new();
        match op {
            EnvOp::Set {
                take_over_foreign, ..
            } => {
                for (path, shell) in self.scan_files(var) {
                    let Some(text) = texts.read(&path)? else {
                        continue;
                    };
                    for f in rc::scan(&text, var) {
                        if f.legacy || *take_over_foreign {
                            line_edits.push((
                                path.clone(),
                                shell.clone(),
                                EnvEditAction::CommentLegacyLine {
                                    line_no: f.line_no,
                                    original: f.text,
                                },
                            ));
                        }
                    }
                }
            }
            EnvOp::Clear | EnvOp::RestoreBaseline { .. } => {
                let mut recorded: Vec<_> = vs.commented().collect();
                let mut all = Vec::new();
                if let EnvOp::RestoreBaseline { legacy_lines, .. } = op {
                    for line in legacy_lines {
                        // Union by occurrence, not by line number: inserting our
                        // marker block may have shifted a baseline line.
                        if let Some(index) = recorded
                            .iter()
                            .position(|r| r.path == line.path && r.text == line.text)
                        {
                            recorded.remove(index);
                        }
                        all.push(line);
                    }
                }
                all.extend(recorded);
                for l in all {
                    if let Some(text) = texts.read(&l.path)?
                        && rc::has_commented(&text, &l.text)
                    {
                        line_edits.push((
                            l.path.clone(),
                            shell_for(&l.path),
                            EnvEditAction::UncommentLegacyLine {
                                line_no: l.line_no,
                                original: l.text.clone(),
                            },
                        ));
                    }
                }
            }
        }
        let mut working: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
        for (path, shell, action) in line_edits {
            let before = match working.get(&path) {
                Some(t) => t.clone(),
                None => texts.read(&path)?,
            };
            let Some(before_text) = before else { continue };
            let after_text = match &action {
                EnvEditAction::CommentLegacyLine { line_no, original } => {
                    rc::comment_line(&before_text, *line_no, original)
                }
                EnvEditAction::UncommentLegacyLine { original, .. } => {
                    rc::uncomment_line(&before_text, original)
                }
                _ => None,
            };
            let Some(after_text) = after_text else {
                continue;
            };
            let preview = match &action {
                EnvEditAction::CommentLegacyLine { original, .. } => {
                    format!("{}{original}", superba_core::env::LEGACY_COMMENT_PREFIX)
                }
                EnvEditAction::UncommentLegacyLine { original, .. } => original.clone(),
                _ => String::new(),
            };
            edits.push(EnvEdit {
                location: EnvLocation::ShellRc {
                    path: path.clone(),
                    shell,
                },
                action,
                preview,
                before: Some(fsx::sha256_hex(before_text.as_bytes())),
                after: Some(fsx::sha256_hex(after_text.as_bytes())),
            });
            working.insert(path, Some(after_text));
        }

        // 2. Blocks.
        let snapshot_for = |loc: &EnvLocation| -> Option<Option<String>> {
            match op {
                EnvOp::RestoreBaseline { snapshots, .. } => snapshots
                    .iter()
                    .find(|s| location_key(&s.location) == location_key(loc))
                    .map(|s| s.value.clone()),
                _ => None,
            }
        };
        for loc in self.file_locations(var) {
            let before = match working.get(&loc.path) {
                Some(t) => t.clone(),
                None => texts.read(&loc.path)?,
            };
            if texts.non_utf8.contains(&loc.path) {
                continue;
            }
            let cur_value = before.as_deref().and_then(|t| file_value(&loc, t, var));
            let want: Option<String> = match op {
                EnvOp::Set { value, .. } => Some(value.clone()),
                EnvOp::Clear => None,
                EnvOp::RestoreBaseline { .. } => snapshot_for(&loc.location).flatten(),
            };
            if cur_value == want {
                continue;
            }
            if !matches!(op, EnvOp::Set { .. }) && vs.changed_since_written(cur_value.as_deref()) {
                kept.push(kept_note(
                    &loc.location,
                    cur_value.as_deref().unwrap_or_default(),
                ));
                continue;
            }
            let (after, action, preview) =
                render_file(&loc, before.as_deref(), var, want.as_deref());
            edits.push(EnvEdit {
                location: loc.location.clone(),
                action,
                preview,
                before: before.as_deref().map(|t| fsx::sha256_hex(t.as_bytes())),
                after: after.as_deref().map(|t| fsx::sha256_hex(t.as_bytes())),
            });
            working.insert(loc.path.clone(), after);
        }

        // 3. Native stores.
        for loc in self.native_locations() {
            let cur = self.native_get(&loc, var)?;
            let key = location_key(&loc);
            let managed = vs.native_originals.get(&key);
            let want: Option<String> = match op {
                EnvOp::Set { value, .. } => Some(value.clone()),
                EnvOp::Clear => match managed {
                    Some(orig) => orig.clone(),
                    None => continue,
                },
                EnvOp::RestoreBaseline { .. } => match (snapshot_for(&loc), managed) {
                    (Some(v), _) => v,
                    (None, Some(orig)) => orig.clone(),
                    (None, None) => continue,
                },
            };
            if cur == want {
                continue;
            }
            if !matches!(op, EnvOp::Set { .. }) && vs.changed_since_written(cur.as_deref()) {
                kept.push(kept_note(&loc, cur.as_deref().unwrap_or_default()));
                continue;
            }
            edits.push(EnvEdit {
                location: loc,
                action: match &want {
                    Some(v) => EnvEditAction::SetValue { value: v.clone() },
                    None => EnvEditAction::DeleteValue,
                },
                preview: want.clone().unwrap_or_default(),
                before: cur,
                after: want,
            });
        }

        // 4. "Run Codex in WSL": `WSLENV` forwards the variable into WSL while
        //    Superba persists it; the entry goes again with the value.
        let mut wsl_forward = false;
        if self.platform == EnvPlatform::Windows
            && matches!(
                var,
                superba_core::codex::ENV_AUTHAPI_BASE_URL | "CODEX_CA_CERTIFICATE"
            )
        {
            let loc = EnvLocation::WslEnvForward;
            let managed = vs.native_originals.contains_key(&location_key(&loc));
            let cur = self.native_get(&loc, var)?;
            // A routed WSL host also needs trust during startup, before its
            // login shell selects its own Linux bundle. Forward Windows CA
            // paths even when the app's legacy WSL setting is off.
            let forward = matches!(op, EnvOp::Set { .. })
                && (var == "CODEX_CA_CERTIFICATE" || self.app_runs_codex_in_wsl());
            let want = if forward {
                if var == "CODEX_CA_CERTIFICATE" {
                    // Windows paths must become /mnt/... inside the app's WSL
                    // Codex, while the native app keeps the Windows path.
                    let without = cur
                        .as_deref()
                        .and_then(|s| superba_core::wsl::wslenv_without(s, var));
                    Some(match without {
                        Some(s) => format!("{s}:{var}/p"),
                        None => format!("{var}/p"),
                    })
                } else {
                    superba_core::wsl::wslenv_with(cur.as_deref(), var).or_else(|| cur.clone())
                }
            } else if managed {
                match cur.as_deref() {
                    Some(c) if superba_core::wsl::wslenv_names(c, var) => {
                        let without = superba_core::wsl::wslenv_without(c, var);
                        let previous = vs
                            .native_originals
                            .get(&location_key(&loc))
                            .and_then(|v| v.as_deref())
                            .and_then(|v| {
                                v.split(':')
                                    .find(|part| part.split('/').next() == Some(var))
                            });
                        match (without, previous) {
                            (Some(rest), Some(entry)) => Some(format!("{rest}:{entry}")),
                            (None, Some(entry)) => Some(entry.to_owned()),
                            (rest, None) => rest,
                        }
                    }
                    _ => cur.clone(),
                }
            } else {
                cur.clone()
            };
            if want != cur {
                wsl_forward = forward;
                edits.push(EnvEdit {
                    location: loc,
                    action: match &want {
                        Some(v) => EnvEditAction::SetValue { value: v.clone() },
                        None => EnvEditAction::DeleteValue,
                    },
                    preview: want.clone().unwrap_or_default(),
                    before: cur,
                    after: want,
                });
            }
        }

        let mut scope_notes = kept;
        scope_notes.extend(self.scope_notes());
        if wsl_forward {
            scope_notes.push(LocalizedText::new(
                "env.scope.windows_wsl",
                "The ChatGPT app runs Codex inside WSL (\"Run Codex in WSL\"): WSLENV now forwards the variable there too; the app's Codex gets it after the app restarts.",
            ));
        }
        scope_notes.extend(texts.notices());
        Ok(EnvPlan {
            var: var.to_owned(),
            op: op.clone(),
            edits,
            scope_notes,
        })
    }

    fn scope_notes(&self) -> Vec<LocalizedText> {
        let t = |k: &str, f: &str| LocalizedText::new(format!("env.scope.{k}"), f);
        match self.platform {
            EnvPlatform::Linux => vec![
                t(
                    "new_shells",
                    "New terminal windows get the new value right away.",
                ),
                t(
                    "linux_session",
                    "Apps started from the desktop menu see it after logging out and back in (systemd environment.d; some KDE / X11 sessions ignore it).",
                ),
                t(
                    "restart_running",
                    "Running IDEs and terminals keep the old value until restarted.",
                ),
            ],
            EnvPlatform::MacOs => vec![
                t(
                    "new_shells",
                    "New terminal windows get the new value right away.",
                ),
                t(
                    "macos_launchd",
                    "Apps opened from the Dock or Finder from now on get it immediately (launchctl); it is re-applied at every login.",
                ),
                t(
                    "restart_running",
                    "Running IDEs and terminals keep the old value until restarted.",
                ),
            ],
            EnvPlatform::Windows => vec![
                t(
                    "windows_broadcast",
                    "New Command Prompt / PowerShell windows opened from Explorer get the new value right away.",
                ),
                t(
                    "windows_terminal",
                    "Windows Terminal and VS Code must be fully restarted to see it.",
                ),
            ],
        }
    }

    // --------------------------------------------------------------- apply

    fn apply_sync(&self, plan: &EnvPlan) -> Result<EnvApplied> {
        // Re-plan if anything moved since planning; then perform those edits.
        let plan = if self.plan_is_current(plan)? {
            plan.clone()
        } else {
            tracing::info!("environment files changed since planning; re-planning");
            // Diagnostic comment-out plans use Clear as their public operation,
            // but must retain their intent when the underlying files move.
            if matches!(plan.op, EnvOp::Clear)
                && plan
                    .edits
                    .iter()
                    .any(|e| matches!(e.action, EnvEditAction::CommentLegacyLine { .. }))
            {
                self.plan_comment_out_sync(&plan.var)?
            } else {
                self.plan_sync(&plan.var, &plan.op)?
            }
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = self.clock.now();
        let backup_dir =
            self.ctx
                .data_dir
                .rc_backups_dir()
                .join(format!("{}-{}", now.as_millis(), &id[..8]));

        // Group file edits by path, keep first-seen order.
        let mut order: Vec<PathBuf> = Vec::new();
        let mut by_path: BTreeMap<PathBuf, Vec<&EnvEdit>> = BTreeMap::new();
        for e in &plan.edits {
            if let Some(p) = edit_path(&e.location) {
                if !by_path.contains_key(&p) {
                    order.push(p.clone());
                }
                by_path.entry(p).or_default().push(e);
            }
        }
        let mut backups = Vec::new();
        let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
        let result = (|| -> Result<()> {
            for path in &order {
                let edits = &by_path[path];
                let current = read_text(path)?;
                let mut text = current.clone();
                for e in edits {
                    text = self.replay(e, &plan.var, text.as_deref())?;
                }
                let expected = edits.last().and_then(|e| e.after.clone());
                if text.as_deref().map(|t| fsx::sha256_hex(t.as_bytes())) != expected {
                    return Err(Error::Conflict {
                        message: format!("{} changed while it was being edited", path.display()),
                    });
                }
                let backup = match &current {
                    Some(c) => {
                        let b = backup_dir.join(backup_name(path));
                        fsx::ensure_private_dir(&backup_dir)?;
                        fsx::write_atomic(&b, c.as_bytes(), Mode::Private)?;
                        backups.push(b.clone());
                        Some(b)
                    }
                    None => None,
                };
                match &text {
                    Some(t) => {
                        if let Some(dir) = path.parent() {
                            fsx::ensure_dir(dir)?;
                        }
                        fsx::write_atomic(path, t.as_bytes(), Mode::PreserveOrPublic)?;
                    }
                    None => fsx::remove_if_exists(path)?,
                }
                done.push((path.clone(), backup));
            }
            for e in plan
                .edits
                .iter()
                .filter(|e| edit_path(&e.location).is_none())
            {
                match &e.action {
                    EnvEditAction::SetValue { value } => {
                        self.native_set(&e.location, &plan.var, value)?
                    }
                    EnvEditAction::DeleteValue => self.native_delete(&e.location, &plan.var)?,
                    _ => {}
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            // Failure atomicity: put back the files already written.
            for (path, backup) in done.iter().rev() {
                let _ = match backup {
                    Some(b) => std::fs::read(b)
                        .map_err(|e| Error::io(b, &e))
                        .and_then(|bytes| fsx::write_atomic(path, &bytes, Mode::PreserveOrPublic)),
                    None => fsx::remove_if_exists(path),
                };
            }
            return Err(e);
        }
        self.record_applied(&plan)?;
        Ok(EnvApplied {
            id,
            var: plan.var.clone(),
            edits: plan.edits.clone(),
            backups,
            applied_at: now,
        })
    }

    fn plan_is_current(&self, plan: &EnvPlan) -> Result<bool> {
        let mut first_before: BTreeMap<PathBuf, Option<String>> = BTreeMap::new();
        for e in &plan.edits {
            match edit_path(&e.location) {
                Some(p) => {
                    first_before.entry(p).or_insert_with(|| e.before.clone());
                }
                None => {
                    if self.native_get(&e.location, &plan.var)? != e.before {
                        return Ok(false);
                    }
                }
            }
        }
        for (p, before) in first_before {
            if fsx::file_sha256(&p)? != before {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Apply one file edit to `text` (`None` = file absent / deleted).
    fn replay(&self, e: &EnvEdit, var: &str, text: Option<&str>) -> Result<Option<String>> {
        let missing = || Error::Conflict {
            message: format!("cannot replay {:?}", e.action),
        };
        match &e.action {
            EnvEditAction::CommentLegacyLine { line_no, original } => {
                rc::comment_line(text.ok_or_else(missing)?, *line_no, original)
                    .map(Some)
                    .ok_or_else(missing)
            }
            EnvEditAction::UncommentLegacyLine { original, .. } => {
                rc::uncomment_line(text.ok_or_else(missing)?, original)
                    .map(Some)
                    .ok_or_else(missing)
            }
            EnvEditAction::WriteBlock | EnvEditAction::RemoveBlock => {
                let loc = self
                    .file_locations(var)
                    .into_iter()
                    .find(|l| location_key(&l.location) == location_key(&e.location))
                    .ok_or_else(missing)?;
                let want = match &e.action {
                    EnvEditAction::WriteBlock => Some(e.preview_value(&loc, var)),
                    _ => None,
                };
                let want = want.flatten();
                Ok(render_file(&loc, text, var, want.as_deref()).0)
            }
            _ => Err(missing()),
        }
    }

    fn record_applied(&self, plan: &EnvPlan) -> Result<()> {
        let mut st = self.load_state()?;
        let vs = st.vars.entry(plan.var.clone()).or_default();
        vs.written = match &plan.op {
            EnvOp::Set { value, .. } => Some(value.clone()),
            EnvOp::Clear | EnvOp::RestoreBaseline { .. } => None,
        };
        for e in &plan.edits {
            match (&e.location, &e.action) {
                (EnvLocation::WslEnvForward, _) => {
                    // Ours while our entry is in it.
                    let key = location_key(&e.location);
                    if matches!(plan.op, EnvOp::Set { .. })
                        && e.after
                            .as_deref()
                            .is_some_and(|a| superba_core::wsl::wslenv_names(a, &plan.var))
                    {
                        vs.native_originals
                            .entry(key)
                            .or_insert_with(|| e.before.clone());
                    } else {
                        vs.native_originals.remove(&key);
                    }
                }
                (loc, EnvEditAction::SetValue { .. }) if edit_path(loc).is_none() => {
                    if matches!(plan.op, EnvOp::Set { .. }) {
                        vs.native_originals
                            .entry(location_key(loc))
                            .or_insert_with(|| e.before.clone());
                    } else {
                        vs.native_originals.remove(&location_key(loc));
                    }
                }
                (loc, EnvEditAction::DeleteValue) if edit_path(loc).is_none() => {
                    vs.native_originals.remove(&location_key(loc));
                }
                (
                    EnvLocation::ShellRc { path, .. },
                    EnvEditAction::CommentLegacyLine { line_no, original },
                ) => {
                    vs.record_comment(path, *line_no, original, &plan.var);
                }
                (
                    EnvLocation::ShellRc { path, .. },
                    EnvEditAction::UncommentLegacyLine { original, .. },
                ) => {
                    vs.forget_comment(path, original);
                }
                _ => {}
            }
        }
        if vs.is_empty() {
            st.vars.remove(&plan.var);
        }
        self.save_state(&st)
    }

    // -------------------------------------------------------------- revert

    fn revert_sync(&self, applied: &EnvApplied) -> Result<()> {
        let mut conflicts = Vec::new();
        let mut reverted_paths = BTreeSet::new();
        let mut order: Vec<PathBuf> = Vec::new();
        let mut by_path: BTreeMap<PathBuf, Vec<&EnvEdit>> = BTreeMap::new();
        for e in &applied.edits {
            if let Some(p) = edit_path(&e.location) {
                if !by_path.contains_key(&p) {
                    order.push(p.clone());
                }
                by_path.entry(p).or_default().push(e);
            }
        }
        for path in order.iter().rev() {
            let edits = &by_path[path];
            let current = read_text(path)?;
            let cur_sha = current.as_deref().map(|t| fsx::sha256_hex(t.as_bytes()));
            let backup = applied.backups.iter().find(|b| {
                b.file_name().is_some_and(|n| {
                    n.to_string_lossy() == backup_name(path)
                        // Receipts from before backup names included a hash.
                        || n.to_string_lossy() == fsx::sanitize_component(&path.to_string_lossy())
                })
            });
            let pre: Option<String> = match backup {
                Some(b) => Some(std::fs::read_to_string(b).map_err(|e| Error::io(b, &e))?),
                None => None,
            };
            let restored = if cur_sha == edits.last().and_then(|e| e.after.clone()) {
                // Untouched since: restore the exact pre-image.
                pre.clone()
            } else {
                // Edited since: undo our edits surgically.
                let Some(mut text) = current.clone() else {
                    conflicts.push(path.display().to_string());
                    continue;
                };
                for e in edits.iter().rev() {
                    text = match &e.action {
                        EnvEditAction::CommentLegacyLine { original, .. } => {
                            rc::uncomment_line(&text, original).unwrap_or(text)
                        }
                        EnvEditAction::UncommentLegacyLine { line_no, original } => {
                            rc::comment_line(&text, *line_no, original).unwrap_or(text)
                        }
                        EnvEditAction::WriteBlock | EnvEditAction::RemoveBlock => {
                            let loc = self
                                .file_locations(&applied.var)
                                .into_iter()
                                .find(|l| location_key(&l.location) == location_key(&e.location));
                            match loc {
                                Some(loc) => {
                                    let prev = pre
                                        .as_deref()
                                        .and_then(|p| file_value(&loc, p, &applied.var));
                                    render_file(&loc, Some(&text), &applied.var, prev.as_deref())
                                        .0
                                        .unwrap_or_default()
                                }
                                None => text,
                            }
                        }
                        _ => text,
                    };
                }
                Some(text)
            };
            let first_before_absent = edits.first().is_some_and(|e| e.before.is_none());
            match restored {
                Some(t) if !(first_before_absent && t.trim().is_empty() && pre.is_none()) => {
                    fsx::write_atomic(path, t.as_bytes(), Mode::PreserveOrPublic)?
                }
                _ => fsx::remove_if_exists(path)?,
            }
            reverted_paths.insert(path.clone());
        }
        // Native values: restore `before` where the store still holds `after`.
        let mut st = self.load_state()?;
        let vs = st.vars.entry(applied.var.clone()).or_default();
        for e in applied
            .edits
            .iter()
            .rev()
            .filter(|e| edit_path(&e.location).is_none())
        {
            let cur = self.native_get(&e.location, &applied.var)?;
            if cur != e.after {
                conflicts.push(format!("{:?}", e.location));
                continue;
            }
            match &e.before {
                Some(v) => self.native_set(&e.location, &applied.var, v)?,
                None => self.native_delete(&e.location, &applied.var)?,
            }
            let key = location_key(&e.location);
            match vs.native_originals.remove(&key) {
                Some(_) => {}
                None => {
                    // Reverting a Clear / restore: we manage it again.
                    vs.native_originals.insert(key, e.after.clone());
                }
            }
        }
        for e in &applied.edits {
            if let EnvLocation::ShellRc { path, .. } = &e.location
                && reverted_paths.contains(path)
            {
                match &e.action {
                    EnvEditAction::CommentLegacyLine { original, .. } => {
                        vs.forget_comment(path, original);
                    }
                    EnvEditAction::UncommentLegacyLine { line_no, original } => {
                        vs.record_comment(path, *line_no, original, &applied.var);
                    }
                    _ => {}
                }
            }
        }
        // What Superba's own blocks and values hold after the revert is what it
        // had written before (GH#5 change detection).
        vs.written = self.managed_value(&applied.var, vs)?;
        if vs.is_empty() {
            st.vars.remove(&applied.var);
        }
        self.save_state(&st)?;
        if conflicts.is_empty() {
            Ok(())
        } else {
            Err(Error::Conflict {
                message: format!(
                    "changed since Superba wrote them, left as is: {}",
                    conflicts.join(", ")
                ),
            })
        }
    }

    /// The value in Superba's own marker blocks (else in a native store Superba
    /// manages), if any.
    fn managed_value(&self, var: &str, vs: &VarState) -> Result<Option<String>> {
        for loc in self.file_locations(var) {
            if let Some(text) = read_text(&loc.path).ok().flatten()
                && let Some(v) = file_value(&loc, &text, var)
            {
                return Ok(Some(v));
            }
        }
        for loc in self.native_locations() {
            if vs.native_originals.contains_key(&location_key(&loc))
                && let Some(v) = self.native.get(&loc, var)?
            {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Plan commenting out every line (legacy and foreign) that sets `var` in the
    /// user's shell files, without writing a new value. Used by diagnostics for
    /// conflicting variables such as `OPENAI_BASE_URL` / `OPENAI_API_KEY` (plan 4
    /// #21); apply it with [`EnvPersister::apply`] (pre-images are kept, `revert`
    /// undoes it). The plan's `op` is recorded as `Clear`.
    pub async fn plan_comment_out(&self, var: &str) -> Result<EnvPlan> {
        let this = self.clone();
        let var = var.to_owned();
        blocking(move || this.plan_comment_out_sync(&var)).await
    }

    fn plan_comment_out_sync(&self, var: &str) -> Result<EnvPlan> {
        validate_name(var)?;
        let mut edits = Vec::new();
        let mut texts = PlannedTexts::default();
        for (path, shell) in self.scan_files(var) {
            let Some(mut text) = texts.read(&path)? else {
                continue;
            };
            for f in rc::scan(&text, var) {
                let Some(after) = rc::comment_line(&text, f.line_no, &f.text) else {
                    continue;
                };
                edits.push(EnvEdit {
                    location: EnvLocation::ShellRc {
                        path: path.clone(),
                        shell: shell.clone(),
                    },
                    action: EnvEditAction::CommentLegacyLine {
                        line_no: f.line_no,
                        original: f.text.clone(),
                    },
                    preview: format!("{}{}", superba_core::env::LEGACY_COMMENT_PREFIX, f.text),
                    before: Some(fsx::sha256_hex(text.as_bytes())),
                    after: Some(fsx::sha256_hex(after.as_bytes())),
                });
                text = after;
            }
        }
        let mut scope_notes = self.scope_notes();
        scope_notes.extend(texts.notices());
        Ok(EnvPlan {
            var: var.to_owned(),
            op: EnvOp::Clear,
            edits,
            scope_notes,
        })
    }
}

trait PreviewValue {
    fn preview_value(&self, loc: &FileLoc, var: &str) -> Option<String>;
}

impl PreviewValue for EnvEdit {
    /// The value a `WriteBlock` edit writes (parsed back from its preview).
    fn preview_value(&self, loc: &FileLoc, var: &str) -> Option<String> {
        file_value(loc, &self.preview, var)
    }
}

/// Value of `var` held at a file location.
fn file_value(loc: &FileLoc, text: &str, var: &str) -> Option<String> {
    match &loc.kind {
        FileKind::Block { dialect, .. } => rc::block_value(text, dialect, var),
        FileKind::Plist => rc::plist_value(text, var),
    }
}

/// New file text for `var = want` at `loc` (`None` text = delete the file), the
/// edit action and its preview.
fn render_file(
    loc: &FileLoc,
    current: Option<&str>,
    var: &str,
    want: Option<&str>,
) -> (Option<String>, EnvEditAction, String) {
    match &loc.kind {
        FileKind::Plist => match want {
            Some(v) => {
                let p = rc::render_plist(var, v);
                (Some(p.clone()), EnvEditAction::WriteBlock, p)
            }
            None => (None, EnvEditAction::RemoveBlock, String::new()),
        },
        FileKind::Block { dialect, placement } => {
            let text = current.unwrap_or("");
            let mut vars = rc::block_vars(text, dialect).unwrap_or_default();
            vars.retain(|(n, _)| n != var);
            if let Some(v) = want {
                vars.push((var.to_owned(), v.to_owned()));
                vars.sort();
            }
            let block = (!vars.is_empty()).then(|| rc::render(dialect, &vars));
            let new = rc::put_block(text, block.as_deref(), *placement);
            let action = if want.is_some() {
                EnvEditAction::WriteBlock
            } else {
                EnvEditAction::RemoveBlock
            };
            let preview = block.unwrap_or_default();
            // A file left empty once our block is gone held nothing but the block
            // (Superba created it, or the user's copy was empty): remove it.
            if new.trim().is_empty() {
                (None, action, preview)
            } else {
                (Some(new), action, preview)
            }
        }
    }
}

fn edit_path(loc: &EnvLocation) -> Option<PathBuf> {
    match loc {
        EnvLocation::ShellRc { path, .. }
        | EnvLocation::EnvironmentD { path }
        | EnvLocation::LaunchAgent { path } => Some(path.clone()),
        _ => None,
    }
}

fn shell_for(path: &Path) -> ShellKind {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.ends_with(".fish") {
        ShellKind::Fish
    } else if name.starts_with(".z") {
        ShellKind::Zsh
    } else if name.starts_with(".bash") {
        ShellKind::Bash
    } else {
        ShellKind::Sh
    }
}

fn backup_name(path: &Path) -> String {
    // Sanitizing truncates long paths and maps distinct punctuation to '_'.
    // Include the full path's hash so two rc files cannot share a pre-image.
    let path = path.to_string_lossy();
    format!(
        "{}-{}",
        fsx::sanitize_component(&path),
        fsx::sha256_hex(path.as_bytes())
    )
}

fn read_text(path: &Path) -> Result<Option<String>> {
    match fsx::read_optional(path)? {
        None => Ok(None),
        Some(b) => String::from_utf8(b).map(Some).map_err(|_| Error::Parse {
            what: path.display().to_string(),
            message: "contains non-UTF-8 bytes; convert it to UTF-8".into(),
        }),
    }
}

fn validate_name(var: &str) -> Result<()> {
    let ok = !var.is_empty()
        && var
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "invalid environment variable name {var:?}"
        )))
    }
}

fn validate_value(value: &str) -> Result<()> {
    if value.chars().any(char::is_control) {
        return Err(Error::invalid(
            "environment values must not contain control characters",
        ));
    }
    if value.trim().is_empty() {
        return Err(Error::invalid("environment value is empty"));
    }
    Ok(())
}

/// The snapshot of `var` at every location (for the baseline).
pub fn snapshots_from(report: &EnvReport) -> Vec<EnvSnapshot> {
    report
        .persisted
        .iter()
        .map(|p| EnvSnapshot {
            location: p.location.clone(),
            value: p.value.clone(),
        })
        .collect()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::other(format!("env persister task failed: {e}")))?
}

#[async_trait]
impl EnvPersister for ShellEnvPersister {
    async fn inspect(&self, var: &str) -> Result<EnvReport> {
        let this = self.clone();
        let var = var.to_owned();
        blocking(move || this.inspect_sync(&var)).await
    }

    async fn plan(&self, var: &str, op: EnvOp) -> Result<EnvPlan> {
        let this = self.clone();
        let var = var.to_owned();
        blocking(move || this.plan_sync(&var, &op)).await
    }

    async fn apply(&self, plan: &EnvPlan) -> Result<EnvApplied> {
        let this = self.clone();
        let plan = plan.clone();
        blocking(move || this.apply_sync(&plan)).await
    }

    async fn revert(&self, applied: &EnvApplied) -> Result<()> {
        let this = self.clone();
        let applied = applied.clone();
        blocking(move || this.revert_sync(&applied)).await
    }

    async fn plan_launch(&self, var: &str, value: &str) -> Result<EnvPlan> {
        let this = self.clone();
        let var = var.to_owned();
        let value = value.to_owned();
        blocking(move || this.plan_launch_sync(&var, &value)).await
    }
}
