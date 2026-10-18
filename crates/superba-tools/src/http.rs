//! HTTP: small metadata requests and resumable downloads with progress,
//! cancel and verification. URLs never carry secrets; errors name the URL.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sha2::{Digest, Sha256, Sha512};
use superba_core::{Error, Result, ToolId, ToolProgress, ToolProgressSink, ToolStage};

/// Largest metadata response read into memory.
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// Largest download accepted (WorkBuddy AI's installer is ~500 MB).
pub const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Deadline of one metadata request.
pub const METADATA_TIMEOUT: Duration = Duration::from_secs(20);

const REPORT_EVERY: Duration = Duration::from_millis(150);
const CANCEL_POLL: Duration = Duration::from_millis(100);
const ATTEMPTS: usize = 3;

/// The client: rustls, the system proxy, a connect and a read timeout (a
/// download has no total deadline).
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("superba/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(90))
        .build()
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

/// GET `url` (at most [`MAX_TEXT_BYTES`]) within [`METADATA_TIMEOUT`];
/// `None` for 404 / 204.
pub async fn get_optional(client: &reqwest::Client, url: &str) -> Result<Option<Vec<u8>>> {
    let work = async {
        let mut resp = client.get(url).send().await.map_err(|e| net_err(url, &e))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(status_err(url, status));
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
        Ok(Some(out))
    };
    tokio::time::timeout(METADATA_TIMEOUT, work)
        .await
        .map_err(|_| Error::Timeout {
            what: format!("GET {url}"),
        })?
}

/// GET `url` as bytes; 404 is an error.
pub async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    get_optional(client, url).await?.ok_or_else(|| Error::Http {
        url: url.to_owned(),
        status: 404,
        message: "Not Found".into(),
    })
}

/// GET `url` as UTF-8 text.
pub async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    String::from_utf8(get_bytes(client, url).await?).map_err(|_| Error::parse(url, "not UTF-8"))
}

/// GET `url` as JSON.
pub async fn get_json(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    let bytes = get_bytes(client, url).await?;
    serde_json::from_slice(&bytes).map_err(|e| Error::parse(url, e.to_string()))
}

/// What a downloaded file must hash to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// sha256, hex.
    Sha256(String),
    /// sha512, standard base64 (electron-updater manifests; npm `integrity`
    /// without its `sha512-` prefix).
    Sha512(String),
}

impl Checksum {
    /// An npm `dist.integrity` (`sha512-<base64>`).
    pub fn from_integrity(integrity: &str) -> Option<Self> {
        integrity
            .split_whitespace()
            .find_map(|i| i.strip_prefix("sha512-"))
            .map(|b| Checksum::Sha512(b.to_owned()))
    }

    /// `sha256` / `sha512`.
    pub fn algorithm(&self) -> &'static str {
        match self {
            Checksum::Sha256(_) => "sha256",
            Checksum::Sha512(_) => "sha512",
        }
    }

    /// The expected value as published.
    pub fn value(&self) -> &str {
        match self {
            Checksum::Sha256(v) | Checksum::Sha512(v) => v,
        }
    }
}

/// sha256 (hex) and sha512 (base64) of a file.
pub fn digests(path: &Path) -> Result<(String, String)> {
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
    Ok((
        hex::encode(s256.finalize()),
        base64::engine::general_purpose::STANDARD.encode(s512.finalize()),
    ))
}

/// Check `path` against `want` and `size`; returns its sha256 (hex).
pub fn verify_file(path: &Path, want: Option<&Checksum>, size: Option<u64>) -> Result<String> {
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
        None => true,
        Some(Checksum::Sha256(hex)) => sha256.eq_ignore_ascii_case(hex.trim()),
        Some(Checksum::Sha512(b64)) => sha512 == b64.trim(),
    };
    if !ok {
        let want = want.map_or("", Checksum::algorithm);
        return Err(Error::Invalid {
            message: format!(
                "{} does not match its published {want} checksum: corrupted or tampered download",
                path.display()
            ),
        });
    }
    Ok(sha256)
}

