//! Coexistence with other tools that manage Codex's configuration (GH#5):
//! cc-switch, AI Toolbox, Codex itself and hand edits.
//!
//! Superba owns exactly the keys its last switch wrote (`config.toml` keys,
//! the `auth.json` credential, its `CODEX_AUTHAPI_BASE_URL` values). Anything
//! that changed one of them since is an [`ExternalChange`]: it is detected when
//! Superba starts, before every write and by the overview's background polling,
//! shown with who most likely made it ([`ChangeSource`]) and the old and new
//! values, and never overwritten without an explicit take-over. Restores,
//! uninstall and the router's fallback put back only what Superba still owns
//! and report the rest. A hand-off ([`crate::SwitchIntent::HandOff`]) releases
//! everything Superba owns without writing over what the other tool changed,
//! so the other tool manages Codex from then on; the next switch takes over
//! again from a fresh baseline.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::auth::AuthShape;
use crate::config::KeyChange;
use crate::env::EnvLocation;
use crate::i18n::LocalizedText;
use crate::time::Timestamp;

/// Pseudo key path of `auth.json`'s credential: in
/// [`crate::SwitchRequest::key_resolutions`] it answers an [`AuthDrift`]
/// (`Overwrite` = take the file over, `KeepCurrent` = leave it to the other
/// tool, only possible for restores).
pub const AUTH_JSON_PATH: &str = "auth.json";

/// Wildcard key path in [`crate::SwitchRequest::key_resolutions`]: the
/// resolution of every key and of the credential that changed outside Superba
/// and has no answer of its own (`Overwrite` = take everything over: `spb
/// --take-over`, the GUI's "Take over"; `KeepCurrent` = keep all of it).
pub const ALL_CHANGED: &str = "*";

/// `auth.json` no longer holds the credential Superba wrote there (another
/// tool, `codex login` or a hand edit replaced or removed it). Shapes only:
/// never secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthDrift {
    /// What Superba wrote.
    pub written: AuthShape,
    /// What is there now.
    pub live: AuthShape,
}

/// A persisted `CODEX_AUTHAPI_BASE_URL` value that changed since Superba set it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvDrift {
    /// Variable.
    pub var: String,
    /// Where.
    pub location: EnvLocation,
    /// The value Superba set.
    pub written: Option<String>,
    /// The value there now (`None` = removed).
    pub live: Option<String>,
}

/// Who most likely changed what Superba manages. Attribution reads the other
/// tools' own stores (read-only) and compares their current Codex provider with
/// the live files; without a match it falls back to what changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeSource {
    /// cc-switch (`farion1231/cc-switch`) switched Codex to one of its providers.
    CcSwitch {
        /// The provider's name in cc-switch, when its store names it.
        #[serde(default)]
        provider: Option<String>,
    },
    /// AI Toolbox (`coulsontl/ai-toolbox`) applied one of its Codex providers.
    AiToolbox {
        /// The provider's name in AI Toolbox, when its store names it.
        #[serde(default)]
        provider: Option<String>,
    },
    /// Codex's own sign-in replaced `auth.json` with a ChatGPT login.
    CodexLogin,
    /// Only keys the Codex app writes itself changed (`model`, reasoning
    /// effort, speed tier: the app's model picker), or a hand edit of them.
    CodexApp,
    /// The user saved the file in Superba's own editor (GitHub issue 9): an
    /// explicit change of their own, kept like any other but never offered
    /// as "another tool".
    SuperbaEditor {
        /// When the edit was saved.
        at: Timestamp,
    },
    /// Another program or a hand edit.
    #[default]
    Unknown,
}

