//! Observations and progress for user-triggered Codex troubleshooting.

use std::sync::Arc;

use crate::{LineId, LocalizedText, ModelList, ProfileId, Timestamp};

/// A problem selected on the troubleshooting page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TroubleshootingIssue {
    /// Request, endpoint, DNS or gateway errors.
    #[default]
    Request,
    /// Intermittent connections or slow streamed output across supplier lines.
    Stability,
    /// HTTP 451: show the conversation and content guidance.
    Policy,
    /// HTTP 500: ask about service status before checking this computer.
    Server,
    /// Fast or Ultrafast is unavailable.
    Speed,
    /// Responses WebSocket transport is unavailable.
    WebSocket,
    /// An expected model is missing.
    Models,
    /// A previous conversation cannot be continued.
    Sessions,
    /// An unclassified problem, with optional recovery tools.
    Other,
    /// Read-only checks of the whole environment.
    All,
}

impl TroubleshootingIssue {
    /// Problem cards, in display order.
    pub const CARDS: [Self; 9] = [
        Self::Request,
        Self::Stability,
        Self::Policy,
        Self::Server,
        Self::Speed,
        Self::WebSocket,
        Self::Models,
        Self::Sessions,
        Self::Other,
    ];

    /// Stable identity for translations and UI ids.
    pub fn id(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Stability => "stability",
            Self::Policy => "policy",
            Self::Server => "server",
            Self::Speed => "speed",
            Self::WebSocket => "websocket",
            Self::Models => "models",
            Self::Sessions => "sessions",
            Self::Other => "other",
            Self::All => "all",
        }
    }
}

/// One bounded inspection. A selected model asks for catalog/configuration
/// checks after the user has seen the freshly fetched provider list.
#[derive(Clone, Debug, Default)]
pub struct TroubleshootingRequest {
    /// Selected problem.
    pub issue: TroubleshootingIssue,
    /// Expected model, selected from the provider's list when available.
    pub model: Option<String>,
    /// Speed mode the user cannot enable (Fast when omitted).
    pub speed: Option<crate::SpeedTier>,
    /// Fetch upstream lists now, bypassing their cache.
    pub refresh_models: bool,
    /// Retry only this network observation, leaving the other checks untouched.
    pub retry: Option<TroubleshootingRetry>,
}

/// Exact network operation to repeat after a failed observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TroubleshootingRetry {
    /// Refresh one supplier's model list.
    Models {
        /// Supplier to refresh.
        profile: ProfileId,
    },
    /// Repeat one supplier's Responses probe with the same model.
    Backend {
        /// Supplier to probe.
        profile: ProfileId,
        /// Model used by the original observation.
        model: Option<String>,
    },
    /// Repeat one upstream line's WebSocket handshake.
    WebSocket {
        /// Supplier to probe.
        profile: ProfileId,
        /// Exact upstream line, when resolved.
        line: Option<LineId>,
    },
}

impl TroubleshootingRetry {
    /// Supplier whose observation is being repeated.
    pub fn profile(&self) -> &ProfileId {
        match self {
            Self::Models { profile }
            | Self::Backend { profile, .. }
            | Self::WebSocket { profile, .. } => profile,
        }
    }
}

/// An observation is never implicitly treated as a successful repair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TroubleshootingStatus {
    /// The check completed and passed.
    Passed,
    /// A known limitation or a problem needing a user decision.
    Warning,
    /// The checked operation failed.
    Failed,
    /// The check could not establish a result.
    Unknown,
}

/// One safe-to-display diagnostic observation.
#[derive(Clone, Debug)]
pub struct TroubleshootingCheck {
    /// Stable identity, also used when appending verification results.
    pub id: String,
    /// Display name.
    pub title: LocalizedText,
    /// What was actually observed (never raw configuration or credentials).
    pub detail: LocalizedText,
    /// Outcome.
    pub status: TroubleshootingStatus,
    /// Available only for a network observation that did not pass.
    pub retry: Option<TroubleshootingRetry>,
}

/// One provider's own model list, not the router union or a bundled fallback.
#[derive(Clone, Debug)]
pub struct TroubleshootingModels {
    /// Provider profile.
    pub profile_id: ProfileId,
    /// Provider display name.
    pub name: String,
    /// The actual upstream answer, including a successful empty list.
    pub list: Option<ModelList>,
    /// Why fetching failed.
    pub error: Option<LocalizedText>,
    /// At least one upstream list answered this explicit refresh.
    pub refreshed: bool,
}

/// Configuration entries offered for removal after a manual confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TroubleshootingCleanup {
    /// Digest of the file the user reviewed.
    pub sha256: String,
    /// Exact key locations reported by the configuration validator.
    pub entries: Vec<TroubleshootingConfigEntry>,
}

/// One deprecated or ignored configuration entry, without its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TroubleshootingConfigEntry {
    /// Display path of the key.
    pub path: String,
    /// Start of the key in the parsed text, excluding a UTF-8 BOM.
    pub start: usize,
    /// End of the key in the parsed text.
    pub end: usize,
}

/// Complete observations for a run. Suggested changes still go through the
/// application's ordinary switch planner and executor.
#[derive(Clone, Debug)]
pub struct TroubleshootingReport {
    /// Problem checked.
    pub issue: TroubleshootingIssue,
    /// Time of the inspection.
    pub checked_at: Timestamp,
    /// Profile actually applied on this computer; never a last-selected guess.
    pub profile_id: Option<ProfileId>,
    /// Applied direct line, when known.
    pub line_id: Option<LineId>,
    /// This computer is configured to use the local router.
    pub routed: bool,
    /// Results in execution order.
    pub checks: Vec<TroubleshootingCheck>,
    /// Per-provider upstream lists.
    pub models: Vec<TroubleshootingModels>,
    /// Non-openai providers with conversations that can be migrated.
    pub legacy_providers: Vec<(String, u32)>,
    /// A precise managed configuration repair is available.
    pub reapply: bool,
    /// Configuration checks completed without an unresolved error.
    pub config_ok: bool,
    /// All feature/backend checks needed before a restart passed.
    pub restart_recommended: bool,
    /// There is a known applied profile from which a new file can be built.
    pub can_reinitialize: bool,
    /// Entries the user can review and remove with a backup.
    pub cleanup: Option<TroubleshootingCleanup>,
}

/// Progress of a user-triggered inspection.
#[derive(Clone, Debug)]
pub enum TroubleshootingProgress {
    /// About to check configuration and installed versions.
    Configuration,
    /// About to check suppliers and their backends.
    Backend,
    /// Step just completed.
    Check(TroubleshootingCheck),
}

/// Page-owned progress callback. Returning false stops before the next check;
/// cancellation never starts a repair.
pub type TroubleshootingProgressSink = Arc<dyn Fn(TroubleshootingProgress) -> bool + Send + Sync>;
