//! Persistent environment variables (`CODEX_AUTHAPI_BASE_URL`): shared marker-block
//! format, legacy tutorial line detection, and the plan/report types of
//! [`crate::EnvPersister`].
//!
//! The marker block format is a contract between the local persister
//! (`superba-config`) and the remote writer (`superba-remote`); both must use
//! [`render_block`] and [`find_block`] so uninstall removes exactly what was written.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::catalog::is_krill_host;
use crate::codex::ENV_AUTHAPI_BASE_URL;
use crate::i18n::LocalizedText;
use crate::time::Timestamp;

/// First line of a Superba-managed block in a shell rc file.
pub const MARKER_BEGIN: &str = "# >>> superba >>>";
/// Last line of a Superba-managed block.
pub const MARKER_END: &str = "# <<< superba <<<";
/// Comment line inside the block.
pub const MARKER_NOTE: &str = "# Managed by Superba. Edits inside this block are overwritten.";
/// Prefix put in front of a legacy tutorial line when it is taken over
/// (`# [superba] export CODEX_AUTHAPI_BASE_URL=...`); removed again on restore.
pub const LEGACY_COMMENT_PREFIX: &str = "# [superba] ";

/// Shell dialects Superba writes to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    /// zsh (`~/.zshenv`).
    Zsh,
    /// bash (top of `~/.bashrc` plus `~/.profile` / `~/.bash_profile`).
    Bash,
    /// fish (`~/.config/fish/conf.d/superba.fish`).
    Fish,
    /// POSIX sh (`~/.profile`).
    Sh,
    /// Something else (named by its basename).
    Other(String),
}

impl ShellKind {
    /// Classify from `$SHELL` or a path like `/usr/bin/zsh`.
    pub fn from_shell_path(path: &str) -> Self {
        let base = path.rsplit(['/', '\\']).next().unwrap_or(path).trim();
        let base = base.strip_suffix(".exe").unwrap_or(base);
        match base {
            "zsh" => ShellKind::Zsh,
            "bash" => ShellKind::Bash,
            "fish" => ShellKind::Fish,
            "sh" | "dash" | "ash" | "ksh" => ShellKind::Sh,
            other => ShellKind::Other(other.to_owned()),
        }
    }

    /// True for fish syntax.
    pub fn is_fish(&self) -> bool {
        matches!(self, ShellKind::Fish)
    }
}

/// Quote a value for POSIX sh double quotes or fish double quotes.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Render a complete marker block (with trailing newline) setting `vars`.
///
/// POSIX shells: `export NAME="value"`; fish: `set -gx NAME "value"`.
pub fn render_block(shell: &ShellKind, vars: &[(&str, &str)]) -> String {
    let mut s = String::new();
    s.push_str(MARKER_BEGIN);
    s.push('\n');
    s.push_str(MARKER_NOTE);
    s.push('\n');
    for (name, value) in vars {
        if shell.is_fish() {
            s.push_str(&format!("set -gx {name} {}\n", quote(value)));
        } else {
            s.push_str(&format!("export {name}={}\n", quote(value)));
        }
    }
    s.push_str(MARKER_END);
    s.push('\n');
    s
}

/// Byte range of the first marker block in `text`, including the end marker line
/// and its newline. `None` if absent or unterminated.
pub fn find_block(text: &str) -> Option<std::ops::Range<usize>> {
    let (start, _) = find_line(text, MARKER_BEGIN)?;
    let (end_start, end_len) = find_line(&text[start..], MARKER_END)?;
    Some(start..start + end_start + end_len)
}

/// Offset and full length (including the line break) of the first line whose
/// trimmed content equals `line`.
fn find_line(text: &str, line: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for l in text.split_inclusive('\n') {
        if l.trim() == line {
            return Some((offset, l.len()));
        }
        offset += l.len();
    }
    None
}

