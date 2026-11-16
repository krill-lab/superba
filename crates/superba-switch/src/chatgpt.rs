//! Saved official ChatGPT accounts (GitHub PR #20, issue #21): add one through
//! Codex's browser sign-in, save the current login, read the five-hour and
//! weekly Codex limits of each, forget one. Switching to an account is an
//! ordinary plan ([`superba_core::RestoreTarget::Account`], see the planner).
//!
//! The logins live in the local home's encrypted login stash
//! ([`superba_core::ConfigEngine::login_accounts`] and friends); this module
//! only runs Codex's own client for them. Every helper is a `codex app-server`
//! in a private, disposable `CODEX_HOME` ([`SessionHome`]) with file
//! credentials, plugins, bundled skills, analytics and update checks off,
//! talked to over stdio. Limits reads send only `account/rateLimits/read`
//! (and `account/read` for a missing plan): nothing is consumed or redeemed.
//! One helper runs at a time, none while a switch executes (a read takes the
//! [`Gate`] shared, `execute` / `recover` exclusively), and tokens Codex
//! rotates meanwhile go back through the engine's compare-and-swap
//! ([`superba_core::ConfigEngine::store_rotated_login`]). Payloads are never
//! logged: they hold tokens and sign-in URLs.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use superba_config::authjson;
use superba_config::chatgpt_home::SessionHome;
use superba_core::{
    ChatGptAccount, ChatGptLoginEvents, ChatGptLoginPrompt, ChatGptQuota, ChatGptWindow, Error,
    Result, SecretBytes, StashScope, Timestamp,
    chatgpt::{FIVE_HOUR_WINDOW_MINS, WEEKLY_WINDOW_MINS},
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};

use crate::Switcher;

/// How long one helper request may take.
const RPC_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a browser sign-in may take.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How often a waiting sign-in asks whether it was cancelled.
const LOGIN_POLL: Duration = Duration::from_millis(250);
/// A live login whose access token expires within this is about to be
/// renewed by the Codex that uses it.
const RENEWAL_MARGIN: Duration = Duration::from_secs(5 * 60);
/// Codex renews a login whose `last_refresh` is older than about 8 days; a
/// day of margin.
const RENEWAL_AGE: Duration = Duration::from_secs(7 * 86_400);

/// `-c` overrides of every helper: credentials in its own `auth.json` (never
/// the OS keyring), and nothing it does not need for an account request.
/// Unknown keys are ignored by Codex (a warning only).
const HELPER_CONFIG: [&str; 6] = [
    "cli_auth_credentials_store=\"file\"",
    "features.plugins=false",
    "skills.bundled.enabled=false",
    "analytics.enabled=false",
    "feedback.enabled=false",
    "check_for_update_on_startup=false",
];

/// Variables of Superba's own environment a helper must not inherit (they
/// would replace the account's login or point Codex elsewhere).
const HELPER_ENV_REMOVE: [&str; 8] = [
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_BASE_URL",
    "CODEX_AUTHAPI_BASE_URL",
    "CHATGPT_BASE_URL",
    "CODEX_SQLITE_HOME",
    "CODEX_HOME",
];

/// Hosts a sign-in page may be on.
const LOGIN_HOSTS: [&str; 3] = ["auth.openai.com", "auth0.openai.com", "chatgpt.com"];

/// Keeps helpers apart from switches and from each other.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    /// Shared by limits reads, exclusive for `execute` / `recover`.
    switching: tokio::sync::RwLock<()>,
    /// One helper at a time.
    helper: tokio::sync::Mutex<()>,
}

impl Gate {
    /// Held by a switch while it writes and verifies.
    pub(crate) async fn switch(&self) -> tokio::sync::RwLockWriteGuard<'_, ()> {
        self.switching.write().await
    }
}

/// JSON-RPC (Codex app-server's newline-delimited JSON) over any stream.
struct Rpc<R, W> {
    output: Lines<R>,
    input: W,
    next_id: u64,
    /// Notifications that arrived while waiting for an answer.
    notifications: VecDeque<Value>,
}

