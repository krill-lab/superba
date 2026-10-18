//! Test helpers: a loopback HTTP/1.1 mock server that records requests and
//! answers with canned responses, and an in-memory key store.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use superba_core::{
    Error, KeyRef, ProfileId, Result, Secret, SecretBackend, SecretStore, async_trait,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One recorded request.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    /// Path and query.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("JSON body")
    }
}

/// A mock server on 127.0.0.1.
pub struct MockServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    /// Answer every request with `status` and `body`.
    pub async fn start(status: u16, body: &str) -> Self {
        let body = body.to_owned();
        Self::start_with(move |_| (status, body.clone())).await
    }

    /// Answer with what `respond` returns for each request.
    pub async fn start_with(
        respond: impl Fn(&Recorded) -> (u16, String) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let recorded = requests.clone();
        let respond = Arc::new(respond);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let recorded = recorded.clone();
                let respond = respond.clone();
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut sock).await else {
                        return;
                    };
                    let (status, body) = respond(&req);
                    recorded.lock().unwrap().push(req);
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self { base, requests }
    }

    pub fn taken(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let start = lines.next()?;
    let mut parts = start.split(' ');
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(Recorded {
        method,
        target,
        headers,
        body,
    })
}

/// Key store in memory.
#[derive(Default)]
pub struct MemSecrets {
    pub entries: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl SecretStore for MemSecrets {
    async fn get(&self, key_ref: &KeyRef) -> Result<Secret> {
        let KeyRef::Keyring { account, .. } = key_ref else {
            return Err(Error::other("unexpected key ref"));
        };
        self.entries
            .lock()
            .unwrap()
            .get(account)
            .map(|v| Secret::new(v.clone()))
            .ok_or_else(|| Error::NotFound {
                what: account.clone(),
            })
    }

    async fn put(&self, profile_id: &ProfileId, secret: &Secret) -> Result<KeyRef> {
        let account = format!("profile:{profile_id}");
        self.entries
            .lock()
            .unwrap()
            .insert(account.clone(), secret.expose().to_owned());
        Ok(KeyRef::Keyring {
            service: "superba".into(),
            account,
        })
    }

    async fn delete(&self, key_ref: &KeyRef) -> Result<()> {
        if let KeyRef::Keyring { account, .. } = key_ref {
            self.entries.lock().unwrap().remove(account);
        }
        Ok(())
    }

    fn backend(&self) -> SecretBackend {
        SecretBackend::OsKeyring
    }
}
