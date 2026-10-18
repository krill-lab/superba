//! Notifications (GitHub issue 12): a short message to a phone or a chat when a
//! coding task finishes, needs an approval or fails, and on Superba's own events
//! (a switch finished or failed, a router member is down, an update is out).
//!
//! The types here are shared by the sender (`superba-notify`), the switch
//! service, `spb notify` and the Preferences page:
//!
//! - [`NotifySettings`] lives in `profiles.json` ([`crate::Settings::notify`]);
//!   it never holds a secret. Each [`NotifyChannel`] keeps its secret fields
//!   (bot token, webhook URL, signing secret, ...) as one JSON object in the key
//!   store and only a [`crate::KeyRef`] here.
//! - [`NotifyEvent`] is what a harness hook (Codex `notify`, Claude Code `Stop` /
//!   `Notification`, ...) or Superba reports; the sender renders it per channel.
//! - Content is privacy-first: the reply summary is off by default
//!   ([`NotifyContent::summary`]); every message goes through the servers of the
//!   chosen channel (and, for [`ChannelKind::ServerChan`] / [`ChannelKind::PushPlus`],
//!   a third-party relay to personal WeChat).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{KeyRef, LocalizedText, Secret, Timestamp};

/// Harness ids used in events and settings (multi-harness design). Unknown ids
/// are kept as they are (`opencode`, `pi`, `dsh`, ...).
pub mod harness {
    /// OpenAI Codex (desktop app, CLI, IDE extension).
    pub const CODEX: &str = "codex";
    /// Anthropic Claude Code.
    pub const CLAUDE_CODE: &str = "claude-code";
    /// Superba itself (switches, router, updates).
    pub const SUPERBA: &str = "superba";

    /// Display name of a harness id (product names are not translated).
    pub fn display_name(id: &str) -> String {
        match id {
            CODEX => "Codex".into(),
            CLAUDE_CODE => "Claude Code".into(),
            SUPERBA => "Superba".into(),
            "opencode" => "opencode".into(),
            "pi" => "pi".into(),
            "dsh" => "DeepSeek harness".into(),
            other => other.into(),
        }
    }
}

/// A notification channel type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChannelKind {
    /// Telegram bot (`sendMessage`).
    Telegram,
    /// Feishu / Lark custom bot webhook (optional signature).
    Feishu,
    /// WeCom (企业微信) group bot webhook.
    #[serde(rename = "wecom")]
    WeCom,
    /// DingTalk custom bot (optional signature / keyword).
    #[serde(rename = "dingtalk")]
    DingTalk,
    /// Server酱 (ServerChan): a third-party relay to personal WeChat.
    #[serde(rename = "serverchan")]
    ServerChan,
    /// PushPlus: a third-party relay to personal WeChat.
    #[serde(rename = "pushplus")]
    PushPlus,
    /// Bark (iOS push through a Bark server).
    Bark,
    /// ntfy (ntfy.sh or a self-hosted server).
    Ntfy,
    /// Any HTTPS endpoint: a JSON POST, optionally signed (HMAC-SHA256).
    Webhook,
}

impl ChannelKind {
    /// Every kind, in the order the UI lists them.
    pub const ALL: [ChannelKind; 9] = [
        ChannelKind::Telegram,
        ChannelKind::Feishu,
        ChannelKind::WeCom,
        ChannelKind::DingTalk,
        ChannelKind::ServerChan,
        ChannelKind::PushPlus,
        ChannelKind::Bark,
        ChannelKind::Ntfy,
        ChannelKind::Webhook,
    ];