impl<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin> Rpc<R, W> {
    fn new(output: R, input: W) -> Self {
        Self {
            output: output.lines(),
            input,
            next_id: 0,
            notifications: VecDeque::new(),
        }
    }

    async fn send(&mut self, message: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(message)
            .map_err(|_| Error::other("cannot encode a Codex request"))?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| Error::other("Codex stopped reading its input"))?;
        self.input
            .flush()
            .await
            .map_err(|_| Error::other("Codex stopped reading its input"))
    }

    async fn receive(&mut self) -> Result<Value> {
        loop {
            let line = self
                .output
                .next_line()
                .await
                .map_err(|_| Error::other("cannot read Codex's answer"))?
                .ok_or_else(|| Error::other("Codex's account helper exited"))?;
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                return Ok(value);
            }
        }
    }

    /// One request; the answer's `result`. An error answer keeps only its
    /// message's first words out of the payload (never logged anyway).
    async fn call(&mut self, method: &str, params: Value, wait: Duration) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"id": id, "method": method, "params": params}))
            .await?;
        tokio::time::timeout(wait, async {
            loop {
                let message = self.receive().await?;
                let answer = message.get("id").and_then(Value::as_u64) == Some(id)
                    && message.get("method").is_none();
                if answer {
                    if let Some(error) = message.get("error") {
                        return Err(rpc_error(method, error));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                if message.get("id").is_none() && message.get("method").is_some() {
                    self.notifications.push_back(message);
                }
            }
        })
        .await
        .map_err(|_| Error::other(format!("Codex did not answer {method} in time")))?
    }

    /// The next notification named `method`, waiting up to `wait`.
    async fn notification(&mut self, method: &str, wait: Duration) -> Result<Option<Value>> {
        if let Some(i) = self
            .notifications
            .iter()
            .position(|n| n.get("method").and_then(Value::as_str) == Some(method))
        {
            return Ok(self.notifications.remove(i));
        }
        let found = tokio::time::timeout(wait, async {
            loop {
                let message = self.receive().await?;
                if message.get("id").is_none()
                    && message.get("method").and_then(Value::as_str) == Some(method)
                {
                    return Ok::<_, Error>(message);
                }
            }
        })
        .await;
        match found {
            Ok(message) => message.map(Some),
            Err(_) => Ok(None),
        }
    }

    async fn initialize(&mut self) -> Result<()> {
        self.call(
            "initialize",
            json!({"clientInfo": {
                "name": "superba",
                "title": "Superba",
                "version": env!("CARGO_PKG_VERSION"),
            }}),
            RPC_TIMEOUT,
        )
        .await?;
        self.send(&json!({"method": "initialized"})).await
    }

    /// `account/rateLimits/read` (a background read: no reset-credit details),
    /// then `account/read` only when the answer names no plan.
    async fn read_limits(&mut self, id: &str, now: Timestamp) -> Result<ChatGptQuota> {
        let limits = match self
            .call(
                "account/rateLimits/read",
                json!({"excludeResetCreditDetails": true}),
                RPC_TIMEOUT,
            )
            .await
        {
            // A Codex before the parameter existed.
            Err(Error::Invalid { .. }) => {
                self.call("account/rateLimits/read", Value::Null, RPC_TIMEOUT)
                    .await?
            }
            other => other?,
        };
        let mut quota = parse_quota(id, &limits, now);
        if quota.plan_type.is_none()
            && let Ok(account) = self
                .call("account/read", json!({"refreshToken": false}), RPC_TIMEOUT)
                .await
        {
            complete_plan(&mut quota, &account);
        }
        Ok(quota)
    }

    /// Codex's browser sign-in: the page goes to `events`, then the answer.
    async fn login(&mut self, events: &ChatGptLoginEvents) -> Result<()> {
        let started = self
            .call(
                "account/login/start",
                json!({"type": "chatgpt"}),
                RPC_TIMEOUT,
            )
            .await?;
        let url = started
            .get("authUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::other("Codex returned no sign-in page"))?
            .to_owned();
        if !login_url_ok(&url) {
            return Err(Error::invalid("Codex returned an unexpected sign-in page"));
        }
        let login_id = started
            .get("loginId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if !events(Some(ChatGptLoginPrompt { url })) {
            return Err(Error::Cancelled);
        }
        let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
        loop {
            if !events(None) {
                if let Some(id) = &login_id {
                    let _ = self
                        .call(
                            "account/login/cancel",
                            json!({"loginId": id}),
                            Duration::from_secs(2),
                        )
                        .await;
                }
                return Err(Error::Cancelled);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::other(
                    "the ChatGPT sign-in took too long; please try again",
                ));
            }
            let Some(done) = self
                .notification("account/login/completed", LOGIN_POLL)
                .await?
            else {
                continue;
            };
            let params = &done["params"];
            let other = match (&login_id, params.get("loginId").and_then(Value::as_str)) {
                (Some(ours), Some(theirs)) => ours != theirs,
                _ => false,
            };
            if other {
                continue;
            }
            return if params.get("success").and_then(Value::as_bool) == Some(true) {
                Ok(())
            } else {
                Err(Error::other(
                    "the ChatGPT sign-in did not complete; please try again",
                ))
            };
        }
    }
}

