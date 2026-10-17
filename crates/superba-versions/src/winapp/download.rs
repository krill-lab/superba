//! Resumable package downloads with progress and cancel.
//!
//! A transfer writes `<dest>.part` and keeps `<dest>.part.json` (the
//! response's validator and size), so a cancelled or broken transfer resumes
//! with `Range` + `If-Range`: a server whose file changed answers with the
//! whole new file instead. The finished file is checked (size, listed SHA-1)
//! before it is renamed into place; a file that fails is deleted. A verified
//! file gets `<dest>.verified.json` (size, mtime, sha256, sha1) so a second
//! request does not download or hash it again.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use superba_core::{
    AppPackageSource, DownloadProgress, DownloadProgressSink, DownloadStage, Error, Result,
};

use super::mirror::ListedSize;

/// How often progress is reported at most.
const REPORT_EVERY: Duration = Duration::from_millis(150);

/// How often a waiting transfer looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Transfers tried for one file (a broken connection resumes).
const ATTEMPTS: usize = 3;

/// What and where to download.
#[derive(Clone, Debug)]
pub struct Target {
    /// URL.
    pub url: String,
    /// Final file.
    pub dest: PathBuf,
    /// SHA-1 the file must have (lowercase hex).
    pub sha1: Option<String>,
    /// Size as listed (approximate).
    pub listed_size: Option<ListedSize>,
    /// Upper bound.
    pub max: u64,
    /// Progress label: source.
    pub source: AppPackageSource,
    /// Progress label: 1-based file number.
    pub index: usize,
    /// Progress label: number of files.
    pub count: usize,
}

/// A verified file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetched {
    /// Size.
    pub size: u64,
    /// sha256 (hex).
    pub sha256: String,
    /// sha1 (hex).
    pub sha1: String,
    /// Modification time of the file (ms since the epoch) when verified.
    #[serde(default)]
    pub mtime_ms: u128,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PartMeta {
    /// Strong `ETag`, else `Last-Modified`.
    validator: Option<String>,
    total: Option<u64>,
    sha1: Option<String>,
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `<dest>.part`.
pub fn part_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".part")
}

fn meta_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".part.json")
}

fn verified_path(dest: &Path) -> PathBuf {
    with_suffix(dest, ".verified.json")
}

fn mtime_ms(path: &Path) -> u128 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis())
}

/// The record of an earlier verification of `dest`, if the file is unchanged.
pub fn already_verified(dest: &Path) -> Option<Fetched> {
    let rec: Fetched = serde_json::from_slice(&std::fs::read(verified_path(dest)).ok()?).ok()?;
    let len = std::fs::metadata(dest).ok()?.len();
    (len == rec.size && mtime_ms(dest) == rec.mtime_ms).then_some(rec)
}

/// Remove a package and every file kept for it.
pub fn remove_all(dest: &Path) {
    for p in [
        dest.to_path_buf(),
        part_path(dest),
        meta_path(dest),
        verified_path(dest),
    ] {
        let _ = std::fs::remove_file(p);
    }
}

/// Remove the app packages in `dir` other than `keep` (older or other-source
/// copies of the ~1 GB app package, finished or partial, with their records).
/// Framework packages and anything else are left alone.
pub fn prune_app_packages(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let keep_name = keep
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let base = name
            .strip_suffix(".part.json")
            .or_else(|| name.strip_suffix(".verified.json"))
            .or_else(|| name.strip_suffix(".part"))
            .unwrap_or(&name);
        let app = (base.starts_with("ChatGPT-") || base.starts_with("OpenAI.Codex_"))
            && superba_core::AppPackageKind::windows_from_file_name(base).is_some();
        if app && base != keep_name && e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The HTTP client for package downloads: redirects only to URLs `accept`s.
pub fn client(accept: Arc<dyn Fn(&str) -> bool + Send + Sync>) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("superba/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if accept(attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| Error::other(format!("HTTP client: {e}")))
}

/// sha1 and sha256 of a file, streamed.
pub fn hash_file(path: &Path) -> Result<(String, String)> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, &e))?;
    let (mut s1, mut s256) = (Sha1::new(), Sha256::new());
    let mut buf = vec![0_u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, &e))?;
        if n == 0 {
            break;
        }
        s1.update(&buf[..n]);
        s256.update(&buf[..n]);
    }
    Ok((hex::encode(s1.finalize()), hex::encode(s256.finalize())))
}

