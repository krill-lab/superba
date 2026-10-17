//! Client of a running router's admin API, for the GUI, `spb` and the watchdog.

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use serde::de::DeserializeOwned;
use superba_core::{ClearReport, ClearRequest, Error, InflightRequest, RequestRow, Result, Secret};

use crate::admin::ADMIN_PREFIX;
use crate::paths::RouterPaths;
use crate::runtime::RuntimeInfo;
use crate::status::{HealthReply, RouterStatus, SERVICE_NAME};

/// Default timeout of an admin call.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Talks to the router on `127.0.0.1:<port>` with its per-instance admin token.
#[derive(Clone)]
pub struct AdminClient {
    port: u16,
    admin_token: Secret,
    instance_id: Option<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for AdminClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminClient")
            .field("port", &self.port)
            .field("instance_id", &self.instance_id)
            .finish_non_exhaustive()
    }
}

impl AdminClient {
    /// A client for the router on `port` with `admin_token`.
    pub fn new(port: u16, admin_token: Secret) -> Result<Self> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::other(format!("admin client: {e}")))?;
        Ok(Self {
            port,
            admin_token,
            instance_id: None,
            http,
        })
    }

    /// A client for the router described by `runtime.json`.
    pub fn from_runtime(paths: &RouterPaths) -> Result<Self> {
        let info = RuntimeInfo::load(paths)?;
        let mut c = Self::new(info.port, info.admin_token)?;
        c.instance_id = Some(info.instance_id);
        Ok(c)
    }

    /// Port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Instance id expected from `runtime.json`, if loaded from it.
    pub fn expected_instance(&self) -> Option<&str> {
        self.instance_id.as_deref()
    }

    fn url(&self, endpoint: &str) -> String {
        format!("http://127.0.0.1:{}{ADMIN_PREFIX}{endpoint}", self.port)
    }

    /// Unauthenticated liveness; checks that a Superba router (and, when known,
    /// the expected instance) answers on the port.
    pub async fn health(&self) -> Result<HealthReply> {
        let url = self.url("health");
        let reply: HealthReply = self.call(self.http.get(&url), &url).await?;
        if reply.service != SERVICE_NAME {
            return Err(Error::other(format!(
                "port {} is not served by superba-router",
                self.port
            )));
        }
        if let Some(expected) = &self.instance_id
            && expected != &reply.instance_id
        {
            return Err(Error::other(format!(
                "port {} is served by another router instance",
                self.port
            )));
        }
        Ok(reply)
    }

    /// Full status.
    pub async fn status(&self) -> Result<RouterStatus> {
        let url = self.url("status");
        self.call(self.authed(self.http.get(&url)), &url).await
    }

    /// Probe every line now and return the status afterwards.
    pub async fn probe(&self) -> Result<RouterStatus> {
        let url = self.url("probe");
        let rb = self.http.post(&url).timeout(Duration::from_secs(15));
        self.call(self.authed(rb), &url).await
    }

    /// Re-read the configuration and secrets; returns the new generation.
    pub async fn reload(&self) -> Result<u64> {
        let url = self.url("reload");
        let v: serde_json::Value = self.call(self.authed(self.http.post(&url)), &url).await?;
        v.get("generation")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| Error::parse("router reload answer", "no generation"))
    }

    /// Recent request rows of the live ring with an id above `after_id`, oldest
    /// first (at most `limit`, the newest ones).
    pub async fn requests(
        &self,
        after_id: Option<i64>,
        limit: Option<usize>,
    ) -> Result<Vec<RequestRow>> {
        let mut query = Vec::new();
        if let Some(a) = after_id {
            query.push(format!("after_id={a}"));
        }
        if let Some(l) = limit {
            query.push(format!("limit={l}"));
        }
        let mut url = self.url("requests");
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        self.call(self.authed(self.http.get(&url)), &url).await
    }

    /// Requests in flight.
    pub async fn inflight(&self) -> Result<Vec<InflightRequest>> {
        let url = self.url("requests/active");
        self.call(self.authed(self.http.get(&url)), &url).await
    }

    /// Delete recorded rows through the router's writer.
    pub async fn clear(&self, request: &ClearRequest) -> Result<ClearReport> {
        let url = self.url("requests/clear");
        let body =
            serde_json::to_vec(request).map_err(|e| Error::other(format!("clear request: {e}")))?;
        let rb = self
            .http
            .post(&url)
            .timeout(Duration::from_secs(35))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        self.call(self.authed(rb), &url).await
    }

    /// Ask the router to stop.
    pub async fn shutdown(&self) -> Result<()> {
        let url = self.url("shutdown");
        let _: serde_json::Value = self.call(self.authed(self.http.post(&url)), &url).await?;
        Ok(())
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut v = reqwest::header::HeaderValue::from_str(&format!(
            "Bearer {}",
            self.admin_token.expose()
        ))
        .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static(""));
        v.set_sensitive(true);
        rb.header(AUTHORIZATION, v)
    }

    async fn call<T: DeserializeOwned>(&self, rb: reqwest::RequestBuilder, url: &str) -> Result<T> {
        let resp = rb.send().await.map_err(|e| Error::Network {
            url: url.to_owned(),
            message: e.without_url().to_string(),
        })?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| Error::Network {
            url: url.to_owned(),
            message: e.without_url().to_string(),
        })?;
        if !status.is_success() {
            let message = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(|m| m.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            return Err(Error::Http {
                url: url.to_owned(),
                status: status.as_u16(),
                message,
            });
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::parse("router admin answer", e))
    }
}
