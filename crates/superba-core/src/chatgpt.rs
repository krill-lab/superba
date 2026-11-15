//! Saved official ChatGPT logins ("accounts") and their subscription limits
//! (GitHub PR #20, issue #21).
//!
//! An account is one entry of the local Codex home's encrypted login stash
//! (`superba-config`): a ChatGPT `auth.json` Superba saved when a switch
//! replaced it, that the user added through a browser sign-in, or that the
//! user saved from the current login. The views here never carry tokens.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Timestamp;

/// One saved official ChatGPT login of this computer (no credentials).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatGptAccount {
    /// Opaque local identity (a hash of account id, user and e-mail; the
    /// login stash key). Never a token.
    pub id: String,
    /// E-mail from the login (falls back to the ChatGPT account id).
    pub email: String,
    /// Subscription claim of the saved login (`plus`, `pro`, ...): a display
    /// label only, never used to infer limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    /// When Superba first saved this login.
    pub added_at: Timestamp,
    /// The login's own `last_refresh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<Timestamp>,
    /// The live `auth.json` of the local Codex home holds this login.
    #[serde(default)]
    pub active: bool,
    /// The login a plain "official login" switch restores (the most recently
    /// saved one when the live `auth.json` holds no ChatGPT login).
    #[serde(default)]
    pub latest: bool,
}

/// Duration of the five-hour window in minutes.
pub const FIVE_HOUR_WINDOW_MINS: u64 = 300;
/// Duration of the weekly window in minutes.
pub const WEEKLY_WINDOW_MINS: u64 = 7 * 24 * 60;

/// One subscription quota window, present only when the service reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatGptWindow {
    /// Percentage remaining, clamped to 0..=100.
    pub remaining: f64,
    /// Window length the service reported, in minutes.
    pub minutes: u64,
    /// When the window resets, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

/// Codex subscription limits of one saved account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatGptQuota {
    /// The account ([`ChatGptAccount::id`]).
    pub account_id: String,
    /// Subscription the limits service reported (normalized, lower case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    /// Five-hour window (never inferred when missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<ChatGptWindow>,
    /// Weekly window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly: Option<ChatGptWindow>,
    /// When these limits were read.
    pub fetched_at: Timestamp,
}

impl ChatGptQuota {
    /// Neither window was reported (shown as "not reported", never as 100 %).
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.weekly.is_none()
    }
}

/// The browser page of a sign-in Codex started (never a token).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGptLoginPrompt {
    /// HTTPS authorization page to open.
    pub url: String,
}

/// Receives the sign-in page (`Some`) and is polled (`None`) while the sign-in
/// waits; returning `false` cancels it.
pub type ChatGptLoginEvents = Arc<dyn Fn(Option<ChatGptLoginPrompt>) -> bool + Send + Sync>;

/// A display name for a subscription identifier (`prolite` and `pro_lite`
/// are "Pro Lite"); a new tier keeps its own name. `None` for an empty or
/// `unknown` value.
pub fn plan_label(plan: &str) -> Option<String> {
    let plan = plan.trim();
    let known = match plan.to_ascii_lowercase().as_str() {
        "" | "unknown" => return None,
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "pro" => "Pro",
        "prolite" | "pro_lite" => "Pro Lite",
        "promax" | "pro_max" => "Pro Max",
        "team" => "Team",
        "business" => "Business",
        "enterprise" => "Enterprise",
        "edu" => "Edu",
        _ => return Some(plan.to_owned()),
    };
    Some(known.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_labels_keep_new_tiers_and_hide_unknown() {
        for (raw, label) in [
            ("free", Some("Free")),
            ("PLUS", Some("Plus")),
            ("prolite", Some("Pro Lite")),
            (" pro_lite ", Some("Pro Lite")),
            ("future_tier", Some("future_tier")),
            (" unknown ", None),
            ("", None),
        ] {
            assert_eq!(plan_label(raw).as_deref(), label, "{raw}");
        }
    }
}
