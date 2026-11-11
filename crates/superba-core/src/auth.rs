//! Authentication modes and the observable shape of `auth.json`.

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;

/// How Codex authenticates for a profile.
///
/// Extensibility: the enum is `non_exhaustive` and all behaviour is exposed through
/// methods ([`AuthMode::family`], [`AuthMode::uses_key`], ...). Other crates must
/// query these methods instead of matching on variants, so a future mode only needs
/// new method arms here plus its own rendering in `superba-config`.
///
/// Key-based profiles always use an API key (user bug 139): the desktop app
/// 26.1106 never confirms an account for a personal access token, so queued
/// messages fail with "Account is not confirmed". Profiles saved with the
/// former PAT mode (`"pat"`) read as an API key and get API-key auth the next
/// time they are applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AuthMode {
    /// Plain API key:
    /// auth.json `{"auth_mode":"apikey","OPENAI_API_KEY":K}`; no env needed.
    #[serde(rename = "apikey", alias = "pat")]
    ApiKey,
    /// The user's own ChatGPT login (restored from the encrypted stash).
    #[serde(rename = "chatgpt")]
    OfficialChatGpt,
    /// Mixed login (router mode only): `auth.json` keeps the user's own
    /// ChatGPT login, so remote control, Codex Cloud tasks and the app's
    /// ChatGPT features use the real account at chatgpt.com; only
    /// `openai_base_url` points at the local router, which accepts that login
    /// and sends model traffic to its members. Never chosen for a profile: the
    /// planner resolves router mode to it when the router setting asks.
    #[serde(rename = "mixed")]
    MixedOfficial,
}

impl AuthMode {
    /// Auth family; switching between families needs a full app restart until spike
    /// S18 proves otherwise (plan 7.1 rule 3).
    pub fn family(self) -> AuthFamily {
        match self {
            AuthMode::ApiKey => AuthFamily::Token,
            AuthMode::OfficialChatGpt | AuthMode::MixedOfficial => AuthFamily::ChatGpt,
        }
    }

    /// True if the mode needs a Superba-managed key ([`crate::KeyRef`]).
    pub fn uses_key(self) -> bool {
        matches!(self, AuthMode::ApiKey)
    }

    /// True if `auth.json` holds the user's own ChatGPT login.
    pub fn uses_official_login(self) -> bool {
        matches!(self, AuthMode::OfficialChatGpt | AuthMode::MixedOfficial)
    }

    /// Display name.
    pub fn text(self) -> LocalizedText {
        match self {
            AuthMode::ApiKey => LocalizedText::new("auth_mode.apikey", "API key"),
            AuthMode::OfficialChatGpt => {
                LocalizedText::new("auth_mode.chatgpt", "Official ChatGPT login")
            }
            AuthMode::MixedOfficial => LocalizedText::new(
                "auth_mode.mixed",
                "Official ChatGPT login + router (mixed login)",
            ),
        }
    }
}

/// The site's auth modes as written, without repeats: catalogs and saved
/// sites from before user bug 139 list `["pat", "apikey"]`, which both read
/// as [`AuthMode::ApiKey`].
pub fn deserialize_auth_modes<'de, D>(deserializer: D) -> Result<Vec<AuthMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut modes = Vec::<AuthMode>::deserialize(deserializer)?;
    let mut seen = Vec::with_capacity(modes.len());
    modes.retain(|m| {
        let new = !seen.contains(m);
        seen.push(*m);
        new
    });
    Ok(modes)
}

/// Families of auth modes that are hot-switchable among themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthFamily {
    /// Key based (PAT, API key).
    Token,
    /// ChatGPT OAuth login.
    ChatGpt,
}

/// What `auth.json` currently contains, without the secret itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthShape {
    /// No `auth.json`.
    Missing,
    /// ChatGPT login (`tokens` object present).
    ChatGpt {
        /// Email from the id_token, when decodable.
        #[serde(default)]
        email: Option<String>,
        /// ChatGPT account id, when present.
        #[serde(default)]
        account_id: Option<String>,
    },
    /// `{"auth_mode":"apikey","OPENAI_API_KEY":K}` (or a bare `OPENAI_API_KEY`).
    ApiKey {
        /// Last four characters of the key.
        key_last4: String,
    },
    /// `{"personal_access_token":K, ...}`: a personal access token, which
    /// Superba no longer writes (user bug 139); only ever observed.
    Pat {
        /// Last four characters of the token.
        key_last4: String,
    },
    /// Present but not understood (never overwrite silently).
    Unknown {
        /// Short description (top-level field names, parse error).
        detail: String,
    },
}

impl AuthShape {
    /// The auth mode this shape corresponds to, if any.
    pub fn mode(&self) -> Option<AuthMode> {
        match self {
            AuthShape::ChatGpt { .. } => Some(AuthMode::OfficialChatGpt),
            AuthShape::ApiKey { .. } => Some(AuthMode::ApiKey),
            AuthShape::Missing | AuthShape::Pat { .. } | AuthShape::Unknown { .. } => None,
        }
    }

    /// Family of the current shape, if known (a personal access token is a
    /// key, like an API key).
    pub fn family(&self) -> Option<AuthFamily> {
        match self {
            AuthShape::Pat { .. } => Some(AuthFamily::Token),
            _ => self.mode().map(AuthMode::family),
        }
    }
}
