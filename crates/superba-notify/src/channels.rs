//! Request building and answer checking per channel kind (pure: no IO).
//!
//! [`build`] turns a rendered [`Message`] into the HTTP request one channel
//! expects; [`check`] reads the answer (several services answer HTTP 200 with
//! an error code in the body). Every URL must be `https://`, or `http://` on a
//! loopback address (self-hosted servers on the same machine, tests).
//!
//! | Kind | Request |
//! |---|---|
//! | Telegram | `POST {api_base}/bot<token>/sendMessage` JSON, HTML parse mode |
//! | Feishu / Lark | `POST <webhook>` JSON `msg_type=post`; `timestamp` + `sign` when signed |
//! | WeCom | `POST <webhook>` JSON `msgtype=text` (≤ 2048 bytes) |
//! | DingTalk | `POST <webhook>[&timestamp&sign]` JSON `msgtype=markdown`; keyword prefixed |
//! | ServerChan | `POST https://sctapi.ftqq.com/<SendKey>.send` (Server酱³ `sctp<uid>t…` keys: `https://<uid>.push.ft07.com/send/<key>.send`) JSON `title` + `desp` |
//! | PushPlus | `POST {api_base}/send` JSON `token`, `title`, `content`, `template=txt` |
//! | Bark | `POST {server}/push` JSON `device_key`, `title`, `body`, `group`, `level` |
//! | ntfy | `POST {server}/` JSON `topic`, `title`, `message`, `priority`, `tags`; Bearer token |
//! | Webhook | `POST <url>` JSON event; `X-Superba-Signature` when signed |

use serde_json::{Value, json};
use superba_core::{
    ChannelKind, EventKind, LocalizedText, NotifyChannel, NotifyEvent, NotifySecrets,
};

use crate::message::{Message, cut, cut_bytes, escape_html};
use crate::sign;

/// An HTTP request to send.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// Full URL (may hold a token: never logged).
    pub url: String,
    /// Headers.
    pub headers: Vec<(String, String)>,
    /// JSON (or text) body.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &"<redacted>")
            .field("headers", &self.headers.len())
            .field("body", &self.body.len())
            .finish()
    }
}

impl HttpRequest {
    fn json(url: String, body: &Value) -> Self {
        Self {
            url,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(body).unwrap_or_default(),
        }
    }

    /// A header value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as JSON (tests).
    pub fn json_body(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Why a request could not be built (missing or malformed fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildError(pub LocalizedText);

/// What [`build`] needs besides the channel and the message.
#[derive(Clone, Copy, Debug)]
pub struct BuildContext<'a> {
    /// The event (the webhook sends its fields).
    pub event: &'a NotifyEvent,
    /// Unix time in milliseconds (signatures).
    pub now_ms: i64,
    /// Feishu post language key (`zh_cn` / `en_us`).
    pub zh: bool,
    /// The webhook includes the reply summary (only when the user turned it on).
    pub include_summary: bool,
}

fn field_label(kind: ChannelKind, key: &str) -> LocalizedText {
    kind.field(key)
        .map(|f| f.label())
        .unwrap_or_else(|| LocalizedText::new(key.to_owned(), key.to_owned()))
}

fn missing(kind: ChannelKind, key: &str) -> BuildError {
    BuildError(
        LocalizedText::new("notify.error.missing_field", "{field} is required")
            .arg_text("field", field_label(kind, key)),
    )
}

fn bad_url(kind: ChannelKind, key: &str) -> BuildError {
    BuildError(
        LocalizedText::new(
            "notify.error.bad_url",
            "{field} must be an https:// address",
        )
        .arg_text("field", field_label(kind, key)),
    )
}

/// `https://`, or `http://` to a loopback address.
pub fn allowed_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw.trim()) else {
        return false;
    };
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => match url.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

fn secret<'a>(
    kind: ChannelKind,
    secrets: &'a NotifySecrets,
    key: &str,
) -> Result<&'a str, BuildError> {
    secrets
        .get(key)
        .map(|s| s.expose().trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| missing(kind, key))
}