/// If `line` is a legacy Krill tutorial line
/// `export CODEX_AUTHAPI_BASE_URL=<krill https origin>` (optionally quoted, not
/// commented), return the value. Lines pointing at non-Krill hosts return `None`
/// (they are "foreign" and need the user's decision).
pub fn legacy_krill_export_value(line: &str) -> Option<String> {
    let value = export_value(line, ENV_AUTHAPI_BASE_URL)?;
    let url = url::Url::parse(&value).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    is_krill_host(url.host_str()?).then_some(value)
}

/// If `line` is an uncommented `export NAME=value` (POSIX) or `set -gx NAME value`
/// (fish) for `name`, return the unquoted value.
pub fn export_value(line: &str, name: &str) -> Option<String> {
    let t = line.trim();
    if t.starts_with('#') {
        return None;
    }
    let rest = if let Some(r) = t.strip_prefix("export ") {
        r.trim_start().strip_prefix(name)?.strip_prefix('=')?
    } else {
        let r = t
            .strip_prefix("set -gx ")
            .or_else(|| t.strip_prefix("set -x "))?;
        r.trim_start()
            .strip_prefix(name)?
            .strip_prefix(' ')?
            .trim_start()
    };
    let rest = rest
        .split(" #")
        .next()
        .unwrap_or(rest)
        .trim()
        .trim_end_matches(';');
    let unquoted = rest
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| rest.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))
        .unwrap_or(rest);
    (!unquoted.is_empty()).then(|| unquoted.to_owned())
}

/// What we know about a variable in some process's environment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum EnvObservation {
    /// Set to this value.
    Present(String),
    /// Known to be unset.
    Absent,
    /// Could not be read (permissions, platform).
    #[default]
    Unknown,
}

impl EnvObservation {
    /// The value, if present.
    pub fn value(&self) -> Option<&str> {
        match self {
            EnvObservation::Present(v) => Some(v),
            _ => None,
        }
    }
}

/// What to do with a variable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EnvOp {
    /// Persist `value` (marker blocks / environment.d / launchctl / HKCU) and take over
    /// legacy Krill tutorial lines (always commented out).
    Set {
        /// New value.
        value: String,
        /// Also comment out the user's own non-Krill lines for the variable
        /// (answer to `PlanChoice::ForeignEnv`); otherwise they are left alone and
        /// reported.
        #[serde(default)]
        take_over_foreign: bool,
    },
    /// Remove Superba's persisted value (restoring a pre-existing value where we
    /// replaced one).
    Clear,
    /// Return to the baseline: remove blocks, un-comment legacy lines, restore the
    /// original registry / launchctl values. Used by "restore" and "uninstall".
    /// The baseline data comes from [`crate::Baseline`] (owned by the config engine).
    RestoreBaseline {
        /// Original values per location.
        snapshots: Vec<EnvSnapshot>,
        /// Legacy lines that were commented out at takeover.
        legacy_lines: Vec<RcLine>,
    },
}

/// A place a variable can be persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EnvLocation {
    /// A shell startup file.
    ShellRc {
        /// File path.
        path: PathBuf,
        /// Dialect.
        shell: ShellKind,
    },
    /// Linux `~/.config/environment.d/60-superba.conf` (systemd user session; needs re-login).
    EnvironmentD {
        /// File path.
        path: PathBuf,
    },
    /// macOS LaunchAgent plist that re-runs `launchctl setenv` at login.
    LaunchAgent {
        /// Plist path.
        path: PathBuf,
    },
    /// macOS `launchctl setenv` for the current login session.
    Launchctl,
    /// Windows `HKCU\Environment` (+ `WM_SETTINGCHANGE` broadcast).
    WindowsUserEnv,
    /// `WSLENV` in `HKCU\Environment` names the variable, so `wsl.exe` forwards
    /// it into WSL: the desktop app's "Run Codex in WSL" mode runs this
    /// computer's Codex there (user bug 54 (b)). The edit's values are the whole
    /// `WSLENV`.
    WslEnvForward,
}

