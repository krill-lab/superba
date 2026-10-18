//! The user's auth choice for a key-based profile and how it resolves to an
//! [`AuthMode`] on a concrete line (decision 3).
//!
//! `Auto` (the default) uses a personal access token (PAT) when the line serves
//! the PAT endpoints and an API key otherwise, and always says why. Manual PAT /
//! API key live under Advanced and never fall back silently.

use serde::{Deserialize, Serialize};

use crate::auth::AuthMode;
use crate::caps::LineCaps;
use crate::i18n::LocalizedText;
use crate::site::{Line, Site};

/// How a key-based profile authenticates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AuthChoice {
    /// PAT when the line supports it, else an API key (with a stated reason).
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// Always PAT; the switch is blocked where PAT does not work (Advanced).
    #[serde(rename = "pat")]
    Pat,
    /// Always a plain API key (Advanced).
    #[serde(rename = "apikey")]
    ApiKey,
}

impl AuthChoice {
    /// Every choice, in display order.
    pub const ALL: [AuthChoice; 3] = [AuthChoice::Auto, AuthChoice::Pat, AuthChoice::ApiKey];

    /// The choice for an auth mode: PAT falls back to an API key on lines without
    /// PAT, which is `Auto`; an API key stays manual.
    pub fn from_mode(mode: AuthMode) -> Self {
        match mode {
            AuthMode::ApiKey => AuthChoice::ApiKey,
            _ => AuthChoice::Auto,
        }
    }

    /// The fixed mode of a manual choice, `None` for `Auto`.
    pub fn manual_mode(self) -> Option<AuthMode> {
        match self {
            AuthChoice::Auto => None,
            AuthChoice::Pat => Some(AuthMode::Pat),
            AuthChoice::ApiKey => Some(AuthMode::ApiKey),
        }
    }

    /// The value mirrored into [`crate::Profile::auth_mode`] for key-based
    /// profiles (PAT with fallback for `Auto`).
    pub fn representative_mode(self) -> AuthMode {
        match self {
            AuthChoice::ApiKey => AuthMode::ApiKey,
            AuthChoice::Auto | AuthChoice::Pat => AuthMode::Pat,
        }
    }

    /// True for `Auto`.
    pub fn is_auto(self) -> bool {
        self == AuthChoice::Auto
    }

    /// Display name.
    pub fn text(self) -> LocalizedText {
        match self {
            AuthChoice::Auto => LocalizedText::new("auth_choice.auto", "Automatic"),
            AuthChoice::Pat => LocalizedText::new("auth_choice.pat", "Personal access token (PAT)"),
            AuthChoice::ApiKey => LocalizedText::new("auth_choice.apikey", "API key"),
        }
    }

    /// Resolve the choice for one destination. Pure: the caller supplies the probe
    /// result and the facts it knows.
    ///
    /// Through the local router a PAT choice resolves like `Auto` (user bug 78):
    /// the router always sends the profile's key as an API key, and the choice
    /// stays what direct switches use.
    pub fn decide(self, cx: &AuthContext<'_>) -> AuthDecision {
        let decision = |mode: Option<AuthMode>, reason: AuthReason| AuthDecision {
            choice: self,
            mode,
            reason,
        };
        let resolve_as = if cx.router && self == AuthChoice::Pat {
            AuthChoice::Auto
        } else {
            self
        };
        let site_pat = cx.site.supports(AuthMode::Pat);
        let site_key = cx.site.supports(AuthMode::ApiKey);
        // Facts that rule PAT out before any probe.
        let pat_blocker = if !site_pat {
            Some(AuthReason::SiteApiKeyOnly)
        } else if cx.custom_base_url {
            Some(AuthReason::CustomBaseUrl)
        } else if cx.router {
            Some(AuthReason::RouterMode)
        } else if cx.pat_version_ok == Some(false) {
            Some(AuthReason::CodexTooOld)
        } else if cx.line.is_some_and(|l| !l.origin.is_https()) {
            Some(AuthReason::PatNeedsHttps)
        } else {
            None
        };
        let line_down = cx
            .caps
            .is_some_and(|c| !c.reachable || c.codex_path.is_none());
        match resolve_as {
            AuthChoice::Auto => {
                if let Some(blocker) = pat_blocker {
                    return if !site_key {
                        decision(None, AuthReason::NoKeyMode)
                    } else if line_down || cx.caps.is_some_and(|c| !c.allows(AuthMode::ApiKey)) {
                        decision(None, AuthReason::LineUnavailable)
                    } else {
                        decision(Some(AuthMode::ApiKey), blocker)
                    };
                }
                match cx.caps {
                    None => decision(Some(AuthMode::Pat), AuthReason::NotProbed),
                    Some(_) if line_down => decision(None, AuthReason::LineUnavailable),
                    Some(c) if c.allows(AuthMode::Pat) => {
                        decision(Some(AuthMode::Pat), AuthReason::PatAvailable)
                    }
                    Some(c) if site_key && c.allows(AuthMode::ApiKey) => decision(
                        Some(AuthMode::ApiKey),
                        AuthReason::PatUnavailable {
                            detail: c.detail.clone(),
                        },
                    ),
                    Some(_) => decision(None, AuthReason::LineUnavailable),
                }
            }
            AuthChoice::Pat => {
                if let Some(blocker) = pat_blocker {
                    return decision(
                        None,
                        AuthReason::ManualUnavailable {
                            mode: AuthMode::Pat,
                            detail: Some(blocker.text()),
                        },
                    );
                }
                match cx.caps {
                    Some(c) if !c.allows(AuthMode::Pat) => decision(
                        None,
                        AuthReason::ManualUnavailable {
                            mode: AuthMode::Pat,
                            detail: c.detail.clone(),
                        },
                    ),
                    _ => decision(Some(AuthMode::Pat), AuthReason::Manual),
                }
            }
            AuthChoice::ApiKey => {
                if !site_key {
                    return decision(
                        None,
                        AuthReason::ManualUnavailable {
                            mode: AuthMode::ApiKey,
                            detail: None,
                        },
                    );
                }
                match cx.caps {
                    Some(c) if !c.allows(AuthMode::ApiKey) => decision(
                        None,
                        AuthReason::ManualUnavailable {
                            mode: AuthMode::ApiKey,
                            detail: c.detail.clone(),
                        },
                    ),
                    _ => decision(Some(AuthMode::ApiKey), AuthReason::Manual),
                }
            }
        }
    }
}

