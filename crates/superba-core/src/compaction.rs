//! Remote compaction support of a provider (GitHub issue 3).
//!
//! Codex compacts a long conversation either **remotely** — it sends the
//! history to the provider with a `compaction_trigger` input item on the
//! ordinary Responses endpoint (HTTP or WebSocket) and keeps the single
//! `compaction` output item (an opaque `encrypted_content` blob) in place of the
//! old history — or **locally**, by asking the model for a summary. Which one
//! it uses is a property of the configured provider, not of the model: the
//! built-in `openai` provider, the only one Superba writes, always compacts
//! remotely (Codex 0.149–0.161 by the default-on `features.remote_compaction_v2`,
//! 0.162+ by the provider's capability; `[model_providers.<id>]
//! capabilities.remote_compaction = "unsupported"` exists only for custom
//! provider ids, which Superba never writes because sessions are listed per
//! provider id, and the built-in ids cannot be redefined). Codex 0.155 removed
//! the old `POST …/responses/compact` request entirely.
//!
//! So a provider that does not understand `compaction_trigger` makes every
//! compaction fail ("expected exactly one compaction output item"), and a
//! `compaction` item made by one provider can only be read by a provider that
//! shares its keys (in practice: the same provider). Superba therefore
//! remembers, per provider, whether it compacts natively
//! ([`ProviderCompaction`]), lets the user override that, warns before a
//! switch, and the local router never sends a compacted conversation to a
//! provider that cannot read it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
use crate::ids::{ProfileId, SiteId};
use crate::time::Timestamp;
use crate::transports::TransportRefresh;

/// How a provider handles Codex's remote compaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompactionSupport {
    /// The provider answers a `compaction_trigger` with one `compaction` item
    /// and reads it back: Codex's requests pass through untouched.
    Native,
    /// The provider refuses or ignores `compaction_trigger`: Codex's
    /// compaction fails there.
    Unsupported,
    /// Not checked, or the check did not tell (key rejected, rate limited,
    /// server error, unreachable, model refused).
    #[default]
    Unknown,
}

impl CompactionSupport {
    /// True for [`CompactionSupport::Native`] and
    /// [`CompactionSupport::Unsupported`].
    pub fn is_definite(self) -> bool {
        !matches!(self, CompactionSupport::Unknown)
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            CompactionSupport::Native => {
                LocalizedText::new("compaction.support.native", "Native remote compaction")
            }
            CompactionSupport::Unsupported => {
                LocalizedText::new("compaction.support.unsupported", "No remote compaction")
            }
            CompactionSupport::Unknown => {
                LocalizedText::new("compaction.support.unknown", "Compaction not checked")
            }
        }
    }
}

/// The user's choice for a provider ([`crate::Profile::compaction`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompactionChoice {
    /// Use what the check found (Superba's built-in fact for Krill until then).
    #[default]
    Auto,
    /// Treat the provider as compacting natively.
    Native,
    /// Treat the provider as not supporting remote compaction.
    Unsupported,
}

impl CompactionChoice {
    /// True for [`CompactionChoice::Auto`] (the stored default).
    pub fn is_auto(&self) -> bool {
        matches!(self, CompactionChoice::Auto)
    }

    /// The support the choice forces (`None` for `Auto`).
    pub fn forced(self) -> Option<CompactionSupport> {
        match self {
            CompactionChoice::Auto => None,
            CompactionChoice::Native => Some(CompactionSupport::Native),
            CompactionChoice::Unsupported => Some(CompactionSupport::Unsupported),
        }
    }

    /// Stable id (`auto`, `native`, `unsupported`), as `spb` takes it.
    pub fn id(self) -> &'static str {
        match self {
            CompactionChoice::Auto => "auto",
            CompactionChoice::Native => "native",
            CompactionChoice::Unsupported => "unsupported",
        }
    }

    /// Parse an id.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "automatic" => Some(CompactionChoice::Auto),
            "native" | "supported" | "yes" => Some(CompactionChoice::Native),
            "unsupported" | "none" | "no" => Some(CompactionChoice::Unsupported),
            _ => None,
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            CompactionChoice::Auto => LocalizedText::new("compaction.choice.auto", "Automatic"),
            CompactionChoice::Native => {
                LocalizedText::new("compaction.choice.native", "Native (pass through)")
            }
            CompactionChoice::Unsupported => {
                LocalizedText::new("compaction.choice.unsupported", "Not supported")
            }
        }
    }
}

