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
/// A planned (not implemented) example is a "Krill account display" mode that wraps
/// the API key in an unsigned ChatGPT-style JWT, so the app shell shows plan, usage
/// and name, and points the shell at a localhost forwarder. It would be a new
/// variant with its own [`AuthFamily`] answer and an [`crate::AuthShape`] variant,
/// and would reuse [`crate::ResolvedEndpoints`] with a loopback origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AuthMode {
    /// Personal access token:
    /// auth.json `{"personal_access_token":K,"OPENAI_API_KEY":K}`,
    /// `chatgpt_base_url = <line>/backend-api/`, env `CODEX_AUTHAPI_BASE_URL=<line origin>`.
    #[serde(rename = "pat")]
    Pat,
    /// Plain API key:
    /// auth.json `{"auth_mode":"apikey","OPENAI_API_KEY":K}`; no env needed.
    #[serde(rename = "apikey")]
    ApiKey,
    /// The user's own ChatGPT login (restored from the encrypted stash).
    #[serde(rename = "chatgpt")]
    OfficialChatGpt,
}

impl AuthMode {
    /// Auth family; switching between families needs a full app restart until spike
    /// S18 proves otherwise (plan 7.1 rule 3).
    pub fn family(self) -> AuthFamily {
        match self {
            AuthMode::Pat | AuthMode::ApiKey => AuthFamily::Token,
            AuthMode::OfficialChatGpt => AuthFamily::ChatGpt,
        }
    }

    /// True if the mode needs a Superba-managed key ([`crate::KeyRef`]).
    pub fn uses_key(self) -> bool {
        matches!(self, AuthMode::Pat | AuthMode::ApiKey)
    }

    /// True if Codex must see `CODEX_AUTHAPI_BASE_URL` (PAT whoami origin).
    pub fn needs_authapi_env(self) -> bool {
        matches!(self, AuthMode::Pat)
    }

    /// True if `chatgpt_base_url` must point at the line (`wham/accounts/check`).
    pub fn writes_chatgpt_base_url(self) -> bool {
        matches!(self, AuthMode::Pat)
    }

    /// Display name.
    pub fn text(self) -> LocalizedText {
        match self {
            AuthMode::Pat => LocalizedText::new("auth_mode.pat", "PAT (personal access token)"),
            AuthMode::ApiKey => LocalizedText::new("auth_mode.apikey", "API key"),
            AuthMode::OfficialChatGpt => {
                LocalizedText::new("auth_mode.chatgpt", "Official ChatGPT login")
            }
        }
    }
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
    /// `{"personal_access_token":K, ...}`.
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
            AuthShape::Pat { .. } => Some(AuthMode::Pat),
            AuthShape::Missing | AuthShape::Unknown { .. } => None,
        }
    }

    /// Family of the current shape, if known.
    pub fn family(&self) -> Option<AuthFamily> {
        self.mode().map(AuthMode::family)
    }
}