/// The SHA-1 a Microsoft CDN `ETag` stands for (`"Kfa1/lLy...bY="` is the
/// base64 of the file's SHA-1), when it has that shape.
pub fn etag_sha1(etag: &str) -> Option<String> {
    let t = etag.trim().trim_matches('"');
    if t.len() != 28 || !t.ends_with('=') {
        return None;
    }
    let bytes = base64_decode(t)?;
    (bytes.len() == 20).then(|| hex::encode(bytes))
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0_u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        let take = match chunk.len() {
            4 => 3,
            3 => 2,
            2 => 1,
            _ => return None,
        };
        for i in 0..take {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

fn net_err(url: &str, e: &reqwest::Error) -> Error {
    if e.is_timeout() {
        return Error::Timeout {
            what: format!("GET {}", shown(url)),
        };
    }
    Error::Network {
        url: shown(url),
        message: e.to_string(),
    }
}

/// A URL without its query (temporary links carry signed tokens).
pub fn shown(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_owned()
}

fn header(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// `bytes <start>-<end>/<total>`.
fn content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let start = range.split_once('-')?.0.trim().parse().ok()?;
    Some((start, total.trim().parse().ok()))
}

struct Reporter<'a> {
    sink: &'a dyn DownloadProgressSink,
    t: &'a Target,
    file: String,
    last: Option<Instant>,
}

impl Reporter<'_> {
    fn send(
        &mut self,
        stage: DownloadStage,
        done: u64,
        total: Option<u64>,
        resumed: u64,
        force: bool,
    ) {
        if !force && self.last.is_some_and(|l| l.elapsed() < REPORT_EVERY) {
            return;
        }
        self.last = Some(Instant::now());
        self.sink.report(DownloadProgress {
            stage,
            source: Some(self.t.source),
            file: self.file.clone(),
            done,
            total,
            resumed_from: resumed,
            index: self.t.index,
            count: self.t.count,
        });
    }
}

enum Transfer {
    /// The part file holds the whole entity.
    Complete,
    /// The connection broke; the part file can be resumed.
    Broken(Error),
}

/// Download `t` (resuming a partial file), verify it and move it into place.
pub async fn fetch(
    client: &reqwest::Client,
    t: &Target,
    sink: &dyn DownloadProgressSink,
) -> Result<Fetched> {
    let file = t
        .dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut rep = Reporter {
        sink,
        t,
        file,
        last: None,
    };
    if let Some(done) = already_verified(&t.dest)
        && t.sha1
            .as_deref()
            .is_none_or(|s| s.eq_ignore_ascii_case(&done.sha1))
    {
        rep.send(
            DownloadStage::Downloading,
            done.size,
            Some(done.size),
            done.size,
            true,
        );
        return Ok(done);
    }
    if let Some(dir) = t.dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    }
    let mut last_err = None;
    for _ in 0..ATTEMPTS {
        if sink.cancel_requested() {
            return Err(Error::Cancelled);
        }
        match transfer(client, t, &mut rep).await? {
            Transfer::Complete => return finish(t, &mut rep).await,
            Transfer::Broken(e) => {
                tracing::warn!(url = %shown(&t.url), error = %e, "download interrupted; resuming");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::other("download failed")))
}