/// One compaction check of a provider
/// ([`crate::LineProber::probe_compaction`]): a `compaction_trigger` after a
/// two-message conversation, the way Codex sends it (costs a few tokens).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionCheck {
    /// Outcome.
    #[serde(default)]
    pub support: CompactionSupport,
    /// The HTTP status the outcome rests on; `None` on a transport failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Short redacted reason (never contains a key or content).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CompactionCheck {
    /// A check with `support`, `status` and `detail`.
    pub fn new(support: CompactionSupport, status: Option<u16>, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            support,
            status,
            detail: (!detail.is_empty()).then_some(detail),
        }
    }

    /// An inconclusive check.
    pub fn unknown(status: Option<u16>, detail: impl Into<String>) -> Self {
        Self::new(CompactionSupport::Unknown, status, detail)
    }
}

/// Where [`ProviderCompaction::support`] comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompactionSource {
    /// The user's choice on the provider ([`CompactionChoice`]).
    Override,
    /// Superba's check.
    Check,
    /// What Superba knows about a built-in site (Krill compacts natively).
    Builtin,
    /// Nothing known.
    #[default]
    None,
}

/// A provider's remote compaction as Superba uses it: the last check, the
/// user's choice and the effective value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCompaction {
    /// The provider (profile).
    pub profile_id: ProfileId,
    /// Its site.
    pub site_id: SiteId,
    /// The model the check used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// When the provider was checked (`None`: never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Timestamp>,
    /// The last check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CompactionCheck>,
    /// The user's choice.
    #[serde(default)]
    pub choice: CompactionChoice,
    /// Superba's fact for a built-in site (Unknown for custom sites).
    #[serde(default)]
    pub builtin: CompactionSupport,
    /// The effective support.
    #[serde(default)]
    pub support: CompactionSupport,
    /// Where it comes from.
    #[serde(default)]
    pub source: CompactionSource,
}

impl ProviderCompaction {
    /// A definite check older than this is repeated when a provider is opened
    /// ([`TransportRefresh::Missing`]).
    pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    /// An inconclusive check is repeated after this long.
    pub const UNKNOWN_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

    /// The effective support of `choice`, the last `check` and the `builtin`
    /// fact: the choice wins, then a definite check, then the built-in fact,
    /// then an inconclusive check.
    pub fn resolve(
        choice: CompactionChoice,
        check: Option<&CompactionCheck>,
        builtin: CompactionSupport,
    ) -> (CompactionSupport, CompactionSource) {
        if let Some(forced) = choice.forced() {
            return (forced, CompactionSource::Override);
        }
        match check.map(|c| c.support) {
            Some(s) if s.is_definite() => (s, CompactionSource::Check),
            _ if builtin.is_definite() => (builtin, CompactionSource::Builtin),
            Some(s) => (s, CompactionSource::Check),
            None => (CompactionSupport::Unknown, CompactionSource::None),
        }
    }

    /// A record for `profile_id` on `site_id` with the effective value
    /// resolved.
    pub fn new(
        profile_id: ProfileId,
        site_id: SiteId,
        choice: CompactionChoice,
        builtin: CompactionSupport,
        last: Option<(Timestamp, Option<String>, CompactionCheck)>,
    ) -> Self {
        let (checked_at, model, check) = match last {
            Some((at, model, check)) => (Some(at), model, Some(check)),
            None => (None, None, None),
        };
        let (support, source) = Self::resolve(choice, check.as_ref(), builtin);
        Self {
            profile_id,
            site_id,
            model,
            checked_at,
            check,
            choice,
            builtin,
            support,
            source,
        }
    }

    /// True when a check is due at `now`: never checked, a definite check
    /// older than [`ProviderCompaction::MAX_AGE`], an inconclusive one older
    /// than [`ProviderCompaction::UNKNOWN_MAX_AGE`] (or a clock that jumped
    /// back).
    pub fn is_stale(&self, now: Timestamp) -> bool {
        let Some(at) = self.checked_at else {
            return true;
        };
        let max = match self.check.as_ref().map(|c| c.support) {
            Some(s) if s.is_definite() => Self::MAX_AGE,
            _ => Self::UNKNOWN_MAX_AGE,
        };
        now.duration_since(at).is_none_or(|age| age > max)
    }
}

/// What [`crate::SwitchService::provider_compaction`] reports on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRequest {
    /// The provider (its site, lines, key and choice).
    pub profile: ProfileId,
    /// Whether to check ([`TransportRefresh::Cached`]: no request is sent;
    /// `Missing`: only when [`ProviderCompaction::is_stale`]; `All`: always).
    /// A check costs a few tokens, so the planner and the router never ask for
    /// one; only the provider page and `spb provider compaction --check` do.
    #[serde(default)]
    pub refresh: TransportRefresh,
}

impl CompactionRequest {
    /// `profile`, cached result only.
    pub fn cached(profile: ProfileId) -> Self {
        Self {
            profile,
            refresh: TransportRefresh::Cached,
        }
    }

    /// `profile` with `refresh`.
    pub fn with_refresh(mut self, refresh: TransportRefresh) -> Self {
        self.refresh = refresh;
        self
    }
}
