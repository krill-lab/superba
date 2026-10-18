//! Start a background process that inherits nothing of ours but what it is
//! given (user bug 54 (c)).
//!
//! `spb router install` hung when its output was captured (`spb router install
//! | Out-String`, `$out = spb …` in PowerShell): `std::process::Command` creates
//! every Windows child with `bInheritHandles = TRUE`, so the supervisor inherited
//! every inheritable handle of `spb` — among them the pipe PowerShell reads
//! `spb`'s output from — and passed it on to the router. The pipe never reached
//! end-of-file while the router ran, so the pipeline waited for it.
//!
//! - **Windows**: `CreateProcessW` with an explicit
//!   `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` that names the null device (stdin) and
//!   the log file (stdout, stderr) and nothing else, `CREATE_NO_WINDOW |
//!   CREATE_NEW_PROCESS_GROUP`. Program, arguments, added variables and the
//!   working directory come from the [`Command`] (built through
//!   `superba_core::process`).
//! - **Unix**: a session of its own (`setsid`: no controlling terminal, no
//!   hang-up when the terminal closes), stdin from `/dev/null`, stdout and stderr
//!   to the log. Our standard streams are replaced in the child and every other
//!   descriptor Rust opens is close-on-exec, so nothing else is inherited.

use std::fs::File;
use std::io;
use std::process::Command;

/// Start `command` in the background with stdin from the null device and
/// stdout + stderr appended to `log`, inheriting no other handle of ours.
/// Returns its pid. The child is never waited for by the caller (Unix reaps it
/// on a thread).
pub(super) fn spawn(command: Command, log: File) -> io::Result<u32> {
    imp::spawn(command, log)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    pub(super) fn spawn(mut command: Command, log: File) -> io::Result<u32> {
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // SAFETY: the closure runs in the forked child before exec and only calls
        // setsid(2), which is async-signal-safe and touches no shared memory.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, UpdateProcThreadAttribute,
    };

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().collect()
    }

    fn nul_terminated(s: &OsStr) -> io::Result<Vec<u16>> {
        let mut w = wide(s);
        if w.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a NUL character in a process argument",
            ));
        }
        w.push(0);
        Ok(w)
    }

    /// One argument quoted for `CommandLineToArgvW` (backslashes doubled before
    /// a quote or the end of a quoted argument).
    fn push_quoted(out: &mut Vec<u16>, arg: &[u16]) {
        let needs = arg.is_empty()
            || arg
                .iter()
                .any(|&c| c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"'));
        if !needs {
            out.extend_from_slice(arg);
            return;
        }
        out.push(u16::from(b'"'));
        let mut slashes = 0usize;
        for &c in arg {
            if c == u16::from(b'\\') {
                slashes += 1;
                continue;
            }
            let n = if c == u16::from(b'"') {
                slashes * 2 + 1
            } else {
                slashes
            };
            out.extend(std::iter::repeat_n(u16::from(b'\\'), n));
            out.push(c);
            slashes = 0;
        }
        out.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
        out.push(u16::from(b'"'));
    }

    /// The environment block for `command`: ours with its additions and
    /// removals, sorted case-insensitively; `None` = inherit ours unchanged.
    fn env_block(command: &Command) -> io::Result<Option<Vec<u16>>> {
        let changes: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        if changes.is_empty() {
            return Ok(None);
        }
        let upper = |k: &OsStr| k.to_string_lossy().to_uppercase();
        let mut vars: BTreeMap<String, (OsString, OsString)> = std::env::vars_os()
            .map(|(k, v)| (upper(&k), (k, v)))
            .collect();
        for (k, v) in changes {
            match v {
                Some(v) => {
                    vars.insert(upper(k), (k.to_owned(), v.to_owned()));
                }
                None => {
                    vars.remove(&upper(k));
                }
            }
        }
        let mut block = Vec::new();
        for (k, v) in vars.values() {
            let (k, v) = (wide(k), wide(v));
            if k.is_empty() || k.contains(&0) || v.contains(&0) || k[1..].contains(&u16::from(b'='))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "an invalid environment variable",
                ));
            }
            block.extend(k);
            block.push(u16::from(b'='));
            block.extend(v);
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        Ok(Some(block))
    }

    fn inheritable(file: &File) -> io::Result<HANDLE> {
        let handle = file.as_raw_handle() as HANDLE;
        // SAFETY: `handle` is a valid handle owned by `file` for this call.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }

    /// An initialized attribute list with room for one attribute.
    struct AttributeList {
        /// usize-aligned storage for the opaque list.
        buffer: Vec<usize>,
    }

    impl AttributeList {
        fn new() -> io::Result<Self> {
            let mut size = 0usize;
            // SAFETY: a size query (null list) only writes `size`.
            unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut list = Self {
                buffer: vec![0usize; size.div_ceil(std::mem::size_of::<usize>())],
            };
            // SAFETY: the buffer holds at least `size` bytes, as asked for above.
            if unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(list)
        }

        fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.buffer.as_mut_ptr().cast()
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            // SAFETY: the list was initialized by `new`.
            unsafe { DeleteProcThreadAttributeList(self.ptr()) };
        }
    }

    pub(super) fn spawn(command: Command, log: File) -> io::Result<u32> {
        let program = command.get_program();
        let application = nul_terminated(program)?;
        let mut line = Vec::new();
        push_quoted(&mut line, &wide(program));
        for arg in command.get_args() {
            line.push(u16::from(b' '));
            push_quoted(&mut line, &wide(arg));
        }
        if line.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a NUL character in a process argument",
            ));
        }
        line.push(0);
        let env = env_block(&command)?;
        let cwd = command
            .get_current_dir()
            .map(|d| nul_terminated(d.as_os_str()))
            .transpose()?;

        let nul = File::open("NUL")?;
        let stdin = inheritable(&nul)?;
        let output = inheritable(&log)?;
        let handles: [HANDLE; 2] = [stdin, output];
        let mut attributes = AttributeList::new()?;
        // SAFETY: `handles` outlives the CreateProcessW call below (the list
        // stores the pointer, not a copy) and the list has room for one entry.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(&handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin;
        startup.StartupInfo.hStdOutput = output;
        startup.StartupInfo.hStdError = output;
        startup.lpAttributeList = attributes.ptr();
        let mut info = PROCESS_INFORMATION::default();
        let mut flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | EXTENDED_STARTUPINFO_PRESENT;
        if env.is_some() {
            flags |= CREATE_UNICODE_ENVIRONMENT;
        }
        // SAFETY: every pointer is valid for the call: NUL-terminated wide
        // strings, a mutable command line, the environment block (or null), the
        // extended startup information whose list and handles are alive.
        let ok = unsafe {
            CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                flags,
                env.as_ref().map_or(std::ptr::null(), |e| e.as_ptr().cast()),
                cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                (&raw const startup).cast(),
                &mut info,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles were returned by CreateProcessW and are ours.
        unsafe {
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
        }
        drop(attributes);
        drop(nul);
        drop(log);
        Ok(info.dwProcessId)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub(super) fn spawn(mut command: Command, log: File) -> io::Result<u32> {
        use std::process::Stdio;
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        Ok(command.spawn()?.id())
    }
}