fn url_secret<'a>(
    kind: ChannelKind,
    secrets: &'a NotifySecrets,
    key: &str,
) -> Result<&'a str, BuildError> {
    let url = secret(kind, secrets, key)?;
    if allowed_url(url) {
        Ok(url)
    } else {
        Err(bad_url(kind, key))
    }
}

fn base(channel: &NotifyChannel, key: &str) -> Result<String, BuildError> {
    let base = channel
        .param(key)
        .ok_or_else(|| missing(channel.kind, key))?;
    if !allowed_url(&base) {
        return Err(bad_url(channel.kind, key));
    }
    Ok(base.trim_end_matches('/').to_owned())
}

/// Check every required field and every address of `channel` (the form's
/// check before a save; [`build`] runs the same checks).
pub fn validate(channel: &NotifyChannel, secrets: &NotifySecrets) -> Result<(), BuildError> {
    for f in channel.kind.fields() {
        let value = if f.secret {
            secrets.get(f.key).map(|s| s.expose().trim().to_owned())
        } else {
            channel.param(f.key)
        };
        let value = value.filter(|v| !v.is_empty());
        if f.required && value.is_none() {
            return Err(missing(channel.kind, f.key));
        }
        let is_url = matches!(f.key, "webhook" | "url" | "api_base" | "server");
        if is_url
            && let Some(v) = value.as_deref()
            && !allowed_url(v)
        {
            return Err(bad_url(channel.kind, f.key));
        }
    }
    Ok(())
}