/// A Codex error answer as an [`Error`]: invalid params become
/// [`Error::Invalid`] (the caller may retry without them); nothing of the
/// payload is kept beyond the method (it may name the account).
fn rpc_error(method: &str, error: &Value) -> Error {
    if error.get("code").and_then(Value::as_i64) == Some(-32602) {
        return Error::invalid(format!("Codex refused the parameters of {method}"));
    }
    let auth = error
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|m| {
            let m = m.to_ascii_lowercase();
            m.contains("auth") || m.contains("sign in") || m.contains("login")
        });
    Error::other(if auth {
        format!("Codex could not use this ChatGPT login for {method}; sign in to the account again")
    } else {
        format!("Codex could not answer {method}")
    })
}

/// A sign-in page is HTTPS on one of OpenAI's sign-in hosts.
pub(crate) fn login_url_ok(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https" && u.host_str().is_some_and(|h| LOGIN_HOSTS.contains(&h))
    })
}

/// A running helper: the child and its stdio.
struct Helper {
    child: tokio::process::Child,
    rpc: Rpc<BufReader<tokio::process::ChildStdout>, tokio::process::ChildStdin>,
}

impl Helper {
    async fn start(codex: &Path, home: &Path) -> Result<Self> {
        let mut std_cmd = superba_core::process::command(codex);
        std_cmd.arg("app-server");
        for setting in HELPER_CONFIG {
            std_cmd.args(["-c", setting]);
        }
        for var in HELPER_ENV_REMOVE {
            std_cmd.env_remove(var);
        }
        std_cmd
            .env("CODEX_HOME", home)
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut cmd = tokio::process::Command::from(std_cmd);
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| Error::io(codex, &e))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill().await;
            return Err(Error::other("Codex's account helper has no stdio"));
        };
        let mut helper = Self {
            child,
            rpc: Rpc::new(BufReader::new(stdout), stdin),
        };
        if let Err(e) = helper.rpc.initialize().await {
            helper.stop().await;
            return Err(e);
        }
        Ok(helper)
    }

    async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

impl Switcher {
    fn local_scope(&self) -> StashScope {
        StashScope::Local(self.deps.ctx.codex_home().root().to_path_buf())
    }

    pub(crate) async fn chatgpt_accounts_impl(&self) -> Result<Vec<ChatGptAccount>> {
        self.deps.config.login_accounts(&self.local_scope()).await
    }

