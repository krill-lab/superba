//! Whether a key-based profile's API key can be used on a destination.
//!
//! Key-based profiles always authenticate with an API key, directly and
//! through the local router (user bug 139): there is no auth choice to make,
//! only whether the site and line take an API key.

use serde::{Deserialize, Serialize};

use crate::auth::AuthMode;
use crate::caps::LineCaps;
use crate::i18n::LocalizedText;
use crate::site::Site;

/// [`AuthDecision`] for `site` with the line's probe result (`None` = not
/// probed yet: the key is checked when switching).
pub fn key_auth(site: &Site, caps: Option<&LineCaps>) -> AuthDecision {
    let reason = if !site.supports(AuthMode::ApiKey) {
        AuthReason::NoKeyMode
    } else if caps.is_some_and(|c| !c.allows(AuthMode::ApiKey)) {
        AuthReason::LineUnavailable
    } else {
        AuthReason::Available
    };
    AuthDecision {
        mode: (reason == AuthReason::Available).then_some(AuthMode::ApiKey),
        reason,
    }
}

/// Result of [`key_auth`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthDecision {
    /// The mode to use; `None` = the key cannot be used here (the switch is
    /// blocked).
    pub mode: Option<AuthMode>,
    /// Why.
    pub reason: AuthReason,
}

impl AuthDecision {
    /// Text such as "API key" or "The line is unavailable.".
    pub fn text(&self) -> LocalizedText {
        match self.mode {
            Some(mode) => mode.text(),
            None => self.reason.text(),
        }
    }
}

/// Why the API key can (or cannot) be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthReason {
    /// The site takes API keys and the line (if probed) accepts one.
    Available,
    /// The line is unreachable, serves no usable Codex path or refuses API keys.
    LineUnavailable,
    /// The site offers no key-based auth mode.
    NoKeyMode,
}

impl AuthReason {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            AuthReason::Available => LocalizedText::new("auth_mode.apikey", "API key"),
            AuthReason::LineUnavailable => {
                LocalizedText::new("auth_reason.line_unavailable", "The line is unavailable.")
            }
            AuthReason::NoKeyMode => {
                LocalizedText::new("auth_reason.no_key_mode", "This site does not use keys.")
            }
        }
    }
}
