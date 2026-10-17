//! Test helpers for the Windows app package: a loopback HTTP/1.1 server with
//! POST bodies, `Range` + `If-Range`, slow and broken transfers and a request
//! log; zip / MSIX / bundle builders (ZIP64 optional).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One canned answer.
#[derive(Clone, Debug)]
pub struct Route {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Honour `Range: bytes=N-` (with `If-Range` against [`Route::validator`]).
    pub ranges: bool,
    /// The current `ETag` / `Last-Modified` (sent, and compared with `If-Range`).
    pub validator: Option<String>,
    /// Write the body in pieces of this size with `delay` between them.
    pub chunk: usize,
    pub delay: Duration,
    /// Close the connection after this many body bytes (a broken transfer).
    pub cut_after: Option<usize>,
}

impl Route {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
            ranges: true,
            validator: None,
            chunk: 64 * 1024,
            delay: Duration::ZERO,
            cut_after: None,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            ranges: false,
            ..Self::ok(Vec::new())
        }
        .with_status(status)
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn etag(mut self, etag: &str) -> Self {
        self.validator = Some(etag.into());
        self.header("ETag", etag)
    }

    pub fn slow(mut self, chunk: usize, delay: Duration) -> Self {
        self.chunk = chunk;
        self.delay = delay;
        self
    }

    pub fn cut_after(mut self, n: usize) -> Self {
        self.cut_after = Some(n);
        self
    }
}

/// A logged request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[derive(Default)]
struct State {
    /// Routes by path; several answers for one path are used in turn (the
    /// last one repeats).
    routes: BTreeMap<String, Vec<Route>>,
    hits: BTreeMap<String, usize>,
    log: Vec<Seen>,
}

pub struct Mock {
    pub base: String,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn parse_request(buf: &[u8]) -> Option<(Seen, usize)> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let mut headers = BTreeMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let len = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Some((
        Seen {
            method,
            path,
            headers,
            body: String::new(),
        },
        end + 4 + len,
    ))
}

impl Mock {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state: Arc<Mutex<State>> = Arc::default();
        let st = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(sock, st.clone()));
            }
        });
        Self { base, state, task }
    }

    /// Answer `path` (without query) with `route`.
    pub fn route(&self, path: &str, route: Route) {
        self.state
            .lock()
            .unwrap()
            .routes
            .insert(path.to_owned(), vec![route]);
    }

    /// Answer `path` with `routes` in turn (the last one repeats).
    pub fn routes(&self, path: &str, routes: Vec<Route>) {
        self.state
            .lock()
            .unwrap()
            .routes
            .insert(path.to_owned(), routes);
    }

    pub fn log(&self) -> Vec<Seen> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn requests(&self, path: &str) -> Vec<Seen> {
        self.log()
            .into_iter()
            .filter(|s| s.path.split('?').next() == Some(path))
            .collect()
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

async fn serve(mut sock: tokio::net::TcpStream, st: Arc<Mutex<State>>) {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 8192];
    let (mut seen, total) = loop {
        let Ok(n) = sock.read(&mut tmp).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(parsed) = parse_request(&buf)
            && buf.len() >= parsed.1
        {
            break parsed;
        }
    };
    let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    seen.body = String::from_utf8_lossy(&buf[head_end..total]).into_owned();
    let key = seen.path.split('?').next().unwrap_or("").to_owned();
    let route = {
        let mut s = st.lock().unwrap();
        s.log.push(seen.clone());
        let n = *s.hits.get(&key).unwrap_or(&0);
        s.hits.insert(key.clone(), n + 1);
        s.routes
            .get(&key)
            .and_then(|list| list.get(n.min(list.len().saturating_sub(1))).cloned())
    };
    let route = route.unwrap_or_else(|| Route::status(404));
    let range_from = seen
        .header("range")
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
    let if_range_ok = match (seen.header("if-range"), &route.validator) {
        (None, _) => true,
        (Some(a), Some(b)) => a == b,
        (Some(_), None) => false,
    };
    let mut headers = route.headers.clone();
    let (status, body) = match (range_from, route.ranges, route.status) {
        (Some(from), true, 200) if if_range_ok => {
            if from >= route.body.len() {
                (416, Vec::new())
            } else {
                headers.push((
                    "Content-Range".into(),
                    format!("bytes {from}-{}/{}", route.body.len() - 1, route.body.len()),
                ));
                (206, route.body[from..].to_vec())
            }
        }
        _ => (route.status, route.body.clone()),
    };
    let body = if seen.method == "HEAD" {
        Vec::new()
    } else {
        body
    };
    let announced = if seen.method == "HEAD" {
        route.body.len()
    } else {
        body.len()
    };
    let mut resp =
        format!("HTTP/1.1 {status} X\r\nContent-Length: {announced}\r\nConnection: close\r\n");
    for (k, v) in headers {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    resp.push_str("\r\n");
    if sock.write_all(resp.as_bytes()).await.is_err() {
        return;
    }
    let limit = route.cut_after.unwrap_or(usize::MAX).min(body.len());
    let mut sent = 0;
    while sent < limit {
        let n = route.chunk.max(1).min(limit - sent);
        if sock.write_all(&body[sent..sent + n]).await.is_err() {
            return;
        }
        sent += n;
        if !route.delay.is_zero() {
            tokio::time::sleep(route.delay).await;
        }
    }
    let _ = sock.flush().await;
    let _ = sock.shutdown().await;
}

// --- zip / MSIX builders ---------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One zip entry.
pub struct Entry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    pub deflate: bool,
    /// General purpose flags (1 = encrypted).
    pub flags: u16,
}

