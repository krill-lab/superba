//! HTTP plumbing: small text/JSON fetches, redirect targets and resumable,
//! verified downloads. URLs never carry secrets; errors name the URL.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64_lite::encode as b64;
use sha2::{Digest, Sha256, Sha512};
use superba_core::{Error, Result};

/// Largest text/JSON/XML response read into memory (the npm dist-tags document
/// is ~4 KB, an RPM `primary.xml.gz` a few KB, the bundled models feed < 1 MB).
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// The client: rustls, system proxy, short connect timeout, a read timeout
/// (large downloads have no total deadline).
pub(crate) fn client(redirects: bool) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(concat!("superba/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60));
    if !redirects {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    b.build()
        .map_err(|e| Error::other(format!("HTTP client: {e}")))
}

fn net_err(url: &str, e: &reqwest::Error) -> Error {
    if e.is_timeout() {
        return Error::Timeout {
            what: format!("GET {url}"),
        };
    }
    Error::Network {
        url: url.to_owned(),
        message: e.to_string(),
    }
}

fn status_err(url: &str, status: reqwest::StatusCode) -> Error {
    Error::Http {
        url: url.to_owned(),
        status: status.as_u16(),
        message: status.canonical_reason().unwrap_or("").to_owned(),
    }
}

/// GET `url` and return the body (at most [`MAX_TEXT_BYTES`]), with an overall
/// `timeout`.
pub(crate) async fn get_bytes(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let work = async {
        let mut resp = client.get(url).send().await.map_err(|e| net_err(url, &e))?;
        if !resp.status().is_success() {
            return Err(status_err(url, resp.status()));
        }
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| net_err(url, &e))? {
            if out.len() + chunk.len() > MAX_TEXT_BYTES {
                return Err(Error::Http {
                    url: url.to_owned(),
                    status: 200,
                    message: format!("response larger than {MAX_TEXT_BYTES} bytes"),
                });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| Error::Timeout {
            what: format!("GET {url}"),
        })?
}

/// [`get_bytes`] as UTF-8 text.
pub(crate) async fn get_text(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<String> {
    String::from_utf8(get_bytes(client, url, timeout).await?)
        .map_err(|_| Error::parse(url, "not UTF-8"))
}

/// `Content-Length` of a `HEAD` answer, best effort.
pub(crate) async fn content_length(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Option<u64> {
    let resp = tokio::time::timeout(timeout, client.head(url).send())
        .await
        .ok()?
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

/// The `Location` of a redirect answer (client built without redirects).
pub(crate) async fn redirect_target(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<String> {
    let resp = tokio::time::timeout(timeout, client.get(url).send())
        .await
        .map_err(|_| Error::Timeout {
            what: format!("GET {url}"),
        })?
        .map_err(|e| net_err(url, &e))?;
    if !resp.status().is_redirection() {
        return Err(status_err(url, resp.status()));
    }
    resp.headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| Error::parse(url, "redirect without a Location"))
}

/// What a downloaded file must hash to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// sha256, lowercase hex.
    Sha256(String),
    /// npm `dist.integrity` (`sha512-<base64>`).
    Sha512Integrity(String),
}

impl Checksum {
    /// Short description for errors.
    pub fn describe(&self) -> &'static str {
        match self {
            Checksum::Sha256(_) => "sha256",
            Checksum::Sha512Integrity(_) => "sha512",
        }
    }
}

/// sha256 (hex) and sha512 (base64) of a file, streamed.
pub(crate) fn digests(path: &Path) -> Result<(String, String)> {
    let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, &e))?;
    let mut s256 = Sha256::new();
    let mut s512 = Sha512::new();
    let mut buf = vec![0_u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, &e))?;
        if n == 0 {
            break;
        }
        s256.update(&buf[..n]);
        s512.update(&buf[..n]);
    }
    Ok((hex::encode(s256.finalize()), b64(&s512.finalize())))
}

/// Check a file against `want` (and `size` when known); returns its sha256.
pub(crate) fn verify_file(path: &Path, want: &Checksum, size: Option<u64>) -> Result<String> {
    let len = std::fs::metadata(path)
        .map_err(|e| Error::io(path, &e))?
        .len();
    if let Some(s) = size
        && s != len
    {
        return Err(Error::Invalid {
            message: format!(
                "{} has {len} bytes, {s} expected: download incomplete or corrupted",
                path.display()
            ),
        });
    }
    let (sha256, sha512) = digests(path)?;
    let ok = match want {
        Checksum::Sha256(hex) => sha256.eq_ignore_ascii_case(hex.trim()),
        Checksum::Sha512Integrity(integrity) => integrity
            .split_whitespace()
            .filter_map(|i| i.strip_prefix("sha512-"))
            .any(|b| b == sha512),
    };
    if !ok {
        return Err(Error::Invalid {
            message: format!(
                "{} does not match its published {} checksum: corrupted or tampered download",
                path.display(),
                want.describe()
            ),
        });
    }
    Ok(sha256)
}

/// Download `url` into `dest` (via `<dest>.part`, resumed with a Range request
/// when a partial file exists), then verify it; a file that fails verification
/// is deleted. `max` bounds the size.
pub(crate) async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    want: &Checksum,
    size: Option<u64>,
    max: u64,
) -> Result<String> {
    let part = part_path(dest);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    }
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if size.is_some_and(|s| have > s) {
        let _ = std::fs::remove_file(&part);
        have = 0;
    }
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = req.send().await.map_err(|e| net_err(url, &e))?;
    let status = resp.status();
    let append = match status.as_u16() {
        206 if have > 0 => true,
        200 => false,
        // The server cannot resume (or the partial file is already complete).
        416 if have > 0 => {
            drop(resp);
            return finish(&part, dest, want, size);
        }
        _ => return Err(status_err(url, status)),
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .map_err(|e| Error::io(&part, &e))?;
    let mut written = if append { have } else { 0 };
    while let Some(chunk) = resp.chunk().await.map_err(|e| net_err(url, &e))? {
        written += chunk.len() as u64;
        if written > max || size.is_some_and(|s| written > s) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(Error::Invalid {
                message: format!("{url} sent more data than expected; download discarded"),
            });
        }
        file.write_all(&chunk).map_err(|e| Error::io(&part, &e))?;
    }
    file.flush().map_err(|e| Error::io(&part, &e))?;
    drop(file);
    finish(&part, dest, want, size)
}

fn finish(part: &Path, dest: &Path, want: &Checksum, size: Option<u64>) -> Result<String> {
    match verify_file(part, want, size) {
        Ok(sha) => {
            std::fs::rename(part, dest).map_err(|e| Error::io(dest, &e))?;
            Ok(sha)
        }
        Err(e) => {
            let _ = std::fs::remove_file(part);
            Err(e)
        }
    }
}

/// `<dest>.part`.
pub(crate) fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Minimal standard base64 (no dependency for one digest).
mod base64_lite {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Encode with padding.
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}

/// Base64 of `bytes` (tests and npm integrity strings).
pub fn base64(bytes: &[u8]) -> String {
    b64(bytes)
}
