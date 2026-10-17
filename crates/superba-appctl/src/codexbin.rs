//! v0.3 Codex binary helpers: `codex debug models` in a private Codex home, how a
//! binary was installed, the app's minimum app-server version (from its
//! `app.asar`) and the process image of the app's app-server (the only proof of
//! a `CODEX_CLI_PATH` pin: a bad path silently falls back to the bundled Codex).

use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use superba_core::codex::ENV_CODEX_CLI_PATH;
use superba_core::{AppInstall, CodexInstallKind, CodexVersion, Error, Result, SettingValue};
use tokio::io::AsyncReadExt as _;

use crate::procs::Snapshot;

/// Deadline of one `codex debug models` run.
pub const DEBUG_MODELS_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest `codex debug models` output accepted (the 0.162 bundled list is
/// about 660 KB with its instructions).
pub const DEBUG_MODELS_MAX_OUTPUT: u64 = 32 * 1024 * 1024;

/// Text the desktop app prefixes to its "app-server too old" error; the
/// minimum version is the string constant declared right before it.
pub const FLOOR_MARKER: &[u8] = b"=`codex-app-server-version-unsupported:`";

/// Environment variables never passed to `codex debug models` (credentials,
/// homes, a pin): the run sees only its private, empty Codex home.
fn scrubbed(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k.starts_with("CODEX_") || k.starts_with("OPENAI_") || k == ENV_CODEX_CLI_PATH
}

/// The `codex debug models` arguments (`--bundled`, or the catalog file).
pub fn debug_models_args(catalog: Option<&Path>) -> Vec<String> {
    let mut args = vec!["debug".to_owned(), "models".to_owned()];
    match catalog {
        Some(path) => {
            let repr = SettingValue::String(path.to_string_lossy().into_owned()).to_toml_repr();
            args.push("-c".to_owned());
            args.push(format!("model_catalog_json={repr}"));
        }
        None => args.push("--bundled".to_owned()),
    }
    args
}

/// Run `binary debug models` (see [`debug_models_args`]) with a fresh, empty
/// `CODEX_HOME` (and `HOME`) in a temporary directory, removed afterwards: the
/// real Codex home is never read. Success = the stdout JSON; a non-zero exit is
/// [`Error::Parse`] with the last line Codex printed (`failed to parse
/// model_catalog_json ...`).
pub async fn debug_models(
    binary: &Path,
    catalog: Option<&Path>,
    timeout: Duration,
) -> Result<String> {
    let home = tempfile::Builder::new()
        .prefix("superba-codex-home-")
        .tempdir()
        .map_err(|e| Error::io(std::env::temp_dir(), &e))?;
    let mut cmd = tokio::process::Command::from(superba_core::process::command(binary));
    cmd.args(debug_models_args(catalog))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_none_or(scrubbed) {
            cmd.env_remove(&key);
        }
    }
    cmd.env("CODEX_HOME", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path());
    let mut child = cmd.spawn().map_err(|e| Error::Process {
        message: format!("cannot run {}: {e}", binary.display()),
    })?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let work = async {
        let out = async {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s
                    .take(DEBUG_MODELS_MAX_OUTPUT + 1)
                    .read_to_end(&mut buf)
                    .await;
            }
            buf
        };
        let err = async {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.take(256 * 1024).read_to_end(&mut buf).await;
            }
            buf
        };
        let (out, err) = tokio::join!(out, err);
        let status = child.wait().await;
        (status, out, err)
    };
    let (status, out, err) =
        tokio::time::timeout(timeout, work)
            .await
            .map_err(|_| Error::Timeout {
                what: format!("{} debug models", binary.display()),
            })?;
    drop(home);
    let status = status.map_err(|e| Error::Process {
        message: format!("waiting for {} failed: {e}", binary.display()),
    })?;
    if out.len() as u64 > DEBUG_MODELS_MAX_OUTPUT {
        return Err(Error::Process {
            message: format!(
                "{} debug models printed more than {DEBUG_MODELS_MAX_OUTPUT} bytes",
                binary.display()
            ),
        });
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&err);
        let message = stderr
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty() && !l.starts_with("WARNING"))
            .unwrap_or("codex debug models failed")
            .chars()
            .take(400)
            .collect::<String>();
        return Err(Error::Parse {
            what: match catalog {
                Some(_) => "model_catalog_json".to_owned(),
                None => "codex debug models --bundled".to_owned(),
            },
            message,
        });
    }
    String::from_utf8(out).map_err(|_| Error::Parse {
        what: "codex debug models".into(),
        message: "output is not UTF-8".into(),
    })
}