/// Build the request `channel` needs to deliver `msg`.
pub fn build(
    channel: &NotifyChannel,
    secrets: &NotifySecrets,
    msg: &Message,
    cx: BuildContext<'_>,
) -> Result<HttpRequest, BuildError> {
    let kind = channel.kind;
    let now_secs = cx.now_ms.div_euclid(1000);
    match kind {
        ChannelKind::Telegram => {
            let token = secret(kind, secrets, "bot_token")?;
            let chat = channel
                .param("chat_id")
                .ok_or_else(|| missing(kind, "chat_id"))?;
            let api = base(channel, "api_base")?;
            let mut text = format!("<b>{}</b>", escape_html(&msg.title));
            for line in &msg.lines {
                text.push('\n');
                text.push_str(&escape_html(line));
            }
            let mut body = json!({
                "chat_id": chat,
                "text": cut(&text, 4000),
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            });
            if let Some(topic) = channel
                .param("thread_id")
                .and_then(|t| t.parse::<i64>().ok())
            {
                body["message_thread_id"] = json!(topic);
            }
            Ok(HttpRequest::json(
                format!("{api}/bot{token}/sendMessage"),
                &body,
            ))
        }
        ChannelKind::Feishu => {
            let url = url_secret(kind, secrets, "webhook")?;
            let content: Vec<Value> = msg
                .lines
                .iter()
                .map(|l| json!([{ "tag": "text", "text": l }]))
                .collect();
            let lang = if cx.zh { "zh_cn" } else { "en_us" };
            let mut body = json!({
                "msg_type": "post",
                "content": { "post": { lang: { "title": msg.title, "content": content } } },
            });
            if let Some(secret) = secrets.get("sign_secret") {
                body["timestamp"] = json!(now_secs.to_string());
                body["sign"] = json!(sign::feishu(secret.expose().trim(), now_secs));
            }
            Ok(HttpRequest::json(url.to_owned(), &body))
        }
        ChannelKind::WeCom => {
            let url = url_secret(kind, secrets, "webhook")?;
            let body = json!({
                "msgtype": "text",
                "text": { "content": cut_bytes(&msg.text(), 2048) },
            });
            Ok(HttpRequest::json(url.to_owned(), &body))
        }
        ChannelKind::DingTalk => {
            let mut url = url_secret(kind, secrets, "webhook")?.to_owned();
            if let Some(secret) = secrets.get("sign_secret") {
                let signature = sign::dingtalk(secret.expose().trim(), cx.now_ms);
                let mut u = url::Url::parse(&url).map_err(|_| bad_url(kind, "webhook"))?;
                u.query_pairs_mut()
                    .append_pair("timestamp", &cx.now_ms.to_string())
                    .append_pair("sign", &signature);
                url = u.to_string();
            }
            let mut text = msg.markdown();
            let mut title = msg.title.clone();
            if let Some(keyword) = channel.param("keyword")
                && !msg.text().contains(&keyword)
            {
                text = format!("{keyword}  \n{text}");
                title = format!("{keyword} {title}");
            }
            let body = json!({
                "msgtype": "markdown",
                "markdown": { "title": cut(&title, 60), "text": cut_bytes(&text, 18_000) },
            });
            Ok(HttpRequest::json(url, &body))
        }
        ChannelKind::ServerChan => {
            let key = secret(kind, secrets, "send_key")?;
            if !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(BuildError(
                    LocalizedText::new("notify.error.missing_field", "{field} is required")
                        .arg_text("field", field_label(kind, "send_key")),
                ));
            }
            let url = match channel.param("api_base") {
                Some(_) => format!("{}/{key}.send", base(channel, "api_base")?),
                None => serverchan_url(key),
            };
            let body = json!({
                "title": cut(&msg.title, 32),
                "desp": msg.lines.join("\n\n"),
            });
            Ok(HttpRequest::json(url, &body))
        }
        ChannelKind::PushPlus => {
            let token = secret(kind, secrets, "token")?;
            let api = base(channel, "api_base")?;
            let mut body = json!({
                "token": token,
                "title": cut(&msg.title, 100),
                "content": msg.body(),
                "template": "txt",
            });
            if let Some(topic) = channel.param("topic") {
                body["topic"] = json!(topic);
            }
            Ok(HttpRequest::json(format!("{api}/send"), &body))
        }
        ChannelKind::Bark => {
            let key = secret(kind, secrets, "device_key")?;
            let server = base(channel, "server")?;
            let mut body = json!({
                "device_key": key,
                "title": msg.title,
                "body": msg.body(),
                "group": channel.param("group").unwrap_or_else(|| "Superba".into()),
                "isArchive": 1,
            });
            if msg.urgent {
                body["level"] = json!("timeSensitive");
            }
            Ok(HttpRequest::json(format!("{server}/push"), &body))
        }
        ChannelKind::Ntfy => {
            let topic = secret(kind, secrets, "topic")?;
            let server = base(channel, "server")?;
            let body = json!({
                "topic": topic,
                "title": msg.title,
                "message": msg.body(),
                "priority": if msg.urgent { 4 } else { 3 },
                "tags": [ntfy_tag(msg.kind)],
            });
            let mut req = HttpRequest::json(format!("{server}/"), &body);
            if let Some(token) = secrets.get("token") {
                req.headers.push((
                    "authorization".into(),
                    format!("Bearer {}", token.expose().trim()),
                ));
            }
            Ok(req)
        }
        ChannelKind::Webhook => {
            let url = url_secret(kind, secrets, "url")?;
            let mut event = serde_json::to_value(cx.event).unwrap_or(Value::Null);
            if let Some(map) = event.as_object_mut() {
                if !cx.include_summary {
                    map.remove("summary");
                }
                // The cwd is local detail; the project name is enough.
                map.remove("cwd");
                if let Some(project) = cx.event.project() {
                    map.insert("project".into(), json!(project));
                }
            }
            let body = json!({
                "source": "superba",
                "version": 1,
                "event": msg.kind.id(),
                "title": msg.title,
                "text": msg.text(),
                "data": event,
            });
            let bytes = serde_json::to_vec(&body).unwrap_or_default();
            let mut req = HttpRequest {
                url: url.to_owned(),
                headers: vec![
                    ("content-type".into(), "application/json".into()),
                    ("x-superba-event".into(), msg.kind.id().into()),
                    ("x-superba-timestamp".into(), now_secs.to_string()),
                ],
                body: bytes,
            };
            if let Some(secret) = secrets.get("sign_secret") {
                let sig = sign::webhook(secret.expose().trim(), now_secs, &req.body);
                req.headers.push(("x-superba-signature".into(), sig));
            }
            Ok(req)
        }
        _ => Err(BuildError(
            LocalizedText::new("notify.error.unknown_kind", "unknown channel type {kind}")
                .arg("kind", kind.id()),
        )),
    }
}