impl ChangeSource {
    /// The other tool's name, when a known configuration manager made the change.
    pub fn tool(&self) -> Option<&'static str> {
        match self {
            ChangeSource::CcSwitch { .. } => Some("cc-switch"),
            ChangeSource::AiToolbox { .. } => Some("AI Toolbox"),
            _ => None,
        }
    }

    /// The provider the other tool applied, when known.
    pub fn provider(&self) -> Option<&str> {
        match self {
            ChangeSource::CcSwitch { provider } | ChangeSource::AiToolbox { provider } => {
                provider.as_deref()
            }
            _ => None,
        }
    }

    /// "cc-switch (provider “X”)", "Codex's own sign-in", "another program or
    /// a hand edit", ... (`ownership.source.*`).
    pub fn text(&self) -> LocalizedText {
        let t = |key: &str, fallback: &str| {
            LocalizedText::new(format!("ownership.source.{key}"), fallback)
        };
        match (self.tool(), self.provider()) {
            (Some(tool), Some(provider)) => t("tool_provider", "{tool} (provider “{provider}”)")
                .arg("tool", tool)
                .arg("provider", provider),
            (Some(tool), None) => t("tool", "{tool}").arg("tool", tool),
            _ => match self {
                ChangeSource::CodexLogin => t("codex_login", "Codex's own sign-in"),
                ChangeSource::CodexApp => t(
                    "codex_app",
                    "the Codex app (its model picker) or a hand edit",
                ),
                ChangeSource::SuperbaEditor { .. } => {
                    t("superba_editor", "your edit in Superba's file editor")
                }
                _ => t("unknown", "another program or a hand edit"),
            },
        }
    }
}

/// What changed outside Superba in the keys, credential and environment values
/// Superba owns in the local Codex home (GH#5). Empty parts are omitted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalChange {
    /// Codex home.
    pub codex_home: PathBuf,
    /// Owned `config.toml` keys: `before` = the value Superba wrote, `after` =
    /// the live value (`None` = removed).
    #[serde(default)]
    pub keys: Vec<KeyChange>,
    /// The `auth.json` credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthDrift>,
    /// Persisted environment values.
    #[serde(default)]
    pub env: Vec<EnvDrift>,
    /// Who most likely made the change.
    #[serde(default)]
    pub source: ChangeSource,
    /// Why Superba thinks so ("cc-switch's current Codex provider “X” uses this
    /// address").
    #[serde(default)]
    pub evidence: Vec<LocalizedText>,
    /// Stable id of this state of the files (hashes of the live files and the
    /// changed environment values): a dismissal ("not now") is remembered for
    /// it, a new change asks again.
    #[serde(default)]
    pub fingerprint: String,
}

impl ExternalChange {
    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.auth.is_none() && self.env.is_empty()
    }

    /// Every path that needs an answer before Superba writes over it: the keys,
    /// plus [`AUTH_JSON_PATH`] for the credential.
    pub fn paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self.keys.iter().map(|k| k.path.clone()).collect();
        if self.auth.is_some() {
            out.push(AUTH_JSON_PATH.to_owned());
        }
        out
    }

    /// Number of changed items (keys, credential, environment locations).
    pub fn count(&self) -> usize {
        self.keys.len() + usize::from(self.auth.is_some()) + self.env.len()
    }
}

/// Superba released the configuration to another tool (a hand-off): it owns
/// nothing in this Codex home until the next switch, which takes over again
/// from a fresh baseline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandOffInfo {
    /// When.
    pub at: Timestamp,
    /// To whom (as attributed when the user handed off).
    #[serde(default)]
    pub to: ChangeSource,
    /// Generation that recorded the hand-off (its backups hold both files as
    /// they were then; the generation before it is what Superba had written).
    pub generation: u64,
}

/// Describe an auth shape for a diff row without secrets ("API key …1234",
/// "ChatGPT login (a@b.c)", "no auth.json").
pub fn auth_shape_label(shape: &AuthShape) -> LocalizedText {
    let t =
        |key: &str, fallback: &str| LocalizedText::new(format!("ownership.auth.{key}"), fallback);
    match shape {
        AuthShape::Missing => t("missing", "no auth.json"),
        AuthShape::ApiKey { key_last4 } => t("api_key", "API key …{last4}").arg("last4", key_last4),
        AuthShape::Pat { key_last4 } => t("pat", "PAT …{last4}").arg("last4", key_last4),
        AuthShape::ChatGpt { email, .. } => match email {
            Some(e) => t("chatgpt_email", "ChatGPT login ({email})").arg("email", e),
            None => t("chatgpt", "ChatGPT login"),
        },
        AuthShape::Unknown { .. } => t("unknown", "an auth.json Superba does not recognize"),
    }
}