impl<'a> Entry<'a> {
    pub fn deflated(name: &'a str, data: &'a [u8]) -> Self {
        Self {
            name,
            data,
            deflate: true,
            flags: 0,
        }
    }

    pub fn stored(name: &'a str, data: &'a [u8]) -> Self {
        Self {
            name,
            data,
            deflate: false,
            flags: 0,
        }
    }
}

/// A zip archive; `zip64` writes ZIP64 extra fields, the ZIP64 end record and
/// locator (with 0xFFFFFFFF in the classic fields), as makeappx does.
pub fn zip(entries: &[Entry<'_>], zip64: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let (method, body) = if e.deflate {
            let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            z.write_all(e.data).unwrap();
            (8_u16, z.finish().unwrap())
        } else {
            (0_u16, e.data.to_vec())
        };
        let offset = out.len() as u64;
        let crc = crc32(e.data);
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&45_u16.to_le_bytes());
        out.extend_from_slice(&e.flags.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&body);
        let extra: Vec<u8> = if zip64 {
            let mut x = Vec::new();
            x.extend_from_slice(&1_u16.to_le_bytes());
            x.extend_from_slice(&24_u16.to_le_bytes());
            x.extend_from_slice(&(e.data.len() as u64).to_le_bytes());
            x.extend_from_slice(&(body.len() as u64).to_le_bytes());
            x.extend_from_slice(&offset.to_le_bytes());
            x
        } else {
            Vec::new()
        };
        let (csize, usize_, off) = if zip64 {
            (u32::MAX, u32::MAX, u32::MAX)
        } else {
            (body.len() as u32, e.data.len() as u32, offset as u32)
        };
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&45_u16.to_le_bytes());
        central.extend_from_slice(&45_u16.to_le_bytes());
        central.extend_from_slice(&e.flags.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&csize.to_le_bytes());
        central.extend_from_slice(&usize_.to_le_bytes());
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&[0; 2]);
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(e.name.as_bytes());
        central.extend_from_slice(&extra);
    }
    let cd_offset = out.len() as u64;
    out.extend_from_slice(&central);
    if zip64 {
        let eocd64 = out.len() as u64;
        out.extend_from_slice(&0x0606_4b50_u32.to_le_bytes());
        out.extend_from_slice(&44_u64.to_le_bytes());
        out.extend_from_slice(&45_u16.to_le_bytes());
        out.extend_from_slice(&45_u16.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        out.extend_from_slice(&(central.len() as u64).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0x0706_4b50_u32.to_le_bytes());
        out.extend_from_slice(&0_u32.to_le_bytes());
        out.extend_from_slice(&eocd64.to_le_bytes());
        out.extend_from_slice(&1_u32.to_le_bytes());
    }
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    let n = if zip64 {
        u16::MAX
    } else {
        entries.len() as u16
    };
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    let (size, off) = if zip64 {
        (u32::MAX, u32::MAX)
    } else {
        (central.len() as u32, cd_offset as u32)
    };
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&off.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

/// The real manifest of the 26.1013.4894.0 x64 package.
pub fn real_manifest() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/AppxManifest-OpenAI.Codex-26.1013.4894.0-x64.xml"
    ))
    .unwrap()
}