    pub(crate) async fn chatgpt_save_current_impl(&self) -> Result<ChatGptAccount> {
        let path = self.deps.ctx.codex_home().auth_json();
        let live = match tokio::fs::read(&path).await {
            Ok(bytes) => SecretBytes::new(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound {
                    what: "a ChatGPT login in auth.json".into(),
                });
            }
            Err(e) => return Err(Error::io(path, &e)),
        };
        if authjson::login_info(live.expose()).is_none() {
            return Err(Error::NotFound {
                what: "a ChatGPT login in auth.json".into(),
            });
        }
        self.deps
            .config
            .save_login_account(&self.local_scope(), &live)
            .await
    }

    pub(crate) async fn chatgpt_remove_impl(&self, id: &str) -> Result<()> {
        if self
            .deps
            .config
            .remove_login_account(&self.local_scope(), id)
            .await?
        {
            Ok(())
        } else {
            Err(Error::NotFound {
                what: "saved ChatGPT account".into(),
            })
        }
    }

    /// The newest Codex this computer can run (the environment monitor's
    /// snapshot: no detection on this path). An MSIX app's packaged Codex
    /// runs as its extracted copy (user bug 143).
    async fn helper_codex(&self) -> Result<PathBuf> {
        let facts = self.monitor.facts().await;
        let mut found: Vec<(PathBuf, Option<superba_core::CodexVersion>)> = Vec::new();
        for i in &facts.installs {
            if let Some(p) = &i.bundled_codex {
                found.push((p.clone(), i.bundled_codex_version.clone()));
            }
        }
        for b in &facts.binaries {
            if !found.iter().any(|(p, _)| *p == b.path) {
                found.push((b.path.clone(), b.version.clone()));
            }
        }
        found.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // The MSIX app's packaged Codex runs as a copy (the app's or
        // Superba's own), like everywhere else Superba starts the app's Codex.
        let ctx = self.deps.ctx.clone();
        tokio::task::spawn_blocking(move || {
            found.into_iter().find_map(|(path, _)| {
                superba_appctl::runnable::runnable_codex(&path, &ctx)
                    .ok()
                    .filter(|p| p.is_file())
            })
        })
        .await
        .ok()
        .flatten()
        .ok_or_else(|| Error::NotFound {
            what: "a Codex program for ChatGPT accounts (install Codex or the ChatGPT app)".into(),
        })
    }

    pub(crate) async fn chatgpt_add_impl(
        &self,
        events: ChatGptLoginEvents,
    ) -> Result<ChatGptAccount> {
        let codex = self.helper_codex().await?;
        let home = SessionHome::create(&self.deps.ctx.data_dir, None)?;
        let login = {
            let _one = self.chatgpt.helper.lock().await;
            let mut helper = Helper::start(&codex, home.path()).await?;
            let result = helper.rpc.login(&events).await;
            helper.stop().await;
            result
        };
        login?;
        if !events(None) {
            return Err(Error::Cancelled);
        }
        let auth = home
            .auth()?
            .filter(|a| authjson::login_info(a.expose()).is_some())
            .ok_or_else(|| Error::other("Codex saved no ChatGPT login"))?;
        let saved = self
            .deps
            .config
            .save_login_account(&self.local_scope(), &auth)
            .await;
        drop(home);
        saved
    }

    pub(crate) async fn chatgpt_quota_impl(&self, id: &str) -> Result<ChatGptQuota> {
        let Ok(_no_switch) = self.chatgpt.switching.try_read() else {
            return Err(Error::Locked {
                what: "a switch is running; the limits are read after it".into(),
            });
        };
        let _one = self.chatgpt.helper.lock().await;
        let d = &self.deps;
        let scope = self.local_scope();
        let live_path = d.ctx.codex_home().auth_json();
        let live = match tokio::fs::read(&live_path).await {
            Ok(b) => Some(SecretBytes::new(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io(live_path, &e)),
        };
        let active = live
            .as_ref()
            .filter(|l| authjson::login_info(l.expose()).is_some_and(|i| i.identity == id));
        let credential = match active {
            Some(live) => {
                // The Codex using this login renews it itself: reading now
                // would race it for the single-use refresh token.
                if authjson::renewal_due(live.expose(), d.clock.now(), RENEWAL_MARGIN, RENEWAL_AGE)
                    && d.app
                        .processes(&d.ctx.codex_home())
                        .await
                        .is_ok_and(|p| !p.is_empty())
                {
                    return Err(Error::Locked {
                        what: "ChatGPT is renewing this sign-in; its limits are read after that"
                            .into(),
                    });
                }
                live.clone()
            }
            None => d
                .config
                .load_login_account(&scope, id)
                .await?
                .ok_or_else(|| Error::NotFound {
                    what: "saved ChatGPT account".into(),
                })?,
        };
        let codex = self.helper_codex().await?;
        let home = SessionHome::create(&d.ctx.data_dir, Some(&credential))?;
        let read = async {
            let mut helper = Helper::start(&codex, home.path()).await?;
            let result = helper.rpc.read_limits(id, d.clock.now()).await;
            helper.stop().await;
            result
        }
        .await;
        // Tokens Codex rotated are kept even when the read itself failed.
        if let Some(after) = home.auth()?
            && after != credential
        {
            d.config
                .store_rotated_login(&scope, &credential, &after)
                .await?;
        }
        drop(home);
        let mut quota = read?;
        if quota.plan_type.is_none() {
            quota.plan_type = authjson::login_info(credential.expose())
                .and_then(|i| i.plan_type)
                .as_deref()
                .and_then(normalize_plan);
        }
        Ok(quota)
    }
}