    /// Stable id (`telegram`, `feishu`, `wecom`, ...), as in `profiles.json`.
    pub fn id(self) -> &'static str {
        match self {
            ChannelKind::Telegram => "telegram",
            ChannelKind::Feishu => "feishu",
            ChannelKind::WeCom => "wecom",
            ChannelKind::DingTalk => "dingtalk",
            ChannelKind::ServerChan => "serverchan",
            ChannelKind::PushPlus => "pushplus",
            ChannelKind::Bark => "bark",
            ChannelKind::Ntfy => "ntfy",
            ChannelKind::Webhook => "webhook",
        }
    }

    /// Parse [`ChannelKind::id`] (also a few common spellings).
    pub fn parse(s: &str) -> Option<ChannelKind> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "telegram" | "tg" => ChannelKind::Telegram,
            "feishu" | "lark" => ChannelKind::Feishu,
            "wecom" | "wework" | "qywx" => ChannelKind::WeCom,
            "dingtalk" | "dingding" => ChannelKind::DingTalk,
            "serverchan" | "server-chan" | "ftqq" => ChannelKind::ServerChan,
            "pushplus" | "push-plus" => ChannelKind::PushPlus,
            "bark" => ChannelKind::Bark,
            "ntfy" => ChannelKind::Ntfy,
            "webhook" | "http" => ChannelKind::Webhook,
            _ => return None,
        })
    }

    /// Localized display name.
    pub fn label(self) -> LocalizedText {
        let fallback = match self {
            ChannelKind::Telegram => "Telegram",
            ChannelKind::Feishu => "Feishu / Lark",
            ChannelKind::WeCom => "WeCom group bot",
            ChannelKind::DingTalk => "DingTalk bot",
            ChannelKind::ServerChan => "WeChat via ServerChan",
            ChannelKind::PushPlus => "WeChat via PushPlus",
            ChannelKind::Bark => "Bark (iOS)",
            ChannelKind::Ntfy => "ntfy",
            ChannelKind::Webhook => "Webhook",
        };
        LocalizedText::new(format!("notify.kind.{}", self.id()), fallback)
    }

    /// One line about where the message goes (shown under the kind).
    pub fn route_note(self) -> LocalizedText {
        let fallback = match self {
            ChannelKind::Telegram => "Through Telegram's servers to your chat with your own bot.",
            ChannelKind::Feishu => "Through Feishu / Lark to a group with a custom bot.",
            ChannelKind::WeCom => "Through WeCom to a group with a group bot.",
            ChannelKind::DingTalk => "Through DingTalk to a group with a custom bot.",
            ChannelKind::ServerChan => {
                "Third party: Server酱 (sct.ftqq.com) receives the message and forwards it to \
                 your personal WeChat."
            }
            ChannelKind::PushPlus => {
                "Third party: PushPlus (pushplus.plus) receives the message and forwards it to \
                 your personal WeChat."
            }
            ChannelKind::Bark => {
                "Through a Bark server (api.day.app by default, or your own) and Apple's push \
                 service to the Bark app."
            }
            ChannelKind::Ntfy => {
                "Through an ntfy server (ntfy.sh by default, or your own); anyone who knows the \
                 topic can read it."
            }
            ChannelKind::Webhook => "A JSON POST to your own HTTPS endpoint.",
        };
        LocalizedText::new(format!("notify.route.{}", self.id()), fallback)
    }

    /// The message leaves for a third-party relay service (not the chat
    /// platform itself): shown with a warning.
    pub fn third_party_relay(self) -> bool {
        matches!(self, ChannelKind::ServerChan | ChannelKind::PushPlus)
    }

    /// The fields of this kind's form, secret ones included.
    pub fn fields(self) -> &'static [ChannelField] {
        use ChannelField as F;
        const TELEGRAM: &[ChannelField] = &[
            F::secret("bot_token", true),
            F::param("chat_id", true),
            F::param("thread_id", false),
            F::param_default("api_base", "https://api.telegram.org"),
        ];
        const FEISHU: &[ChannelField] =
            &[F::secret("webhook", true), F::secret("sign_secret", false)];
        const WECOM: &[ChannelField] = &[F::secret("webhook", true)];
        const DINGTALK: &[ChannelField] = &[
            F::secret("webhook", true),
            F::secret("sign_secret", false),
            F::param("keyword", false),
        ];
        const SERVERCHAN: &[ChannelField] =
            &[F::secret("send_key", true), F::param("api_base", false)];
        const PUSHPLUS: &[ChannelField] = &[
            F::secret("token", true),
            F::param("topic", false),
            F::param_default("api_base", "https://www.pushplus.plus"),
        ];
        const BARK: &[ChannelField] = &[
            F::secret("device_key", true),
            F::param_default("server", "https://api.day.app"),
            F::param_default("group", "Superba"),
        ];
        const NTFY: &[ChannelField] = &[
            F::secret("topic", true),
            F::secret("token", false),
            F::param_default("server", "https://ntfy.sh"),
        ];
        const WEBHOOK: &[ChannelField] = &[F::secret("url", true), F::secret("sign_secret", false)];
        match self {
            ChannelKind::Telegram => TELEGRAM,
            ChannelKind::Feishu => FEISHU,
            ChannelKind::WeCom => WECOM,
            ChannelKind::DingTalk => DINGTALK,
            ChannelKind::ServerChan => SERVERCHAN,
            ChannelKind::PushPlus => PUSHPLUS,
            ChannelKind::Bark => BARK,
            ChannelKind::Ntfy => NTFY,
            ChannelKind::Webhook => WEBHOOK,
        }
    }

    /// The field `key` of this kind.
    pub fn field(self, key: &str) -> Option<&'static ChannelField> {
        self.fields().iter().find(|f| f.key == key)
    }
}