/// `<dest>.part`.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// One file to download.
#[derive(Clone, Debug)]
pub struct Download<'a> {
    /// The tool (progress label).
    pub tool: ToolId,
    /// URL.
    pub url: &'a str,
    /// Final file.
    pub dest: &'a Path,
    /// What it must hash to; `None` only when a signature is checked next.
    pub checksum: Option<&'a Checksum>,
    /// Size as published.
    pub size: Option<u64>,
}

/// Download into `dest` through `<dest>.part` (resumed with `Range` when a
/// partial file is there; a broken transfer is retried), then verify. A file
/// that fails verification is deleted; a cancelled one is kept for the next
/// attempt. Returns the sha256 (hex).
pub async fn download(
    client: &reqwest::Client,
    d: &Download<'_>,
    sink: &dyn ToolProgressSink,
) -> Result<String> {
    if let Some(dir) = d.dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    }
    let part = part_path(d.dest);
    let name = d
        .dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut last_err = None;
    for _ in 0..ATTEMPTS {
        if sink.cancel_requested() {
            return Err(Error::Cancelled);
        }
        match transfer(client, d, &part, &name, sink).await {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e @ (Error::Cancelled | Error::Http { .. } | Error::Invalid { .. })) => {
                return Err(e);
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    sink.report(ToolProgress {
        detail: Some(name),
        ..ToolProgress::stage(d.tool, ToolStage::Verifying)
    });
    let (part2, checksum, size) = (part.clone(), d.checksum.cloned(), d.size);
    let verified =
        tokio::task::spawn_blocking(move || verify_file(&part2, checksum.as_ref(), size))
            .await
            .map_err(|e| Error::other(format!("verify task: {e}")))?;
    match verified {
        Ok(sha) => {
            replace(&part, d.dest)?;
            Ok(sha)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

fn replace(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        let _ = std::fs::remove_file(to);
    }
    std::fs::rename(from, to).map_err(|e| Error::io(to, &e))
}

async fn transfer(
    client: &reqwest::Client,
    d: &Download<'_>,
    part: &Path,
    name: &str,
    sink: &dyn ToolProgressSink,
) -> Result<()> {
    let url = d.url;
    let mut have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if d.size.is_some_and(|s| have > s) {
        let _ = std::fs::remove_file(part);
        have = 0;
    }
    if d.size.is_some_and(|s| have == s && s > 0) {
        return Ok(());
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
        416 if have > 0 => return Ok(()),
        _ => return Err(status_err(url, status)),
    };
    let total = d.size.or_else(|| {
        resp.content_length()
            .map(|n| if append { n + have } else { n })
    });
    if total.is_some_and(|t| t > MAX_DOWNLOAD_BYTES) {
        return Err(Error::Invalid {
            message: format!("{url} is larger than {MAX_DOWNLOAD_BYTES} bytes"),
        });
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(part)
        .map_err(|e| Error::io(part, &e))?;
    let mut done = if append { have } else { 0 };
    let mut last = Instant::now()
        .checked_sub(REPORT_EVERY)
        .unwrap_or_else(Instant::now);
    let report = |done: u64| {
        sink.report(ToolProgress {
            done,
            total,
            detail: Some(name.to_owned()),
            ..ToolProgress::stage(d.tool, ToolStage::Downloading)
        });
    };
    report(done);
    loop {
        let chunk = loop {
            if sink.cancel_requested() {
                file.flush().map_err(|e| Error::io(part, &e))?;
                return Err(Error::Cancelled);
            }
            match tokio::time::timeout(CANCEL_POLL, resp.chunk()).await {
                Ok(r) => break r.map_err(|e| net_err(url, &e))?,
                Err(_) => continue,
            }
        };
        let Some(chunk) = chunk else { break };
        done += chunk.len() as u64;
        if done > MAX_DOWNLOAD_BYTES || d.size.is_some_and(|s| done > s) {
            drop(file);
            let _ = std::fs::remove_file(part);
            return Err(Error::Invalid {
                message: format!("{url} sent more data than published; download discarded"),
            });
        }
        file.write_all(&chunk).map_err(|e| Error::io(part, &e))?;
        if last.elapsed() >= REPORT_EVERY {
            last = Instant::now();
            report(done);
        }
    }
    file.flush().map_err(|e| Error::io(part, &e))?;
    report(done);
    Ok(())
}
