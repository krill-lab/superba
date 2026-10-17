//! Upstream transports of a line: the Responses WebSocket and zstd-compressed
//! request bodies ([`crate::LineProber::probe_transports`],
//! [`crate::SwitchService::line_transports`]).
//!
//! Both are checked cheaply and never with a billed turn: the WebSocket by its
//! handshake alone (closed at once), zstd by a tiny body the server must decode
//! before it can reject it for validation, compared with the same body sent
//! uncompressed and with the compressed bytes sent as if they were plain JSON.
//! Results are cached per line and key with the time of the check; the local
//! router reads them as hints (compress request bodies, offer the WebSocket)
//! and remembers refusals it meets at run time on its own.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
use crate::ids::{LineId, ProfileId, SiteId};
use crate::time::Timestamp;

/// Whether a line offers a transport feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransportSupport {
    /// The check showed that it works (WebSocket 101; zstd body decoded).
    Supported,
    /// The line refused it (WebSocket 426 or no upgrade; zstd 415 or a body
    /// the server could not read).
    Unsupported,
    /// Not checked, or the answer did not tell (key rejected, rate limited,
    /// server error, unreachable).
    #[default]
    Unknown,
}

impl TransportSupport {
    /// `Some(true)` / `Some(false)` for a definite answer, `None` when unknown
    /// (the router's per-line hint).
    pub fn as_hint(self) -> Option<bool> {
        match self {
            TransportSupport::Supported => Some(true),
            TransportSupport::Unsupported => Some(false),
            TransportSupport::Unknown => None,
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            TransportSupport::Supported => {
                LocalizedText::new("transport_support.supported", "Supported")
            }
            TransportSupport::Unsupported => {
                LocalizedText::new("transport_support.unsupported", "Not supported")
            }
            TransportSupport::Unknown => LocalizedText::new("transport_support.unknown", "Unknown"),
        }
    }
}

/// One transport check of a line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportCheck {
    /// Outcome.
    #[serde(default)]
    pub support: TransportSupport,
    /// The HTTP status the outcome rests on (101, 426, 400, 415, ...); `None`
    /// on a transport failure or when nothing was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Short redacted reason (never contains a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl TransportCheck {
    /// A check with `support`, `status` and `detail`.
    pub fn new(support: TransportSupport, status: Option<u16>, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            support,
            status,
            detail: (!detail.is_empty()).then_some(detail),
        }
    }

    /// Not checked.
    pub fn unknown(detail: impl Into<String>) -> Self {
        Self::new(TransportSupport::Unknown, None, detail)
    }
}

/// What [`crate::LineProber::probe_transports`] found at one Responses
/// endpoint with one key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportProbe {
    /// The Responses WebSocket handshake.
    pub websocket: TransportCheck,
    /// zstd request bodies.
    pub zstd: TransportCheck,
}

/// Transport checks of one line for one key, as cached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineTransports {
    /// Site.
    pub site_id: SiteId,
    /// Line.
    pub line_id: LineId,
    /// When the line was checked.
    pub checked_at: Timestamp,
    /// The Responses WebSocket handshake.
    #[serde(default)]
    pub websocket: TransportCheck,
    /// zstd request bodies.
    #[serde(default)]
    pub zstd: TransportCheck,
}

impl LineTransports {
    /// Results older than this are checked again when a provider is opened
    /// ([`TransportRefresh::Missing`]).
    pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

    /// A result for `site` / `line` from `probe` at `at`.
    pub fn from_probe(
        site_id: SiteId,
        line_id: LineId,
        checked_at: Timestamp,
        probe: TransportProbe,
    ) -> Self {
        Self {
            site_id,
            line_id,
            checked_at,
            websocket: probe.websocket,
            zstd: probe.zstd,
        }
    }

    /// True when the result is older than [`LineTransports::MAX_AGE`] at `now`
    /// (or lies in the future: a clock that jumped back).
    pub fn is_stale(&self, now: Timestamp) -> bool {
        now.duration_since(self.checked_at)
            .is_none_or(|age| age > Self::MAX_AGE)
    }
}

/// When [`crate::SwitchService::line_transports`] sends requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransportRefresh {
    /// Cached results only; no request is sent.
    #[default]
    Cached,
    /// Check the lines without a result or with one older than
    /// [`LineTransports::MAX_AGE`].
    Missing,
    /// Check every requested line again.
    All,
}

/// What [`crate::SwitchService::line_transports`] reports on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineTransportsRequest {
    /// The provider (its site, lines and key).
    pub profile: ProfileId,
    /// Lines to report (empty = every line of the provider).
    #[serde(default)]
    pub lines: Vec<LineId>,
    /// Whether to check.
    #[serde(default)]
    pub refresh: TransportRefresh,
}

impl LineTransportsRequest {
    /// Every line of `profile`, cached results only.
    pub fn cached(profile: ProfileId) -> Self {
        Self {
            profile,
            lines: Vec::new(),
            refresh: TransportRefresh::Cached,
        }
    }

    /// Every line of `profile` with `refresh`.
    pub fn with_refresh(mut self, refresh: TransportRefresh) -> Self {
        self.refresh = refresh;
        self
    }
}