/// One field of a channel form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelField {
    /// Key (`bot_token`, `chat_id`, `webhook`, ...).
    pub key: &'static str,
    /// Stored in the key store, never in `profiles.json`, never shown again.
    pub secret: bool,
    /// Must be filled.
    pub required: bool,
    /// Value used when empty (shown as the placeholder).
    pub default: Option<&'static str>,
}

impl ChannelField {
    const fn secret(key: &'static str, required: bool) -> Self {
        Self {
            key,
            secret: true,
            required,
            default: None,
        }
    }

    const fn param(key: &'static str, required: bool) -> Self {
        Self {
            key,
            secret: false,
            required,
            default: None,
        }
    }

    const fn param_default(key: &'static str, default: &'static str) -> Self {
        Self {
            key,
            secret: false,
            required: false,
            default: Some(default),
        }
    }

    /// Localized label (`notify.field.<key>`).
    pub fn label(&self) -> LocalizedText {
        let fallback = match self.key {
            "bot_token" => "Bot token",
            "chat_id" => "Chat id",
            "thread_id" => "Topic id (forum groups)",
            "api_base" => "API address",
            "webhook" => "Webhook URL",
            "sign_secret" => "Signing secret",
            "keyword" => "Keyword",
            "send_key" => "SendKey",
            "token" => "Token",
            "topic" => "Topic",
            "device_key" => "Device key",
            "server" => "Server",
            "group" => "Group",
            "url" => "URL",
            other => other,
        };
        LocalizedText::new(format!("notify.field.{}", self.key), fallback)
    }
}

/// A configured channel (`profiles.json`; secrets only by reference).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyChannel {
    /// Stable id (`tg-1a2b3c`).
    pub id: String,
    /// Type.
    pub kind: ChannelKind,
    /// The user's name for it ("My phone").
    #[serde(default)]
    pub name: String,
    /// Sends when on.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Non-secret fields ([`ChannelField::secret`] = false).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// Where the secret fields are stored (one JSON object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_ref: Option<KeyRef>,
    /// Masked hints of the stored secret fields (`••••abcd`), for display only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_hints: BTreeMap<String, String>,
    /// Only these event kinds go to this channel (empty = all that are on).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub only: BTreeSet<EventKind>,
}

fn yes() -> bool {
    true
}

impl NotifyChannel {
    /// A new channel. Ids are `<kind>-<random>` ([`NotifyChannel::id_for`]);
    /// an empty id gets one when the channel is saved.
    pub fn new(id: impl Into<String>, kind: ChannelKind, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            name: name.into(),
            enabled: true,
            params: BTreeMap::new(),
            key_ref: None,
            secret_hints: BTreeMap::new(),
            only: BTreeSet::new(),
        }
    }

    /// An id for a new channel of `kind` from a random `suffix` (letters and
    /// digits are kept, at most 8).
    pub fn id_for(kind: ChannelKind, suffix: &str) -> String {
        let suffix: String = suffix
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(8)
            .collect::<String>()
            .to_ascii_lowercase();
        format!("{}-{suffix}", kind.id())
    }

    /// A parameter, else the field's default.
    pub fn param(&self, key: &str) -> Option<String> {
        self.params
            .get(key)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                self.kind
                    .field(key)
                    .and_then(|f| f.default)
                    .map(str::to_owned)
            })
    }

    /// Name for lists ("My phone", else the kind's English name).
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.kind.label().fallback.clone()
        } else {
            self.name.clone()
        }
    }
}

