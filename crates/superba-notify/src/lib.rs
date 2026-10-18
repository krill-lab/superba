//! Superba notifications (GitHub issue 12).
//!
//! A harness hook (Codex `notify`, Codex / Claude Code lifecycle hooks), the
//! rollout watcher or Superba itself produces a [`superba_core::NotifyEvent`];
//! [`Notifier::dispatch`] decides which channels get it ([`policy`]), renders
//! the text in the user's language ([`message`]), builds each channel's request
//! ([`channels`], signed where the channel signs: [`sign`]) and sends them at
//! the same time. Secrets come from the key store per send and never reach a
//! log: errors are stripped of URLs and of every secret value.
//!
//! Pure parts (payloads, signatures, policy, hook parsing) are separate
//! modules so they are tested without a network; the sender is tested against
//! local mock servers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod channels;
pub mod codex_config;
pub mod codex_rpc;
pub mod enrich;
pub mod hooks;
pub mod message;
pub mod policy;
pub mod sign;
pub mod watch;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use superba_core::{
    Catalog, ChannelOutcome, ChannelResult, Error, KeyRef, LocalizedText, NotifyChannel,
    NotifyEvent, NotifyReport, NotifySecrets, NotifySettings, NotifySkip, Result, Secret,
    SecretStore, Timestamp,
};

pub use channels::{BuildContext, BuildError, HttpRequest};
pub use message::Message;
pub use policy::{PolicyInput, PolicyState, StateFile};

/// Time limit of one send.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// The delivery log rotates past this size.
const LOG_MAX_BYTES: u64 = 512 * 1024;

/// Secret fields of a channel as the one JSON object the key store keeps.
pub fn secrets_to_secret(secrets: &NotifySecrets) -> Secret {
    let map: serde_json::Map<String, serde_json::Value> = secrets
        .0
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.expose().to_owned())))
        .collect();
    Secret::new(serde_json::Value::Object(map).to_string())
}

/// The secret fields stored by [`secrets_to_secret`].
pub fn secrets_from_secret(secret: &Secret) -> NotifySecrets {
    let mut out = NotifySecrets::default();
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(secret.expose()) {
        for (k, v) in map {
            if let serde_json::Value::String(s) = v {
                out.set(k, Secret::new(s));
            }
        }
    }
    out
}

/// Masked hints (`••••abcd`) of the secret fields, for display.
pub fn secret_hints(secrets: &NotifySecrets) -> std::collections::BTreeMap<String, String> {
    secrets
        .0
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.masked()))
        .collect()
}

/// Remove every secret value (and URLs) from a text meant for people / logs.
pub fn redact(text: &str, secrets: &NotifySecrets) -> String {
    let mut out = text.to_owned();
    for s in secrets.0.values() {
        let v = s.expose().trim();
        if v.len() >= 4 {
            out = out.replace(v, "••••");
        }
    }
    out
}

/// Sends events to channels.
#[derive(Clone, Debug)]
pub struct Notifier {
    client: reqwest::Client,
    state: StateFile,
}

/// What [`Notifier::dispatch`] needs besides the event.
pub struct Dispatch<'a> {
    /// The preferences.
    pub settings: &'a NotifySettings,
    /// The user's language.
    pub catalog: &'a Catalog,
    /// Policy inputs (time, local minute, channel selection, force).
    pub policy: PolicyInput,
    /// Where secrets come from.
    pub secrets: &'a dyn SecretStore,
}

#[derive(Serialize)]
struct LogLine<'a> {
    at: String,
    event: &'a str,
    harness: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped: Option<NotifySkip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a ChannelOutcome>,
}