/// What [`AuthChoice::decide`] knows about the destination.
#[derive(Clone, Copy, Debug)]
pub struct AuthContext<'a> {
    /// The site.
    pub site: &'a Site,
    /// The line, when the destination is a line (not a custom base URL).
    pub line: Option<&'a Line>,
    /// Probe result for the line (with the profile's key, ideally).
    pub caps: Option<&'a LineCaps>,
    /// The profile writes a custom `openai_base_url` (API key only).
    pub custom_base_url: bool,
    /// Codex reaches the site through Superba's local router, which sends the
    /// profile's key as an API key whatever the choice.
    pub router: bool,
    /// Every Codex that reads the configuration supports PAT (`None` = unknown).
    pub pat_version_ok: Option<bool>,
}

impl<'a> AuthContext<'a> {
    /// Context with only the site known.
    pub fn new(site: &'a Site) -> Self {
        Self {
            site,
            line: None,
            caps: None,
            custom_base_url: false,
            router: false,
            pat_version_ok: None,
        }
    }

    /// Set the line.
    pub fn line(mut self, line: &'a Line) -> Self {
        self.line = Some(line);
        self
    }

    /// Set the probe result.
    pub fn caps(mut self, caps: &'a LineCaps) -> Self {
        self.caps = Some(caps);
        self
    }

    /// Mark a custom base URL.
    pub fn custom_base_url(mut self, on: bool) -> Self {
        self.custom_base_url = on;
        self
    }

    /// Mark router mode.
    pub fn router(mut self, on: bool) -> Self {
        self.router = on;
        self
    }

    /// Set whether the Codex versions support PAT.
    pub fn pat_version_ok(mut self, ok: Option<bool>) -> Self {
        self.pat_version_ok = ok;
        self
    }
}

/// Result of [`AuthChoice::decide`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthDecision {
    /// What the profile asked for.
    pub choice: AuthChoice,
    /// The mode to use; `None` = no auth mode works here (the switch is blocked).
    pub mode: Option<AuthMode>,
    /// Why.
    pub reason: AuthReason,
}

impl AuthDecision {
    /// `Auto` picked an API key on a site that offers PAT (show the reason).
    pub fn fell_back(&self) -> bool {
        self.choice.is_auto() && self.mode == Some(AuthMode::ApiKey) && self.reason.is_fallback()
    }

    /// Text such as "Automatic: API key. The local router is on: ...".
    pub fn text(&self) -> LocalizedText {
        match self.mode {
            Some(mode) => LocalizedText::new("auth_reason.decision", "{mode}: {reason}")
                .arg_text("mode", mode.text())
                .arg_text("reason", self.reason.text()),
            None => self.reason.text(),
        }
    }
}

