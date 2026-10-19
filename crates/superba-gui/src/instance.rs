//! A running Superba window takes requests from a second start of `superba`
//! (GH#7): `superba --launch-chatgpt` (the "ChatGPT (Superba)" Start-menu
//! shortcut, the Linux desktop action) hands its request to the window that is
//! already open instead of opening another one, and a plain second start
//! shows that window (single instance, item 70: also when it hides in the
//! tray).
//!
//! The window listens on a loopback TCP port and writes [`FILE`] (its pid, the
//! port and a random token) into Superba's data directory, which only its user
//! can read. A request is one line, `superba-gui/1 <token> <command>`, answered
//! with `ok`. A file left behind by a window that is gone fails to connect; the
//! caller then does the work itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The instance file in Superba's data directory.
pub const FILE: &str = "gui-instance.json";

/// First word of a request line.
const PROTOCOL: &str = "superba-gui/1";

/// How long a forward may take to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// How long either side waits for the other's line.
const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// Longest request line read.
const MAX_LINE: u64 = 512;

/// What a second start asks the running window to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Request {
    /// Open ChatGPT through Superba (or reopen it, or bring it forward).
    LaunchChatGpt,
    /// Show the window and bring it to the front: a plain second start of
    /// `superba` while this one runs, e.g. hidden in the tray (item 70).
    Show,
    /// A login start only acknowledges the existing process; never shows it.
    Autostart,
}

impl Request {
    /// The command word on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Request::LaunchChatGpt => "launch-chatgpt",
            Request::Show => "show",
            Request::Autostart => "autostart",
        }
    }

    /// Parse a command word.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "launch-chatgpt" => Some(Request::LaunchChatGpt),
            "show" => Some(Request::Show),
            "autostart" => Some(Request::Autostart),
            _ => None,
        }
    }
}

/// The contents of [`FILE`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Process id of the window.
    pub pid: u32,
    /// Loopback port it listens on.
    pub port: u16,
    /// Token a request must carry.
    pub token: String,
}

impl Endpoint {
    fn to_json(&self) -> String {
        serde_json::json!({ "pid": self.pid, "port": self.port, "token": self.token }).to_string()
    }

    fn from_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            port: u16::try_from(v.get("port")?.as_u64()?)
                .ok()
                .filter(|p| *p != 0)?,
            token: v
                .get("token")?
                .as_str()
                .filter(|t| t.len() >= 16)?
                .to_owned(),
        })
    }

    /// Read the endpoint of the window registered in `data_dir`.
    pub fn read(data_dir: &Path) -> Option<Self> {
        Self::from_json(&std::fs::read_to_string(data_dir.join(FILE)).ok()?)
    }
}

/// Requests of other starts, while the window lives. Dropping it stops
/// listening and removes [`FILE`] (unless a newer window wrote its own).
#[derive(Debug)]
pub struct Listener {
    path: PathBuf,
    endpoint: Endpoint,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    /// What this window registered.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

/// Listen for requests and register this process in `data_dir`.
/// `on_request` runs on the listener's thread for every valid request.
pub fn listen(
    data_dir: &Path,
    on_request: impl Fn(Request) + Send + 'static,
) -> std::io::Result<Listener> {
    std::fs::create_dir_all(data_dir)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let endpoint = Endpoint {
        pid: std::process::id(),
        port: listener.local_addr()?.port(),
        token: uuid::Uuid::new_v4().simple().to_string(),
    };
    let path = data_dir.join(FILE);
    write_private(&path, endpoint.to_json().as_bytes())?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = Arc::clone(&stop);
        let token = endpoint.token.clone();
        std::thread::Builder::new()
            .name("superba-instance".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream
                        && let Some(request) = answer(stream, &token)
                    {
                        on_request(request);
                    }
                }
            })?
    };
    Ok(Listener {
        path,
        endpoint,
        stop,
        thread: Some(thread),
    })
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept.
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.endpoint.port));
        let _ = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // Another window may have registered since: leave its file alone.
        if Endpoint::read(self.path.parent().unwrap_or(Path::new(".")))
            .is_some_and(|e| e.token == self.endpoint.token)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Read one request from `stream`; answer `ok` when it carries `token` and a