/// The secret fields of one channel, in memory only (never serialized).
#[derive(Clone, Debug, Default)]
pub struct NotifySecrets(pub BTreeMap<String, Secret>);

impl NotifySecrets {
    /// One field.
    pub fn get(&self, key: &str) -> Option<&Secret> {
        self.0.get(key).filter(|s| !s.is_empty())
    }

    /// Set one field.
    pub fn set(&mut self, key: impl Into<String>, value: Secret) {
        self.0.insert(key.into(), value);
    }

    /// No field.
    pub fn is_empty(&self) -> bool {
        self.0.values().all(Secret::is_empty)
    }
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// A turn / task finished.
    TurnComplete,
    /// The agent waits for an approval (command, file edit, tool).
    NeedsApproval,
    /// The agent waits for an answer (a question, an idle prompt).
    NeedsInput,
    /// A turn ended with an error.
    TurnFailed,
    /// A Superba switch finished.
    SwitchDone,
    /// A Superba switch failed or was rolled back.
    SwitchFailed,
    /// A router member has been unavailable for a while.
    RouterMemberDown,
    /// A newer Superba is available.
    UpdateAvailable,
    /// The test button / `spb notify test`.
    Test,
    /// `spb notify send` (a script's own message).
    Message,
}

impl EventKind {
    /// Every kind.
    pub const ALL: [EventKind; 10] = [
        EventKind::TurnComplete,
        EventKind::NeedsApproval,
        EventKind::NeedsInput,
        EventKind::TurnFailed,
        EventKind::SwitchDone,
        EventKind::SwitchFailed,
        EventKind::RouterMemberDown,
        EventKind::UpdateAvailable,
        EventKind::Test,
        EventKind::Message,
    ];

    /// Stable id (`turn_complete`, ...).
    pub fn id(self) -> &'static str {
        match self {
            EventKind::TurnComplete => "turn_complete",
            EventKind::NeedsApproval => "needs_approval",
            EventKind::NeedsInput => "needs_input",
            EventKind::TurnFailed => "turn_failed",
            EventKind::SwitchDone => "switch_done",
            EventKind::SwitchFailed => "switch_failed",
            EventKind::RouterMemberDown => "router_member_down",
            EventKind::UpdateAvailable => "update_available",
            EventKind::Test => "test",
            EventKind::Message => "message",
        }
    }

    /// Parse [`EventKind::id`] (dashes allowed).
    pub fn parse(s: &str) -> Option<EventKind> {
        let s = s.trim().replace('-', "_").to_ascii_lowercase();
        EventKind::ALL.into_iter().find(|k| k.id() == s)
    }

    /// Localized name (`notify.event.<id>`).
    pub fn label(self) -> LocalizedText {
        let fallback = match self {
            EventKind::TurnComplete => "Task finished",
            EventKind::NeedsApproval => "Needs approval",
            EventKind::NeedsInput => "Waiting for you",
            EventKind::TurnFailed => "Task failed",
            EventKind::SwitchDone => "Switch finished",
            EventKind::SwitchFailed => "Switch failed",
            EventKind::RouterMemberDown => "Router member down",
            EventKind::UpdateAvailable => "Superba update available",
            EventKind::Test => "Test message",
            EventKind::Message => "Message",
        };
        LocalizedText::new(format!("notify.event.{}", self.id()), fallback)
    }

    /// Needs the user now (approvals, questions): passes quiet hours when
    /// [`QuietHours::allow_urgent`] is on and is sent with high priority where
    /// a channel has one.
    pub fn urgent(self) -> bool {
        matches!(self, EventKind::NeedsApproval | EventKind::NeedsInput)
    }

    /// Bypasses triggers, quiet hours, rate limits and deduplication.
    pub fn always_sent(self) -> bool {
        matches!(self, EventKind::Test | EventKind::Message)
    }
}

