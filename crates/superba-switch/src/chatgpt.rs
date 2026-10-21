//! Managed ChatGPT login and limits through Codex's public app-server protocol.
//! Each helper has a private CODEX_HOME; adding an account never signs out the app.

use crate::Switcher;
use serde_json::{Value, json};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use superba_config::{authjson, chatgpt::Accounts};
use superba_core::{
    ChatGptAccount, ChatGptLoginEvents, ChatGptLoginPrompt, ChatGptQuota, ChatGptWindow, Error,
    Result, SecretBytes, StashScope, Timestamp,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

struct Rpc {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    pending: Vec<Value>,
}

impl Rpc {
    async fn start(binary: &Path, home: &Path) -> Result<Self> {
        let mut command: tokio::process::Command = superba_core::process::command(binary).into();
        command
            .args(["app-server", "-c", "cli_auth_credentials_store=\"file\""])
            .env("CODEX_HOME", home)
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for key in [
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "OPENAI_BASE_URL",
            "CODEX_AUTHAPI_BASE_URL",
            "CHATGPT_BASE_URL",
            "CODEX_SQLITE_HOME",
        ] {
            command.env_remove(key);
        }
        let mut child = command.spawn().map_err(|e| Error::io(binary, &e))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::other("Codex input unavailable"))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Error::other("Codex output unavailable"))?,
        )
        .lines();
        let mut rpc = Self {
            child,
            input,
            output,
            next_id: 0,
            pending: Vec::new(),
        };
        if let Err(error) = rpc.call("initialize", json!({"clientInfo": {"name":"superba", "title":"Superba", "version":env!("CARGO_PKG_VERSION")}})).await {
            rpc.stop().await;
            return Err(error);
        }
        rpc.send(json!({"method":"initialized"})).await?;
        Ok(rpc)
    }

    async fn send(&mut self, message: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&message)
            .map_err(|_| Error::other("cannot encode Codex request"))?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| Error::other("Codex input closed"))
    }

    async fn receive(&mut self) -> Result<Value> {
        let line = self
            .output
            .next_line()
            .await
            .map_err(|_| Error::other("cannot read Codex response"))?
            .ok_or_else(|| {
                Error::other("Codex helper exited; check the installed Codex version")
            })?;
        serde_json::from_str(&line).map_err(|_| Error::other("invalid Codex app-server response"))
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let message = self.receive().await?;
                if message.get("id").and_then(Value::as_u64) == Some(id) {
                    // Never put server payloads (which may contain tokens/URLs) in logs.
                    if message.get("error").is_some() {
                        return Err(Error::other(format!(
                            "Codex {method} failed; the account may need to sign in again"
                        )));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                if message.get("method").and_then(Value::as_str) == Some("account/login/completed")
                {
                    self.pending.push(message);
                }
            }
        })
        .await
        .map_err(|_| Error::other(format!("Codex {method} timed out")))?
    }

    async fn login(&mut self, events: &ChatGptLoginEvents) -> Result<()> {
        let result = self
            .call("account/login/start", json!({"type":"chatgpt"}))
            .await?;
        let url = result
            .get("authUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::other("Codex did not return a browser login URL"))?;
        let parsed = url::Url::parse(url).map_err(|_| Error::other("invalid login URL"))?;
        if parsed.scheme() != "https"
            || !matches!(
                parsed.host_str(),
                Some("auth.openai.com" | "auth0.openai.com" | "chatgpt.com")
            )
        {
            return Err(Error::invalid("Codex returned an unexpected login host"));
        }
        let login_id = result
            .get("loginId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !(events)(Some(ChatGptLoginPrompt { url: url.into() })) {
            return Err(Error::Cancelled);
        }
        tokio::time::timeout(Duration::from_secs(15 * 60), async {
            loop {
                if !(events)(None) {
                    return Err(Error::Cancelled);
                }
                let message = if let Some(message) = self.pending.pop() {
                    message
                } else {
                    match tokio::time::timeout(Duration::from_millis(500), self.receive()).await {
                        Ok(message) => message?,
                        Err(_) => continue,
                    }
                };
                if message.get("method").and_then(Value::as_str) != Some("account/login/completed")
                {
                    continue;
                }
                let params = &message["params"];
                if params.get("loginId").and_then(Value::as_str) != login_id.as_deref() {
                    continue;
                }
                return if params.get("success").and_then(Value::as_bool) == Some(true) {
                    Ok(())
                } else {
                    Err(Error::other(
                        "ChatGPT sign-in did not complete; please try again",
                    ))
                };
            }
        })
        .await
        .map_err(|_| Error::other("ChatGPT sign-in expired; please try again"))?
    }

    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

impl Switcher {
    async fn chatgpt_binary(&self) -> Result<std::path::PathBuf> {
        let binaries = self.deps.app.codex_binaries().await?;
        binaries
            .iter()
            .find(|b| b.path.is_file())
            .map(|b| b.path.clone())
            .ok_or_else(|| {
                Error::other("Install Codex or its desktop app before adding a ChatGPT account")
            })
    }

    async fn live_chatgpt_auth(&self) -> Result<Option<SecretBytes>> {
        let path = self.deps.ctx.codex_home().auth_json();
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(SecretBytes::new(bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(path, &e)),
        }
    }

    pub(crate) async fn managed_chatgpt_accounts(&self) -> Result<Vec<ChatGptAccount>> {
        Accounts::new(self.deps.ctx.clone()).list(self.live_chatgpt_auth().await?.as_ref())
    }

    pub(crate) async fn import_chatgpt_login(&self) -> Result<ChatGptAccount> {
        let store = Accounts::new(self.deps.ctx.clone());
        let live = self
            .live_chatgpt_auth()
            .await?
            .filter(|a| authjson::login_info(a.expose()).is_some());
        let auth = match live {
            Some(auth) => auth,
            None => self
                .deps
                .config
                .load_login_stash(&StashScope::Local(
                    self.deps.ctx.codex_home().root().to_path_buf(),
                ))
                .await?
                .ok_or_else(|| {
                    Error::other("No current or saved ChatGPT login; use Add account")
                })?,
        };
        store.put(&auth, self.deps.clock.now())
    }

    pub(crate) async fn add_chatgpt_login(
        &self,
        events: ChatGptLoginEvents,
    ) -> Result<ChatGptAccount> {
        let store = Accounts::new(self.deps.ctx.clone());
        let home = store.session(None)?;
        let mut rpc = Rpc::start(&self.chatgpt_binary().await?, home.path()).await?;
        let login = rpc.login(&events).await;
        rpc.stop().await;
        login?;
        if !(events)(None) {
            return Err(Error::Cancelled);
        }
        store.put(&home.auth()?, self.deps.clock.now())
    }

    pub(crate) async fn read_chatgpt_quota(&self, id: &str) -> Result<ChatGptQuota> {
        let store = Accounts::new(self.deps.ctx.clone());
        let live = self.live_chatgpt_auth().await?;
        let active = live
            .as_ref()
            .is_some_and(|a| authjson::login_info(a.expose()).is_some_and(|i| i.identity == id));
        let credential = if active {
            live.clone()
                .ok_or_else(|| Error::other("missing active login"))?
        } else {
            store.load(id)?
        };
        let home = store.session(Some(&credential))?;
        let mut rpc = Rpc::start(&self.chatgpt_binary().await?, home.path()).await?;
        let mut result = rpc
            .call("account/rateLimits/read", json!({}))
            .await
            .map(|value| parse_quota(id, &value, self.deps.clock.now()));
        if let Ok(quota) = &mut result
            && quota.plan_type.is_none()
            && let Ok(account) = rpc
                .call("account/read", json!({"refreshToken": false}))
                .await
        {
            complete_quota_plan(quota, &account);
        }
        rpc.stop().await;
        let refreshed = home.auth()?;
        let identity = authjson::login_info(refreshed.expose())
            .ok_or_else(|| Error::other("Codex returned an invalid login"))?
            .identity;
        if identity != id {
            return Err(Error::other(
                "ChatGPT account identity changed; please sign in again",
            ));
        }
        if active && refreshed != credential {
            store.refresh_live(&credential, &refreshed)?;
        }
        // Preserve rotated tokens even if the subsequent limits request failed.
        // The desktop app's newer live refresh wins over this helper's snapshot.
        let current = self.live_chatgpt_auth().await?;
        let latest = current
            .as_ref()
            .filter(|auth| authjson::login_info(auth.expose()).is_some_and(|i| i.identity == id));
        let saved = store.put(latest.unwrap_or(&refreshed), self.deps.clock.now())?;
        let mut quota = result?;
        if quota.plan_type.is_none() {
            quota.plan_type = saved.plan_type.as_deref().and_then(normalize_plan);
        }
        Ok(quota)
    }
}

/// An absent/placeholder plan is a read state, not a subscription tier.
fn normalize_plan(plan: &str) -> Option<String> {
    let plan = plan.trim().to_ascii_lowercase();
    (!plan.is_empty() && plan != "unknown").then_some(plan)
}

fn complete_quota_plan(quota: &mut ChatGptQuota, result: &Value) {
    if quota.plan_type.is_none() && result["account"]["type"] == "chatgpt" {
        quota.plan_type = result["account"]["planType"]
            .as_str()
            .and_then(normalize_plan);
    }
}

/// Choose the Codex bucket and match windows by duration, not by primary/secondary position.
fn parse_quota(id: &str, result: &Value, now: Timestamp) -> ChatGptQuota {
    let bucket = result
        .get("rateLimitsByLimitId")
        .and_then(|b| b.get("codex"))
        .filter(|b| b.is_object())
        .or_else(|| result.get("rateLimits"))
        .unwrap_or(&Value::Null);
    let mut quota = ChatGptQuota {
        account_id: id.into(),
        plan_type: bucket
            .get("planType")
            .and_then(Value::as_str)
            .and_then(normalize_plan),
        five_hour: None,
        weekly: None,
        fetched_at: now,
    };
    for key in ["primary", "secondary"] {
        let window = &bucket[key];
        let Some(used) = window
            .get("usedPercent")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
        else {
            continue;
        };
        let Some(minutes) = window.get("windowDurationMins").and_then(Value::as_u64) else {
            continue;
        };
        let parsed = ChatGptWindow {
            remaining: (100.0 - used).clamp(0.0, 100.0),
            minutes,
            resets_at: window
                .get("resetsAt")
                .and_then(Value::as_i64)
                .map(|s| Timestamp::from_millis(s.saturating_mul(1000))),
        };
        match minutes {
            300 => quota.five_hour = Some(parsed),
            10080 => quota.weekly = Some(parsed),
            _ => {}
        }
    }
    quota
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_quota_plan_uses_account_info_without_overwriting_a_reported_plan() {
        for plan in ["free", "plus", "pro"] {
            let mut quota = parse_quota(
                "a",
                &json!({"rateLimits":{"planType":"unknown"}}),
                Timestamp::EPOCH,
            );
            assert!(quota.plan_type.is_none());
            complete_quota_plan(
                &mut quota,
                &json!({"account":{"type":"chatgpt", "planType":plan.to_uppercase()}}),
            );
            assert_eq!(quota.plan_type.as_deref(), Some(plan));
            complete_quota_plan(
                &mut quota,
                &json!({"account":{"type":"chatgpt", "planType":"other"}}),
            );
            assert_eq!(quota.plan_type.as_deref(), Some(plan));
        }
        let mut quota = parse_quota("a", &json!({}), Timestamp::EPOCH);
        complete_quota_plan(&mut quota, &json!({"account":null}));
        assert!(quota.plan_type.is_none());
        assert!(normalize_plan(" UNKNOWN ").is_none());
        assert!(normalize_plan(" ").is_none());
    }

    #[test]
    fn weekly_only_subscriptions_keep_the_plan_and_do_not_invent_five_hour_limits() {
        for plan in ["free", "plus", "pro", "business"] {
            let quota = parse_quota(
                "a",
                &json!({"rateLimits": {
                    "planType": plan,
                    "primary": {"usedPercent": 6, "windowDurationMins": 10080},
                    "secondary": null
                }}),
                Timestamp::EPOCH,
            );
            assert_eq!(quota.plan_type.as_deref(), Some(plan));
            assert!(quota.five_hour.is_none());
            assert_eq!(quota.weekly.unwrap().remaining, 94.0);
        }
        let quota = parse_quota(
            "a",
            &json!({"rateLimits": {"planType":"plus"},
                "rateLimitsByLimitId":{"codex":{"planType":"pro", "primary":{"usedPercent":100,"windowDurationMins":300}}}
            }),
            Timestamp::EPOCH,
        );
        assert_eq!(quota.plan_type.as_deref(), Some("pro"));
        assert_eq!(quota.five_hour.unwrap().remaining, 0.0);
        assert!(quota.weekly.is_none());
    }

    #[test]
    fn limits_use_duration_and_never_invent_missing_values() {
        let response = json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":31,"windowDurationMins":10080,"resetsAt":100},"secondary":{"usedPercent":-2,"windowDurationMins":300}}},"rateLimits":{"primary":{"usedPercent":99,"windowDurationMins":300}}});
        let quota = parse_quota("a", &response, Timestamp::EPOCH);
        assert_eq!(quota.five_hour.unwrap().remaining, 100.0);
        let weekly = quota.weekly.unwrap();
        assert_eq!(weekly.remaining, 69.0);
        assert_eq!(weekly.resets_at, Some(Timestamp::from_millis(100000)));
        let missing = parse_quota(
            "a",
            &json!({"rateLimits":{"primary":{"usedPercent":3}}}),
            Timestamp::EPOCH,
        );
        assert!(missing.five_hour.is_none() && missing.weekly.is_none());
    }
}