/// What an [`EnvEdit`] does at its location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EnvEditAction {
    /// Insert or replace the Superba marker block / file / value.
    WriteBlock,
    /// Remove the Superba marker block / file / value.
    RemoveBlock,
    /// Comment out a legacy tutorial line (prefix [`LEGACY_COMMENT_PREFIX`]).
    CommentLegacyLine {
        /// 1-based line number at plan time.
        line_no: usize,
        /// The original line text.
        original: String,
    },
    /// Restore a previously commented legacy line.
    UncommentLegacyLine {
        /// 1-based line number at plan time.
        line_no: usize,
        /// The restored line text.
        original: String,
    },
    /// Set a registry / launchctl value.
    SetValue {
        /// New value.
        value: String,
    },
    /// Delete a registry / launchctl value.
    DeleteValue,
}

/// One concrete change, shown in the wizard's diff preview. `before` / `after`
/// make it revertible: revert restores `before` only where the location still holds
/// `after`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvEdit {
    /// Where.
    pub location: EnvLocation,
    /// What.
    pub action: EnvEditAction,
    /// Text preview of the result (block content, new line), for display.
    #[serde(default)]
    pub preview: String,
    /// Value (or rc-file sha256 for file locations) before the edit; `None` = absent.
    #[serde(default)]
    pub before: Option<String>,
    /// Value (or rc-file sha256) after the edit; `None` = absent.
    #[serde(default)]
    pub after: Option<String>,
}

/// A planned environment change; applying it performs exactly these edits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvPlan {
    /// Variable name (normally [`ENV_AUTHAPI_BASE_URL`]).
    pub var: String,
    /// Requested operation.
    pub op: EnvOp,
    /// Concrete edits.
    pub edits: Vec<EnvEdit>,
    /// Where/when the change takes effect on this platform (plan 6.1), for the UI.
    #[serde(default)]
    pub scope_notes: Vec<LocalizedText>,
}

impl EnvPlan {
    /// True if applying would change nothing.
    pub fn is_noop(&self) -> bool {
        self.edits.is_empty()
    }
}

/// Result of applying an [`EnvPlan`]; enough to revert it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvApplied {
    /// Durable operation id (recorded in the switch journal).
    pub id: String,
    /// Variable name.
    pub var: String,
    /// Edits performed.
    pub edits: Vec<EnvEdit>,
    /// Backups of files modified (pre-images).
    #[serde(default)]
    pub backups: Vec<PathBuf>,
    /// When.
    pub applied_at: Timestamp,
}

/// A legacy or foreign line found in a shell rc file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcLine {
    /// File.
    pub path: PathBuf,
    /// 1-based line number.
    pub line_no: usize,
    /// Full line text.
    pub text: String,
    /// The value it sets.
    pub value: String,
}

/// Value of the variable at one persisted location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedValue {
    /// Where.
    pub location: EnvLocation,
    /// Value found, if any.
    pub value: Option<String>,
    /// True if it is inside a Superba marker block / written by Superba.
    pub managed: bool,
}

/// Current persisted state of a variable ([`crate::EnvPersister::inspect`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvReport {
    /// Variable name.
    pub var: String,
    /// Values per location.
    pub persisted: Vec<PersistedValue>,
    /// Legacy Krill tutorial lines (will be commented out on takeover).
    pub legacy_lines: Vec<RcLine>,
    /// User lines pointing elsewhere (need a "take over / keep yours" decision).
    pub foreign_lines: Vec<RcLine>,
    /// Value in Superba's own process environment.
    pub process_value: EnvObservation,
    /// The value Superba last set for the variable (`None` = it set none, or
    /// cleared it since). A managed location holding something else was changed
    /// outside Superba (GH#5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superba_value: Option<String>,
}

/// Baseline snapshot of a variable at one location (for restore / uninstall).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvSnapshot {
    /// Where.
    pub location: EnvLocation,
    /// Original value (`None` = unset).
    pub value: Option<String>,
}