/// Token counts of a turn (OpenAI style: `input` includes cached input).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTokens {
    /// Input tokens, cached included.
    pub input: u64,
    /// Cached input tokens.
    #[serde(default)]
    pub cached: u64,
    /// Output tokens (reasoning included).
    pub output: u64,
}

/// One event to report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyEvent {
    /// What happened.
    pub kind: EventKind,
    /// Who reports it ([`harness`] ids).
    pub harness: String,
    /// The harness's client (`codex_exec`, `codex-tui`, the desktop app, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Machine name (this computer, a WSL distribution, a remote host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Working directory (kept for replies; only its last component is sent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Thread / session title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The last assistant message (sent only when [`NotifyContent::summary`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// What it is about: the error, the approval asked for, the switch target,
    /// the router member, the new version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Thread / session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Turn id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How long the turn ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Tokens of the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TurnTokens>,
    /// Cost of the turn in millionths of a US dollar, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micro_usd: Option<u64>,
    /// When.
    pub at: Timestamp,
}

impl NotifyEvent {
    /// A bare event of `kind` from `harness` at `at`.
    pub fn new(kind: EventKind, harness: impl Into<String>, at: Timestamp) -> Self {
        Self {
            kind,
            harness: harness.into(),
            client: None,
            host: None,
            cwd: None,
            title: None,
            summary: None,
            detail: None,
            thread_id: None,
            turn_id: None,
            model: None,
            duration_ms: None,
            tokens: None,
            cost_micro_usd: None,
            at,
        }
    }

    /// The last component of [`NotifyEvent::cwd`] (the project).
    pub fn project(&self) -> Option<String> {
        let cwd = self.cwd.as_deref()?.trim_end_matches(['/', '\\']);
        let name = cwd.rsplit(['/', '\\']).next().unwrap_or(cwd);
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// Key for deduplication: the same report of the same turn is sent once.
    /// Only the turn id counts (a session id would swallow every later turn of
    /// the session); Superba's own events put a unique id there (the version
    /// of an update, a router member's outage).
    pub fn dedupe_key(&self) -> Option<String> {
        let id = self.turn_id.as_deref()?;
        Some(format!("{}:{}:{}", self.harness, self.kind.id(), id))
    }
}

/// Which events are sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyTriggers {
    /// A task finished.
    pub turn_complete: bool,
    /// An approval is needed.
    pub needs_approval: bool,
    /// The agent waits for an answer.
    pub needs_input: bool,
    /// A task failed.
    pub turn_failed: bool,
    /// A switch finished.
    pub switch_done: bool,
    /// A switch failed.
    pub switch_failed: bool,
    /// A router member is down.
    pub router_member_down: bool,
    /// A Superba update is available.
    pub update_available: bool,
    /// Report a finished task only when it ran at least this long (0 = always).
    pub min_turn_secs: u32,
    /// Harnesses whose events are not sent ([`harness`] ids).
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub muted_harnesses: BTreeSet<String>,
}

impl Default for NotifyTriggers {
    fn default() -> Self {
        Self {
            turn_complete: true,
            needs_approval: true,
            needs_input: true,
            turn_failed: true,
            switch_done: false,
            switch_failed: true,
            router_member_down: true,
            update_available: true,
            min_turn_secs: 0,
            muted_harnesses: BTreeSet::new(),
        }
    }
}

impl NotifyTriggers {
    /// Whether `kind` is on.
    pub fn wants(&self, kind: EventKind) -> bool {
        match kind {
            EventKind::TurnComplete => self.turn_complete,
            EventKind::NeedsApproval => self.needs_approval,
            EventKind::NeedsInput => self.needs_input,
            EventKind::TurnFailed => self.turn_failed,
            EventKind::SwitchDone => self.switch_done,
            EventKind::SwitchFailed => self.switch_failed,
            EventKind::RouterMemberDown => self.router_member_down,
            EventKind::UpdateAvailable => self.update_available,
            EventKind::Test | EventKind::Message => true,
        }
    }