/// A manifest with the given identity and dependencies (`(name, min, publisher)`).
pub fn manifest(
    name: &str,
    version: &str,
    arch: &str,
    publisher: &str,
    framework: bool,
    deps: &[(&str, &str, &str)],
) -> String {
    let deps: String = deps
        .iter()
        .map(|(n, v, p)| {
            format!("<PackageDependency Name=\"{n}\" MinVersion=\"{v}\" Publisher=\"{p}\" />")
        })
        .collect();
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <Package xmlns=\"http://schemas.microsoft.com/appx/manifest/foundation/windows10\">\
         <Identity Name=\"{name}\" ProcessorArchitecture=\"{arch}\" Version=\"{version}\" Publisher=\"{publisher}\" />\
         <Properties><DisplayName>{name}</DisplayName><PublisherDisplayName>Someone</PublisherDisplayName>\
         <Framework>{framework}</Framework></Properties>\
         <Dependencies><TargetDeviceFamily Name=\"Windows.Desktop\" MinVersion=\"10.0.19041.0\" MaxVersionTested=\"10.0.26100.0\" />{deps}</Dependencies>\
         </Package>"
    )
}

/// An MSIX: the manifest plus some payload, ZIP64 like makeappx writes it.
pub fn msix(manifest: &str, payload: &[u8]) -> Vec<u8> {
    zip(
        &[
            Entry::deflated("app/ChatGPT.exe", payload),
            Entry::deflated("AppxManifest.xml", manifest.as_bytes()),
            Entry::deflated("AppxBlockMap.xml", b"<BlockMap/>"),
            Entry::deflated("AppxSignature.p7x", b"PKCX"),
        ],
        true,
    )
}

/// A bundle holding `packages` (`(file name, arch, bytes)`), stored.
pub fn bundle(
    name: &str,
    version: &str,
    publisher: &str,
    packages: &[(&str, &str, Vec<u8>)],
) -> Vec<u8> {
    let list: String = packages
        .iter()
        .map(|(f, a, _)| {
            format!("<Package Type=\"application\" Version=\"1.0.0.0\" Architecture=\"{a}\" FileName=\"{f}\" Offset=\"0\" Size=\"0\" />")
        })
        .collect();
    let bundle_manifest = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Bundle xmlns=\"http://schemas.microsoft.com/appx/2013/bundle\" SchemaVersion=\"5.0\">\
         <Identity Name=\"{name}\" Publisher=\"{publisher}\" Version=\"{version}\" />\
         <Packages>{list}</Packages></Bundle>"
    );
    let mut entries: Vec<Entry<'_>> = packages
        .iter()
        .map(|(f, _, bytes)| Entry::stored(f, bytes))
        .collect();
    entries.push(Entry::deflated(
        "AppxMetadata/AppxBundleManifest.xml",
        bundle_manifest.as_bytes(),
    ));
    zip(&entries, true)
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::Digest;
    hex::encode(sha1::Sha1::digest(bytes))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A resolver answer listing `rows` (`(file name, url, sha1, size text)`),
/// in the site's own markup.
pub fn listing(rows: &[(&str, &str, &str, &str)]) -> String {
    let mut s = String::from(
        "<p>The links were successfully received from the Microsoft Store server.</p>\
         <table class=\"tftable\" border=\"1\" align=\"center\"><thead><tr><th>File:</th>\
         <th style=\"width:180px;\">Expire:</th><th style=\"width:300px;\">SHA-1:</th>\
         <th style=\"width:60px;\">Size:</th></tr></thead>",
    );
    for (name, url, sha1, size) in rows {
        s.push_str(&format!(
            "<tr style=\"background-color:rgba(255, 255, 255, 0.8)\"><td><a href=\"{}\" rel=\"noreferrer\">{name}</a></td>\
             <td align=\"center\">2099-01-01 00:00:00 GMT</td><td align=\"center\">{sha1}</td>\
             <td align=\"center\">{size}</td></tr>\n",
            url.replace('&', "&amp;")
        ));
    }
    s.push_str("</table>");
    s
}

/// `n` bytes as shown by the resolver (`1.23 MB`).
pub fn listed_size(n: usize) -> String {
    let n = n as f64;
    if n < 1024.0 {
        format!("{n} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.2} KB", n / 1024.0)
    } else {
        format!("{:.2} MB", n / 1024.0 / 1024.0)
    }
}
