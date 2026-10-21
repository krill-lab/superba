//! Managed ChatGPT logins. Public views contain no credentials.

use crate::Timestamp;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One encrypted ChatGPT login available on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatGptAccount {
    /// Opaque local identity, independent of the email display label.
    pub id: String,
    /// Account email reported by the login token.
    pub email: String,
    /// Subscription label from the saved login; optional for older stores.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// When this account was first saved.
    pub added_at: Timestamp,
    /// Matches this computer's live official credential.
    pub active: bool,
}

/// One subscription quota window, absent when the service does not report it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatGptWindow {
    /// Percentage remaining, clamped to 0..100.
    pub remaining: f64,
    /// Actual window length reported by the service.
    pub minutes: u64,
    /// Reset time, when available.
    pub resets_at: Option<Timestamp>,
}

/// Codex subscription limits for a specific account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatGptQuota {
    /// The account this result belongs to.
    pub account_id: String,
    /// Subscription reported by the quota service, when available.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// Five-hour window, if reported (never inferred from a missing value).
    pub five_hour: Option<ChatGptWindow>,
    /// Weekly window, if reported.
    pub weekly: Option<ChatGptWindow>,
    /// When these limits were fetched.
    pub fetched_at: Timestamp,
}

/// Browser sign-in information returned by Codex, never authentication tokens.
#[derive(Clone, Debug)]
pub struct ChatGptLoginPrompt {
    /// HTTPS authorization page.
    pub url: String,
}

/// Receives a browser prompt; `None` is a cancellation/liveness check.
/// Returning false cancels the isolated login child.
pub type ChatGptLoginEvents = Arc<dyn Fn(Option<ChatGptLoginPrompt>) -> bool + Send + Sync>;