/// ServerChan's endpoint for a SendKey: Server酱³ keys (`sctp<uid>t…`) have
/// their own host, Turbo keys (`SCT…`) use `sctapi.ftqq.com`.
pub fn serverchan_url(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("sctp")
        && let Some(end) = rest.find('t')
        && end > 0
        && rest[..end].bytes().all(|b| b.is_ascii_digit())
    {
        let uid = &rest[..end];
        return format!("https://{uid}.push.ft07.com/send/{key}.send");
    }
    format!("https://sctapi.ftqq.com/{key}.send")
}

fn ntfy_tag(kind: EventKind) -> &'static str {
    match kind {
        EventKind::TurnComplete => "white_check_mark",
        EventKind::NeedsApproval => "raised_hand",
        EventKind::NeedsInput => "speech_balloon",
        EventKind::TurnFailed => "x",
        EventKind::SwitchDone => "twisted_rightwards_arrows",
        EventKind::SwitchFailed => "warning",
        EventKind::RouterMemberDown => "electric_plug",
        EventKind::UpdateAvailable => "arrow_up",
        EventKind::Test => "bell",
        _ => "loudspeaker",
    }
}

/// Whether the answer means "delivered"; else a short reason from the body.
pub fn check(kind: ChannelKind, status: u16, body: &[u8]) -> Result<(), LocalizedText> {
    let json: Option<Value> = serde_json::from_slice(body).ok();
    let refused = |message: String| {
        LocalizedText::new(
            "notify.error.refused",
            "the server refused the message: {message}",
        )
        .arg("message", cut(&message, 200))
    };
    let status_error = || {
        let reason = json.as_ref().and_then(|v| {
            [
                "description",
                "msg",
                "message",
                "errmsg",
                "error",
                "StatusMessage",
            ]
            .iter()
            .find_map(|k| v.get(*k).and_then(Value::as_str))
            .map(str::to_owned)
        });
        match reason {
            Some(r) => refused(format!("{status} {r}")),
            None => LocalizedText::new("notify.error.status", "the server answered {status}")
                .arg("status", status),
        }
    };
    if !(200..300).contains(&status) {
        return Err(status_error());
    }
    let Some(v) = json else {
        // Plain 2xx bodies (webhooks, some ntfy servers) are fine.
        return Ok(());
    };
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let code = |k: &str| v.get(k).and_then(Value::as_i64);
    match kind {
        ChannelKind::Telegram => match v.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(()),
            _ => Err(refused(text("description"))),
        },
        ChannelKind::Feishu => match code("code").or_else(|| code("StatusCode")) {
            Some(0) | None => Ok(()),
            Some(c) => Err(refused(format!("{c} {}", text("msg")))),
        },
        ChannelKind::WeCom | ChannelKind::DingTalk => match code("errcode") {
            Some(0) | None => Ok(()),
            Some(c) => Err(refused(format!("{c} {}", text("errmsg")))),
        },
        ChannelKind::ServerChan => match code("code") {
            Some(0) | None => Ok(()),
            Some(c) => Err(refused(format!("{c} {}", text("message")))),
        },
        ChannelKind::PushPlus | ChannelKind::Bark => match code("code") {
            Some(200) | None => Ok(()),
            Some(c) => {
                let m = if kind == ChannelKind::PushPlus {
                    text("msg")
                } else {
                    text("message")
                };
                Err(refused(format!("{c} {m}")))
            }
        },
        ChannelKind::Ntfy => match v.get("error").and_then(Value::as_str) {
            Some(e) => Err(refused(e.to_owned())),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}