/// Why an auth mode was chosen (or none could be).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthReason {
    /// Chosen manually under Advanced.
    Manual,
    /// The line serves the PAT endpoints.
    PatAvailable,
    /// No probe result yet: PAT, the site's preference, is assumed and checked when
    /// switching.
    NotProbed,
    /// The site offers API keys only.
    SiteApiKeyOnly,
    /// A custom base URL is set; PAT needs a line.
    CustomBaseUrl,
    /// Superba's local router is on: Codex authenticates to it with a local key and
    /// the router uses each site's own API key.
    RouterMode,
    /// A Codex that reads this configuration is too old for PAT.
    CodexTooOld,
    /// PAT needs an https line.
    PatNeedsHttps,
    /// The line does not serve the PAT endpoints.
    PatUnavailable {
        /// Probe detail, if any.
        #[serde(default)]
        detail: Option<LocalizedText>,
    },
    /// The line is unreachable or serves no usable Codex path.
    LineUnavailable,
    /// The site offers no key-based auth mode.
    NoKeyMode,
    /// The manual choice does not work here.
    ManualUnavailable {
        /// The mode that was asked for.
        mode: AuthMode,
        /// Why, if known.
        #[serde(default)]
        detail: Option<LocalizedText>,
    },
}

/// One short sentence for a line probe's "PAT unavailable" detail, without the
/// endpoint path or the "an API key still works" tail the probe detail carries
/// (show the full detail on demand: hover, diagnostics).
pub fn pat_unavailable_summary(detail: &LocalizedText) -> LocalizedText {
    match detail.key.as_str() {
        "probe.pat_edge_blocked" => LocalizedText::new(
            "auth_reason.pat_edge_blocked",
            "The line's edge blocks PAT sign-in.",
        ),
        "probe.pat_needs_https" => {
            LocalizedText::new("auth_reason.pat_needs_https", "PAT needs an https line.")
        }
        _ => LocalizedText::new(
            "auth_reason.pat_unavailable",
            "PAT does not work on this line.",
        ),
    }
}

impl AuthReason {
    /// The probe detail behind the reason, when there is one (for a hover).
    pub fn detail(&self) -> Option<&LocalizedText> {
        match self {
            AuthReason::PatUnavailable { detail }
            | AuthReason::ManualUnavailable { detail, .. } => detail.as_ref(),
            _ => None,
        }
    }

    /// True for reasons that make `Auto` use an API key on a PAT site.
    pub fn is_fallback(&self) -> bool {
        matches!(
            self,
            AuthReason::CustomBaseUrl
                | AuthReason::RouterMode
                | AuthReason::CodexTooOld
                | AuthReason::PatNeedsHttps
                | AuthReason::PatUnavailable { .. }
        )
    }

    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            AuthReason::Manual => LocalizedText::new("auth_reason.manual", "Chosen manually."),
            AuthReason::PatAvailable => {
                LocalizedText::new("auth_reason.pat_available", "This line supports PAT.")
            }
            AuthReason::NotProbed => LocalizedText::new(
                "auth_reason.not_probed",
                "The line has not been checked yet; PAT is tried first.",
            ),
            AuthReason::SiteApiKeyOnly => {
                LocalizedText::new("auth_reason.site_apikey_only", "This site uses API keys.")
            }
            AuthReason::CustomBaseUrl => LocalizedText::new(
                "auth_reason.custom_base_url",
                "A custom base URL is set; PAT needs one of the site's lines.",
            ),
            AuthReason::RouterMode => LocalizedText::new(
                "auth_reason.router_mode",
                "Traffic goes through Superba's local router, which sends your key as an API key.",
            ),
            AuthReason::CodexTooOld => LocalizedText::new(
                "auth_reason.codex_too_old",
                "A Codex that reads this configuration is too old for PAT.",
            ),
            AuthReason::PatNeedsHttps => {
                LocalizedText::new("auth_reason.pat_needs_https", "PAT needs an https line.")
            }
            AuthReason::PatUnavailable { detail: Some(d) } => pat_unavailable_summary(d),
            AuthReason::PatUnavailable { detail: None } => LocalizedText::new(
                "auth_reason.pat_unavailable",
                "PAT does not work on this line.",
            ),
            AuthReason::LineUnavailable => {
                LocalizedText::new("auth_reason.line_unavailable", "The line is unavailable.")
            }
            AuthReason::NoKeyMode => {
                LocalizedText::new("auth_reason.no_key_mode", "This site does not use keys.")
            }
            AuthReason::ManualUnavailable {
                mode,
                detail: Some(d),
            } => LocalizedText::new(
                "auth_reason.manual_unavailable_detail",
                "{mode} cannot be used here: {detail}",
            )
            .arg_text("mode", mode.text())
            .arg_text("detail", d.clone()),
            AuthReason::ManualUnavailable { mode, detail: None } => LocalizedText::new(
                "auth_reason.manual_unavailable",
                "{mode} cannot be used here.",
            )
            .arg_text("mode", mode.text()),
        }
    }
}
