//! One process at a time, straight from the OS: the facts a quit report names
//! (image path, architecture, creation time, whether it still runs) and ending
//! a process without starting another program (user bug 53).
//!
//! Windows:
//!
//! - Facts come from a handle opened with `PROCESS_QUERY_LIMITED_INFORMATION |
//!   SYNCHRONIZE`, the access every process of the same user grants:
//!   `QueryFullProcessImageNameW` (the kernel's image name, no loader data of
//!   the target is read), `GetProcessTimes`, `GetProcessInformation(
//!   ProcessMachineTypeInfo)` (Windows 11; x64 under emulation on ARM64 is told
//!   apart from native ARM64 there) else `IsWow64Process2`, and a zero wait on
//!   the handle (a process that exited but is still held open by a handle is
//!   reported as not running).
//! - Ending: `OpenProcess(PROCESS_TERMINATE | ...)`, the creation time checked
//!   on that same handle (a pid reused by another program is never ended),
//!   `TerminateProcess`, then a bounded wait on the handle. Every failure keeps
//!   its stage and OS error, so a report says why a process could not be ended.
//!   `taskkill.exe` (what `sysinfo` runs) is not used: one child program per
//!   process is slow under x64 emulation and its result is lost.
//!
//! Elsewhere the facts come from `sysinfo` and ending is SIGKILL.

use std::path::PathBuf;
use std::time::Duration;

use superba_core::Timestamp;

/// What the OS says about one process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcFacts {
    /// Full image path.
    pub image: Option<PathBuf>,
    /// Architecture the process runs as (`x64`, `arm64`, `x86`, `arm`).
    pub arch: Option<&'static str>,
    /// Creation time (millisecond resolution on Windows).
    pub created: Option<Timestamp>,
    /// `Some(false)` when the process object exists but the process exited
    /// (held open by someone's handle); `None` when unknown.
    pub running: Option<bool>,
    /// Why the process could not be opened (e.g. `access denied (os error 5)`).
    pub open_error: Option<String>,
    /// No process has this pid (any more).
    pub gone: bool,
}

/// How ending one process went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndResult {
    /// Ended (or exited on its own meanwhile) and confirmed gone.
    Ended,
    /// Not running any more before Superba acted.
    AlreadyGone,
    /// The pid belongs to another program now (different creation time): left alone.
    Reused,
    /// Terminated, but still running after the wait.
    StillRunning,
    /// Could not be ended.
    Failed {
        /// `identity`, `open`, `terminate` or `wait`.
        stage: &'static str,
        /// OS error text.
        error: String,
    },
}

impl EndResult {
    /// True when the process is gone (ended, already gone or reused).
    pub fn is_gone(&self) -> bool {
        matches!(
            self,
            EndResult::Ended | EndResult::AlreadyGone | EndResult::Reused
        )
    }

    /// Short English description for logs and reports.
    pub fn describe(&self) -> String {
        match self {
            EndResult::Ended => "ended".into(),
            EndResult::AlreadyGone => "already gone".into(),
            EndResult::Reused => "pid reused by another program; left alone".into(),
            EndResult::StillRunning => "still running after TerminateProcess".into(),
            EndResult::Failed { stage, error } => format!("{stage} failed: {error}"),
        }
    }
}

/// Start times recorded with second resolution (sysinfo) match a creation
/// time within this slack.
#[cfg(not(windows))]
const SLACK_MS: i64 = crate::procs::START_TIME_SLACK_MS;

/// True when `created` (from the OS now) is the process recorded as `started`.
pub fn same_process(started: Option<Timestamp>, created: Option<Timestamp>) -> bool {
    match (started, created) {
        // Windows snapshots use the native creation time, so a PID reused even
        // within sysinfo's two-second slack must not match the old process.
        #[cfg(windows)]
        (Some(a), Some(b)) => a == b,
        #[cfg(not(windows))]
        (Some(a), Some(b)) => (a.as_millis() - b.as_millis()).abs() <= SLACK_MS,
        _ => true,
    }
}

/// Native creation time for Windows snapshots, before a PID can be selected
/// for termination. Unknown identity must never authorize ending a process.
#[cfg(windows)]
pub(crate) fn creation_time(pid: u32) -> Option<Timestamp> {
    imp::creation_time(pid)
}

/// Facts about `pid` (see the module docs).
pub fn facts(pid: u32) -> ProcFacts {
    imp::facts(pid)
}

/// End `pid` if it is still the process recorded with `started` (see the
/// module docs); waits up to `wait` for it to be gone.
pub fn end(pid: u32, started: Option<Timestamp>, wait: Duration) -> EndResult {
    imp::end(pid, started, wait)
}

