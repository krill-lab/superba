//! The user's own official ChatGPT login inside the router (mixed login).
//!
//! Two independent uses, both for one person's own account on their own
//! computer (never shared, never pooled):
//!
//! - **Mixed login client** ([`OfficialClientConfig`]): Codex keeps its real
//!   ChatGPT login in `auth.json` (so remote control, Codex Cloud tasks, the
//!   app's browser plugin and usage pages talk to chatgpt.com directly with
//!   the real account) and only `openai_base_url` points at the router. Codex
//!   then presents its ChatGPT access token here. The router accepts it on the
//!   loopback Codex ingress when the JWT names the configured account, never
//!   forwards it to a relay, and turns a member's 401 into a 502 so Codex does
//!   not refresh its own login for a relay's fault.
//! - **Official member** ([`crate::MemberKind::Official`]): a member that sends
//!   Responses to `chatgpt.com/backend-api/codex` with the account's own
//!   token. The token comes from the request itself when it is the mixed-login
//!   client's (passthrough: Codex owns refresh), otherwise from the router's
//!   secret store ([`OfficialTokens`], `secrets.json`, 0600), refreshed only
//!   when a request needs it, under one lock per member, and written back to
//!   disk before the lock is released (refresh tokens are single-use). Status,
//!   health probes and the admin API never refresh.
//!
//! JWT signatures are not verified: the router only listens on loopback and
//! the claim decides which account a request belongs to, not whether it is
//! genuine (chatgpt.com verifies that).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use superba_core::{Error, Result, Secret};
use url::Url;

/// Default upstream of an official member.
pub const OFFICIAL_CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
/// Default OAuth token endpoint (Codex's own).
pub const OFFICIAL_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex CLI's public OAuth client id (the login the tokens came from).
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Header naming the ChatGPT account of a request.
pub const ACCOUNT_HEADER: &str = "chatgpt-account-id";
/// Refresh when the access token expires within this margin.
const REFRESH_MARGIN: Duration = Duration::from_secs(120);

/// Router configuration of the mixed-login client (no secret).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficialClientConfig {
    /// `chatgpt_account_id` of the login Codex holds.
    pub account_id: String,
}

/// Claims of a ChatGPT access or id token the router cares about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    /// `https://api.openai.com/auth.chatgpt_account_id`.
    pub account_id: String,
    /// `exp` (seconds since the epoch), if present.
    pub exp: Option<u64>,
    /// `https://api.openai.com/auth.chatgpt_plan_type`, if present.
    pub plan: Option<String>,
}

/// Decode the payload of a JWT without verifying it. `None` when it is not a
/// ChatGPT token (no account id claim).
pub fn claims(token: &str) -> Option<Claims> {
    let mut parts = token.split('.');
    let (_, payload, _) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let auth = v.get("https://api.openai.com/auth")?;
    let account_id = auth.get("chatgpt_account_id")?.as_str()?.to_owned();
    if account_id.is_empty() {
        return None;
    }
    Some(Claims {
        account_id,
        exp: v.get("exp").and_then(serde_json::Value::as_u64),
        plan: auth
            .get("chatgpt_plan_type")
            .and_then(|p| p.as_str())
            .map(str::to_owned),
    })
}

/// What the mixed-login client presented: put in the request extensions by
/// the listener after it matched the configured account.
#[derive(Clone)]
pub(crate) struct OfficialClient {
    pub account_id: String,
    pub token: Secret,
}

/// Result of matching a presented bearer against the mixed-login account.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClientMatch {
    /// Not a ChatGPT token, or no mixed login configured.
    NotOfficial,
    /// The configured account.
    Accepted,
    /// A ChatGPT token of another account (the user signed in elsewhere).
    OtherAccount,
}

