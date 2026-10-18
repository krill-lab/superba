//! `<data dir>/logs/selfupdate.log`: every step of one install attempt, in
//! order and with times: the download and its verification, the swap or the
//! hand-off, and the second stage (`--finish-update`: waiting for the old
//! process, `msiexec` with its exit code, the router restart, the relaunch).
//!
//! Each attempt starts a new file ([`RunLog::begin`]); the previous attempts
//! are kept as `selfupdate.1.log` (the one before) up to
//! `selfupdate.<KEEP_PREVIOUS>.log`. The second stage appends to the file of
//! its attempt ([`RunLog::resume`]). Writing is best effort: a log that cannot
//! be written never stops an update. No secrets are logged (paths, versions,
//! exit codes and error messages only).

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use superba_core::{DataDir, Timestamp};

/// The log of the current (or last) attempt, in the logs directory.
pub const LOG_FILE: &str = "selfupdate.log";

/// The verbose `msiexec` log of the last Windows install, next to it.
pub const MSIEXEC_LOG_FILE: &str = "selfupdate-msiexec.log";

/// How many earlier attempts are kept (`selfupdate.1.log` ...).
pub const KEEP_PREVIOUS: usize = 4;

/// A file that is larger than this is not appended to any more (a runaway
/// loop cannot fill the disk).
const MAX_BYTES: u64 = 4 << 20;

/// The log of one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLog {
    path: PathBuf,
    who: String,
}

impl RunLog {
    /// Where the current attempt's log of `data_dir` is.
    pub fn path_in(data_dir: &DataDir) -> PathBuf {
        data_dir.logs_dir().join(LOG_FILE)
    }

    /// Where the `msiexec` log of `data_dir` is.
    pub fn msiexec_path_in(data_dir: &DataDir) -> PathBuf {
        data_dir.logs_dir().join(MSIEXEC_LOG_FILE)
    }

    /// The log of an earlier attempt: `n` = 1 is the one before the current.
    pub fn previous(path: &Path, n: usize) -> PathBuf {
        let stem = path
            .file_stem()
            .map_or_else(|| "selfupdate".into(), |s| s.to_string_lossy().into_owned());
        path.with_file_name(format!("{stem}.{n}.log"))
    }

    /// Start a new attempt in `path`: the earlier attempts move one place back
    /// (the oldest beyond [`KEEP_PREVIOUS`] is removed), then `header` starts
    /// the new file.
    pub fn begin(path: impl Into<PathBuf>, header: &str) -> Self {
        let log = Self::resume(path, "update");
        if let Some(dir) = log.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        rotate(&log.path);
        let _ = fs::write(&log.path, "");
        log.line(header);
        log
    }

    /// Append to the attempt already in `path`; lines are tagged with `who`
    /// (`update` for the process that downloads, `finish` for the second
    /// stage) and the process id.
    pub fn resume(path: impl Into<PathBuf>, who: &str) -> Self {
        Self {
            path: path.into(),
            who: who.to_owned(),
        }
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line (`<time> [<who> <pid>] <text>`); multi-line text is
    /// indented under it. Best effort.
    pub fn line(&self, text: impl AsRef<str>) {
        if fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_BYTES) {
            return;
        }
        let text = text.as_ref().trim_end().replace('\n', "\n    ");
        let entry = format!(
            "{} [{} {}] {text}\n",
            Timestamp::now().to_rfc3339(),
            self.who,
            std::process::id()
        );
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| f.write_all(entry.as_bytes()));
    }
}

/// Move `selfupdate.log` to `selfupdate.1.log`, `.1` to `.2`, ... dropping the
/// oldest. Missing files are skipped.
fn rotate(path: &Path) {
    let _ = fs::remove_file(RunLog::previous(path, KEEP_PREVIOUS));
    for n in (1..KEEP_PREVIOUS).rev() {
        let from = RunLog::previous(path, n);
        if from.exists() {
            let _ = fs::rename(&from, RunLog::previous(path, n + 1));
        }
    }
    if path.exists() {
        let _ = fs::rename(path, RunLog::previous(path, 1));
    }
}

/// What a Windows Installer exit code means (`msiexec`), in English for the
/// log, and whether the product was installed.
pub fn msiexec_meaning(code: i32) -> (&'static str, bool) {
    match code {
        0 => ("success", true),
        1641 => (
            "success; Windows Installer started a restart of Windows",
            true,
        ),
        3010 => ("success; Windows must restart to finish the update", true),
        1601 => ("the Windows Installer service could not be accessed", false),
        1602 => ("the installation was cancelled", false),
        1603 => ("a fatal error occurred during the installation", false),
        1612 => ("the installation source is not available", false),
        1618 => ("another installation is already in progress", false),
        1619 => ("the installation package could not be opened", false),
        1620 => ("the installation package is invalid", false),
        1625 => ("the installation is forbidden by system policy", false),
        1633 => ("the package is not supported on this platform", false),
        1638 => (
            "another version of this product is already installed",
            false,
        ),
        1639 => ("invalid command line", false),
        _ => ("unknown error", false),
    }
}

/// The exit codes [`msiexec_meaning`] names (each has a translated text,
/// `selfupdate.msiexec.c<code>`).
pub const MSIEXEC_KNOWN_CODES: [i32; 14] = [
    0, 1641, 3010, 1601, 1602, 1603, 1612, 1618, 1619, 1620, 1625, 1633, 1638, 1639,
];