async fn transfer(
    client: &reqwest::Client,
    t: &Target,
    rep: &mut Reporter<'_>,
) -> Result<Transfer> {
    let part = part_path(&t.dest);
    let meta_file = meta_path(&t.dest);
    let meta: PartMeta = std::fs::read(&meta_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    // Resume only a part of the same file: same expected checksum, a validator
    // to send, not past its end.
    let resumable = have > 0
        && meta.validator.is_some()
        && meta.sha1 == t.sha1
        && meta.total.is_none_or(|total| have <= total);
    if !resumable {
        have = 0;
    }
    let mut req = client.get(&t.url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        if let Some(v) = &meta.validator {
            req = req.header(reqwest::header::IF_RANGE, v);
        }
    }
    let mut resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return Ok(Transfer::Broken(net_err(&t.url, &e))),
    };
    let status = resp.status().as_u16();
    let (append, total) = match status {
        206 if have > 0 => {
            let range = header(&resp, reqwest::header::CONTENT_RANGE)
                .and_then(|v| content_range(&v))
                .ok_or_else(|| Error::parse(shown(&t.url), "206 without a usable Content-Range"))?;
            if range.0 != have {
                return Err(Error::parse(
                    shown(&t.url),
                    format!("resumed at byte {} instead of {have}", range.0),
                ));
            }
            (true, range.1.or(meta.total))
        }
        200 => (false, resp.content_length()),
        416 if have > 0 && meta.total == Some(have) => {
            drop(resp);
            return Ok(Transfer::Complete);
        }
        416 if have > 0 => {
            // The part no longer fits the file: start over.
            drop(resp);
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&meta_file);
            return Ok(Transfer::Broken(Error::other("partial download discarded")));
        }
        _ => {
            return Err(Error::Http {
                url: shown(&t.url),
                status,
                message: match status {
                    403 | 410 => "the link was refused or has expired".into(),
                    404 => "no such file".into(),
                    _ => resp.status().canonical_reason().unwrap_or("").to_owned(),
                },
            });
        }
    };
    if let Some(total) = total {
        if total > t.max {
            return Err(Error::Invalid {
                message: format!("{} is {total} bytes, more than accepted", shown(&t.url)),
            });
        }
        if let Some(listed) = &t.listed_size
            && !listed.admits(total)
        {
            return Err(Error::Invalid {
                message: format!(
                    "{} is {total} bytes but the listing says {}: refused",
                    shown(&t.url),
                    listed.text
                ),
            });
        }
    }
    let etag = header(&resp, reqwest::header::ETAG);
    if let (Some(want), Some(from_etag)) = (t.sha1.as_deref(), etag.as_deref().and_then(etag_sha1))
        && !want.eq_ignore_ascii_case(&from_etag)
    {
        return Err(Error::Invalid {
            message: format!(
                "the CDN says {} has SHA-1 {from_etag}, the listing says {want}: refused",
                rep.file
            ),
        });
    }
    let validator = etag
        .filter(|e| !e.starts_with("W/"))
        .or_else(|| header(&resp, reqwest::header::LAST_MODIFIED));
    let new_meta = PartMeta {
        validator,
        total,
        sha1: t.sha1.clone(),
    };
    if let Ok(b) = serde_json::to_vec(&new_meta) {
        std::fs::write(&meta_file, b).map_err(|e| Error::io(&meta_file, &e))?;
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&part)
        .map_err(|e| Error::io(&part, &e))?;
    let resumed = if append { have } else { 0 };
    let mut written = resumed;
    rep.send(DownloadStage::Downloading, written, total, resumed, true);
    loop {
        // Wait for the next chunk, looking at the cancel flag meanwhile (a
        // stalled server must not delay a cancel until the read timeout).
        let next = {
            let fut = resp.chunk();
            tokio::pin!(fut);
            loop {
                if rep.sink.cancel_requested() {
                    break None;
                }
                tokio::select! {
                    c = &mut fut => break Some(c),
                    () = tokio::time::sleep(CANCEL_POLL) => {}
                }
            }
        };
        let chunk = match next {
            None => {
                out.flush().map_err(|e| Error::io(&part, &e))?;
                return Err(Error::Cancelled);
            }
            Some(Ok(Some(c))) => c,
            Some(Ok(None)) => break,
            Some(Err(e)) => {
                out.flush().map_err(|e| Error::io(&part, &e))?;
                return Ok(Transfer::Broken(net_err(&t.url, &e)));
            }
        };
        written += chunk.len() as u64;
        if written > t.max || total.is_some_and(|total| written > total) {
            drop(out);
            remove_all(&t.dest);
            return Err(Error::Invalid {
                message: format!("{} sent more data than announced; discarded", shown(&t.url)),
            });
        }
        out.write_all(&chunk).map_err(|e| Error::io(&part, &e))?;
        rep.send(DownloadStage::Downloading, written, total, resumed, false);
    }
    out.flush().map_err(|e| Error::io(&part, &e))?;
    rep.send(DownloadStage::Downloading, written, total, resumed, true);
    if total.is_some_and(|total| written < total) {
        return Ok(Transfer::Broken(Error::Network {
            url: shown(&t.url),
            message: format!(
                "the connection closed after {written} of {} bytes",
                total.unwrap_or(0)
            ),
        }));
    }
    Ok(Transfer::Complete)
}

async fn finish(t: &Target, rep: &mut Reporter<'_>) -> Result<Fetched> {
    let part = part_path(&t.dest);
    let len = std::fs::metadata(&part)
        .map_err(|e| Error::io(&part, &e))?
        .len();
    rep.send(DownloadStage::Verifying, len, Some(len), 0, true);
    let p = part.clone();
    let (sha1, sha256) = tokio::task::spawn_blocking(move || hash_file(&p))
        .await
        .map_err(|e| Error::other(format!("background task failed: {e}")))??;
    let refuse = |message: String| {
        remove_all(&t.dest);
        Err(Error::Invalid { message })
    };
    if let Some(want) = &t.sha1
        && !want.eq_ignore_ascii_case(&sha1)
    {
        return refuse(format!(
            "{} has SHA-1 {sha1}, the listing says {want}: corrupted or tampered download, deleted",
            rep.file
        ));
    }
    if let Some(listed) = &t.listed_size
        && !listed.admits(len)
    {
        return refuse(format!(
            "{} has {len} bytes, the listing says {}: deleted",
            rep.file, listed.text
        ));
    }
    std::fs::rename(&part, &t.dest).map_err(|e| Error::io(&t.dest, &e))?;
    let _ = std::fs::remove_file(meta_path(&t.dest));
    let done = Fetched {
        size: len,
        sha256,
        sha1,
        mtime_ms: mtime_ms(&t.dest),
    };
    if let Ok(b) = serde_json::to_vec(&done) {
        let _ = std::fs::write(verified_path(&t.dest), b);
    }
    Ok(done)
}