    /// Turn `kind` on or off (the always-sent kinds ignore it).
    pub fn set(&mut self, kind: EventKind, on: bool) {
        match kind {
            EventKind::TurnComplete => self.turn_complete = on,
            EventKind::NeedsApproval => self.needs_approval = on,
            EventKind::NeedsInput => self.needs_input = on,
            EventKind::TurnFailed => self.turn_failed = on,
            EventKind::SwitchDone => self.switch_done = on,
            EventKind::SwitchFailed => self.switch_failed = on,
            EventKind::RouterMemberDown => self.router_member_down = on,
            EventKind::UpdateAvailable => self.update_available = on,
            EventKind::Test | EventKind::Message => {}
        }
    }
}

/// What a message contains besides the event itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyContent {
    /// The machine's name.
    pub host: bool,
    /// The project folder's name and the thread title.
    pub project: bool,
    /// The start of the last reply (privacy: off by default).
    pub summary: bool,
    /// Duration, tokens and cost.
    pub usage: bool,
    /// Longest summary in characters.
    pub summary_chars: u32,
}

impl Default for NotifyContent {
    fn default() -> Self {
        Self {
            host: true,
            project: true,
            summary: false,
            usage: true,
            summary_chars: 200,
        }
    }
}

/// Do-not-disturb window in local time (may cross midnight).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuietHours {
    /// On.
    pub enabled: bool,
    /// Start, minutes after local midnight.
    pub start_min: u16,
    /// End, minutes after local midnight.
    pub end_min: u16,
    /// Approvals and questions are still sent.
    pub allow_urgent: bool,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start_min: 23 * 60,
            end_min: 8 * 60,
            allow_urgent: false,
        }
    }
}

impl QuietHours {
    /// Whether `minute` (0..1440, local) is inside the window.
    pub fn contains(&self, minute: u16) -> bool {
        if !self.enabled || self.start_min == self.end_min {
            return false;
        }
        let m = minute % 1440;
        if self.start_min < self.end_min {
            (self.start_min..self.end_min).contains(&m)
        } else {
            m >= self.start_min || m < self.end_min
        }
    }

    /// `HH:MM-HH:MM`.
    pub fn display(&self) -> String {
        format!("{}-{}", hhmm(self.start_min), hhmm(self.end_min))
    }

    /// Parse `HH:MM-HH:MM` (enables the window).
    pub fn parse(s: &str) -> Option<QuietHours> {
        let (a, b) = s.trim().split_once('-')?;
        Some(QuietHours {
            enabled: true,
            start_min: parse_hhmm(a)?,
            end_min: parse_hhmm(b)?,
            allow_urgent: false,
        })
    }
}

/// `HH:MM` of minutes after midnight.
pub fn hhmm(min: u16) -> String {
    format!("{:02}:{:02}", (min / 60) % 24, min % 60)
}

/// Minutes after midnight of `HH:MM` (also `H`, `HH`).
pub fn parse_hhmm(s: &str) -> Option<u16> {
    let s = s.trim();
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    let h: u16 = h.trim().parse().ok()?;
    let m: u16 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Per-channel limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimit {
    /// Messages per channel per minute (0 = no limit). Messages over the limit
    /// are dropped and counted in the next one that is sent.
    pub per_minute: u32,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self { per_minute: 6 }
    }
}

/// Notification preferences ([`crate::Settings::notify`]). No secrets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifySettings {
    /// Master switch (off until the user adds a channel and turns it on).
    pub enabled: bool,
    /// Channels.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<NotifyChannel>,
    /// Which events.
    pub triggers: NotifyTriggers,
    /// What a message says.
    pub content: NotifyContent,
    /// Do not disturb.
    pub quiet_hours: QuietHours,
    /// Limits.
    pub rate_limit: RateLimit,
}

impl NotifySettings {
    /// True for the defaults (serde skip helper).
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// A channel by id.
    pub fn channel(&self, id: &str) -> Option<&NotifyChannel> {
        self.channels.iter().find(|c| c.id == id)
    }
}

/// Why a channel did not send.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NotifySkip {
    /// Notifications are off.
    Disabled,
    /// The channel is off.
    ChannelOff,
    /// This event kind is off (or not for this channel).
    TriggerOff,
    /// The harness is muted.
    HarnessMuted,
    /// The task was shorter than the minimum.
    TooShort,
    /// Inside quiet hours.
    QuietHours,
    /// Over the channel's rate limit.
    RateLimited,
    /// Already sent for this turn.
    Duplicate,
}

