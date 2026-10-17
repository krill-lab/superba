//! Test helpers: a tiny HTTP/1.1 server on loopback with canned routes, Range
//! support and a request log; fake Codex packages.

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
                    let route = {
                        let mut s = st.lock().unwrap();
                        s.log.push(Seen {
                            path: path.clone(),
                            range: range.clone(),
                        });
                        let key = path.split('?').next().unwrap_or("").to_owned();
                        s.routes.get(&key).cloned()
                    };
                    let route = route.unwrap_or_else(|| Route::status(404));
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

/// A `codex` that prints `codex-cli <version>`.
pub fn fake_codex(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"codex-cli {version}\"\n").into_bytes()
}

/// A Codex package (GitHub layout, or npm layout under package/vendor/<target>).
pub fn package(version: &str, target: &str, npm: bool) -> Vec<u8> {
    let codex = fake_codex(version);
    let prefix = if npm {
        format!("package/vendor/{target}/")
    } else {
        String::new()
    };
    let p = |rel: &str| format!("{prefix}{rel}");
    let (bin, json, rg) = (p("bin/codex"), p("codex-package.json"), p("codex-path/rg"));
    let mut files: Vec<(&str, &[u8], u32)> = vec![
        (&bin, &codex, 0o755),
        (&json, b"{\"version\":\"x\"}", 0o644),
        (&rg, b"rg", 0o755),
    ];
    let readme = "package/README.md".to_owned();
    if npm {
        files.push((&readme, b"npm readme", 0o644));
    }
    targz(&files, &[])
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

pub fn integrity(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!(
        "sha512-{}",
        superba_versions::http::base64(&sha2::Sha512::digest(bytes))
    )
}

pub fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}
