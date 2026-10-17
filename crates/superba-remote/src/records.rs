//! Parsing of the nonce-tagged result records printed by [`crate::script`].

use std::collections::BTreeMap;

use superba_core::{Error, Result, SecretBytes};

use crate::script::RECORD_PREFIX;

/// A file as read by `sb_file`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileRead {
    /// The file does not exist.
    Absent,
    /// It exists but could not be read (permissions, not a regular file).
    Unreadable,
    /// Larger than [`crate::script::MAX_READ_BYTES`].
    TooLarge,
    /// Its bytes.
    Bytes(SecretBytes),
}

impl FileRead {
    /// The bytes, if read.
    pub fn bytes(&self) -> Option<&SecretBytes> {
        match self {
            FileRead::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// True if the file exists but its bytes are unavailable.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, FileRead::Unreadable | FileRead::TooLarge)
    }
}

/// Parsed records of one script run.
#[derive(Clone, Debug, Default)]
pub struct Records {
    values: Vec<(String, String)>,
    files: BTreeMap<String, FileRead>,
}

impl Records {
    /// Parse `stdout`, keeping only lines tagged with `nonce`. Malformed tagged
    /// lines are an error (they indicate a broken transport, not shell noise).
    pub fn parse(stdout: &[u8], nonce: &str) -> Result<Self> {
        let prefix = format!("{RECORD_PREFIX}{nonce}:");
        let mut out = Records::default();
        for raw in stdout.split(|&b| b == b'\n') {
            let line = trim_cr(raw);
            // Records may follow noise on the same line (an rc file printing without
            // a trailing newline), so search instead of requiring a line start.
            let Some(pos) = find(line, prefix.as_bytes()) else {
                continue;
            };
            let rest = &line[pos + prefix.len()..];
            let rest = std::str::from_utf8(rest)
                .map_err(|_| Error::parse("remote output", "record is not ASCII"))?;
            let mut parts = rest.splitn(3, ':');
            let (Some(kind), Some(name), Some(hexval)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(Error::parse("remote output", "truncated record"));
            };
            let decode = || {
                hex::decode(hexval.trim())
                    .map_err(|e| Error::parse("remote output", format!("record {name}: {e}")))
            };
            match kind {
                "K" => {
                    let v = decode()?;
                    out.values
                        .push((name.to_owned(), String::from_utf8_lossy(&v).into_owned()));
                }
                "F" => {
                    out.files.insert(
                        name.to_owned(),
                        FileRead::Bytes(SecretBytes::new(decode()?)),
                    );
                }
                "A" => {
                    out.files.insert(name.to_owned(), FileRead::Absent);
                }
                "U" => {
                    out.files.insert(name.to_owned(), FileRead::Unreadable);
                }
                "L" => {
                    out.files.insert(name.to_owned(), FileRead::TooLarge);
                }
                other => {
                    return Err(Error::parse(
                        "remote output",
                        format!("unknown record kind {other:?}"),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Last value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Last non-empty value of `name`, trimmed.
    pub fn get_nonempty(&self, name: &str) -> Option<&str> {
        self.get(name).map(str::trim).filter(|v| !v.is_empty())
    }

    /// Last value of `name` if non-empty, untrimmed (paths may end in spaces).
    pub fn get_raw_nonempty(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|v| !v.is_empty())
    }

    /// All values of `name`, in order.
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// True if a value named `name` was printed.
    pub fn has(&self, name: &str) -> bool {
        self.values.iter().any(|(n, _)| n == name)
    }

    /// All `(name, value)` pairs whose name starts with `prefix`.
    pub fn with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.values
            .iter()
            .filter(move |(n, _)| n.starts_with(prefix))
            .map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// A file record.
    pub fn file(&self, name: &str) -> Option<&FileRead> {
        self.files.get(name)
    }

    /// Error unless the script reached its final `done` record.
    pub fn require_done(&self, what: &str) -> Result<()> {
        if self.has("done") {
            Ok(())
        } else {
            Err(Error::parse(
                format!("remote {what} output"),
                "the script did not finish (missing tools or interrupted)",
            ))
        }
    }
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