/// An absent or placeholder plan is no subscription tier.
fn normalize_plan(plan: &str) -> Option<String> {
    let plan = plan.trim().to_ascii_lowercase();
    (!plan.is_empty() && plan != "unknown").then_some(plan)
}

/// The plan of `account/read`, for an answer that named none.
fn complete_plan(quota: &mut ChatGptQuota, account: &Value) {
    if quota.plan_type.is_none() && account["account"]["type"] == "chatgpt" {
        quota.plan_type = account["account"]["planType"]
            .as_str()
            .and_then(normalize_plan);
    }
}

/// The limits of `account/rateLimits/read`: the `codex` bucket of
/// `rateLimitsByLimitId`, else the single-bucket `rateLimits`; windows by
/// their duration (300 and 10080 minutes), not by primary / secondary
/// position. A window that is not reported stays `None` (never 100 %).
pub(crate) fn parse_quota(id: &str, result: &Value, now: Timestamp) -> ChatGptQuota {
    let bucket = result
        .get("rateLimitsByLimitId")
        .and_then(|b| b.get("codex"))
        .filter(|b| b.is_object())
        .or_else(|| result.get("rateLimits").filter(|b| b.is_object()))
        .unwrap_or(&Value::Null);
    let mut quota = ChatGptQuota {
        account_id: id.to_owned(),
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
            FIVE_HOUR_WINDOW_MINS => quota.five_hour = Some(parsed),
            WEEKLY_WINDOW_MINS => quota.weekly = Some(parsed),
            _ => {}
        }
    }
    quota
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_matched_by_duration_and_never_invented() {
        let both = json!({"rateLimitsByLimitId": {"codex": {
            "planType": "pro",
            "primary": {"usedPercent": 31, "windowDurationMins": 10080, "resetsAt": 100},
            "secondary": {"usedPercent": -2, "windowDurationMins": 300}
        }}, "rateLimits": {"primary": {"usedPercent": 99, "windowDurationMins": 300}}});
        let q = parse_quota("a", &both, Timestamp::EPOCH);
        assert_eq!(q.plan_type.as_deref(), Some("pro"));
        assert_eq!(q.five_hour.as_ref().unwrap().remaining, 100.0);
        let weekly = q.weekly.unwrap();
        assert_eq!(weekly.remaining, 69.0);
        assert_eq!(weekly.resets_at, Some(Timestamp::from_millis(100_000)));

        let weekly_only = json!({"rateLimits": {
            "planType": "free",
            "primary": {"usedPercent": 6, "windowDurationMins": 10080},
            "secondary": null
        }});
        let q = parse_quota("a", &weekly_only, Timestamp::EPOCH);
        assert!(q.five_hour.is_none());
        assert_eq!(q.weekly.unwrap().remaining, 94.0);

        for empty in [
            json!({}),
            json!({"rateLimits": {"primary": {"usedPercent": 3}}}),
            json!({"rateLimits": {"primary": {"usedPercent": 3, "windowDurationMins": 60}}}),
            json!({"rateLimits": null}),
        ] {
            let q = parse_quota("a", &empty, Timestamp::EPOCH);
            assert!(q.is_empty(), "{empty}");
        }
    }

    #[test]
    fn a_missing_plan_comes_from_the_account_and_unknown_is_none() {
        let mut q = parse_quota(
            "a",
            &json!({"rateLimits": {"planType": "unknown"}}),
            Timestamp::EPOCH,
        );
        assert!(q.plan_type.is_none());
        complete_plan(
            &mut q,
            &json!({"account": {"type": "chatgpt", "planType": "PLUS"}}),
        );
        assert_eq!(q.plan_type.as_deref(), Some("plus"));
        complete_plan(
            &mut q,
            &json!({"account": {"type": "chatgpt", "planType": "pro"}}),
        );
        assert_eq!(
            q.plan_type.as_deref(),
            Some("plus"),
            "a reported plan stays"
        );
        let mut q = parse_quota("a", &json!({}), Timestamp::EPOCH);
        complete_plan(&mut q, &json!({"account": {"type": "apiKey"}}));
        assert!(q.plan_type.is_none());
    }

    #[test]
    fn sign_in_pages_must_be_openai_https() {
        assert!(login_url_ok("https://auth.openai.com/oauth/authorize?x=1"));
        assert!(!login_url_ok("http://auth.openai.com/oauth/authorize"));
        assert!(!login_url_ok("https://auth.openai.com.example.net/"));
        assert!(!login_url_ok("not a url"));
    }

    /// A scripted app-server on the other end of in-memory pipes.
    async fn scripted(
        answers: Vec<Value>,
    ) -> (
        Rpc<BufReader<tokio::io::DuplexStream>, tokio::io::DuplexStream>,
        tokio::task::JoinHandle<Vec<Value>>,
    ) {
        let (client_out, mut server_in) = tokio::io::duplex(64 * 1024);
        let (mut server_out, client_in) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let mut lines = BufReader::new(&mut server_in).lines();
            let mut answers = answers.into_iter();
            while let Ok(Some(line)) = lines.next_line().await {
                let request: Value = serde_json::from_str(&line).unwrap();
                let id = request.get("id").cloned();
                requests.push(request);
                let Some(id) = id else { continue };
                let Some(mut answer) = answers.next() else {
                    break;
                };
                // Messages before the answer (notifications) are passed as an array.
                if let Some(list) = answer.as_array().cloned() {
                    for (i, m) in list.iter().enumerate() {
                        let mut m = m.clone();
                        if i + 1 == list.len() {
                            m["id"] = id.clone();
                        }
                        let mut b = serde_json::to_vec(&m).unwrap();
                        b.push(b'\n');
                        server_out.write_all(&b).await.unwrap();
                    }
                    continue;
                }
                answer["id"] = id;
                let mut b = serde_json::to_vec(&answer).unwrap();
                b.push(b'\n');
                server_out.write_all(&b).await.unwrap();
            }
            requests
        });
        (Rpc::new(BufReader::new(client_in), client_out), server)
    }

    #[tokio::test]
    async fn a_limits_read_sends_only_read_requests() {
        let (mut rpc, server) = scripted(vec![
            json!({"result": {}}),
            json!([
                {"method": "account/rateLimits/updated", "params": {}},
                {"result": {"rateLimits": {
                    "primary": {"usedPercent": 40, "windowDurationMins": 300},
                    "secondary": {"usedPercent": 10, "windowDurationMins": 10080}
                }}}
            ]),
            json!({"result": {"account": {"type": "chatgpt", "planType": "plus"}}}),
        ])
        .await;
        rpc.initialize().await.unwrap();
        let q = rpc.read_limits("acct", Timestamp::EPOCH).await.unwrap();
        assert_eq!(q.five_hour.unwrap().remaining, 60.0);
        assert_eq!(q.weekly.unwrap().remaining, 90.0);
        assert_eq!(q.plan_type.as_deref(), Some("plus"));
        drop(rpc);
        let methods: Vec<String> = server
            .await
            .unwrap()
            .iter()
            .map(|r| r["method"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            methods,
            [
                "initialize",
                "initialized",
                "account/rateLimits/read",
                "account/read"
            ]
        );
    }

    #[tokio::test]
    async fn an_error_answer_is_an_error_without_the_payload() {
        let (mut rpc, _server) = scripted(vec![json!({"error": {
            "code": -32600,
            "message": "codex account authentication required for someone@example.com"
        }})])
        .await;
        let e = rpc
            .read_limits("acct", Timestamp::EPOCH)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("sign in to the account again"), "{e}");
        assert!(!e.contains("someone@example.com"), "{e}");
    }

    fn recording(
        cancel_after_polls: Option<usize>,
    ) -> (
        ChatGptLoginEvents,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let pages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = pages.clone();
        let events: ChatGptLoginEvents = std::sync::Arc::new(move |prompt| {
            match prompt {
                Some(p) => seen.lock().unwrap().push(p.url),
                None => {
                    let n = polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if cancel_after_polls.is_some_and(|max| n >= max) {
                        return false;
                    }
                }
            }
            true
        });
        (events, pages)
    }

    const PAGE: &str = "https://auth.openai.com/oauth/authorize?state=s";

    #[tokio::test]
    async fn a_sign_in_reports_its_page_and_ends_with_its_own_completion() {
        let (mut rpc, _server) = scripted(vec![json!([
            {"method": "account/login/completed", "params": {"loginId": "other", "success": false}},
            {"method": "account/login/completed", "params": {"loginId": "mine", "success": true}},
            {"result": {"type": "chatgpt", "loginId": "mine", "authUrl": PAGE}}
        ])])
        .await;
        let (events, pages) = recording(None);
        rpc.login(&events).await.unwrap();
        assert_eq!(pages.lock().unwrap().as_slice(), [PAGE]);
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_asks_codex_to_cancel_it() {
        let (mut rpc, server) = scripted(vec![
            json!({"result": {"type": "chatgpt", "loginId": "mine", "authUrl": PAGE}}),
            json!({"result": {}}),
        ])
        .await;
        let (events, _) = recording(Some(2));
        assert!(matches!(rpc.login(&events).await, Err(Error::Cancelled)));
        drop(rpc);
        let requests = server.await.unwrap();
        assert_eq!(requests.last().unwrap()["method"], "account/login/cancel");
        assert_eq!(requests.last().unwrap()["params"]["loginId"], "mine");
    }

    #[tokio::test]
    async fn a_sign_in_page_elsewhere_is_refused() {
        let (mut rpc, _server) = scripted(vec![json!({"result": {
            "type": "chatgpt", "loginId": "mine", "authUrl": "https://evil.example/oauth"
        }})])
        .await;
        let (events, pages) = recording(None);
        assert!(matches!(
            rpc.login(&events).await,
            Err(Error::Invalid { .. })
        ));
        assert!(pages.lock().unwrap().is_empty(), "never opened");
    }
}
