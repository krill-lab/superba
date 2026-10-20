//! Test helpers: a tiny HTTP/1.1 server on loopback with canned routes, Range
//! support and a request log; tar.gz / zip builders; temp contexts; fake
//! tool programs (shell scripts).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One canned answer.
#[derive(Clone, Debug)]
pub struct Route {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Honour `Range: bytes=N-`.
    pub ranges: bool,
}

impl Route {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
            ranges: true,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            ranges: false,
        }
    }

    /// The `ETag` header this route sends, if any.
    pub fn etag(&self) -> Option<String> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
            .map(|(_, v)| v.clone())
    }

    /// With an extra header.
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn redirect(location: &str) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".into(), location.into())],
            body: Vec::new(),
            ranges: false,
        }
    }
}

/// A logged request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub path: String,
    pub range: Option<String>,
    pub if_none_match: Option<String>,
}

#[derive(Default)]
struct State {
    routes: BTreeMap<String, Route>,
    log: Vec<Seen>,
}

/// The server; dropped = stops accepting.
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

impl Mock {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state: Arc<Mutex<State>> = Arc::default();
        let st = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let st = st.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0_u8; 4096];
                    loop {
                        let Ok(n) = sock.read(&mut tmp).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    let range = head.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("range")
                            .then(|| v.trim().to_owned())
                    });
                    let if_none_match = head.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case("if-none-match")
                            .then(|| v.trim().to_owned())
                    });
                    let route = {
                        let mut s = st.lock().unwrap();
                        s.log.push(Seen {
                            path: path.clone(),
                            range: range.clone(),
                            if_none_match: if_none_match.clone(),
                        });
                        let key = path.split('?').next().unwrap_or("").to_owned();
                        s.routes.get(&key).cloned()
                    };
                    let route = route.unwrap_or_else(|| Route::status(404));
                    // A matching ETag answers 304 with no body.
                    let route = match (&if_none_match, route.etag()) {
                        (Some(a), Some(b)) if a == &b => Route::status(304),
                        _ => route,
                    };
                    let (status, body, mut headers) = match (&range, route.ranges, route.status) {
                        (Some(r), true, 200) => {
                            let from: usize = r
                                .trim_start_matches("bytes=")
                                .trim_end_matches('-')
                                .parse()
                                .unwrap_or(0);
                            if from >= route.body.len() {
                                (416, Vec::new(), Vec::new())
                            } else {
                                (
                                    206,
                                    route.body[from..].to_vec(),
                                    vec![(
                                        "Content-Range".to_owned(),
                                        format!(
                                            "bytes {from}-{}/{}",
                                            route.body.len() - 1,
                                            route.body.len()
                                        ),
                                    )],
                                )
                            }
                        }
                        _ => (route.status, route.body.clone(), Vec::new()),
                    };
                    headers.extend(route.headers.clone());
                    let mut resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    for (k, v) in headers {
                        resp.push_str(&format!("{k}: {v}\r\n"));
                    }
                    resp.push_str("\r\n");
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self { base, state, task }
    }

    pub fn route(&self, path: &str, route: Route) {
        self.state
            .lock()
            .unwrap()
            .routes
            .insert(path.to_owned(), route);
    }

    pub fn log(&self) -> Vec<Seen> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn hits(&self, path: &str) -> usize {
        self.log()
            .iter()
            .filter(|s| s.path.starts_with(path))
            .count()
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// A `.tar.gz` of `(path, contents, mode)` files plus `(path, target)` links.
pub fn targz(files: &[(&str, &[u8], u32)], links: &[(&str, &str)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, data, mode) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(*mode);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, path, *data).unwrap();
    }
    for (path, target) in links {
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o777);
        h.set_entry_type(tar::EntryType::Symlink);
        b.append_link(&mut h, path, target).unwrap();
    }
    let tar = b.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

/// A raw tar entry with an arbitrary (possibly unsafe) path, gzip-compressed.
pub fn targz_raw_path(path: &[u8], data: &[u8]) -> Vec<u8> {
    let mut h = tar::Header::new_old();
    {
        let name = &mut h.as_old_mut().name;
        name[..path.len()].copy_from_slice(path);
    }
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    h.set_cksum();
    let mut out = Vec::new();
    out.extend_from_slice(h.as_bytes());
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out.extend_from_slice(&[0_u8; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&out).unwrap();
    gz.finish().unwrap()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

pub fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

/// One entry of [`zip_bytes`].
pub struct ZipFile<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    /// Unix mode including the file type bits (`0o100755`, `0o120777` for a
    /// link, `0o040755` for a directory); 0 = written by a non-Unix host.
    pub mode: u32,
    pub deflate: bool,
}

impl<'a> ZipFile<'a> {
    pub fn file(name: &'a str, data: &'a [u8], perm: u32) -> Self {
        Self {
            name,
            data,
            mode: 0o100_000 | perm,
            deflate: true,
        }
    }
}

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

/// A zip archive (central directory authoritative, like `ditto` / `zip`).
pub fn zip_bytes(files: &[ZipFile<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for f in files {
        let (method, body) = if f.deflate {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(f.data).unwrap();
            (8_u16, e.finish().unwrap())
        } else {
            (0_u16, f.data.to_vec())
        };
        let offset = out.len() as u32;
        let crc = crc32(f.data);
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&20_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(f.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(f.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(f.name.as_bytes());
        out.extend_from_slice(&body);
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        let host: u16 = if f.mode == 0 { 0 } else { 3 };
        central.extend_from_slice(&((host << 8) | 20).to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(f.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(f.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 6]);
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&(f.mode << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(f.name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

/// sha512 as standard base64.
pub fn sha512_b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest;
    base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(bytes))
}

/// A temp root with `home/` and `data/`, an environment with `PATH` =
/// `home/.local/bin` plus `extra_path`, `SHELL=/bin/sh`.
pub fn context(root: &Path, extra_path: &[&Path]) -> superba_core::LocalContext {
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    let mut dirs: Vec<std::path::PathBuf> = vec![home.join(".local/bin")];
    dirs.extend(extra_path.iter().map(|p| p.to_path_buf()));
    let path = std::env::join_paths(dirs).unwrap();
    let env = superba_core::MapEnv::new()
        .with("PATH", path.to_string_lossy().as_ref())
        .with("SHELL", "/bin/sh")
        .with(
            "SUPERBA_SEARCH_ROOT",
            root.join("sysroot").to_string_lossy().as_ref(),
        );
    superba_core::LocalContext::new(
        &home,
        superba_core::DataDir::new(root.join("data")),
        std::sync::Arc::new(env),
    )
}

/// A shell script program.
#[cfg(unix)]
pub fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, format!("#!/bin/sh\n{body}\n").as_bytes());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Sources that all point at `base`.
pub fn sources(base: &str) -> superba_tools::Sources {
    superba_tools::Sources {
        claude: format!("{base}/claude"),
        npm: format!("{base}/npm"),
        pypi: format!("{base}/pypi"),
        opencode: format!("{base}/opencode"),
        github: format!("{base}/github"),
        github_api: format!("{base}/ghapi"),
        pi: format!("{base}/pi"),
        zcode: format!("{base}/zcode"),
        workbuddy_cn: format!("{base}/wbcn"),
        workbuddy_intl: format!("{base}/wbintl"),
    }
}
