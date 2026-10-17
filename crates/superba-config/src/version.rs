//! Version gate helpers (plan 9.8): parse `codex --version` output and desktop app
//! version strings, and a provisional table of minimum Codex versions per
//! capability.
//!
//! The authoritative table belongs to `superba-switch` (it applies the gates and
//! may refine these values once spike S9 lands); the values here are the facts
//! known today plus provisional floors, each marked with its confidence.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use superba_core::codex::{CodexVersion, VersionCapability};
use superba_core::{Error, Result};

/// Parse the output of `codex --version` (e.g. `codex-cli 0.162.0`,
/// `codex 0.162.0-alpha.17.2`). Lines that mention a version of something else
/// (e.g. a Node warning) are skipped in favour of a line naming codex.
pub fn parse_codex_version_output(output: &str) -> Result<CodexVersion> {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(v) = lines
        .iter()
        .filter(|l| l.to_ascii_lowercase().contains("codex"))
        .find_map(|l| CodexVersion::parse(l).ok())
    {
        return Ok(v);
    }
    lines
        .iter()
        .find_map(|l| CodexVersion::parse(l).ok())
        .ok_or_else(|| Error::parse("codex --version", format!("no version in {output:?}")))
}

/// A desktop app version such as `26.1013` or `26.1013.2` (year + build date,
/// optional patch). An omitted patch compares and hashes as zero; display keeps
/// the original omission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppVersion {
    /// Leading component (`26`).
    pub major: u64,
    /// Second component (`1013`).
    pub minor: u64,
    /// Optional third component.
    pub patch: Option<u64>,
}

impl AppVersion {
    /// Parse the first `N.N[.N]` token in `text` (`ChatGPT 26.1013`, `v26.1013.2`,
    /// `26.1013.1-beta`). Four-component tokens are rejected.
    pub fn parse(text: &str) -> Option<Self> {
        text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '"' | '\''))
            .find_map(|tok| {
                let tok = tok.trim_end_matches(['.', ':']);
                let tok = tok.strip_prefix(['v', 'V']).unwrap_or(tok);
                let core = tok.split(['-', '+']).next().unwrap_or(tok);
                let parts: Vec<&str> = core.split('.').collect();
                let num = |s: &str| {
                    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| s.parse::<u64>().ok())
                        .flatten()
                };
                match parts.as_slice() {
                    [a, b] => Some(Self {
                        major: num(a)?,
                        minor: num(b)?,
                        patch: None,
                    }),
                    [a, b, c] => Some(Self {
                        major: num(a)?,
                        minor: num(b)?,
                        patch: Some(num(c)?),
                    }),
                    _ => None,
                }
            })
    }

    fn key(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch.unwrap_or(0))
    }
}

impl PartialEq for AppVersion {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for AppVersion {}

impl Hash for AppVersion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl Ord for AppVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for AppVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for AppVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)?;
        if let Some(p) = self.patch {
            write!(f, ".{p}")?;
        }
        Ok(())
    }
}

/// Known desktop app builds and the Codex version they bundle (plan 9.8 fact:
/// 26.1007 / 26.1013 -> 0.162). Used only when the bundled binary cannot be run.
pub const KNOWN_APP_BUNDLES: &[KnownBundle] = &[
    KnownBundle {
        app: (26, 1007),
        codex: (0, 162, 0),
    },
    KnownBundle {
        app: (26, 1013),
        codex: (0, 162, 0),
    },
];

/// One known app build and its bundled Codex version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownBundle {
    /// App `major.minor` (`26.1013`).
    pub app: (u64, u64),
    /// Bundled Codex `major.minor.patch`.
    pub codex: (u64, u64, u64),
}

/// Bundled Codex version for a known app build (`None` = unknown build: run the
/// bundled `codex --version` instead).
pub fn bundled_codex_for_app(app: &AppVersion) -> Option<CodexVersion> {
    KNOWN_APP_BUNDLES
        .iter()
        .find(|b| b.app == (app.major, app.minor))
        .map(|b| CodexVersion::new(b.codex.0, b.codex.1, b.codex.2))
}

/// How sure we are about a minimum version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Verified fact (source code / measurement).
    Verified,
    /// Provisional floor until spike S9 (the newest version known to work, or the
    /// tutorial's claim).
    Provisional,
}

/// One row of the minimum-version table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinVersion {
    /// Capability.
    pub capability: VersionCapability,
    /// Minimum Codex version (pre-release suffixes are ignored when comparing).
    pub min: CodexVersion,
    /// Confidence.
    pub confidence: Confidence,
}

/// Minimum Codex versions per capability (plan 9.8). `WhamAccountsCheck` 0.160 is a
/// fact (relay `accounts_check.rs`); PAT and top-level `openai_base_url` are
/// provisional at 0.160 (the Krill tutorial's floor) pending spike S9.
pub fn min_version_table() -> Vec<MinVersion> {
    vec![
        MinVersion {
            capability: VersionCapability::TopLevelOpenaiBaseUrl,
            min: CodexVersion::new(0, 160, 0),
            confidence: Confidence::Provisional,
        },
        MinVersion {
            capability: VersionCapability::PersonalAccessToken,
            min: CodexVersion::new(0, 160, 0),
            confidence: Confidence::Provisional,
        },
        MinVersion {
            capability: VersionCapability::WhamAccountsCheck,
            min: CodexVersion::new(0, 160, 0),
            confidence: Confidence::Verified,
        },
    ]
}

/// Result of checking one capability against a found version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Gate {
    /// The version meets the minimum.
    Supported,
    /// Too old.
    TooOld {
        /// Required minimum.
        required: CodexVersion,
        /// Found.
        found: CodexVersion,
    },
    /// Version unknown (remote host, binary not runnable): ask the user (plan 9.8).
    Unknown {
        /// Required minimum.
        required: CodexVersion,
    },
}

impl Gate {
    /// True only for [`Gate::Supported`].
    pub fn is_supported(&self) -> bool {
        matches!(self, Gate::Supported)
    }
}

/// Check `capability` for `found` against [`min_version_table`].
pub fn check(capability: VersionCapability, found: Option<&CodexVersion>) -> Gate {
    let Some(row) = min_version_table()
        .into_iter()
        .find(|r| r.capability == capability)
    else {
        return Gate::Supported;
    };
    match found {
        None => Gate::Unknown { required: row.min },
        Some(v) if v.meets(&row.min) => Gate::Supported,
        Some(v) => Gate::TooOld {
            required: row.min,
            found: v.clone(),
        },
    }
}

/// Capabilities a token profile needs (PAT additionally needs the PAT and wham
/// capabilities; every Krill/compatible profile needs top-level `openai_base_url`
/// or keys would go to api.openai.com).
pub fn required_capabilities(pat: bool) -> Vec<VersionCapability> {
    let mut v = vec![VersionCapability::TopLevelOpenaiBaseUrl];
    if pat {
        v.push(VersionCapability::PersonalAccessToken);
        v.push(VersionCapability::WhamAccountsCheck);
    }
    v
}