impl NotifySkip {
    /// Localized reason.
    pub fn label(self) -> LocalizedText {
        let (id, fallback) = match self {
            NotifySkip::Disabled => ("disabled", "notifications are off"),
            NotifySkip::ChannelOff => ("channel_off", "the channel is off"),
            NotifySkip::TriggerOff => ("trigger_off", "this event is off"),
            NotifySkip::HarnessMuted => ("harness_muted", "this tool is muted"),
            NotifySkip::TooShort => ("too_short", "the task was shorter than the minimum"),
            NotifySkip::QuietHours => ("quiet_hours", "quiet hours"),
            NotifySkip::RateLimited => ("rate_limited", "rate limit reached"),
            NotifySkip::Duplicate => ("duplicate", "already sent"),
        };
        LocalizedText::new(format!("notify.skip.{id}"), fallback)
    }
}

/// The result for one channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ChannelOutcome {
    /// Accepted by the channel's server.
    Sent {
        /// Round trip.
        ms: u64,
    },
    /// Not sent, on purpose.
    Skipped {
        /// Why.
        reason: NotifySkip,
    },
    /// The server refused or could not be reached (secrets redacted).
    Failed {
        /// What went wrong.
        error: String,
    },
}

/// One channel's result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelResult {
    /// Channel id.
    pub channel: String,
    /// Its name.
    pub name: String,
    /// Its kind.
    pub kind: ChannelKind,
    /// What happened.
    pub outcome: ChannelOutcome,
}

/// What a dispatch did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyReport {
    /// The event was dropped before any channel (with the reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<NotifySkip>,
    /// Per channel.
    #[serde(default)]
    pub channels: Vec<ChannelResult>,
}

impl NotifyReport {
    /// Channels that sent.
    pub fn sent(&self) -> usize {
        self.channels
            .iter()
            .filter(|c| matches!(c.outcome, ChannelOutcome::Sent { .. }))
            .count()
    }

    /// Channels that failed.
    pub fn failed(&self) -> usize {
        self.channels
            .iter()
            .filter(|c| matches!(c.outcome, ChannelOutcome::Failed { .. }))
            .count()
    }
}

/// How an event is sent ([`crate::SwitchService::notify_send`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendOptions {
    /// Only this channel.
    pub channel: Option<String>,
    /// Ignore triggers, quiet hours, rate limit and deduplication.
    pub force: bool,
}

/// State of the hook that makes a harness report events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HookState {
    /// Superba's hook is in the harness's configuration.
    Installed,
    /// Installed, but the harness runs it only after the user trusts it
    /// (Codex lifecycle hooks).
    Untrusted,
    /// Not there.
    Missing,
    /// Another program is configured; Superba's hook would run it too.
    Foreign,
    /// The harness's configuration cannot be read.
    Unreadable,
    /// Superba writes it elsewhere (another page owns that file).
    External,
}

/// One harness hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookView {
    /// Harness id.
    pub harness: String,
    /// What the hook reports (e.g. Codex `notify` = finished tasks).
    pub events: Vec<EventKind>,
    /// Which mechanism (`notify`, `hooks.PermissionRequest`, `Stop`, ...).
    pub mechanism: String,
    /// The file it lives in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// State.
    pub state: HookState,
    /// The command line as configured (program and arguments).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The foreign program Superba's hook runs as well.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chained: Vec<String>,
    /// Remarks (restart needed, review in Codex, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<LocalizedText>,
}

/// The hooks of every harness ([`crate::SwitchService::notify_hooks`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyHooks {
    /// The program the hooks run (`spb`).
    pub program: String,
    /// Per harness / mechanism.
    pub hooks: Vec<HookView>,
}

/// Install or remove a hook ([`crate::SwitchService::notify_set_hook`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookRequest {
    /// Harness id ([`harness::CODEX`]).
    pub harness: String,
    /// The mechanism ([`HookView::mechanism`]).
    pub mechanism: String,
    /// Install (true) or remove (false).
    pub install: bool,
}