/// known command (returned), else `denied`.
fn answer(stream: TcpStream, token: &str) -> Option<Request> {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let mut line = String::new();
    let mut reader = BufReader::new((&stream).take(MAX_LINE));
    reader.read_line(&mut line).ok()?;
    let request = parse_line(&line, token);
    let mut stream = stream;
    let _ = stream.write_all(if request.is_some() {
        b"ok\n"
    } else {
        b"denied\n"
    });
    request
}

/// The request of `line` when it is `superba-gui/1 <token> <command>` with
/// this `token`.
pub fn parse_line(line: &str, token: &str) -> Option<Request> {
    let mut parts = line.trim_end_matches(['\r', '\n']).split(' ');
    let (protocol, sent, command) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || protocol != PROTOCOL || !same(sent.as_bytes(), token.as_bytes()) {
        return None;
    }
    Request::parse(command)
}

/// Constant-time comparison (the token is a secret).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Hand `request` to the window registered in `data_dir`. Returns its pid
/// once it accepted the request; `None` when no window is registered, the
/// registered one is gone, or it refused.
pub fn forward(data_dir: &Path, request: Request) -> Option<u32> {
    let endpoint = Endpoint::read(data_dir)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    // Windows lets only the foreground process raise a window: this start was
    // the user's click, so pass that right on to the window (it comes forward
    // when the request needs an answer from the user).
    if request != Request::Autostart {
        allow_foreground(endpoint.pid);
    }
    stream
        .write_all(format!("{PROTOCOL} {} {}\n", endpoint.token, request.as_str()).as_bytes())
        .ok()?;
    let mut reply = String::new();
    BufReader::new((&stream).take(MAX_LINE))
        .read_line(&mut reply)
        .ok()?;
    (reply.trim() == "ok").then_some(endpoint.pid)
}

/// A plain second start of `superba` (item 70): show the window of the
/// Superba that already runs for this user, also when it hides in the tray.
/// `true` when one took the request (this start then ends).
pub fn show_running() -> bool {
    let Ok(context) = superba_switch::detect_context() else {
        return false;
    };
    forward(context.data_dir.root(), Request::Show).is_some()
}

#[cfg(windows)]
fn allow_foreground(pid: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    // SAFETY: plain Win32 call with a process id; failure only means the
    // window may stay behind.
    unsafe {
        AllowSetForegroundWindow(pid);
    }
}

#[cfg(not(windows))]
fn allow_foreground(_pid: u32) {}

/// Write `bytes` to `path` through a temporary file, readable by this user
/// only on Unix (the data directory already is on Windows).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Process-lifetime lock acquired before creating a window. IPC handles later
/// starts; this lock also covers two starts racing before the listener exists.
#[derive(Debug)]
pub struct InstanceLock(std::fs::File);

impl InstanceLock {
    /// Try to become the one GUI for this data directory. The lock file is
    /// intentionally kept after exit: unlinking a locked file permits races.
    pub fn acquire(data_dir: &Path) -> std::io::Result<Option<Self>> {
        std::fs::create_dir_all(data_dir)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(data_dir.join("gui-instance.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }
}
impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Hold the startup lock or deliver a request to its owner. A bounded wait
/// covers the owner's renderer initialization; an unreachable owner is an
/// error, never permission to create a duplicate GUI.
pub fn claim(data_dir: &Path, request: Request) -> std::io::Result<Option<InstanceLock>> {
    let until = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if forward(data_dir, request).is_some() {
            return Ok(None);
        }
        if let Some(lock) = InstanceLock::acquire(data_dir)? {
            return Ok(Some(lock));
        }
        if std::time::Instant::now() >= until {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the running Superba did not answer startup IPC",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