impl Notifier {
    /// A sender keeping its state and log in `dir` (`<data>/notify`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(SEND_TIMEOUT)
            .connect_timeout(Duration::from_secs(6))
            // A token in a URL must never follow a redirect elsewhere.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("Superba/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            client,
            state: StateFile::new(dir),
        }
    }

    /// The state file (the watcher keeps its bookmarks there too).
    pub fn state(&self) -> &StateFile {
        &self.state
    }

    /// Decide, render, send and log. Never fails: per-channel problems are in
    /// the report.
    pub async fn dispatch(&self, event: &NotifyEvent, d: Dispatch<'_>) -> NotifyReport {
        let decision = match self
            .state
            .update(|state| policy::decide(event, d.settings, &d.policy, state))
        {
            Ok(decision) => decision,
            Err(e) => {
                tracing::warn!(error = %e, "notification state unavailable; sending without it");
                let mut scratch = PolicyState::default();
                policy::decide(event, d.settings, &d.policy, &mut scratch)
            }
        };
        let mut report = NotifyReport {
            skipped: decision.skipped,
            channels: Vec::new(),
        };
        let zh = d.catalog.locale() == superba_core::Locale::ZhCn;
        let now_ms = d.policy.now_ms;
        let content = &d.settings.content;
        let sends = decision
            .channels
            .into_iter()
            .map(|(channel, verdict)| async move {
                let outcome = match verdict {
                    Err(reason) => ChannelOutcome::Skipped { reason },
                    Ok(dropped) => {
                        let msg = message::render(event, content, d.catalog, dropped);
                        self.send_one(
                            &channel,
                            &msg,
                            event,
                            now_ms,
                            zh,
                            content.summary,
                            d.secrets,
                            d.catalog,
                        )
                        .await
                    }
                };
                ChannelResult {
                    channel: channel.id.clone(),
                    name: channel.display_name(),
                    kind: channel.kind,
                    outcome,
                }
            });
        report.channels = futures_util::future::join_all(sends).await;
        self.log(event, &report);
        report
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_one(
        &self,
        channel: &NotifyChannel,
        msg: &Message,
        event: &NotifyEvent,
        now_ms: i64,
        zh: bool,
        include_summary: bool,
        store: &dyn SecretStore,
        catalog: &Catalog,
    ) -> ChannelOutcome {
        let secrets = match load_secrets(channel, store).await {
            Ok(s) => s,
            Err(e) => {
                return ChannelOutcome::Failed {
                    error: e.to_string(),
                };
            }
        };
        let cx = BuildContext {
            event,
            now_ms,
            zh,
            include_summary,
        };
        let request = match channels::build(channel, &secrets, msg, cx) {
            Ok(r) => r,
            Err(BuildError(text)) => {
                return ChannelOutcome::Failed {
                    error: catalog.text(&text),
                };
            }
        };
        match self.send(channel.kind, &request).await {
            Ok(ms) => ChannelOutcome::Sent { ms },
            Err(text) => ChannelOutcome::Failed {
                error: redact(&catalog.text(&text), &secrets),
            },
        }
    }

    /// Send one built request and check the answer; returns the round trip.
    pub async fn send(
        &self,
        kind: superba_core::ChannelKind,
        request: &HttpRequest,
    ) -> std::result::Result<u64, LocalizedText> {
        let started = Instant::now();
        let mut builder = self.client.post(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = builder
            .body(request.body.clone())
            .send()
            .await
            .map_err(|e| network_error(&e))?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|e| network_error(&e))?;
        channels::check(kind, status, &body)?;
        Ok(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    fn log(&self, event: &NotifyEvent, report: &NotifyReport) {
        let dir = self.state.dir();
        let path = dir.join("log.jsonl");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX_BYTES) {
            let _ = std::fs::rename(&path, dir.join("log.1.jsonl"));
        }
        let at = event_time(event);
        let mut lines = Vec::new();
        let base = |channel, outcome, skipped| LogLine {
            at: at.clone(),
            event: event.kind.id(),
            harness: &event.harness,
            skipped,
            channel,
            outcome,
        };
        if report.channels.is_empty() {
            lines.push(base(
                None,
                None,
                report.skipped.or(Some(NotifySkip::ChannelOff)),
            ));
        }
        for c in &report.channels {
            lines.push(base(Some(c.channel.as_str()), Some(&c.outcome), None));
        }
        let _ = std::fs::create_dir_all(dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            for line in lines {
                if let Ok(s) = serde_json::to_string(&line) {
                    let _ = writeln!(f, "{s}");
                }
            }
        }
    }
}

fn event_time(event: &NotifyEvent) -> String {
    if event.at == Timestamp::EPOCH {
        Timestamp::now().to_rfc3339()
    } else {
        event.at.to_rfc3339()
    }
}

/// A network error without its URL (which may hold a token).
fn network_error(e: &reqwest::Error) -> LocalizedText {
    let kind = if e.is_timeout() {
        "timed out".to_owned()
    } else if e.is_connect() {
        "cannot connect".to_owned()
    } else {
        // `without_url` drops the URL from the message.
        let mut text = String::new();
        let mut source: Option<&dyn std::error::Error> = Some(e);
        while let Some(s) = source {
            let part = s.to_string();
            if !part.contains("://") && !text.contains(&part) {
                if !text.is_empty() {
                    text.push_str(": ");
                }
                text.push_str(&part);
            }
            source = s.source();
        }
        if text.is_empty() {
            "request failed".into()
        } else {
            text
        }
    };
    LocalizedText::new(
        "notify.error.refused",
        "the server refused the message: {message}",
    )
    .arg("message", kind)
}

/// The stored secret fields of `channel` (none when it has no key reference).
pub async fn load_secrets(
    channel: &NotifyChannel,
    store: &dyn SecretStore,
) -> Result<NotifySecrets> {
    match &channel.key_ref {
        None => Ok(NotifySecrets::default()),
        Some(r) => Ok(secrets_from_secret(&store.get(r).await?)),
    }
}

/// The key store entry name of a channel's secrets (`notify-<channel id>`).
pub fn secret_entry(channel_id: &str) -> superba_core::ProfileId {
    superba_core::ProfileId::new(format!("notify-{channel_id}"))
}

/// Whether `key_ref` was written for a notification channel (so deleting the
/// channel may delete it).
pub fn is_notify_key(key_ref: &KeyRef) -> bool {
    match key_ref {
        KeyRef::Keyring { account, .. } => account.starts_with("profile:notify-"),
        KeyRef::File { path } => path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("profile-notify-")),
        _ => false,
    }
}

/// `<data>/notify` of a data directory.
pub fn dir_of(data_dir: &superba_core::DataDir) -> PathBuf {
    data_dir.notify_dir()
}

/// The last lines of the delivery log (newest last), for diagnostics.
pub fn read_log(dir: &Path, max: usize) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("log.jsonl")).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(max)..]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// An [`Error`] for an unknown channel id.
pub fn no_channel(id: &str) -> Error {
    Error::NotFound {
        what: format!("notification channel {id}"),
    }
}