/// Identity of a file for caches: canonical path, size and modification time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileStamp {
    /// Canonical path.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Modification time (ms since the epoch).
    pub mtime_ms: i64,
}

impl FileStamp {
    /// Stamp of `path` (`None` when it cannot be read).
    pub fn of(path: &Path) -> Option<Self> {
        let path = fs::canonicalize(path).ok()?;
        let meta = fs::metadata(&path).ok()?;
        let mtime_ms = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis();
        Some(Self {
            path,
            size: meta.len(),
            mtime_ms: i64::try_from(mtime_ms).unwrap_or(i64::MAX),
        })
    }
}

/// sha256 (hex) of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).map_err(|e| Error::io(path, &e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0_u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, &e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// In-memory caches of the controller (per process).
#[derive(Debug, Default)]
pub struct Caches {
    /// Binary stamp -> sha256.
    binary_sha: Mutex<HashMap<FileStamp, String>>,
    /// (binary sha256, catalog sha256 or "bundled") -> `debug models` output.
    debug_models: Mutex<HashMap<(String, String), String>>,
    /// asar stamp -> floor.
    floors: Mutex<HashMap<FileStamp, CodexVersion>>,
    /// CDP listener checks of app main processes ([`crate::owner::OwnerMemo`]).
    pub(crate) owners: Mutex<HashMap<crate::owner::OwnerKey, crate::owner::OwnerMemo>>,
    /// The change-check path list of the last detection result: (identity of
    /// the result, paths) ([`crate::watch`]).
    pub(crate) watch: Mutex<Option<(u64, Vec<PathBuf>)>>,
}

/// Entries kept in the `debug models` cache (outputs are up to a few MB).
const DEBUG_MODELS_CACHE_ENTRIES: usize = 16;

impl Caches {
    /// sha256 of a binary, cached by its stamp.
    pub fn binary_sha(&self, path: &Path) -> Result<String> {
        let stamp = FileStamp::of(path).ok_or_else(|| Error::NotFound {
            what: format!("codex binary {}", path.display()),
        })?;
        if let Some(sha) = lock(&self.binary_sha).get(&stamp) {
            return Ok(sha.clone());
        }
        let sha = sha256_file(&stamp.path)?;
        lock(&self.binary_sha).insert(stamp, sha.clone());
        Ok(sha)
    }

    /// Cached `debug models` output.
    pub fn debug_models(&self, key: &(String, String)) -> Option<String> {
        lock(&self.debug_models).get(key).cloned()
    }

    /// Remember a `debug models` output (bounded).
    pub fn put_debug_models(&self, key: (String, String), out: String) {
        let mut map = lock(&self.debug_models);
        if map.len() >= DEBUG_MODELS_CACHE_ENTRIES {
            map.clear();
        }
        map.insert(key, out);
    }

    /// Cached floor of an asar.
    pub fn floor(&self, stamp: &FileStamp) -> Option<CodexVersion> {
        lock(&self.floors).get(stamp).cloned()
    }