pub(crate) fn match_client(config: Option<&OfficialClientConfig>, token: &[u8]) -> ClientMatch {
    let (Some(config), Ok(token)) = (config, std::str::from_utf8(token)) else {
        return ClientMatch::NotOfficial;
    };
    match claims(token) {
        None => ClientMatch::NotOfficial,
        Some(c) if c.account_id == config.account_id => ClientMatch::Accepted,
        Some(_) => ClientMatch::OtherAccount,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Stored login of an official member (inside `secrets.json`).
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OfficialTokens {
    /// Access token (JWT).
    pub access_token: String,
    /// Single-use refresh token.
    pub refresh_token: String,
    /// Id token, kept as received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    /// Account id (from the access token when imported).
    pub account_id: String,
    /// The refresh token was refused: the user must sign in again. Never
    /// retried.
    #[serde(default)]
    pub expired: bool,
}

impl std::fmt::Debug for OfficialTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfficialTokens")
            .field("account_id", &self.account_id)
            .field("expired", &self.expired)
            .finish_non_exhaustive()
    }
}

impl OfficialTokens {
    /// Build from an access and a refresh token; the account comes from the
    /// access token.
    pub fn new(access_token: &str, refresh_token: &str, id_token: Option<&str>) -> Result<Self> {
        let c = claims(access_token)
            .ok_or_else(|| Error::invalid("the access token is not a ChatGPT token"))?;
        Ok(Self {
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.to_owned(),
            id_token: id_token.map(str::to_owned),
            account_id: c.account_id,
            expired: false,
        })
    }

    fn fresh_at(&self, now: u64) -> bool {
        claims(&self.access_token)
            .and_then(|c| c.exp)
            .is_some_and(|exp| exp > now + REFRESH_MARGIN.as_secs())
    }
}

/// Where a vault writes refreshed tokens (called under the vault lock).
pub(crate) type Persist = Arc<dyn Fn(&str, &OfficialTokens) -> Result<()> + Send + Sync>;

/// Persist into the router's `secrets.json` (read, replace this member's
/// login, write atomically with mode 0600).
pub(crate) fn persist_to(paths: crate::paths::RouterPaths) -> Persist {
    Arc::new(move |member: &str, t: &OfficialTokens| {
        let mut secrets = crate::config::RouterSecrets::load(&paths)?;
        secrets.official_tokens.insert(member.to_owned(), t.clone());
        secrets.save(&paths)
    })
}

/// Why no bearer is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// No stored login.
    Missing,
    /// The refresh token was refused (sign in again).
    Expired,
    /// The token endpoint could not be reached or answered 5xx (retry later).
    Unavailable(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Missing => f.write_str("no official ChatGPT login is stored"),
            VaultError::Expired => {
                f.write_str("the official ChatGPT login expired; sign in again in Superba")
            }
            VaultError::Unavailable(m) => write!(f, "cannot refresh the official login: {m}"),
        }
    }
}

/// One member's stored login with single-flight refresh. Kept across
/// configuration reloads (a reload must not resurrect a spent refresh token).
pub(crate) struct Vault {
    member: String,
    tokens: tokio::sync::Mutex<Option<OfficialTokens>>,
    token_url: Url,
    persist: Option<Persist>,
    refreshes: std::sync::atomic::AtomicU64,
    /// An access token the backend refused (401): the next caller refreshes.
    refused: std::sync::Mutex<Option<String>>,
}

impl Vault {
    pub fn new(
        member: &str,
        tokens: Option<OfficialTokens>,
        token_url: Url,
        persist: Option<Persist>,
    ) -> Self {
        Self {
            member: member.to_owned(),
            tokens: tokio::sync::Mutex::new(tokens),
            token_url,
            persist,
            refreshes: Default::default(),
            refused: std::sync::Mutex::new(None),
        }
    }

    /// The backend refused `token` (HTTP 401): refresh before its next use.
    pub fn note_refused(&self, token: &str) {
        if let Ok(mut r) = self.refused.lock() {
            *r = Some(token.to_owned());
        }
    }

    /// The stored login if no refresh holds the lock right now.
    pub fn try_current(&self) -> Option<OfficialTokens> {
        self.tokens.try_lock().ok().and_then(|g| g.clone())
    }

