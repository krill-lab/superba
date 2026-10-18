//! Line origins and the Codex base-URL paths derived from them.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::error::{Error, Result};

/// An https origin (`scheme://host[:port]`, no path, query, fragment or userinfo).
///
/// `http` is accepted only for loopback hosts (`127.0.0.1`, `::1`, `localhost`) so
/// e2e line forwarders and a future localhost shell forwarder can be expressed;
/// real lines must be https (Codex requires https for `chatgpt_base_url`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Origin(Url);

impl Origin {
    /// Parse and validate. A single trailing `/` is tolerated.
    pub fn parse(s: &str) -> Result<Self> {
        let url = Url::parse(s.trim()).map_err(|e| Error::invalid(format!("origin {s:?}: {e}")))?;
        if url.host().is_none() {
            return Err(Error::invalid(format!("origin {s:?}: no host")));
        }
        let loopback = is_loopback(&url);
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            other => {
                return Err(Error::invalid(format!(
                    "origin {s:?}: scheme {other} not allowed (https required)"
                )));
            }
        }
        if url.path() != "/" && !url.path().is_empty() {
            return Err(Error::invalid(format!(
                "origin {s:?}: must not contain a path"
            )));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Error::invalid(format!(
                "origin {s:?}: must not contain query or fragment"
            )));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::invalid(format!(
                "origin {s:?}: must not contain credentials"
            )));
        }
        Ok(Origin(url))
    }

    /// `http://<address>:<port>` of Superba's local router as a WSL 2
    /// distribution behind NAT reaches it: the Windows host's address on the
    /// WSL adapter (user bug 45). Plain http is allowed only here, for a private
    /// IPv4 address the router listens on for WSL (never parsed from user input);
    /// the request does not leave the computer.
    pub fn local_router_for_wsl(address: std::net::Ipv4Addr, port: u16) -> Result<Self> {
        if !address.is_private() {
            return Err(Error::invalid(format!(
                "{address} is not a private WSL adapter address"
            )));
        }
        let url = Url::parse(&format!("http://{address}:{port}"))
            .map_err(|e| Error::invalid(format!("router address {address}:{port}: {e}")))?;
        Ok(Origin(url))
    }

    /// `https://host[:port]` without a trailing slash. This exact string is the value
    /// of `CODEX_AUTHAPI_BASE_URL` for PAT mode.
    pub fn as_str(&self) -> String {
        self.0.origin().ascii_serialization()
    }

    /// The underlying URL (path `/`).
    pub fn url(&self) -> &Url {
        &self.0
    }

    /// Host name.
    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    /// True for loopback hosts (tests / local forwarders).
    pub fn is_loopback(&self) -> bool {
        is_loopback(&self.0)
    }

    /// True for `https` origins. Codex refuses a plain-http `chatgpt_base_url`
    /// ("workspace backend must use an HTTPS origin"), so PAT needs one.
    pub fn is_https(&self) -> bool {
        self.0.scheme() == "https"
    }

    /// Append an absolute path (`/backend-api/codex`) to the origin.
    pub fn join(&self, path: &str) -> Url {
        let mut s = self.as_str();
        if !path.starts_with('/') {
            s.push('/');
        }
        s.push_str(path);
        // An origin plus an absolute path is always a valid URL.
        Url::parse(&s).expect("origin + absolute path is a valid URL")
    }
}

/// Loopback by the parsed host: IPv4 127.0.0.0/8, IPv6 ::1, or the name `localhost`.
/// A DNS name that merely starts with `127.` is not loopback.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

impl fmt::Debug for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Origin({})", self.as_str())
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl FromStr for Origin {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Origin::parse(s)
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Origin::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Which relay path the Codex Responses traffic uses on a line
/// (`openai_base_url = <origin><suffix>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexPath {
    /// `/backend-api/codex` (Krill "OpenAI official" channel; default).
    BackendApiCodex,
    /// `/codex/v1` (fallback when the edge blocks `/backend-api/codex`; API-key only).
    CodexV1,
    /// The profile's base URL is used verbatim (OpenAI-compatible sites).
    AsIs,
}

impl CodexPath {
    /// Path suffix appended to the origin (empty for [`CodexPath::AsIs`]).
    pub fn suffix(self) -> &'static str {
        match self {
            CodexPath::BackendApiCodex => "/backend-api/codex",
            CodexPath::CodexV1 => "/codex/v1",
            CodexPath::AsIs => "",
        }
    }

    /// `openai_base_url` for `origin`.
    pub fn base_url(self, origin: &Origin) -> Url {
        match self {
            CodexPath::AsIs => origin.url().clone(),
            p => origin.join(p.suffix()),
        }
    }
}

/// `chatgpt_base_url` for PAT mode: `<origin>/backend-api/` (trailing slash matters:
/// Codex appends `wham/accounts/check`).
pub fn chatgpt_base_url(origin: &Origin) -> Url {
    origin.join("/backend-api/")
}
