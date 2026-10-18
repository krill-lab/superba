//! `<data dir>/logs/tools/<tool>.log`: every step of one install, update or
//! removal with times (versions, URLs, checksums, command lines, their
//! output and exit codes). Each operation starts a new file; the previous
//! four are kept as `<tool>.1.log` ... Writing is best effort and never stops
//! an operation. Nothing secret is logged.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use superba_core::{DataDir, Timestamp, ToolId};

/// Earlier operations kept per tool.
pub const KEEP_PREVIOUS: usize = 4;

/// A log larger than this is not appended to any more.
const MAX_BYTES: u64 = 8 << 20;

/// The log of one operation.
#[derive(Clone, Debug)]
pub struct OpLog {
    path: Option<PathBuf>,
}

impl OpLog {
    /// The log directory of `data`.
    pub fn dir(data: &DataDir) -> PathBuf {
        data.logs_dir().join("tools")
    }

    /// The current log of `tool`.
    pub fn path_of(data: &DataDir, tool: ToolId) -> PathBuf {
        Self::dir(data).join(format!("{}.log", tool.as_str()))
    }

    /// Start a new log for `tool` (the earlier ones move one place back).
    pub fn begin(data: &DataDir, tool: ToolId, header: &str) -> Self {
        let path = Self::path_of(data, tool);
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        rotate(&path);
        let _ = fs::write(&path, "");
        let log = Self { path: Some(path) };
        log.line(header);
        log
    }

    /// A log that writes nothing (tests, quiet probes).
    pub fn none() -> Self {
        Self { path: None }
    }

    /// The file.
    pub fn path(&self) -> PathBuf {
        self.path.clone().unwrap_or_default()
    }

    /// Append `<time> <text>`; more lines are indented.
    pub fn line(&self, text: impl AsRef<str>) {
        let Some(path) = &self.path else { return };
        if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
            return;
        }
        let text = text.as_ref().trim_end().replace('\n', "\n    ");
        let entry = format!("{} {text}\n", Timestamp::now().to_rfc3339());
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(entry.as_bytes()));
    }
}

fn previous(path: &Path, n: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .map_or_else(|| "tool".into(), |s| s.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem}.{n}.log"))
}

fn rotate(path: &Path) {
    let _ = fs::remove_file(previous(path, KEEP_PREVIOUS));
    for n in (1..KEEP_PREVIOUS).rev() {
        let from = previous(path, n);
        if from.exists() {
            let _ = fs::rename(&from, previous(path, n + 1));
        }
    }
    if path.exists() {
        let _ = fs::rename(path, previous(path, 1));
    }
}