    /// Number of refreshes made (tests).
    #[cfg(test)]
    pub fn refreshes(&self) -> u64 {
        self.refreshes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A valid access token and its account, refreshing first when it is
    /// about to expire (or `force`, after the backend refused it). Concurrent
    /// callers wait for one refresh and share its result.
    pub async fn bearer(
        &self,
        http: &reqwest::Client,
        stale: Option<&str>,
    ) -> std::result::Result<(Secret, String), VaultError> {
        let mut guard = self.tokens.lock().await;
        let t = guard.as_ref().ok_or(VaultError::Missing)?;
        if t.expired {
            return Err(VaultError::Expired);
        }
        // `stale`: the token a request was refused with; if another caller
        // already replaced it, use the new one without refreshing again.
        let noted = self.refused.lock().ok().and_then(|mut r| r.take());
        let refused = stale
            .or(noted.as_deref())
            .is_some_and(|s| s == t.access_token);
        if !refused && t.fresh_at(now_secs()) {
            return Ok((Secret::new(t.access_token.as_str()), t.account_id.clone()));
        }
        let next = self.refresh(http, t).await;
        match next {
            Ok(next) => {
                if let Some(p) = &self.persist {
                    // Persist before anyone can use (and the process lose)
                    // the new refresh token.
                    p(&self.member, &next).map_err(|e| VaultError::Unavailable(e.to_string()))?;
                }
                let out = (
                    Secret::new(next.access_token.as_str()),
                    next.account_id.clone(),
                );
                *guard = Some(next);
                Ok(out)
            }
            Err(VaultError::Expired) => {
                let mut spent = t.clone();
                spent.expired = true;
                if let Some(p) = &self.persist {
                    let _ = p(&self.member, &spent);
                }
                *guard = Some(spent);
                Err(VaultError::Expired)
            }
            Err(e) => Err(e),
        }
    }

    async fn refresh(
        &self,
        http: &reqwest::Client,
        t: &OfficialTokens,
    ) -> std::result::Result<OfficialTokens, VaultError> {
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": t.refresh_token,
            "scope": "openid profile email",
        });
        let resp = http
            .post(self.token_url.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| VaultError::Unavailable(crate::forward::describe_error(&e)))?;
        let status = resp.status();
        if status.is_server_error() || status == http::StatusCode::TOO_MANY_REQUESTS {
            return Err(VaultError::Unavailable(format!("HTTP {}", status.as_u16())));
        }
        if !status.is_success() {
            // 400/401: invalid_grant, refresh_token_reused, revoked.
            return Err(VaultError::Expired);
        }
        #[derive(Deserialize)]
        struct Answer {
            access_token: Option<String>,
            refresh_token: Option<String>,
            id_token: Option<String>,
        }
        let a: Answer = resp
            .json()
            .await
            .map_err(|_| VaultError::Unavailable("invalid token answer".into()))?;
        let access = a
            .access_token
            .ok_or_else(|| VaultError::Unavailable("no access token".into()))?;
        let c =
            claims(&access).ok_or_else(|| VaultError::Unavailable("not a ChatGPT token".into()))?;
        if c.account_id != t.account_id {
            return Err(VaultError::Unavailable(
                "the refreshed login names another account".into(),
            ));
        }
        Ok(OfficialTokens {
            access_token: access,
            refresh_token: a.refresh_token.unwrap_or_else(|| t.refresh_token.clone()),
            id_token: a.id_token.or_else(|| t.id_token.clone()),
            account_id: t.account_id.clone(),
            expired: false,
        })
    }
}