/// The architecture this process runs as (`x64`, `arm64`, ...).
pub fn own_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        "arm" => "arm",
        other => other,
    }
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::time::Duration;

    use superba_core::Timestamp;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, HANDLE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessInformation, GetProcessTimes, OpenProcess, OpenProcessToken,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        ProcessMachineTypeInfo, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };

    use super::{EndResult, ProcFacts};
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };

    /// `SYNCHRONIZE` standard access right (waiting on the process handle).
    const SYNCHRONIZE: u32 = 0x0010_0000;

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcess and is closed once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn last_error() -> (u32, String) {
        // SAFETY: no preconditions.
        let code = unsafe { GetLastError() };
        (
            code,
            std::io::Error::from_raw_os_error(code as i32).to_string(),
        )
    }

    fn open(pid: u32, access: u32) -> Result<Handle, (u32, String)> {
        // SAFETY: plain call; a null result is an error.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            Err(last_error())
        } else {
            Ok(Handle(h))
        }
    }

    fn image(h: &Handle) -> Option<PathBuf> {
        let mut buf = vec![0_u16; 32_768];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` UTF-16 units; `len` is updated on success.
        let ok = unsafe {
            QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        (ok != 0).then(|| {
            let s = String::from_utf16_lossy(&buf[..len as usize]);
            crate::appx::plain_path(std::path::Path::new(&s))
        })
    }

    fn filetime_ms(ft: FILETIME) -> Option<Timestamp> {
        let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
        // 100 ns ticks since 1601-01-01; the Unix epoch is 11644473600 s later.
        let ms = (ticks / 10_000).checked_sub(11_644_473_600_000)?;
        Some(Timestamp::from_millis(i64::try_from(ms).ok()?))
    }

    fn created(h: &Handle) -> Option<Timestamp> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
        // SAFETY: four valid out pointers.
        let ok = unsafe { GetProcessTimes(h.0, &mut c, &mut e, &mut k, &mut u) };
        if ok == 0 {
            return None;
        }
        filetime_ms(c)
    }

    fn user(h: HANDLE) -> Result<Vec<usize>, String> {
        let mut token = std::ptr::null_mut();
        // SAFETY: the process handle is valid and token is a valid out pointer.
        if unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_error().1);
        }
        let token = Handle(token);
        let mut size = 0;
        // SAFETY: a null buffer asks for its required size.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut size) };
        if size == 0 {
            return Err(last_error().1);
        }
        // TOKEN_USER contains pointers, so the storage must be aligned.
        let mut data = vec![0_usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: aligned writable buffer with at least size bytes.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                data.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err(last_error().1);
        }
        Ok(data)
    }

    fn owned_by_us(h: &Handle) -> Result<bool, String> {
        let theirs = user(h.0)?;
        // SAFETY: GetCurrentProcess has no preconditions; the returned pseudo
        // handle is borrowed, never closed.
        let ours = user(unsafe { GetCurrentProcess() })?;
        // SAFETY: both buffers were filled with TOKEN_USER by Windows and stay
        // alive while their embedded SID pointers are compared.
        Ok(unsafe {
            EqualSid(
                (*(theirs.as_ptr().cast::<TOKEN_USER>())).User.Sid,
                (*(ours.as_ptr().cast::<TOKEN_USER>())).User.Sid,
            ) != 0
        })
    }

    /// `PROCESS_MACHINE_INFORMATION` (declared here: the windows-sys type sits
    /// behind a feature the workspace does not enable).
    #[repr(C)]
    #[derive(Default)]
    struct MachineInfo {
        process_machine: u16,
        res0: u16,
        machine_attributes: u32,
    }

    fn arch(h: &Handle) -> Option<&'static str> {
        let mut info = MachineInfo::default();
        // SAFETY: `info` matches PROCESS_MACHINE_INFORMATION in size and layout.
        let ok = unsafe {
            GetProcessInformation(
                h.0,
                ProcessMachineTypeInfo,
                (&raw mut info).cast(),
                std::mem::size_of::<MachineInfo>() as u32,
            )
        };
        if ok != 0 {
            return crate::package::machine_arch(info.process_machine);
        }
        // Before Windows 11: WOW64 processes report their machine, every other
        // process runs as the native one (no x64 emulation there).
        wow64_arch(h)
    }

    fn wow64_arch(h: &Handle) -> Option<&'static str> {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        type IsWow64Process2 = unsafe extern "system" fn(HANDLE, *mut u16, *mut u16) -> i32;
        let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
        // SAFETY: kernel32 is always loaded; the symbol, when present, has the
        // documented signature; both out pointers are valid locals.
        unsafe {
            let module = GetModuleHandleW(kernel32.as_ptr());
            if module.is_null() {
                return None;
            }
            let f = GetProcAddress(module, c"IsWow64Process2".as_ptr().cast())?;
            let f = std::mem::transmute::<unsafe extern "system" fn() -> isize, IsWow64Process2>(f);
            let (mut process, mut native) = (0_u16, 0_u16);
            if f(h.0, &raw mut process, &raw mut native) == 0 {
                return None;
            }
            crate::package::machine_arch(if process == 0 { native } else { process })
        }
    }

    fn running(h: &Handle) -> Option<bool> {
        // SAFETY: valid handle opened with SYNCHRONIZE.
        match unsafe { WaitForSingleObject(h.0, 0) } {
            WAIT_TIMEOUT => Some(true),
            WAIT_OBJECT_0 => Some(false),
            _ => None,
        }
    }

    pub(super) fn creation_time(pid: u32) -> Option<Timestamp> {
        let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
        created(&h)
    }

    pub(super) fn facts(pid: u32) -> ProcFacts {
        match open(pid, PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)
            .or_else(|_| open(pid, PROCESS_QUERY_LIMITED_INFORMATION))
        {
            Ok(h) => ProcFacts {
                image: image(&h),
                arch: arch(&h),
                created: created(&h),
                running: running(&h),
                open_error: None,
                gone: false,
            },
            Err((code, _)) if code == ERROR_INVALID_PARAMETER => ProcFacts {
                gone: true,
                ..ProcFacts::default()
            },
            Err((_, text)) => ProcFacts {
                open_error: Some(text),
                ..ProcFacts::default()
            },
        }
    }

    pub(super) fn end(pid: u32, started: Option<Timestamp>, wait: Duration) -> EndResult {
        let h = match open(
            pid,
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
        ) {
            Ok(h) => h,
            // No such process (any more).
            Err((code, _)) if code == ERROR_INVALID_PARAMETER => return EndResult::AlreadyGone,
            Err((_, text)) => {
                return EndResult::Failed {
                    stage: "open",
                    error: text,
                };
            }
        };
        let created = created(&h);
        if !super::same_process(started, created) {
            return EndResult::Reused;
        }
        if running(&h) == Some(false) {
            return EndResult::AlreadyGone;
        }
        if started.is_none() || created.is_none() {
            return EndResult::Failed {
                stage: "identity",
                error: "process creation time is unknown; left alone".into(),
            };
        }
        match owned_by_us(&h) {
            Ok(true) => {}
            Ok(false) => {
                return EndResult::Failed {
                    stage: "identity",
                    error: "process belongs to another Windows user; left alone".into(),
                };
            }
            Err(error) => {
                return EndResult::Failed {
                    stage: "identity",
                    error,
                };
            }
        }
        // SAFETY: valid handle opened with PROCESS_TERMINATE.
        if unsafe { TerminateProcess(h.0, 1) } == 0 {
            let (_, text) = last_error();
            // It may have exited between the check and the call.
            if running(&h) == Some(false) {
                return EndResult::Ended;
            }
            return EndResult::Failed {
                stage: "terminate",
                error: text,
            };
        }
        let ms = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid handle opened with SYNCHRONIZE.
        match unsafe { WaitForSingleObject(h.0, ms) } {
            WAIT_OBJECT_0 => EndResult::Ended,
            WAIT_TIMEOUT => EndResult::StillRunning,
            _ => EndResult::Failed {
                stage: "wait",
                error: last_error().1,
            },
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::time::Duration;

    use superba_core::Timestamp;

    use super::{EndResult, ProcFacts};
    use crate::procs::{self, Liveness};

    pub(super) fn facts(pid: u32) -> ProcFacts {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let mut sys = System::new();
        let spid = Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[spid]),
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
        match sys.process(spid) {
            Some(p) => ProcFacts {
                image: p.exe().map(std::path::Path::to_path_buf),
                arch: None,
                created: (p.start_time() > 0).then(|| {
                    Timestamp::from_millis(i64::try_from(p.start_time()).unwrap_or(0) * 1000)
                }),
                running: Some(procs::liveness(pid, None) == Liveness::Alive),
                open_error: None,
                gone: false,
            },
            None => ProcFacts {
                running: Some(false),
                gone: true,
                ..ProcFacts::default()
            },
        }
    }

    pub(super) fn end(pid: u32, started: Option<Timestamp>, wait: Duration) -> EndResult {
        if procs::liveness(pid, None) == Liveness::Gone {
            return EndResult::AlreadyGone;
        }
        if procs::liveness(pid, started) == Liveness::Gone {
            return EndResult::Reused;
        }
        if !procs::kill(pid) && procs::liveness(pid, started) == Liveness::Alive {
            return EndResult::Failed {
                stage: "terminate",
                error: std::io::Error::last_os_error().to_string(),
            };
        }
        let deadline = std::time::Instant::now() + wait;
        loop {
            if procs::liveness(pid, started) == Liveness::Gone {
                return EndResult::Ended;
            }
            if std::time::Instant::now() >= deadline {
                return EndResult::StillRunning;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