    /// Remember a floor.
    pub fn put_floor(&self, stamp: FileStamp, floor: CodexVersion) {
        lock(&self.floors).insert(stamp, floor);
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `app.asar` of an install: `Contents/Resources/app.asar` (macOS bundle),
/// else `resources/app.asar` next to the executable or under the install root.
pub fn asar_path(install: &AppInstall) -> Option<PathBuf> {
    [
        Some(install.path.join("Contents/Resources/app.asar")),
        install
            .executable
            .parent()
            .map(|d| d.join("resources/app.asar")),
        Some(install.path.join("resources/app.asar")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
}

/// Floor candidates in `bytes`: for each [`FLOOR_MARKER`], the string constant
/// assigned right before it (`var x=`0.141.0`,S=`codex-app-server-...`).
pub fn floor_candidates(bytes: &[u8]) -> Vec<CodexVersion> {
    let mut out = Vec::new();
    for pos in memchr::memmem::find_iter(bytes, FLOOR_MARKER) {
        if let Some(v) = floor_before(&bytes[..pos]) {
            out.push(v);
        }
    }
    out
}

/// Parse `` =`<semver>`,<ident> `` backwards from the end of `head`.
fn floor_before(head: &[u8]) -> Option<CodexVersion> {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut i = head.len();
    let ident_end = i;
    while i > 0 && is_ident(head[i - 1]) && ident_end - i < 64 {
        i -= 1;
    }
    if i == ident_end || i < 2 || head[i - 1] != b',' || head[i - 2] != b'`' {
        return None;
    }
    let close = i - 2;
    let mut j = close;
    while j > 0 && close - j < 48 && head[j - 1] != b'`' {
        j -= 1;
    }
    if j < 2 || head[j - 1] != b'`' || head[j - 2] != b'=' {
        return None;
    }
    let text = std::str::from_utf8(&head[j..close]).ok()?;
    if !text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    CodexVersion::parse(text).ok()
}

/// Scan a (large) file for the floor, streaming in chunks with an overlap so a
/// marker across a chunk boundary is found. Several distinct values: the
/// highest. None found: [`Error::NotFound`] (callers fall back to
/// [`superba_core::codex::APP_SERVER_FLOOR_FALLBACK`]).
pub fn scan_floor(path: &Path) -> Result<CodexVersion> {
    const CHUNK: usize = 8 * 1024 * 1024;
    const OVERLAP: usize = 256;
    let mut f = fs::File::open(path).map_err(|e| Error::io(path, &e))?;
    let mut buf = vec![0_u8; CHUNK + OVERLAP];
    let mut carry = 0_usize;
    let mut found: Vec<CodexVersion> = Vec::new();
    loop {
        let n = f.read(&mut buf[carry..]).map_err(|e| Error::io(path, &e))?;
        if n == 0 {
            break;
        }
        let len = carry + n;
        found.extend(floor_candidates(&buf[..len]));
        carry = len.min(OVERLAP);
        buf.copy_within(len - carry..len, 0);
    }
    found.sort();
    found.dedup();
    found.pop().ok_or_else(|| Error::NotFound {
        what: format!("the app-server version floor in {}", path.display()),
    })
}

/// Text the Codex binary embeds its own version right after (`codex-doctor/<v>`,
/// the app-server daemon's name): the version of a binary that cannot be run
/// (inside an MSIX package) is read from these, without running it.
const VERSION_MARKERS: &[&[u8]] = &[b"codex-doctor/", b"Codex App Server Daemon"];

/// A Codex version at the start of `bytes`: `MAJOR.MINOR.PATCH`, then an
/// optional `-<word>` with dot-separated numbers (`0.163.0-alpha.10.2`). Text
/// glued to it (`...10.2failed`) is not part of it.
fn version_at(bytes: &[u8]) -> Option<CodexVersion> {
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() && *i - start < 9 {
            *i += 1;
        }
        *i > start
    };
    for part in 0..3 {
        if part > 0 {
            if bytes.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
        if !digits(&mut i) {
            return None;
        }
    }
    if bytes.get(i) == Some(&b'-') {
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_lowercase() && j - i < 16 {
            j += 1;
        }
        if j > i + 1 {
            i = j;
            while bytes.get(i) == Some(&b'.') {
                let mut k = i + 1;
                if !digits(&mut k) {
                    break;
                }
                i = k;
            }
        }
    }
    CodexVersion::parse(std::str::from_utf8(&bytes[..i]).ok()?).ok()
}

/// The version a Codex binary embeds after one of [`VERSION_MARKERS`].
pub fn version_in_bytes(bytes: &[u8]) -> Option<CodexVersion> {
    VERSION_MARKERS.iter().find_map(|marker| {
        memchr::memmem::find_iter(bytes, marker)
            .find_map(|pos| version_at(&bytes[pos + marker.len()..]))
    })
}

/// [`version_in_bytes`] of a (large) file, streamed in chunks with an overlap,
/// remembered per file stamp. `None` when the file cannot be read or holds no
/// marked version.
pub fn scanned_version(path: &Path) -> Option<CodexVersion> {
    type Cache = Mutex<HashMap<FileStamp, Option<CodexVersion>>>;
    static CACHE: std::sync::LazyLock<Cache> = std::sync::LazyLock::new(Default::default);
    const CHUNK: usize = 8 * 1024 * 1024;
    const OVERLAP: usize = 128;

    let stamp = FileStamp::of(path)?;
    if let Some(v) = lock(&CACHE).get(&stamp) {
        return v.clone();
    }
    let scan = || -> Option<CodexVersion> {
        let mut f = fs::File::open(path).ok()?;
        let mut buf = vec![0_u8; CHUNK + OVERLAP];
        let mut carry = 0_usize;
        loop {
            let n = f.read(&mut buf[carry..]).ok()?;
            if n == 0 {
                return None;
            }
            let len = carry + n;
            if let Some(v) = version_in_bytes(&buf[..len]) {
                return Some(v);
            }
            carry = len.min(OVERLAP);
            buf.copy_within(len - carry..len, 0);
        }
    };
    let version = scan();
    let mut cache = lock(&CACHE);
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(stamp, version.clone());
    version
}

/// How `binary` was installed: Superba's store (`store_dir`), bundled in a
/// desktop app install, else by its resolved path
/// ([`CodexInstallKind::from_resolved_path`]).
pub fn install_kind(binary: &Path, store_dir: &Path, installs: &[AppInstall]) -> CodexInstallKind {
    let resolved = fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    let store = fs::canonicalize(store_dir).unwrap_or_else(|_| store_dir.to_path_buf());
    if resolved.starts_with(&store) {
        return CodexInstallKind::SuperbaStore;
    }
    let bundled = installs.iter().any(|i| {
        i.bundled_codex
            .as_deref()
            .and_then(|b| fs::canonicalize(b).ok())
            .is_some_and(|b| b == resolved)
            || fs::canonicalize(&i.path)
                .is_ok_and(|root| root.is_dir() && resolved.starts_with(root))
    });
    if bundled {
        return CodexInstallKind::AppBundled;
    }
    CodexInstallKind::from_resolved_path(&resolved.to_string_lossy())
}

/// Process image of the newest app-server child of `main_pid` (Linux
/// `/proc/<pid>/exe` with a `" (deleted)"` suffix removed, macOS
/// `proc_pidpath`, Windows `QueryFullProcessImageName`, through `sysinfo`).
pub fn app_server_exe(snap: &Snapshot, main_pid: u32) -> Option<PathBuf> {
    snap.app_servers_of(main_pid)
        .into_iter()
        .filter(|e| !e.zombie)
        .max_by_key(|e| (e.started_at, e.pid))
        .and_then(|e| e.exe.clone())
        .map(|p| {
            let s = p.to_string_lossy();
            match s.strip_suffix(" (deleted)") {
                Some(t) => PathBuf::from(t),
                None => p,
            }
        })
}