/// Is a 429 from an official member a plan limit (5 h / weekly window)? The
/// backend's body names `usage_limit_reached`; any official 429 is treated as
/// one (the plan's own budget, not a transient overload).
pub(crate) fn plan_limit(status: http::StatusCode) -> bool {
    status == http::StatusCode::TOO_MANY_REQUESTS
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn jwt(account: &str, exp: u64) -> String {
        let e = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!(
            "{}.{}.sig",
            e(serde_json::json!({"alg":"none"})),
            e(
                serde_json::json!({"exp":exp,"https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_plan_type":"pro"}})
            )
        )
    }

    #[test]
    fn claims_read_account_and_expiry() {
        let c = claims(&jwt("acc-1", 42)).unwrap();
        assert_eq!(c.account_id, "acc-1");
        assert_eq!(c.exp, Some(42));
        assert_eq!(c.plan.as_deref(), Some("pro"));
        assert!(claims("sk-not-a-jwt").is_none());
        assert!(claims("a.b").is_none());
    }

    #[test]
    fn client_match_names_the_account() {
        let cfg = OfficialClientConfig {
            account_id: "acc-1".into(),
        };
        let t = jwt("acc-1", 1);
        assert_eq!(
            match_client(Some(&cfg), t.as_bytes()),
            ClientMatch::Accepted
        );
        let o = jwt("acc-2", 1);
        assert_eq!(
            match_client(Some(&cfg), o.as_bytes()),
            ClientMatch::OtherAccount
        );
        assert_eq!(match_client(None, t.as_bytes()), ClientMatch::NotOfficial);
        assert_eq!(match_client(Some(&cfg), b"sbr_x"), ClientMatch::NotOfficial);
    }

    type Answer = Arc<dyn Fn(&str) -> (u16, String) + Send + Sync>;

    /// A fake OAuth token endpoint: `answer` decides each reply; counts hits
    /// and records the refresh tokens presented.
    async fn token_server(answer: Answer) -> (Url, Arc<std::sync::Mutex<Vec<String>>>) {
        use http_body_util::{BodyExt, Full};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let s2 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (seen, answer) = (s2.clone(), answer.clone());
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(
                        move |req: http::Request<hyper::body::Incoming>| {
                            let (seen, answer) = (seen.clone(), answer.clone());
                            async move {
                                let body = req.into_body().collect().await.unwrap().to_bytes();
                                let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                                let rt = v["refresh_token"].as_str().unwrap_or_default().to_owned();
                                assert_eq!(v["grant_type"], "refresh_token");
                                assert_eq!(v["client_id"], CODEX_CLIENT_ID);
                                seen.lock().unwrap().push(rt.clone());
                                // Slow enough for every caller to queue on the lock.
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                let (status, body) = answer(&rt);
                                let mut r =
                                    http::Response::new(Full::new(bytes::Bytes::from(body)));
                                *r.status_mut() = http::StatusCode::from_u16(status).unwrap();
                                Ok::<_, std::convert::Infallible>(r)
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), svc)
                        .await;
                });
            }
        });
        (
            Url::parse(&format!("http://{addr}/oauth/token")).unwrap(),
            seen,
        )
    }

    fn rotating() -> Answer {
        Arc::new(|rt: &str| {
            let n: u32 = rt.trim_start_matches("rt-").parse().unwrap_or(0);
            (
                200,
                serde_json::json!({
                    "access_token": jwt("acc-1", now_secs() + 3600 + u64::from(n) + 1),
                    "refresh_token": format!("rt-{}", n + 1),
                })
                .to_string(),
            )
        })
    }

    fn stale_login() -> OfficialTokens {
        OfficialTokens::new(&jwt("acc-1", now_secs().saturating_sub(10)), "rt-0", None).unwrap()
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh_and_it_is_persisted_first() {
        let (url, seen) = token_server(rotating()).await;
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::RouterPaths::from_dir(dir.path());
        let mut secrets = crate::config::RouterSecrets::generate(Default::default());
        secrets
            .official_tokens
            .insert("official".into(), stale_login());
        secrets.save(&paths).unwrap();
        let vault = Arc::new(Vault::new(
            "official",
            Some(stale_login()),
            url,
            Some(persist_to(paths.clone())),
        ));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut jobs = Vec::new();
        for _ in 0..16 {
            let (v, h) = (vault.clone(), http.clone());
            jobs.push(tokio::spawn(async move { v.bearer(&h, None).await }));
        }
        let mut tokens = std::collections::BTreeSet::new();
        for j in jobs {
            let (t, account) = j.await.unwrap().unwrap();
            assert_eq!(account, "acc-1");
            tokens.insert(t.expose().to_owned());
        }
        assert_eq!(tokens.len(), 1, "every caller got the one refreshed token");
        assert_eq!(*seen.lock().unwrap(), vec!["rt-0".to_owned()]);
        assert_eq!(vault.refreshes(), 1);
        let on_disk = crate::config::RouterSecrets::load(&paths).unwrap();
        assert_eq!(on_disk.official_tokens["official"].refresh_token, "rt-1");
        // A fresh token is served without another refresh.
        vault.bearer(&http, None).await.unwrap();
        assert_eq!(vault.refreshes(), 1);
    }

    #[tokio::test]
    async fn a_refused_token_refreshes_once_for_all_who_saw_it() {
        let (url, seen) = token_server(rotating()).await;
        let fresh = OfficialTokens::new(&jwt("acc-1", now_secs() + 3600), "rt-0", None).unwrap();
        let refused = fresh.access_token.clone();
        let vault = Arc::new(Vault::new("official", Some(fresh), url, None));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let (a, b) = tokio::join!(
            vault.bearer(&http, Some(&refused)),
            vault.bearer(&http, Some(&refused))
        );
        assert_eq!(a.unwrap().0.expose(), b.unwrap().0.expose());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_spent_refresh_token_is_never_retried() {
        let (url, seen) = token_server(Arc::new(|_: &str| {
            (
                400,
                r#"{"error":"invalid_grant","error_description":"refresh_token_reused"}"#.into(),
            )
        }))
        .await;
        let persisted: Arc<std::sync::Mutex<Vec<bool>>> = Arc::default();
        let p2 = persisted.clone();
        let persist: Persist = Arc::new(move |_: &str, t: &OfficialTokens| {
            p2.lock().unwrap().push(t.expired);
            Ok(())
        });
        let vault = Vault::new("official", Some(stale_login()), url, Some(persist));
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            vault.bearer(&http, None).await.unwrap_err(),
            VaultError::Expired
        );
        assert_eq!(
            vault.bearer(&http, None).await.unwrap_err(),
            VaultError::Expired
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(*persisted.lock().unwrap(), vec![true]);
    }

    #[tokio::test]
    async fn an_unreachable_token_endpoint_keeps_the_login() {
        let vault = Vault::new(
            "official",
            Some(stale_login()),
            Url::parse("http://127.0.0.1:9/oauth/token").unwrap(),
            None,
        );
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(matches!(
            vault.bearer(&http, None).await,
            Err(VaultError::Unavailable(_))
        ));
        assert!(!vault.try_current().unwrap().expired);
    }

    #[test]
    fn debug_hides_tokens() {
        let t = OfficialTokens::new(&jwt("a", 1), "rt-secret", None).unwrap();
        assert!(!format!("{t:?}").contains("rt-secret"));
    }
}

/// Credentials of one attempt at an official member, resolved before it is
/// sent (request extension read by the proxy's `send`).
#[derive(Clone)]
pub(crate) struct ResolvedAuth {
    pub auth: http::HeaderValue,
    pub account_id: String,
    /// The token, when it came from the member's stored login.
    pub stored: Option<Secret>,
}

/// The bearer for an attempt at official member `m`: the mixed-login
/// client's own token when it is that account's (Codex owns its refresh),
/// otherwise the member's stored login. `Err` is a failover reason.
pub(crate) async fn resolve(
    m: &crate::state::MemberRt,
    client: Option<&OfficialClient>,
    http: &reqwest::Client,
) -> std::result::Result<ResolvedAuth, String> {
    let stored_account = m
        .vault
        .as_ref()
        .and_then(|v| v.try_current())
        .map(|t| t.account_id);
    let (token, account_id, stored) = match client {
        Some(c) if stored_account.as_deref().is_none_or(|a| a == c.account_id) => {
            (c.token.clone(), c.account_id.clone(), false)
        }
        _ => match &m.vault {
            Some(v) => {
                let (t, a) = v.bearer(http, None).await.map_err(|e| e.to_string())?;
                (t, a, true)
            }
            None => {
                return Err(
                    "the official ChatGPT member serves only Codex signed in with that account (mixed login)"
                        .into(),
                );
            }
        },
    };
    let mut auth = http::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| "invalid official token".to_owned())?;
    auth.set_sensitive(true);
    Ok(ResolvedAuth {
        auth,
        account_id,
        stored: stored.then_some(token),
    })
}
